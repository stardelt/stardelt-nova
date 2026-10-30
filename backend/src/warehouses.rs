//! Which SQL warehouse a person uses, and what it allows (stardelt-nova#236, warehouses 3/3;
//! design note `design/multitenant-catalogs.md` §3.4, "Nova's job is to show the classes your
//! groups hold `use` on, non-authoritative").
//!
//! `GET /api/warehouses` answers the `SqlWarehouse` objects of the platform namespace as rows:
//! the name, the display name, the class size, the three numbers the class renders with
//! (running, queued, memory share), whether it is the default, whether the caller may use it
//! and which row the caller will actually land in.
//!
//! - **Non-authoritative.** The engine picks the class, not Nova: `access-sync-job.py` renders
//!   one `userGroup: wh-<name>` selector per warehouse, largest first, and the first one the
//!   authenticated identity matches wins. Nova repeats that order to answer `yours`, so a
//!   person sees what they will get — bypassing Nova and calling Trino directly changes
//!   nothing, because the selection reads the identity, never a header.
//! - **`canUse`** is one of the caller's `identity.groups` in `spec.grants[].group`, matched by
//!   the exact name: the group file the selectors match is written from the same names.
//! - **`yours`** is the first `canUse` row in the answer's own order — the first selector the
//!   engine reads — or, when nothing is granted, the one `default: true` warehouse, which is
//!   the engine's catch-all. Exactly one row carries it.
//! - **Administrators** land in the `admin` root group the sync renders (100 % memory, 50
//!   running, 100 queued) before any warehouse selector matches, so one synthetic row for it
//!   is prepended and every other row answers `yours: false`. It is not a `SqlWarehouse`
//!   object and carries no size; `canUse` on the other rows still follows the grants.
//! - **The numbers** are the class defaults — xs 2/10/10 %, small 10/100/20 %, medium 5/50/40 %,
//!   large 6/70/70 % — each field overridable by `spec.concurrency`, the same table and merge
//!   the access-sync job uses. An absent field keeps the class value; a size the table does
//!   not know falls back to `small`, the CRD's own default.
//! - **No cluster** (demo mode, a local run, a pod without RBAC): one `small` row, usable and
//!   in use. The demo lakehouse has exactly one warehouse; the shape stays the same, so the
//!   UI renders identically with and without a cluster.
//! - A cluster that refuses or fails answers **502** `the cluster did not answer` (rule U12),
//!   never a 500 and never an empty list that would read as "no warehouses exist".
//!
//! The route is uncacheable (`Cache-Control: no-store`): it answers per caller.

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{Identity, Principal};
use crate::connection_routes::Fail;

/// The class defaults, the table the access-sync job renders resource groups from
/// (`scripts/access-sync-job.py`, `CLASS_DEFAULTS`): `(size, running, queued, memoryShare)`.
const CLASS_DEFAULTS: [(&str, u32, u32, &str); 4] = [
    ("xs", 2, 10, "10%"),
    ("small", 10, 100, "20%"),
    ("medium", 5, 50, "40%"),
    ("large", 6, 70, "70%"),
];

/// The class the CRD's `size` defaults to, and the fallback for an object that names none or
/// one the table does not know.
const DEFAULT_SIZE: &str = "small";

/// Largest first: the order the access-sync job renders its selectors in, so the first
/// granted class a person matches is the one the engine picks.
const SIZE_ORDER: [&str; 4] = ["large", "medium", "small", "xs"];

/// The `admin` root group the sync renders (`ADMIN_ROOT_GROUP`): the class every
/// administrator lands in, before any warehouse selector matches.
const ADMIN_ROW: (&str, &str, u32, u32, &str) = ("admin", "Administrators", 50, 100, "100%");

/// One warehouse a caller may use, as the UI shows it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SqlWarehouse {
    /// `metadata.name`: the `SqlWarehouse` object, and the `wh-<name>` resource group.
    pub name: String,
    /// `spec.displayName`, or `null` when the object carries none. The UI falls back to
    /// `name`; Nova never invents a label.
    pub display_name: Option<String>,
    /// `spec.size` (`xs`, `small`, `medium`, `large`). `null` for the synthetic administrator
    /// row, which is not a class of its own.
    pub size: Option<String>,
    /// Queries that run at the same time (the resource group's hard concurrency limit).
    pub running: u32,
    /// Queries that may wait (the resource group's queue limit).
    pub queued: u32,
    /// Share of the engine's memory the class may use, for example `20%`.
    pub memory_share: String,
    /// `spec.default`: the class a query lands in when no selector matches.
    pub default: bool,
    /// One of the caller's groups is granted this class (`spec.grants`). Non-authoritative:
    /// the engine decides, this only fails before the person tries.
    pub can_use: bool,
    /// The row this caller will land in: the largest usable class, or the default one.
    pub yours: bool,
}

