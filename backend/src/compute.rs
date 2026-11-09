//! The `SparkCluster` API (internal#161 slice 5a): the Spark compute a person may use.
//!
//! The operator translates a `SparkCluster` into the Spark Operator's `SparkConnect`; Nova
//! only exposes it — lists the clusters a caller may use and sets `spec.state` (Start and
//! Stop). Nothing in Nova is state of its own.

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use kube::api::{Api, ApiResource, Patch, PatchParams};
use kube::core::GroupVersionKind;
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{Identity, Principal};
use crate::connection_routes::Fail;
use crate::pipelines::{ResourceError, answer, no_cluster, resource_fail};

/// The `SparkCluster`'s owner: a group or a person, exactly one of the two.
#[derive(Debug, Serialize, ToSchema)]
pub struct ComputeOwner {
    pub group: Option<String>,
    pub user: Option<String>,
}

/// One `SparkCluster`, as the Compute page needs it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputeCluster {
    pub name: String,
    pub display_name: Option<String>,
    pub owner: ComputeOwner,
    pub size: String,
    pub runtime: String,
    pub phase: String,
    pub endpoint: Option<String>,
    pub started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub idle_stop_minutes: i64,
    pub can_use: bool,
    pub shared: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ComputeResponse {
    pub clusters: Vec<ComputeCluster>,
}

/// The owner block of a spec; a missing half stays `None`.
fn owner_of(spec: &Value) -> ComputeOwner {
    let text = |pointer: &str| {
        spec.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    ComputeOwner {
        group: text("/owner/group"),
        user: text("/owner/user"),
    }
}

/// Whether the caller may see the cluster: an administrator, everyone for `shared`, the
/// owner's group, or the owner themselves (`owner.user` is the sign-in name).
fn may_use(who: &Identity, name: &str, owner: &ComputeOwner) -> bool {
    who.is_admin
        || name == "shared"
        || owner
            .group
            .as_ref()
            .is_some_and(|group| who.groups.iter().any(|g| g == group))
        || owner.user.as_deref() == Some(who.display_name.as_str())
}

/// Whether the caller may start or stop the cluster. The shared cluster is always on
/// (`idleStopMinutes: 0`): only an administrator starts or stops it.
fn may_act(who: &Identity, name: &str, owner: &ComputeOwner) -> bool {
    who.is_admin
        || (name != "shared"
            && (owner
                .group
                .as_ref()
                .is_some_and(|group| who.groups.iter().any(|g| g == group))
                || owner.user.as_deref() == Some(who.display_name.as_str())))
}

/// The phase the page shows: the operator's `status.phase` once it has reported, otherwise
/// what the request implies (`Starting` for a requested `Running`, `Stopped` for everything
/// else).
fn phase_of(object: &Value) -> String {
    if let Some(phase) = object
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .filter(|phase| !phase.is_empty())
    {
        return phase.to_string();
    }
    if object.pointer("/spec/state").and_then(Value::as_str) == Some("Running") {
        "Starting".to_string()
    } else {
        "Stopped".to_string()
    }
}

/// One cluster as the API answers it, or `None` when the caller may not use it.
fn view(object: &Value, who: &Identity) -> Option<ComputeCluster> {
    let name = object
        .pointer("/metadata/name")
        .and_then(Value::as_str)?
        .to_string();
    let spec = object.get("spec").unwrap_or(object);
    let owner = owner_of(spec);
    if !may_use(who, &name, &owner) {
        return None;
    }
    let status = object.get("status").unwrap_or(&Value::Null);
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
    Some(ComputeCluster {
        display_name: text(spec.get("displayName")),
        shared: name == "shared",
        name,
        owner,
        size: spec
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("small")
            .to_string(),
        runtime: spec
            .get("runtime")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        phase: phase_of(object),
        endpoint: text(status.get("endpoint")),
        started_at: text(status.get("startedAt")),
        last_activity_at: text(status.get("lastActivityAt")),
        idle_stop_minutes: spec
            .get("idleStopMinutes")
            .and_then(Value::as_i64)
            .unwrap_or(60),
        can_use: true,
    })
}

/// Every cluster the caller may use, sorted by name. Pure: no cluster.
pub fn visible_clusters(objects: &[Value], who: &Identity) -> Vec<ComputeCluster> {
    let mut views: Vec<ComputeCluster> = objects
        .iter()
        .filter_map(|object| view(object, who))
        .collect();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

/// The one row without a cluster (demo mode, a local run): the shared cluster is always on.
fn demo_clusters() -> Vec<ComputeCluster> {
    vec![ComputeCluster {
        name: "shared".to_string(),
        display_name: Some("Shared".to_string()),
        owner: ComputeOwner {
            group: Some("stardelt-users".to_string()),
            user: None,
        },
        size: "small".to_string(),
        runtime: "spark-4.1-py3.11".to_string(),
        phase: "Running".to_string(),
        endpoint: None,
        started_at: None,
        last_activity_at: None,
        idle_stop_minutes: 0,
        can_use: true,
        shared: true,
    }]
}

/// The `SparkCluster` resources of Nova's namespace. `None` without a cluster connection.
fn compute_api(state: &AppState) -> Result<Api<kube::api::DynamicObject>, Fail> {
    let Some(client) = state.kube.clone() else {
        return Err(no_cluster());
    };
    Ok(Api::namespaced_with(
        client,
        &crate::cluster::namespace(),
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "SparkCluster",
        )),
    ))
}

