//! The `StreamApp` read API (D42): what runs on the streaming bus, and how it is doing.
//!
//! Read-only in slice 1: the operator owns the resources, Nova explains them. Starting and
//! stopping a stream (the operator's `spec.stopped`) is a separate ticket.

use std::sync::Arc;

use axum::{Router, response::Response, routing::get};
use kube::api::{Api, ApiResource};
use kube::core::GroupVersionKind;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::Identity;
use crate::connection_routes::{Fail, SignedIn};
use crate::pipelines::{ResourceError, answer, no_cluster, resource_fail, visible};

/// One `StreamApp`, as the Streams page needs it. No secret, no checkpoint path.
#[derive(Serialize, ToSchema)]
pub struct StreamView {
    pub namespace: String,
    pub name: String,
    pub owner_group: String,
    pub phase: String,
    pub last_error: Option<String>,
    pub last_checkpoint_at: Option<String>,
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

/// Every stream the caller may see, sorted by namespace then name. Pure: no cluster.
pub fn visible_streams(apps: &[Value], who: &Identity) -> Vec<StreamView> {
    let mut views: Vec<StreamView> = apps
        .iter()
        .filter_map(|app| {
            let spec = app.get("spec").unwrap_or(app);
            if !visible(who, spec) {
                return None;
            }
            let text =
                |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
            let status = app.get("status").unwrap_or(&Value::Null);
            let phase = text(status.get("phase"));
            Some(StreamView {
                namespace: text(app.pointer("/metadata/namespace")),
                name: text(app.pointer("/metadata/name")),
                owner_group: text(spec.get("ownerGroup")),
                phase: if phase.is_empty() {
                    "Pending".to_string()
                } else {
                    phase
                },
                last_error: status
                    .get("lastError")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                last_checkpoint_at: status
                    .get("lastCheckpointAt")
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
            })
        })
        .collect();
    views.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    views
}

/// The `StreamApp` list of Nova's namespace; `None` without a cluster connection.
async fn stream_apps(state: &AppState) -> Result<Vec<Value>, Fail> {
    let Some(client) = state.kube.clone() else {
        return Err(no_cluster());
    };
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(
        client,
        &crate::cluster::namespace(),
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "StreamApp",
        )),
    );
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

#[utoipa::path(
    get,
    operation_id = "streams_list",
    path = "/api/streams",
    tag = "public",
    description = "The calling user's streams: every `StreamApp` of their groups, and all of them for an administrator. `phase` is the operator's `status.phase` (`Pending` when it has not reported yet); `lastError` is the application's own terminal message when the phase is `Failed`; `lastCheckpointAt` is empty until the operator can read the checkpoint and the UI must show `—`. `stopped` is `spec.stopped`. No connection credentials and no checkpoint prefix are part of an answer. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "The caller's streams, sorted by namespace and name", body = StreamsResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(signed: SignedIn) -> Response {
    let result = match stream_apps(signed.state()).await {
        Ok(apps) => Ok(StreamsResponse {
            streams: visible_streams(&apps, signed.identity()),
        }),
        Err(fail) => Err(fail),
    };
    answer(result)
}

/// The two routes, uncacheable like the pipelines ones.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/streams", get(list))
        .route_layer(axum::middleware::from_fn(crate::pipelines::no_store))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::visible_streams;
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

    fn app(name: &str, group: &str) -> serde_json::Value {
        json!({
            "metadata": {"name": name, "namespace": "stardelt"},
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
    }
}