/// The `SqlWarehouse` resources of the platform namespace, through the cluster client.
fn warehouses_api(client: kube::Client) -> Api<DynamicObject> {
    Api::namespaced_with(
        client,
        &crate::cluster::namespace(),
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "SqlWarehouse",
        )),
    )
}

/// The three numbers one class renders with: the table row, or the `small` row for a size the
/// table does not know.
fn class_defaults(size: &str) -> (u32, u32, &'static str) {
    CLASS_DEFAULTS
        .iter()
        .find(|(name, ..)| *name == size)
        .or_else(|| {
            CLASS_DEFAULTS
                .iter()
                .find(|(name, ..)| *name == DEFAULT_SIZE)
        })
        .map(|(_, running, queued, share)| (*running, *queued, *share))
        .expect("the table holds the default size")
}

/// The size of one object: `spec.size`, or the CRD's default when the object names none.
fn size_of(spec: &Value) -> &str {
    spec.get("size")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_SIZE)
}

/// A positive whole number from the object, or the class default: a field that is absent,
/// `null` or not a number keeps the class value, exactly like the access-sync job's merge.
fn number(value: Option<&Value>, fallback: u32) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(fallback)
}

/// The numbers one object renders with: the class defaults, each overridden by
/// `spec.concurrency` (`running`, `queued`, `memoryShare`).
fn numbers(size: &str, concurrency: Option<&Value>) -> (u32, u32, String) {
    let (running, queued, share) = class_defaults(size);
    let field = |key: &str| concurrency.and_then(|c| c.get(key));
    (
        number(field("running"), running),
        number(field("queued"), queued),
        field("memoryShare")
            .and_then(Value::as_str)
            .unwrap_or(share)
            .to_string(),
    )
}

/// Whether one of the caller's groups is granted this class. The comparison is by the exact
/// group name: the sync writes the same names into the group file the selectors match.
fn can_use(groups: &[String], spec: &Value) -> bool {
    let Some(grants) = spec.get("grants").and_then(Value::as_array) else {
        return false;
    };
    grants.iter().any(|grant| {
        grant
            .get("group")
            .and_then(Value::as_str)
            .is_some_and(|granted| groups.iter().any(|group| group == granted))
    })
}

/// The position of one row's class in [`SIZE_ORDER`]; a size the order does not know sorts
/// last, after every class it does.
fn size_rank(size: Option<&str>) -> usize {
    size.and_then(|size| SIZE_ORDER.iter().position(|known| *known == size))
        .unwrap_or(SIZE_ORDER.len())
}

/// One `SqlWarehouse` object as a row, without `yours`: that is decided for the whole list.
fn row(object: &Value, identity: &Identity) -> SqlWarehouse {
    let spec = object.get("spec").unwrap_or(&Value::Null);
    let size = size_of(spec);
    let (running, queued, memory_share) = numbers(size, spec.get("concurrency"));
    SqlWarehouse {
        name: object
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        display_name: spec
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::to_string),
        size: Some(size.to_string()),
        running,
        queued,
        memory_share,
        default: spec
            .get("default")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        can_use: can_use(&identity.groups, spec),
        yours: false,
    }
}

/// The administrator's synthetic row: the `admin` root group access-sync renders, not a
/// `SqlWarehouse` object.
fn admin_row() -> SqlWarehouse {
    let (name, display_name, running, queued, memory_share) = ADMIN_ROW;
    SqlWarehouse {
        name: name.to_string(),
        display_name: Some(display_name.to_string()),
        size: None,
        running,
        queued,
        memory_share: memory_share.to_string(),
        default: false,
        can_use: true,
        yours: true,
    }
}