/// Who the caller is. `dev` mode has no IdP: the single user is the operator, an
/// administrator with no groups — exactly like `warehouses::actor`.
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

fn not_found() -> Fail {
    // Indistinguishable from a cluster that does not exist (and from one the caller may not
    // use).
    Fail::new(StatusCode::NOT_FOUND, "not found")
}

/// Every cluster of Nova's namespace the caller may use.
async fn list_clusters(state: &AppState, who: &Identity) -> Result<Vec<ComputeCluster>, Fail> {
    let api = compute_api(state)?;
    let objects = api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    let mut values = Vec::with_capacity(objects.items.len());
    for object in objects.items {
        values.push(
            serde_json::to_value(object)
                .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?,
        );
    }
    Ok(visible_clusters(&values, who))
}

/// Set `spec.state` and answer the updated cluster. Start and Stop are the same verb with a
/// different desired state, and both are idempotent.
async fn set_state(
    state: &AppState,
    who: &Identity,
    name: &str,
    desired: &str,
) -> Result<ComputeCluster, Fail> {
    let api = compute_api(state)?;
    let object = api.get(name).await.map_err(|e| match e {
        kube::Error::Api(er) if er.code == 404 => not_found(),
        other => resource_fail(ResourceError::Unavailable(other.to_string())),
    })?;
    let value = serde_json::to_value(&object)
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    let Some(current) = view(&value, who) else {
        return Err(not_found());
    };
    let spec = value.get("spec").unwrap_or(&value);
    let owner = owner_of(spec);
    if !may_act(who, name, &owner) {
        return Err(Fail::new(
            StatusCode::FORBIDDEN,
            "only an administrator may start or stop the shared cluster",
        ));
    }
    match phase_of(&value).as_str() {
        "Starting" => {
            return Err(Fail::new(StatusCode::CONFLICT, "the cluster is starting"));
        }
        "Stopping" => {
            return Err(Fail::new(StatusCode::CONFLICT, "the cluster is stopping"));
        }
        _ => {}
    }
    if spec.get("state").and_then(Value::as_str) == Some(desired) {
        return Ok(current);
    }
    let patched = api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({"spec": {"state": desired}})),
        )
        .await
        .map_err(|e| match e {
            kube::Error::Api(er) if er.code == 404 => not_found(),
            other => resource_fail(ResourceError::Unavailable(other.to_string())),
        })?;
    let patched = serde_json::to_value(&patched)
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    view(&patched, who).ok_or_else(|| {
        Fail::new(
            StatusCode::CONFLICT,
            "the cluster changed while it was patched",
        )
    })
}

