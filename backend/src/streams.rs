//! The `StreamApp` API (D42): what runs on the streaming bus, and how it is doing.
//!
//! Slice 1 is read-only. Slice 3a adds the one verb a stream has — stop and resume by
//! patching `spec.stopped` (the checkpoint is kept, so `stopped: false` resumes from it).

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::Path,
    response::{IntoResponse, Response},
    routing::{get, patch as patch_route},
};
use kube::api::{Api, ApiResource, Patch, PatchParams};
use kube::core::GroupVersionKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::Identity;
use crate::connection_routes::{Caller, Fail, SignedIn};
use crate::pipelines::{ResourceError, answer, may_act, no_cluster, resource_fail, visible};

/// One `StreamApp`, as the Streams page needs it. No secret, no checkpoint path.
#[derive(Debug, Serialize, ToSchema)]
pub struct StreamView {
    pub namespace: String,
    pub name: String,
    pub owner_group: String,
    pub phase: String,
    /// The `Ready` condition's reason, when the operator set one (operator#322: `Restarting`).
    pub reason: Option<String>,
    pub last_error: Option<String>,
    pub last_checkpoint_at: Option<String>,
    /// How many times Spark has submitted the application (`status.attempts`); absent until
    /// the operator reports it (operator#336).
    pub attempts: Option<i64>,
    /// When Spark last submitted the application, RFC 3339 (`status.lastSubmittedAt`).
    pub last_submitted_at: Option<String>,
    pub stopped: bool,
    pub source_connection: String,
    pub source_topic: String,
    pub sink_catalog: String,
    pub sink_table: String,
    pub sink_mode: String,
    pub run_as: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct StreamsResponse {
    pub streams: Vec<StreamView>,
}

/// The one verb a stream has (D42 §4.4): stop or resume by setting `spec.stopped`.
#[derive(Deserialize, ToSchema)]
pub struct StreamState {
    /// The desired `spec.stopped`.
    pub stopped: bool,
}

/// The reason on the operator's `Ready` condition (`StreamAppStatus.conditions`).
fn ready_reason(status: &Value) -> Option<String> {
    status
        .get("conditions")?
        .as_array()?
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some("Ready"))
        .and_then(|c| c.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One stream as the API answers it. No connection credentials and no checkpoint prefix.
pub fn stream_view(app: &Value, _who: &Identity) -> StreamView {
    let spec = app.get("spec").unwrap_or(app);
    let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    let status = app.get("status").unwrap_or(&Value::Null);
    let phase = text(status.get("phase"));
    StreamView {
        namespace: text(app.pointer("/metadata/namespace")),
        name: text(app.pointer("/metadata/name")),
        owner_group: text(spec.get("ownerGroup")),
        phase: if phase.is_empty() {
            "Pending".to_string()
        } else {
            phase
        },
        reason: ready_reason(status),
        last_error: status
            .get("lastError")
            .and_then(Value::as_str)
            .map(str::to_string),
        last_checkpoint_at: status
            .get("lastCheckpointAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        attempts: status.get("attempts").and_then(Value::as_i64),
        last_submitted_at: status
            .get("lastSubmittedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        stopped: spec
            .get("stopped")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        source_connection: text(spec.pointer("/source/connectionRef")),
        source_topic: text(spec.pointer("/source/topic")),
        sink_catalog: text(spec.pointer("/sink/catalog")),
        sink_table: text(spec.pointer("/sink/table")),
        sink_mode: text(spec.pointer("/sink/mode")),
        run_as: spec
            .get("runAs")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Every stream the caller may see, sorted by namespace then name. Pure: no cluster.
pub fn visible_streams(apps: &[Value], who: &Identity) -> Vec<StreamView> {
    let mut views: Vec<StreamView> = apps
        .iter()
        .filter_map(|app| {
            let spec = app.get("spec").unwrap_or(app);
            visible(who, spec).then(|| stream_view(app, who))
        })
        .collect();
    views.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    views
}

/// The `StreamApp` resources of Nova's namespace. `None` without a cluster connection.
fn stream_api(state: &AppState) -> Result<Api<kube::api::DynamicObject>, Fail> {
    let Some(client) = state.kube.clone() else {
        return Err(no_cluster());
    };
    Ok(Api::namespaced_with(
        client,
        &crate::cluster::namespace(),
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "StreamApp",
        )),
    ))
}

/// Every `StreamApp` of Nova's namespace.
async fn stream_apps(state: &AppState) -> Result<Vec<Value>, Fail> {
    let api = stream_api(state)?;
    let objects = api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    objects
        .items
        .into_iter()
        .map(|o| {
            serde_json::to_value(o)
                .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))
        })
        .collect()
}

fn not_found() -> Fail {
    // Indistinguishable from a stream that does not exist (and from one the caller may not see).
    Fail::new(axum::http::StatusCode::NOT_FOUND, "not found")
}

#[utoipa::path(
    get,
    operation_id = "streams_list",
    path = "/api/streams",
    tag = "public",
    description = "The calling user's streams: every `StreamApp` of their groups, and all of them for an administrator. `phase` is the operator's `status.phase` (`Pending` when it has not reported yet); `lastError` is the application's own terminal message when the phase is `Failed`; `lastCheckpointAt` is empty until the operator can read the checkpoint and the UI must show `—`. `stopped` is `spec.stopped`. `attempts` is how many times Spark has submitted the application and `lastSubmittedAt` when it last did; both are absent until the operator reports them. No connection credentials and no checkpoint prefix are part of an answer. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "The caller's streams, sorted by namespace and name", body = StreamsResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(caller: Caller) -> Response {
    let signed = match caller {
        Caller::Demo => return Json(crate::demo::streams()).into_response(),
        Caller::Signed(signed) => *signed,
    };
    let result = match stream_apps(signed.state()).await {
        Ok(apps) => Ok(StreamsResponse {
            streams: visible_streams(&apps, signed.identity()),
        }),
        Err(fail) => Err(fail),
    };
    answer(result)
}

/// Stop or resume one stream: merge `spec.stopped` and answer the updated view.
#[utoipa::path(
    patch,
    operation_id = "streams_patch",
    path = "/api/streams/{namespace}/{name}",
    tag = "public",
    description = "Stop or resume one stream by setting `spec.stopped` (the one verb a stream has, D42 §4.4). `true` stops the job and keeps the checkpoint so `false` resumes from it. A stream the caller may not see, a foreign namespace, and a name that does not exist all answer the same `404`. The answer is the updated stream view. Answers `Cache-Control: no-store`.",
    params(
        ("namespace" = String, Path, description = "The stream's namespace; only Nova's own namespace"),
        ("name" = String, Path, description = "The stream's name"),
    ),
    request_body = StreamState,
    responses(
        (status = 200, description = "The stream, with `spec.stopped` applied", body = StreamView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such stream, or the caller may not see it, or the namespace is not Nova's own", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn patch(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
    Json(body): Json<StreamState>,
) -> Response {
    answer(
        patch_stream(
            signed.state(),
            signed.identity(),
            &namespace,
            &name,
            body.stopped,
        )
        .await,
    )
}

async fn patch_stream(
    state: &AppState,
    who: &Identity,
    namespace: &str,
    name: &str,
    stopped: bool,
) -> Result<StreamView, Fail> {
    if namespace != crate::cluster::namespace() {
        return Err(not_found());
    }
    let api = stream_api(state)?;
    let app = api.get(name).await.map_err(|e| match e {
        kube::Error::Api(er) if er.code == 404 => not_found(),
        other => resource_fail(ResourceError::Unavailable(other.to_string())),
    })?;
    let app_value = serde_json::to_value(&app)
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    let spec = app_value.get("spec").unwrap_or(&app_value);
    if !may_act(who, spec) {
        return Err(not_found());
    }
    let patched = api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({"spec": {"stopped": stopped}})),
        )
        .await
        .map_err(|e| match e {
            kube::Error::Api(er) if er.code == 404 => not_found(),
            other => resource_fail(ResourceError::Unavailable(other.to_string())),
        })?;
    let patched = serde_json::to_value(patched)
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    Ok(stream_view(&patched, who))
}