/// The whole answer for these objects and this caller: largest class first, name order inside
/// one size, the administrator's row first of all, `yours` on exactly one row.
fn rows(objects: &[Value], identity: &Identity) -> Vec<SqlWarehouse> {
    let mut rows: Vec<SqlWarehouse> = objects.iter().map(|object| row(object, identity)).collect();
    rows.sort_by(|a, b| {
        size_rank(a.size.as_deref())
            .cmp(&size_rank(b.size.as_deref()))
            .then_with(|| a.name.cmp(&b.name))
    });

    if identity.is_admin {
        // The admin selector is rendered before every warehouse selector, so an administrator
        // never lands in a class: the synthetic row is the one in use and no other is.
        for row in rows.iter_mut() {
            row.yours = false;
        }
        rows.insert(0, admin_row());
        return rows;
    }

    // `yours` is the first usable row in this order (the engine's first matching selector) or,
    // with nothing granted, the default class (the engine's catch-all).
    if let Some(index) = rows
        .iter()
        .position(|row| row.can_use)
        .or_else(|| rows.iter().position(|row| row.default))
    {
        rows[index].yours = true;
    }
    rows
}

/// The one-row answer without a cluster (demo mode, a local run): the demo lakehouse has one
/// warehouse, `small`, and it is the class a query lands in.
fn demo_rows() -> Vec<SqlWarehouse> {
    let (running, queued, memory_share) = class_defaults(DEFAULT_SIZE);
    vec![SqlWarehouse {
        name: DEFAULT_SIZE.to_string(),
        display_name: Some(DEFAULT_SIZE.to_string()),
        size: Some(DEFAULT_SIZE.to_string()),
        running,
        queued,
        memory_share: memory_share.to_string(),
        default: true,
        can_use: true,
        yours: true,
    }]
}

/// Who the caller is. `dev` mode has no IdP: the single user is the operator, an
/// administrator with no groups — exactly like `admin_catalogs::actor`.
fn actor(principal: Option<Extension<Principal>>, state: &AppState) -> Identity {
    match principal {
        Some(Extension(principal)) => principal.identity,
        None => Identity {
            sub: state.dev_user.clone(),
            display_name: state.dev_user.clone(),
            email: None,
            name: None,
            groups: Vec::new(),
            roles: Vec::new(),
            groups_overage: false,
            is_admin: true,
        },
    }
}