/// `GET /api/compute`: the Spark clusters this caller may use.
#[utoipa::path(
    get,
    operation_id = "compute_list",
    path = "/api/compute",
    tag = "public",
    description = "The Spark clusters the caller may use: an administrator sees every `SparkCluster` of the platform namespace, everyone else the ones owned by one of their groups, owned by them (`owner.user` is the sign-in name) and the `shared` one. Each row carries the owner, the class `size`, the `runtime`, the operator's `phase` (`Starting` for a requested `Running` before it has reported, `Stopped` otherwise), the Connect `endpoint` and `startedAt` while it runs, `lastActivityAt` once the operator reports it, `idleStopMinutes` (`0` on the shared cluster), `canUse` and `shared`. Nothing in Nova is state of its own: the operator translates a `SparkCluster` into the Spark server. Without a cluster connection (demo mode, a local run) the answer is the one shared cluster. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "The caller's clusters, sorted by name", body = ComputeResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let identity = actor(principal, &state);
    let result = if state.kube.is_none() {
        Ok(ComputeResponse {
            clusters: demo_clusters(),
        })
    } else {
        list_clusters(&state, &identity)
            .await
            .map(|clusters| ComputeResponse { clusters })
    };
    match result {
        Ok(body) => (
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(body),
        )
            .into_response(),
        Err(fail) => fail.into_response(),
    }
}

