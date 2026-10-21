//! Administrator view of every catalog and its grants (stardelt-nova#251, part 1/2).
//!
//! `GET /api/admin/catalogs` answers the `Catalog` resources of the platform
//! namespace (`stardelt.io/v1alpha1`) as rows: identity, owner, managed access,
//! the storage key prefix, the phase, and the grants and conditions the
//! access-sync job last wrote. It is a read-only viewport — Nova never writes a
//! `Catalog`, and the resource stays the source of truth.
//!
//! Administrator-only. `main` puts `auth::require_admin` in front of the route
//! in `oidc` mode, exactly like `/api/apps`, so a non-administrator answers 403
//! `this view is for platform administrators`. In `dev` mode there is no IdP and
//! the single user is the operator.
//!
//! Permissions are catalog-wide today (`spec.grants[{group,verb}]` plus the one
//! `spec.owner` group, which holds `manage` and not `select`). Platform
//! administrators hold `project_admin` by role, so they are not listed in a
//! catalog's grants and can do everything in every catalog; the page says so in
//! one line. Table- and schema-level grants do not exist yet.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::AppState;
use crate::connection_routes::Fail;

/// One catalog, as an administrator sees it.
///
/// `displayName`, `description`, `owner`, `keyPrefix`, `warehouseId` and the two
/// generation numbers are `null` when the resource does not carry them — never
/// an empty string or a guessed value.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminCatalog {
    /// `metadata.name`: the name users type (`finance.orders`) and the warehouse name.
    pub name: String,
    /// `spec.displayName`, or `null`.
    pub display_name: Option<String>,
    /// `spec.description`, or `null`.
    pub description: Option<String>,
    /// `spec.owner`: exactly one group. Ownership means `manage`, not `select`.
    pub owner: Option<String>,
    /// `spec.managedAccess`: the owner cannot grant on this catalog.
    pub managed_access: bool,
    /// `spec.storage.keyPrefix`, or `null` when the catalog uses the lakehouse's defaults.
    pub key_prefix: Option<String>,
    /// `status.phase` (`Ready` | `Invalid`), or `Unknown` when the resource has no status.
    pub phase: String,
    /// `status.warehouseId`: the Lakekeeper warehouse, as the access-sync job wrote it.
    pub warehouse_id: Option<String>,
    /// `metadata.generation`.
    pub generation: Option<i64>,
    /// `status.observedGeneration`: the generation the last sync observed.
    pub observed_generation: Option<i64>,
    /// `metadata.resourceVersion`: the version this row was read at.
    pub resource_version: Option<String>,
    /// `spec.grants`, in the order the resource declares them.
    pub grants: Vec<CatalogGrant>,
    /// `status.conditions`: what the access-sync job last reported.
    pub conditions: Vec<CatalogCondition>,
}

/// One declared grant: a group and one verb.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CatalogGrant {
    pub group: String,
    /// One of `browse`, `select`, `write`, `create`, `manage`.
    pub verb: String,
}

/// One condition of a `Catalog`'s status, reduced to the four fields the page shows.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CatalogCondition {
    /// `CatalogReady`, `GrantsReady` or `Drift`.
    pub r#type: String,
    /// `"True"` or `"False"`.
    pub status: String,
    /// `Ready`, `UnknownGroup`, `RoleConflict`, `RoleCreateFailed`, `AssigneesFailed`,
    /// `GrantsFailed`, `ForeignGrants` or `Clean`.
    pub reason: Option<String>,
    /// The sentence the access-sync job wrote, or `null`.
    pub message: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/admin/catalogs",
    tag = "internal",
    description = "Every `Catalog` of the platform namespace, sorted by name: identity, owner, managed access, the storage key prefix, phase and the grants and conditions the access-sync job last wrote. Administrator-only; a non-administrator answers 403 `this view is for platform administrators`. Read-only: Nova never writes a `Catalog`. A catalog the job has not observed yet answers `phase: \"Unknown\"` with no conditions. Without a cluster connection (demo mode, a local run) the answer is the one in-memory `warehouse` catalog. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "One row per catalog, sorted by name", body = [AdminCatalog]),
        (status = 403, description = "The caller is not a platform administrator", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer, or Nova may not list catalogs", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(State(state): State<Arc<AppState>>) -> Response {
    let rows = catalog_rows(&state).await;
    match rows {
        Ok(rows) => (
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(rows),
        )
            .into_response(),
        Err(fail) => fail.into_response(),
    }
}