/// The stream routes, uncacheable like the pipelines ones.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/streams", get(list))
        .route("/streams/{namespace}/{name}", patch_route(patch))
        .route_layer(axum::middleware::from_fn(crate::pipelines::no_store))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::{patch_stream, router, stream_view, visible_streams};
    use crate::auth::Identity;

    fn who(sub: &str, groups: &[&str], admin: bool) -> Identity {
        Identity {
            sub: sub.into(),
            display_name: sub.into(),
            email: None,
            name: None,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            roles: vec![],
            groups_overage: false,
            is_admin: admin,
        }
    }

    fn app(name: &str, group: &str) -> Value {
        json!({
            "metadata": {"name": name, "namespace": crate::cluster::namespace()},
            "spec": {
                "ownerGroup": group,
                "runAs": format!("pipeline-{group}"),
                "source": {"connectionRef": "pegelonline", "topic": "pegelonline.measurements"},
                "sink": {"catalog": "shared", "table": "silver.pegelonline_measurements", "mode": "merge"}
            },
            "status": {"phase": "Running", "lastError": null}
        })
    }

    #[test]
    fn a_member_sees_only_their_groups_streams() {
        let apps = vec![app("a", "sales"), app("b", "marketing")];
        let views = visible_streams(&apps, &who("u", &["sales"], false));
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "a");
        assert_eq!(views[0].owner_group, "sales");
    }

    #[test]
    fn an_administrator_sees_every_stream() {
        let apps = vec![app("a", "sales"), app("b", "marketing")];
        let views = visible_streams(&apps, &who("admin", &["stardelt-admins"], true));
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "a");
        assert_eq!(views[1].name, "b");
    }

    #[test]
    fn a_stream_without_status_is_pending_and_has_no_error() {
        let mut no_status = app("fresh", "sales");
        no_status.as_object_mut().unwrap().remove("status");
        let views = visible_streams(&[no_status], &who("u", &["sales"], false));
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].phase, "Pending");
        assert!(views[0].last_error.is_none());
        assert!(views[0].last_checkpoint_at.is_none());
        assert!(!views[0].stopped);
        assert!(views[0].reason.is_none());
        assert!(views[0].attempts.is_none());
        assert!(views[0].last_submitted_at.is_none());
    }

    #[test]
    fn a_restarting_stream_carries_the_ready_reason() {
        let mut restarting = app("loops", "sales");
        restarting["status"] = json!({
            "phase": "Running",
            "lastError": "container exited with code 1",
            "conditions": [
                {"type": "Ready", "status": "True", "reason": "Restarting"}
            ]
        });
        let view = stream_view(&restarting, &who("u", &["sales"], false));
        assert_eq!(view.phase, "Running");
        assert_eq!(view.reason.as_deref(), Some("Restarting"));
        assert_eq!(
            view.last_error.as_deref(),
            Some("container exited with code 1")
        );
    }

    #[test]
    fn a_restarted_stream_carries_its_attempt_count() {
        let mut looped = app("loops", "sales");
        looped["status"] = json!({
            "phase": "Running",
            "attempts": 25,
            "lastSubmittedAt": "2026-11-08T04:57:34Z"
        });
        let view = stream_view(&looped, &who("u", &["sales"], false));
        assert_eq!(view.attempts, Some(25));
        assert_eq!(
            view.last_submitted_at.as_deref(),
            Some("2026-11-08T04:57:34Z")
        );
    }

    #[test]
    fn a_foreign_namespace_is_not_found() {
        let caller = who("u", &["sales"], false);
        let ns = crate::cluster::namespace();
        let view = stream_view(&app("a", "sales"), &caller);
        assert_eq!(view.namespace, ns);
        // The namespace guard is enforced by patch_stream; the pure mapping stays small.
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

        type Shared = Arc<Mutex<Fake>>;

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
            if let Some(stopped) = body.pointer("/spec/stopped").and_then(Value::as_bool) {
                item["spec"]["stopped"] = json!(stopped);
            }
            Ok(Json(item.clone()))
        }

        pub async fn start_with_patch(items: Vec<Value>) -> (kube::Client, Shared) {
            let shared = Arc::new(Mutex::new(Fake {
                items,
                patches: Vec::new(),
            }));
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/streamapps",
                    get(|| async {
                        Json(json!({"apiVersion":"stardelt.io/v1alpha1","kind":"StreamAppList","items":[]}))
                    }),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/streamapps/{name}",
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

    #[tokio::test]
    async fn an_owner_may_patch_and_the_merge_body_sets_stopped() {
        let (client, shared) = fake_kube::start_with_patch(vec![app("demo", "sales")]).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let ns = crate::cluster::namespace();
        let view = patch_stream(&state, &who("u", &["sales"], false), &ns, "demo", true)
            .await
            .expect("owner may patch");
        assert!(view.stopped);
        assert_eq!(view.name, "demo");
        let patches = shared.lock().unwrap().patches.clone();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0], json!({"spec": {"stopped": true}}));
    }

    #[tokio::test]
    async fn a_member_of_another_group_gets_404() {
        let (client, _) = fake_kube::start_with_patch(vec![app("demo", "sales")]).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let ns = crate::cluster::namespace();
        let err = patch_stream(&state, &who("u", &["marketing"], false), &ns, "demo", true)
            .await
            .expect_err("outsider gets 404");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_foreign_namespace_gets_404() {
        let (client, shared) = fake_kube::start_with_patch(vec![app("demo", "sales")]).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let err = patch_stream(&state, &who("u", &["sales"], false), "other", "demo", true)
            .await
            .expect_err("foreign namespace gets 404");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert!(shared.lock().unwrap().patches.is_empty());
    }

    /// Demo mode has neither a principal nor a cluster, so `GET /api/streams` answers the two
    /// sample rows instead of the 503 the extractor used to give (#500).
    #[tokio::test]
    async fn demo_mode_lists_streams_without_a_store() {
        use tower::ServiceExt;

        let app = axum::Router::new()
            .nest("/api", router())
            .with_state(crate::router_tests::test_state());
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/streams")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("pegelonline-live"), "{text}");
        assert!(text.contains("orders-cdc"), "{text}");
    }
}