/// `POST /api/compute/{name}/start`: set `spec.state: Running`.
#[utoipa::path(
    post,
    operation_id = "compute_start",
    path = "/api/compute/{name}/start",
    tag = "public",
    description = "Start one Spark cluster by merge-patching `spec.state: Running`; the answer is the updated cluster. Idempotent: a cluster already `Running` answers its view without a patch. A cluster the caller may not use and a name that does not exist both answer the same `404`. A non-administrator on the `shared` cluster answers `403` (it is always on). A cluster the operator is still `Starting` or `Stopping` answers `409`.",
    params(
        ("name" = String, Path, description = "The cluster's name"),
    ),
    responses(
        (status = 200, description = "The cluster, with `spec.state: Running` applied", body = ComputeCluster),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "A non-administrator on the shared cluster", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such cluster, or the caller may not use it", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The cluster is starting or stopping", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn start(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(name): Path<String>,
) -> Response {
    let identity = actor(principal, &state);
    answer(set_state(&state, &identity, &name, "Running").await)
}

/// `POST /api/compute/{name}/stop`: set `spec.state: Stopped`.
#[utoipa::path(
    post,
    operation_id = "compute_stop",
    path = "/api/compute/{name}/stop",
    tag = "public",
    description = "Stop one Spark cluster by merge-patching `spec.state: Stopped`; the answer is the updated cluster. Idempotent: a cluster already `Stopped` answers its view without a patch. A cluster the caller may not use and a name that does not exist both answer the same `404`. A non-administrator on the `shared` cluster answers `403` (it is always on). A cluster the operator is still `Starting` or `Stopping` answers `409`.",
    params(
        ("name" = String, Path, description = "The cluster's name"),
    ),
    responses(
        (status = 200, description = "The cluster, with `spec.state: Stopped` applied", body = ComputeCluster),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "A non-administrator on the shared cluster", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such cluster, or the caller may not use it", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The cluster is starting or stopping", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn stop(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(name): Path<String>,
) -> Response {
    let identity = actor(principal, &state);
    answer(set_state(&state, &identity, &name, "Stopped").await)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::{may_act, may_use, owner_of, phase_of, visible_clusters};
    use crate::auth::Identity;

    fn who(user: &str, groups: &[&str], admin: bool) -> Identity {
        Identity {
            sub: format!("{user}-sub"),
            display_name: user.into(),
            email: None,
            name: None,
            groups: groups.iter().map(|g| (*g).to_string()).collect(),
            roles: vec![],
            groups_overage: false,
            is_admin: admin,
        }
    }

    /// A `SparkCluster` object as the API server holds it.
    fn cluster(name: &str, spec: Value) -> Value {
        json!({
            "metadata": {"name": name, "namespace": crate::cluster::namespace()},
            "spec": spec
        })
    }

    #[test]
    fn owner_of_reads_a_group_a_user_or_neither() {
        let group = owner_of(&json!({"owner": {"group": "sales"}}));
        assert_eq!(group.group.as_deref(), Some("sales"));
        assert!(group.user.is_none());

        let user = owner_of(&json!({"owner": {"user": "alice"}}));
        assert_eq!(user.user.as_deref(), Some("alice"));
        assert!(user.group.is_none());

        let neither = owner_of(&json!({}));
        assert!(neither.group.is_none());
        assert!(neither.user.is_none());
    }

    #[test]
    fn may_use_admits_the_admin_the_group_the_owner_and_everyone_for_shared() {
        let group = owner_of(&json!({"owner": {"group": "sales"}}));
        let person = owner_of(&json!({"owner": {"user": "alice"}}));
        let shared = owner_of(&json!({"owner": {"group": "stardelt-users"}}));

        assert!(may_use(&who("root", &[], true), "trial", &group));
        assert!(may_use(&who("bob", &["sales"], false), "trial", &group));
        assert!(may_use(&who("alice", &[], false), "trial", &person));
        assert!(!may_use(
            &who("bob", &["marketing"], false),
            "trial",
            &group
        ));
        assert!(!may_use(&who("bob", &[], false), "trial", &person));
        assert!(may_use(&who("bob", &[], false), "shared", &shared));
    }

    #[test]
    fn may_act_admits_the_same_set_but_never_a_non_admin_on_shared() {
        let group = owner_of(&json!({"owner": {"group": "sales"}}));
        let person = owner_of(&json!({"owner": {"user": "alice"}}));
        let shared = owner_of(&json!({"owner": {"group": "stardelt-users"}}));

        assert!(may_act(&who("root", &[], true), "trial", &group));
        assert!(may_act(&who("bob", &["sales"], false), "trial", &group));
        assert!(may_act(&who("alice", &[], false), "trial", &person));
        assert!(!may_act(
            &who("bob", &["marketing"], false),
            "trial",
            &group
        ));
        assert!(!may_act(&who("bob", &[], false), "trial", &person));
        assert!(!may_act(
            &who("bob", &["stardelt-users"], false),
            "shared",
            &shared
        ));
        assert!(may_act(&who("root", &[], true), "shared", &shared));
    }

    #[test]
    fn phase_of_prefers_the_operator_and_falls_back_to_the_request() {
        let reported = cluster("trial", json!({"state": "Running"}));
        let mut reported = reported;
        reported["status"] = json!({"phase": "Running"});
        assert_eq!(phase_of(&reported), "Running");

        let requested = cluster("trial", json!({"state": "Running"}));
        assert_eq!(phase_of(&requested), "Starting");

        let idle = cluster("trial", json!({}));
        assert_eq!(phase_of(&idle), "Stopped");
    }

    #[test]
    fn visible_clusters_filters_and_sorts() {
        let objects = vec![
            cluster(
                "zzz-trial",
                json!({"owner": {"group": "sales"}, "runtime": "spark-4.1-py3.11"}),
            ),
            cluster(
                "shared",
                json!({"owner": {"group": "stardelt-users"}, "idleStopMinutes": 0}),
            ),
            cluster("aaa-trial", json!({"owner": {"group": "sales"}})),
            cluster("marketing", json!({"owner": {"group": "marketing"}})),
        ];
        let views = visible_clusters(&objects, &who("bob", &["sales"], false));
        let names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["aaa-trial", "shared", "zzz-trial"]);
        assert_eq!(views[0].size, "small");
        assert_eq!(views[0].idle_stop_minutes, 60);
        assert!(views[0].can_use);
        assert!(!views[0].shared);
        assert!(views[1].shared);
        assert_eq!(views[1].idle_stop_minutes, 0);
    }

    mod fake_kube {
        use std::sync::{Arc, Mutex};

        use axum::{
            Json, Router,
            extract::{Path, State},
            http::StatusCode,
            routing::get,
        };
        use serde_json::{Value, json};

        pub struct Fake {
            pub items: Vec<Value>,
            pub patches: Vec<Value>,
        }

        pub type Shared = Arc<Mutex<Fake>>;

        fn failure(code: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
            (
                code,
                Json(json!({
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "message": message,
                    "code": code.as_u16()
                })),
            )
        }

        async fn many(State(state): State<Shared>) -> Json<Value> {
            let state = state.lock().unwrap();
            Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "SparkClusterList",
                "items": state.items,
            }))
        }

        async fn one(
            State(state): State<Shared>,
            Path((_namespace, name)): Path<(String, String)>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let state = state.lock().unwrap();
            state
                .items
                .iter()
                .find(|item| item["metadata"]["name"] == name)
                .cloned()
                .map(Json)
                .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not found"))
        }

        async fn patch_one(
            State(state): State<Shared>,
            Path((_namespace, name)): Path<(String, String)>,
            body: String,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let body: Value = serde_json::from_str(&body)
                .map_err(|_| failure(StatusCode::BAD_REQUEST, "not json"))?;
            let mut state = state.lock().unwrap();
            state.patches.push(body.clone());
            let item = state
                .items
                .iter_mut()
                .find(|item| item["metadata"]["name"] == name)
                .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not found"))?;
            if let Some(desired) = body.pointer("/spec/state").and_then(Value::as_str) {
                item["spec"]["state"] = json!(desired);
            }
            Ok(Json(item.clone()))
        }

        pub async fn start(items: Vec<Value>) -> (kube::Client, Shared) {
            let shared = Arc::new(Mutex::new(Fake {
                items,
                patches: Vec::new(),
            }));
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/sparkclusters",
                    get(many),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/sparkclusters/{name}",
                    get(one).patch(patch_one),
                )
                .with_state(shared.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let url = format!("http://{addr}").parse().unwrap();
            let client = kube::Client::try_from(kube::Config::new(url)).expect("client");
            (client, shared)
        }
    }

    /// The compute routes on a test state with a fake API server, `who` signed in.
    async fn app_with(
        items: Vec<Value>,
        who: Option<Identity>,
    ) -> (axum::Router, fake_kube::Shared) {
        use axum::routing::post;

        let (client, shared) = fake_kube::start(items).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let router = axum::Router::new()
            .route("/compute", axum::routing::get(super::list))
            .route("/compute/{name}/start", post(super::start))
            .route("/compute/{name}/stop", post(super::stop))
            .with_state(state);
        let router = match who {
            Some(identity) => router.layer(axum::Extension(crate::auth::Principal {
                identity,
                via: "session",
                access_token: None,
            })),
            None => router,
        };
        (router, shared)
    }

    async fn call(app: axum::Router, method: &str, uri: &str) -> (StatusCode, Value) {
        use tower::ServiceExt;

        let request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, value)
    }

    fn trial() -> Value {
        cluster(
            "trial",
            json!({"owner": {"group": "sales"}, "runtime": "spark-4.1-py3.11", "state": "Stopped"}),
        )
    }

    #[tokio::test]
    async fn the_list_answers_the_one_visible_cluster() {
        let items = vec![
            trial(),
            cluster(
                "marketing",
                json!({"owner": {"group": "marketing"}, "runtime": "spark-4.1-py3.11"}),
            ),
        ];
        let (app, _) = app_with(items, Some(who("bob", &["sales"], false))).await;
        let (status, body) = call(app, "GET", "/compute").await;
        assert_eq!(status, StatusCode::OK);
        let clusters = body["clusters"].as_array().expect("clusters");
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0]["name"], "trial");
        assert_eq!(clusters[0]["owner"]["group"], "sales");
    }

    #[tokio::test]
    async fn start_patches_the_state_and_answers_starting() {
        let (app, shared) = app_with(vec![trial()], Some(who("bob", &["sales"], false))).await;
        let (status, body) = call(app, "POST", "/compute/trial/start").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["phase"], "Starting");
        let patches = shared.lock().unwrap().patches.clone();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0], json!({"spec": {"state": "Running"}}));
    }

    #[tokio::test]
    async fn a_stranger_gets_404() {
        let (app, shared) = app_with(vec![trial()], Some(who("eve", &["marketing"], false))).await;
        let (status, body) = call(app, "POST", "/compute/trial/start").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not found");
        assert!(shared.lock().unwrap().patches.is_empty());
    }

    #[tokio::test]
    async fn a_non_admin_on_the_shared_cluster_gets_403() {
        let items = vec![cluster(
            "shared",
            json!({"owner": {"group": "stardelt-users"}, "idleStopMinutes": 0, "state": "Running"}),
        )];
        let (app, shared) = app_with(items, Some(who("bob", &["stardelt-users"], false))).await;
        let (status, body) = call(app, "POST", "/compute/shared/start").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body["error"],
            "only an administrator may start or stop the shared cluster"
        );
        assert!(shared.lock().unwrap().patches.is_empty());
    }

    #[tokio::test]
    async fn a_cluster_that_is_starting_gets_409() {
        let mut object = cluster(
            "trial",
            json!({"owner": {"group": "sales"}, "runtime": "spark-4.1-py3.11", "state": "Running"}),
        );
        object["status"] = json!({"phase": "Starting"});
        let (app, shared) = app_with(vec![object], Some(who("bob", &["sales"], false))).await;
        let (status, body) = call(app, "POST", "/compute/trial/start").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "the cluster is starting");
        assert!(shared.lock().unwrap().patches.is_empty());
    }
}