/// The rows the page shows: from the cluster, or the demo one when Nova has no
/// cluster client (demo mode, or a local run outside a cluster).
async fn catalog_rows(state: &AppState) -> Result<Vec<AdminCatalog>, Fail> {
    let Some(client) = state.kube.clone() else {
        return Ok(demo_rows());
    };
    let api = catalogs_api(client);
    let objects = api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| {
            // The detail names the API server, so it stays in the log; the answer is generic
            // for the same reason `pipelines::resource_fail` is.
            tracing::warn!(error = %e, "admin catalog list failed");
            Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
        })?;
    let mut rows = Vec::with_capacity(objects.items.len());
    for object in objects.items {
        let value = serde_json::to_value(object).map_err(|e| {
            tracing::warn!(error = %e, "admin catalog object is not JSON");
            Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
        })?;
        rows.push(row_from_object(&value));
    }
    sort_by_name(&mut rows);
    Ok(rows)
}

/// The `Catalog` resources of Nova's namespace, through the cluster client.
fn catalogs_api(client: kube::Client) -> Api<DynamicObject> {
    Api::namespaced_with(
        client,
        &crate::cluster::namespace(),
        &ApiResource::from_gvk(&GroupVersionKind::gvk("stardelt.io", "v1alpha1", "Catalog")),
    )
}

/// Map one `Catalog` object (the JSON the API server returned) to a row.
///
/// Missing optional fields stay `None`; a `status` that is absent or carries no
/// phase is `Unknown` with no conditions, which is what an unobserved catalog
/// looks like. Grants and conditions with no `group`/`verb` or `type`/`status`
/// are dropped rather than guessed at.
fn row_from_object(object: &Value) -> AdminCatalog {
    let spec = object.get("spec").unwrap_or(&Value::Null);
    let status = object
        .get("status")
        .filter(|status| !status.is_null())
        .unwrap_or(&Value::Null);
    let metadata = object.get("metadata").unwrap_or(&Value::Null);

    AdminCatalog {
        name: text(metadata.get("name")).unwrap_or_default(),
        display_name: text(spec.get("displayName")),
        description: text(spec.get("description")),
        owner: text(spec.get("owner")),
        managed_access: spec
            .get("managedAccess")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        key_prefix: text(spec.get("storage").and_then(|s| s.get("keyPrefix"))),
        phase: text(status.get("phase"))
            .filter(|phase| !phase.is_empty())
            .unwrap_or_else(|| "Unknown".to_string()),
        warehouse_id: text(status.get("warehouseId")),
        generation: integer(metadata.get("generation")),
        observed_generation: integer(status.get("observedGeneration")),
        resource_version: text(metadata.get("resourceVersion")),
        grants: grants_of(spec),
        conditions: conditions_of(status),
    }
}