/// `GET /api/warehouses`: the compute classes of the platform, which one this caller may use
/// and which one a query will land in.
#[utoipa::path(
    get,
    operation_id = "warehouses_list",
    path = "/api/warehouses",
    tag = "public",
    description = "Every `SqlWarehouse` of the platform namespace, largest class first (`large`, `medium`, `small`, `xs`; name order inside one size), each row carrying the class `size`, the numbers the resource group is rendered with (`running`, `queued`, `memoryShare`: the class defaults, overridden by `spec.concurrency`), `default`, `canUse` (one of the caller's groups is in `spec.grants`) and `yours` (the first usable row — the class the engine's first matching selector picks — or the default class when nothing is granted). An administrator gets one synthetic row first (`name: \"admin\"`, `displayName: \"Administrators\"`, `size: null`, 50/100/100 %: the `admin` root group access-sync renders) with `yours` on it and `yours: false` on every other row. Non-authoritative: the engine selects by identity, Nova only shows it. Without a cluster connection (demo mode, a local run) the answer is the one `small` class. A cluster that refuses or fails is a 502. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "One row per class, largest first, the administrator's row first of all", body = [SqlWarehouse]),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer, or Nova may not list sqlwarehouses", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let identity = actor(principal, &state);

    let objects = match state.kube.clone() {
        None => None,
        Some(client) => match warehouses_api(client)
            .list(&kube::api::ListParams::default())
            .await
        {
            Ok(list) => Some(list.items),
            Err(e) => {
                // The detail names the API server (and sometimes the ServiceAccount), so it
                // stays in the log; the answer is the generic sentence the other routes use.
                tracing::warn!(error = %e, "sqlwarehouse list failed");
                return Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
                    .into_response();
            }
        },
    };

    let rows = match objects {
        None => demo_rows(),
        Some(objects) => {
            let mut values = Vec::with_capacity(objects.len());
            for object in objects {
                match serde_json::to_value(object) {
                    Ok(value) => values.push(value),
                    Err(e) => {
                        tracing::warn!(error = %e, "sqlwarehouse object is not JSON");
                        return Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
                            .into_response();
                    }
                }
            }
            rows(&values, &identity)
        }
    };

    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(rows),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `SqlWarehouse` object as the API server holds it: `metadata.name` plus a `spec`.
    fn object(name: &str, spec: Value) -> Value {
        json!({ "metadata": { "name": name, "namespace": "stardelt" }, "spec": spec })
    }

    /// A caller with these groups; never an administrator unless a test says so.
    fn person(groups: &[&str]) -> Identity {
        Identity {
            sub: "sub-1".into(),
            display_name: "dev".into(),
            email: None,
            name: None,
            groups: groups.iter().map(|g| (*g).to_string()).collect(),
            roles: Vec::new(),
            groups_overage: false,
            is_admin: false,
        }
    }

    fn administrator() -> Identity {
        Identity {
            is_admin: true,
            ..person(&["stardelt-admins"])
        }
    }

    /// The three seeded classes, as `dev/seed/warehouses.yaml` writes them.
    fn seeded() -> Vec<Value> {
        vec![
            object(
                "small",
                json!({
                    "displayName": "Small", "size": "small", "default": true,
                    "grants": [{"group": "stardelt-users"}, {"group": "stardelt-admins"}],
                }),
            ),
            object(
                "medium",
                json!({ "displayName": "Medium", "size": "medium", "grants": [{"group": "finops"}] }),
            ),
            object(
                "large",
                json!({ "displayName": "Large", "size": "large", "grants": [{"group": "analytics"}] }),
            ),
        ]
    }

    #[test]
    fn class_defaults_table() {
        // The table the access-sync job renders resource groups from, field by field.
        assert_eq!(class_defaults("xs"), (2, 10, "10%"));
        assert_eq!(class_defaults("small"), (10, 100, "20%"));
        assert_eq!(class_defaults("medium"), (5, 50, "40%"));
        assert_eq!(class_defaults("large"), (6, 70, "70%"));
        // A size the table does not know falls back to the CRD's default, never to a panic.
        assert_eq!(class_defaults("huge"), class_defaults("small"));
        assert_eq!(class_defaults(""), class_defaults("small"));
    }

    #[test]
    fn concurrency_overrides_one_field_and_keeps_the_rest() {
        // `running` only: the class's queue limit and memory share stay.
        assert_eq!(
            numbers("large", Some(&json!({ "running": 1 }))),
            (1, 70, "70%".to_string())
        );
        // Every field at once, and a `null` field keeps the class value.
        assert_eq!(
            numbers(
                "medium",
                Some(
                    &json!({ "running": 3, "queued": 7, "memoryShare": "15%", "scheduling": "fifo" })
                )
            ),
            (3, 7, "15%".to_string())
        );
        assert_eq!(
            numbers("medium", Some(&json!({ "running": null, "queued": 7 }))),
            (5, 7, "40%".to_string())
        );
        // Absent `concurrency` (or an object without one) is the class default.
        assert_eq!(numbers("small", None), (10, 100, "20%".to_string()));
        assert_eq!(
            numbers("small", Some(&Value::Null)),
            (10, 100, "20%".to_string())
        );
    }

    #[test]
    fn yours_is_the_largest_usable_class() {
        let answer = rows(&seeded(), &person(&["stardelt-users", "finops"]));
        // Largest first, name order inside a size: the answer's own order.
        assert_eq!(
            answer.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["large", "medium", "small"]
        );
        assert!(!answer[0].can_use, "analytics is not this person's group");
        assert!(answer[1].can_use);
        assert!(answer[2].can_use, "stardelt-users is granted small");
        // Two usable classes: the larger one wins, the smaller stays usable but unused.
        assert_eq!(
            answer
                .iter()
                .filter(|r| r.yours)
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["medium"]
        );

        // Nobody granted anything: the default class is the engine's catch-all, and it is
        // `yours` even though `canUse` is false.
        let answer = rows(&seeded(), &person(&[]));
        assert!(answer.iter().all(|r| !r.can_use));
        assert_eq!(
            answer
                .iter()
                .filter(|r| r.yours)
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["small"]
        );

        // One granted class: that one, wherever it sits in the order.
        let answer = rows(&seeded(), &person(&["analytics"]));
        assert_eq!(
            answer
                .iter()
                .filter(|r| r.yours)
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["large"]
        );
    }

    #[test]
    fn an_administrator_leads_with_the_synthetic_row() {
        let answer = rows(&seeded(), &administrator());
        // Every warehouse is still listed; the admin row is first and is the one in use.
        assert_eq!(
            answer.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["admin", "large", "medium", "small"]
        );
        let admin = &answer[0];
        assert_eq!(admin.display_name.as_deref(), Some("Administrators"));
        assert_eq!(admin.size, None);
        assert_eq!((admin.running, admin.queued), (50, 100));
        assert_eq!(admin.memory_share, "100%");
        assert!(!admin.default);
        assert!(admin.can_use && admin.yours);
        assert_eq!(
            answer.iter().skip(1).filter(|r| r.yours).count(),
            0,
            "only the admin row is `yours`"
        );
        // `canUse` on the warehouses still follows the grants: `small` names the admin group.
        let small = answer.iter().find(|r| r.name == "small").expect("small");
        assert!(small.can_use);
        let large = answer.iter().find(|r| r.name == "large").expect("large");
        assert!(!large.can_use);
    }

    #[test]
    fn grants_match_by_exact_group_name() {
        let spec = json!({ "size": "medium", "grants": [{ "group": "finops" }] });
        assert!(can_use(&["finops".to_string()], &spec));
        assert!(can_use(
            &["finops".to_string(), "analytics".to_string()],
            &spec
        ));
        // A path-prefixed or decorated name is a different group, and so is a substring.
        assert!(!can_use(&["/finops".to_string()], &spec));
        assert!(!can_use(&["finops-team".to_string()], &spec));
        assert!(!can_use(&["finop".to_string()], &spec));
        // No grants at all is nobody's class.
        assert!(!can_use(
            &["finops".to_string()],
            &json!({ "size": "medium" })
        ));
        assert!(!can_use(
            &["finops".to_string()],
            &json!({ "grants": [{ "verb": "use" }] })
        ));
    }

    #[test]
    fn one_row_is_the_documented_json_shape() {
        let answer = rows(&seeded(), &person(&["finops"]));
        let medium = answer.iter().find(|r| r.name == "medium").expect("medium");
        assert_eq!(
            serde_json::to_value(medium).expect("JSON"),
            json!({
                "name": "medium",
                "displayName": "Medium",
                "size": "medium",
                "running": 5,
                "queued": 50,
                "memoryShare": "40%",
                "default": false,
                "canUse": true,
                "yours": true,
            })
        );
        // The synthetic row: `size` is present and null, never omitted.
        assert_eq!(
            serde_json::to_value(admin_row()).expect("JSON"),
            json!({
                "name": "admin",
                "displayName": "Administrators",
                "size": null,
                "running": 50,
                "queued": 100,
                "memoryShare": "100%",
                "default": false,
                "canUse": true,
                "yours": true,
            })
        );
        // An object without a display name: null, never an invented label.
        let answer = rows(
            &[object(
                "plain",
                json!({ "size": "xs", "grants": [{"group": "g"}] }),
            )],
            &person(&["g"]),
        );
        assert_eq!(answer[0].display_name, None);
        assert_eq!(answer[0].running, 2);
    }

    #[test]
    fn a_missing_size_is_the_default_class() {
        let answer = rows(
            &[object("handwritten", json!({ "grants": [{"group": "g"}] }))],
            &person(&["g"]),
        );
        assert_eq!(answer[0].size.as_deref(), Some("small"));
        assert_eq!((answer[0].running, answer[0].queued), (10, 100));
        assert_eq!(answer[0].memory_share, "20%");
        assert!(answer[0].yours);
    }

    #[test]
    fn without_a_cluster_the_answer_is_the_one_small_class() {
        let rows = demo_rows();
        assert_eq!(
            serde_json::to_value(&rows).expect("JSON"),
            json!([{
                "name": "small",
                "displayName": "small",
                "size": "small",
                "running": 10,
                "queued": 100,
                "memoryShare": "20%",
                "default": true,
                "canUse": true,
                "yours": true,
            }])
        );
    }

    /// `GET /api/warehouses` answers the demo row and `Cache-Control: no-store` without a
    /// cluster client — the shape the e2e suite and `make demo` see.
    #[tokio::test]
    async fn the_route_answers_no_store() {
        use axum::{Router, body::Body, http::Request, routing::get};
        use tower::ServiceExt;

        let state = crate::router_tests::test_state();
        let app = Router::new()
            .route("/warehouses", get(list))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/warehouses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(body[0]["name"], "small");
        assert_eq!(body[0]["yours"], true);
    }
}