fn grants_of(spec: &Value) -> Vec<CatalogGrant> {
    spec.get("grants")
        .and_then(Value::as_array)
        .map(|grants| {
            grants
                .iter()
                .filter_map(|grant| {
                    Some(CatalogGrant {
                        group: text(grant.get("group"))?,
                        verb: text(grant.get("verb"))?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn conditions_of(status: &Value) -> Vec<CatalogCondition> {
    status
        .get("conditions")
        .and_then(Value::as_array)
        .map(|conditions| {
            conditions
                .iter()
                .filter_map(|condition| {
                    Some(CatalogCondition {
                        r#type: text(condition.get("type"))?,
                        status: text(condition.get("status"))?,
                        reason: text(condition.get("reason")),
                        message: text(condition.get("message")),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A present string or number as text; anything else (absent, `null`, a
/// structure) is `None`. `resourceVersion` and the generation numbers arrive as
/// whichever the API server chose for that field.
fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn integer(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn sort_by_name(rows: &mut [AdminCatalog]) {
    rows.sort_by(|a, b| a.name.cmp(&b.name));
}

/// What a Nova without a cluster client answers: the in-memory demo lakehouse,
/// which is the one catalog `GET /api/catalog/catalogs` lists in demo mode.
/// It is `Ready` by construction — there is no `Catalog` resource and no sync
/// job behind it — and it carries no conditions.
fn demo_rows() -> Vec<AdminCatalog> {
    vec![AdminCatalog {
        name: "warehouse".to_string(),
        display_name: None,
        description: None,
        owner: Some("stardelt-admins".to_string()),
        managed_access: false,
        key_prefix: None,
        phase: "Ready".to_string(),
        warehouse_id: None,
        generation: None,
        observed_generation: None,
        resource_version: None,
        grants: ["browse", "select", "write", "create"]
            .into_iter()
            .map(|verb| CatalogGrant {
                group: "stardelt-users".to_string(),
                verb: verb.to_string(),
            })
            .collect(),
        conditions: Vec::new(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    /// A `Catalog` object shaped like the API server hands it over: `metadata`,
    /// `spec` and (optionally) `status`, including the fields Nova must not expose
    /// (the per-catalog storage credential reference).
    fn catalog_object(name: &str, phase: Option<&str>) -> Value {
        let mut object = json!({
            "apiVersion": "stardelt.io/v1alpha1",
            "kind": "Catalog",
            "metadata": {
                "name": name,
                "namespace": "stardelt",
                "generation": 7,
                "resourceVersion": "4211"
            },
            "spec": {
                "lakehouseRef": { "name": "default" },
                "displayName": "Finance",
                "description": "Contracted revenue, orders and billing.",
                "owner": "stardelt-finops",
                "storage": {
                    "bucket": "finance",
                    "keyPrefix": "finance/",
                    "credentialSecretRef": { "name": "finance-s3-creds", "key": "identity-entry" }
                },
                "managedAccess": true,
                "grants": [
                    { "group": "stardelt-finops", "verb": "browse" },
                    { "group": "stardelt-analytics", "verb": "select" }
                ]
            }
        });
        if let Some(phase) = phase {
            object["status"] = json!({
                "phase": phase,
                "warehouseId": "9f1c",
                "observedGeneration": 7,
                "conditions": [
                    {
                        "type": "CatalogReady",
                        "status": if phase == "Ready" { "True" } else { "False" },
                        "reason": if phase == "Ready" { "Ready" } else { "UnknownGroup" },
                        "message": if phase == "Ready" {
                            "catalog and its grants are ready"
                        } else {
                            "group stardelt-ghost does not exist in the realm"
                        }
                    },
                    { "type": "Drift", "status": "False", "reason": "Clean", "message": "no foreign grants" }
                ]
            });
        }
        object
    }

    fn row(name: &str) -> AdminCatalog {
        row_from_object(&catalog_object(name, Some("Ready")))
    }

    #[test]
    fn maps_a_catalog_object_to_a_row() {
        let mapped = row_from_object(&catalog_object("finance", Some("Ready")));
        assert_eq!(
            mapped,
            AdminCatalog {
                name: "finance".into(),
                display_name: Some("Finance".into()),
                description: Some("Contracted revenue, orders and billing.".into()),
                owner: Some("stardelt-finops".into()),
                managed_access: true,
                key_prefix: Some("finance/".into()),
                phase: "Ready".into(),
                warehouse_id: Some("9f1c".into()),
                generation: Some(7),
                observed_generation: Some(7),
                resource_version: Some("4211".into()),
                grants: vec![
                    CatalogGrant {
                        group: "stardelt-finops".into(),
                        verb: "browse".into()
                    },
                    CatalogGrant {
                        group: "stardelt-analytics".into(),
                        verb: "select".into()
                    },
                ],
                conditions: vec![
                    CatalogCondition {
                        r#type: "CatalogReady".into(),
                        status: "True".into(),
                        reason: Some("Ready".into()),
                        message: Some("catalog and its grants are ready".into()),
                    },
                    CatalogCondition {
                        r#type: "Drift".into(),
                        status: "False".into(),
                        reason: Some("Clean".into()),
                        message: Some("no foreign grants".into()),
                    },
                ],
            }
        );
        // The wire shape is camelCase, and the API server's credential reference is
        // never part of it (it names a Secret, it is not a permission).
        let json = serde_json::to_value(&mapped).unwrap();
        assert_eq!(json["displayName"], "Finance");
        assert_eq!(json["managedAccess"], true);
        assert_eq!(json["keyPrefix"], "finance/");
        assert_eq!(json["conditions"][0]["type"], "CatalogReady");
        assert!(json.get("credentialSecretRef").is_none());
        assert!(!json.to_string().contains("finance-s3-creds"));
    }

    #[test]
    fn missing_status_is_unknown() {
        let mapped = row_from_object(&catalog_object("sandbox", None));
        assert_eq!(mapped.phase, "Unknown");
        assert!(mapped.conditions.is_empty());
        assert_eq!(mapped.warehouse_id, None);
        assert_eq!(mapped.observed_generation, None);
        // The rest of the row is still read from the resource.
        assert_eq!(mapped.owner.as_deref(), Some("stardelt-finops"));
        assert_eq!(mapped.grants.len(), 2);
        assert_eq!(mapped.resource_version.as_deref(), Some("4211"));
    }

    #[test]
    fn rows_sorted_by_name() {
        let mut rows: Vec<AdminCatalog> = ["product", "finance", "analytics", "sandbox"]
            .iter()
            .map(|name| row(name))
            .collect();
        sort_by_name(&mut rows);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["analytics", "finance", "product", "sandbox"]);
    }

    #[tokio::test]
    async fn demo_mode_row() {
        let state = crate::router_tests::test_state();
        let rows = catalog_rows(&state).await.expect("the demo row");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "warehouse");
        assert_eq!(row.owner.as_deref(), Some("stardelt-admins"));
        assert_eq!(
            row.grants,
            vec![
                CatalogGrant {
                    group: "stardelt-users".into(),
                    verb: "browse".into()
                },
                CatalogGrant {
                    group: "stardelt-users".into(),
                    verb: "select".into()
                },
                CatalogGrant {
                    group: "stardelt-users".into(),
                    verb: "write".into()
                },
                CatalogGrant {
                    group: "stardelt-users".into(),
                    verb: "create".into()
                },
            ]
        );
        assert_eq!(row.phase, "Ready");
        assert!(row.conditions.is_empty());
    }

    #[tokio::test]
    async fn admin_only_403() {
        use crate::auth::{Principal, require_admin};

        fn identity(admin: bool) -> crate::auth::Identity {
            crate::auth::Identity {
                sub: "s".into(),
                display_name: "u".into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: admin,
            }
        }

        async fn status_for(identity: Option<crate::auth::Identity>) -> (StatusCode, String) {
            // The same order `main` builds: require_admin inner, the principal outer.
            let app = crate::admin_router()
                .route_layer(axum::middleware::from_fn(require_admin))
                .layer(axum::middleware::from_fn(
                    move |mut req: Request<Body>, next: axum::middleware::Next| {
                        let identity = identity.clone();
                        async move {
                            if let Some(identity) = identity {
                                req.extensions_mut().insert(Principal {
                                    identity,
                                    via: "session",
                                    access_token: None,
                                });
                            }
                            next.run(req).await
                        }
                    },
                ))
                .with_state(crate::router_tests::test_state());
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/admin/catalogs")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, String::from_utf8_lossy(&body).into_owned())
        }

        let (status, body) = status_for(Some(identity(false))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("this view is for platform administrators"));

        // No principal is refused too (the dev router has no check, oidc always has one).
        let (status, _) = status_for(None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // An administrator reaches the handler; with no cluster client it answers the demo row.
        let (status, body) = status_for(Some(identity(true))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"warehouse\""));
    }

    #[tokio::test]
    async fn answer_is_uncacheable_and_carries_no_credentials() {
        let state = crate::router_tests::test_state();
        let response = list(State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(!body.contains("credentialSecretRef"));
        assert!(!body.contains("s3-creds"));
    }

    /// An API server that fails the list, so the generic sentence can be pinned: the
    /// detail (the API server's own message) must not reach the browser.
    mod fake_kube {
        use std::sync::{Arc, Mutex};

        use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
        use serde_json::{Value, json};

        pub struct Fake {
            pub items: Vec<Value>,
            pub status: StatusCode,
        }

        type Shared = Arc<Mutex<Fake>>;

        async fn list(
            State(state): State<Shared>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let state = state.lock().unwrap();
            if state.status != StatusCode::OK {
                return Err((
                    state.status,
                    Json(json!({
                        "kind": "Status",
                        "apiVersion": "v1",
                        "status": "Failure",
                        "message": "catalogs.stardelt.io is forbidden: User \"system:serviceaccount:stardelt:nova\" cannot list resource \"catalogs\"",
                        "reason": "Forbidden",
                        "code": 403
                    })),
                ));
            }
            Ok(Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "CatalogList",
                "metadata": { "resourceVersion": "1" },
                "items": state.items
            })))
        }

        pub async fn start(items: Vec<Value>, status: StatusCode) -> kube::Client {
            let shared = Arc::new(Mutex::new(Fake { items, status }));
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/catalogs",
                    get(list),
                )
                .with_state(shared);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let url = format!("http://{addr}").parse().unwrap();
            kube::Client::try_from(kube::Config::new(url)).expect("a client for the fake API")
        }
    }

    #[tokio::test]
    async fn cluster_failure_is_502_without_the_api_servers_words() {
        let client = fake_kube::start(vec![], StatusCode::FORBIDDEN).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = list(State(state)).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("the cluster did not answer"), "{body}");
        assert!(!body.contains("system:serviceaccount"), "{body}");
        assert!(!body.contains("forbidden"), "{body}");
    }

    #[tokio::test]
    async fn lists_the_clusters_catalogs_sorted() {
        let client = fake_kube::start(
            vec![
                catalog_object("warehouse", Some("Ready")),
                catalog_object("finance", Some("Ready")),
                catalog_object("sandbox", None),
            ],
            StatusCode::OK,
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        let rows = catalog_rows(&state).await.expect("the cluster's catalogs");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["finance", "sandbox", "warehouse"]);
        assert_eq!(rows[1].phase, "Unknown");
        assert_eq!(rows[2].owner.as_deref(), Some("stardelt-finops"));
        assert_eq!(
            rows[0].grants[1],
            CatalogGrant {
                group: "stardelt-analytics".into(),
                verb: "select".into()
            }
        );
    }
}
