//! The `StreamApp` API (D42): what runs on the streaming bus, and how it is doing.
//!
//! Slice 1 is read-only. Slice 3a adds the one verb a stream has — stop and resume by
//! patching `spec.stopped` (the checkpoint is kept, so `stopped: false` resumes from it).

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch as patch_route, post},
};
use kube::api::{Api, ApiResource, Patch, PatchParams};
use kube::core::GroupVersionKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::auth::Identity;
use crate::connection_routes::{Caller, Fail, SignedIn};
use crate::pipelines::{
    MANAGED_BY_LABEL, MANAGED_BY_NOVA, ResourceError, answer, may_act, no_cluster, resource_fail,
    visible,
};

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

/// The one verb a stream has: stop or resume by setting `spec.stopped`.
#[derive(Deserialize, ToSchema)]
pub struct StreamState {
    /// The desired `spec.stopped`.
    pub stopped: bool,
}

/// The body of a stream write (`nova#617` slice a). No credentials: `connectionRef` is the
/// name of a `Connection`, and Nova never stores or forwards a secret.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamWrite {
    /// The stream's name; the `PUT` path is authoritative and a different body name is refused.
    pub name: String,
    /// The group that owns the stream; the caller must be a member.
    pub owner_group: String,
    pub source: StreamSourceBody,
    pub sink: StreamSinkBody,
    /// `always` (default) or `on-failure`.
    #[serde(default)]
    pub restart_policy: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamSourceBody {
    /// Name of the `Connection` (`type: kafka`); absent means the platform's own bus.
    #[serde(default)]
    pub connection_ref: Option<String>,
    pub topic: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StreamSinkBody {
    pub catalog: String,
    /// `schema.table`.
    pub table: String,
    /// `merge` (default) or `append`.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub merge_keys: Vec<String>,
    #[serde(default)]
    pub checkpoint_prefix: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StreamValidation {
    pub ok: bool,
    pub errors: Vec<StreamIssue>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StreamIssue {
    pub message: String,
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

// ---------------------------------------------------------------------------
// The write rules (pure: no cluster)
// ---------------------------------------------------------------------------

/// What a write produces once every rule has passed.
#[derive(Debug)]
struct PlannedStream {
    group: String,
    spec: Value,
}

/// `^[a-z0-9]([-a-z0-9]*[a-z0-9])?$` with a length limit: a connection reference.
fn is_reference(value: &str) -> bool {
    let bytes = value.as_bytes();
    let plain = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    !bytes.is_empty()
        && bytes.len() <= 63
        && plain(bytes[0])
        && plain(bytes[bytes.len() - 1])
        && bytes.iter().all(|b| plain(*b) || *b == b'-')
}

/// `^[a-z0-9][a-z0-9-]*$` with a length limit: a catalog name.
fn is_catalog(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `[a-z0-9_]+`: one part of a `schema.table`.
fn is_table_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn bad_topic() -> String {
    "a stream needs a topic of 1 to 249 characters: letters, digits, dot, underscore or dash"
        .to_string()
}

fn bad_table() -> String {
    "sink.table is schema.table, lower-case letters, digits and underscores".to_string()
}

/// Every rule a stream write runs, in order, with the CRD's own limits
/// (`crates/stardelt-operator/src/crd.rs:1953-2072`). `Ok` carries the spec to write.
fn check_write(name: &str, body: &StreamWrite, who: &Identity) -> Result<PlannedStream, String> {
    // 1. The name the CRD (and the operator) accepts.
    crate::pipelines::validate_name(name)?;

    // 2. The path names the stream; a body may not rename it.
    if body.name != name {
        return Err("the body's name does not match the path".to_string());
    }

    // 3. One of the caller's own groups — an administrator writes for their groups too.
    if !who.groups.iter().any(|group| group == &body.owner_group) {
        return Err(format!(
            "you are not a member of the group {}",
            body.owner_group
        ));
    }

    // 4. The topic (the CRD's `^[A-Za-z0-9._-]{1,249}$`).
    let topic = &body.source.topic;
    if topic.is_empty()
        || topic.len() > 249
        || !topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(bad_topic());
    }

    // 5. The connection reference, when one is named (a `Connection` name).
    if let Some(reference) = body.source.connection_ref.as_deref()
        && !is_reference(reference)
    {
        return Err(
            "source.connectionRef is a Connection name: 1 to 63 lower-case letters, digits and dashes"
                .to_string(),
        );
    }

    // 6. The catalog the sink table lives in.
    if !is_catalog(&body.sink.catalog) {
        return Err("sink.catalog is 1 to 63 lower-case letters, digits and dashes".to_string());
    }

    // 7. The sink table: `schema.table`, both parts lower-case, at most 128 characters.
    let table = &body.sink.table;
    match table.split_once('.') {
        Some((schema, rest))
            if is_table_part(schema) && is_table_part(rest) && table.len() <= 128 => {}
        _ => return Err(bad_table()),
    }

    // 8. The sink mode, `merge` when absent.
    let mode = body.sink.mode.as_deref().unwrap_or("merge");
    if mode != "merge" && mode != "append" {
        return Err("sink.mode is merge or append".to_string());
    }

    // 9. A merge sink needs 1 to 8 keys; an append sink names none.
    if mode == "merge" {
        if body.sink.merge_keys.is_empty() || body.sink.merge_keys.len() > 8 {
            return Err(
                "a merge sink needs 1 to 8 mergeKeys, each lower-case letters, digits and underscores"
                    .to_string(),
            );
        }
        if body
            .sink
            .merge_keys
            .iter()
            .any(|key| key.len() > 63 || !is_table_part(key))
        {
            return Err(
                "a merge key is 1 to 63 lower-case letters, digits and underscores".to_string(),
            );
        }
    } else if !body.sink.merge_keys.is_empty() {
        return Err("an append sink may not name mergeKeys".to_string());
    }

    // 10. The checkpoint prefix, when one is named.
    if let Some(prefix) = body.sink.checkpoint_prefix.as_deref() {
        let first = prefix.chars().next();
        if prefix.is_empty()
            || prefix.len() > 512
            || !first.is_some_and(|c| c.is_ascii_alphanumeric())
            || !prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
        {
            return Err(
                "sink.checkpointPrefix is 1 to 512 characters: letters, digits, dot, underscore, slash or dash"
                    .to_string(),
            );
        }
    }

    // 11. The restart policy, `always` when absent.
    let restart = body.restart_policy.as_deref().unwrap_or("always");
    if restart != "always" && restart != "on-failure" {
        return Err("restartPolicy is always or on-failure".to_string());
    }

    let mut source = json!({ "topic": body.source.topic });
    if let Some(reference) = &body.source.connection_ref {
        source["connectionRef"] = json!(reference);
    }
    let mut sink = json!({
        "catalog": body.sink.catalog,
        "table": body.sink.table,
        "mode": mode,
        "mergeKeys": body.sink.merge_keys,
    });
    if let Some(prefix) = &body.sink.checkpoint_prefix {
        sink["checkpointPrefix"] = json!(prefix);
    }
    Ok(PlannedStream {
        group: body.owner_group.clone(),
        // `runAs` is server-set: the stream runs as the group's principal (D42 decision 6).
        // `stopped` is never part of a write: it defaults to `false` on create and an update
        // leaves the stored value alone.
        spec: json!({
            "ownerGroup": body.owner_group,
            "runAs": format!("pipeline-{}", body.owner_group),
            "source": source,
            "sink": sink,
            "restartPolicy": restart,
        }),
    })
}

/// Nova's labels for a stream: who wrote it and which group owns it.
fn stream_labels(group: &str) -> Value {
    let mut labels = serde_json::Map::new();
    labels.insert(MANAGED_BY_LABEL.to_string(), json!(MANAGED_BY_NOVA));
    labels.insert("stardelt.io/owner-group".to_string(), json!(group));
    Value::Object(labels)
}

/// The `StreamApp` a create writes: Nova's labels, the group's principal, no client-supplied
/// metadata.
fn stream_resource(name: &str, planned: &PlannedStream, created_by: &str) -> Value {
    json!({
        "apiVersion": "stardelt.io/v1alpha1",
        "kind": "StreamApp",
        "metadata": {
            "name": name,
            "namespace": crate::cluster::namespace(),
            "labels": stream_labels(&planned.group),
            "annotations": { "stardelt.io/created-by": created_by },
        },
        "spec": planned.spec,
    })
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
    description = "The calling user's streams: every `StreamApp` of their groups, and all of them for an administrator. `phase` is the operator's `status.phase` (`Pending` when it has not reported yet); `lastError` is the application's own terminal message when the phase is `Failed`; `lastCheckpointAt` is the time of the newest checkpoint commit, absent until the operator reports it. `stopped` is `spec.stopped`. `attempts` is how many times Spark has submitted the application and `lastSubmittedAt` when it last did; both are absent until the operator reports them. No connection credentials and no checkpoint prefix are part of an answer. Answers `Cache-Control: no-store`.",
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
    description = "Stop or resume one stream by setting `spec.stopped` (the one verb a stream has). `true` stops the job and keeps the checkpoint so `false` resumes from it. A stream the caller may not see, a foreign namespace, and a name that does not exist all answer the same `404`. The answer is the updated stream view. Answers `Cache-Control: no-store`.",
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

/// The principal check: `Ok(Ok(()))` when the group's `ServicePrincipal` is ready,
/// `Ok(Err(sentence))` when it is not, `Err` when the cluster did not answer.
async fn principal_ready(state: &AppState, group: &str) -> Result<Result<(), String>, Fail> {
    let Some(client) = state.kube.clone() else {
        return Err(no_cluster());
    };
    let principals = crate::pipelines::PipelineResources::cluster(client)
        .principals()
        .await
        .map_err(resource_fail)?;
    let view = crate::pipelines::principal_view(&principals, group);
    if view.ready {
        Ok(Ok(()))
    } else {
        Ok(Err(view.message.unwrap_or_default()))
    }
}

fn refused(message: String) -> StreamValidation {
    StreamValidation {
        ok: false,
        errors: vec![StreamIssue { message }],
    }
}

/// Check a write without writing it: every rule, then the group's principal.
async fn validate_stream(
    state: &AppState,
    who: &Identity,
    body: StreamWrite,
) -> Result<StreamValidation, Fail> {
    if let Err(message) = check_write(&body.name, &body, who) {
        return Ok(refused(message));
    }
    match principal_ready(state, &body.owner_group).await? {
        Ok(()) => Ok(StreamValidation {
            ok: true,
            errors: Vec::new(),
        }),
        Err(message) => Ok(refused(message)),
    }
}

fn to_value(app: kube::api::DynamicObject) -> Result<Value, Fail> {
    serde_json::to_value(app).map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))
}

fn unavailable(error: kube::Error) -> Fail {
    resource_fail(ResourceError::Unavailable(error.to_string()))
}

/// A stream Nova did not write is not changed here: it belongs to whoever wrote it.
fn ensure_nova_managed(app: &Value) -> Result<(), Fail> {
    if crate::pipelines::nova_managed(app) {
        Ok(())
    } else {
        Err(Fail::new(
            StatusCode::CONFLICT,
            "this stream was created outside Nova; change it where it was created",
        ))
    }
}

/// The update path: merge the planned spec and Nova's labels into the stored object, answer the
/// written object's view.
async fn update_stream(
    api: &Api<kube::api::DynamicObject>,
    name: &str,
    planned: &PlannedStream,
    who: &Identity,
) -> Result<StreamView, Fail> {
    let patched = api
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"labels": stream_labels(&planned.group)},
                "spec": planned.spec,
            })),
        )
        .await
        .map_err(|e| match e {
            kube::Error::Api(er) if er.code == 404 => not_found(),
            kube::Error::Api(er) if er.code == 409 => Fail::new(
                StatusCode::CONFLICT,
                "the stream changed while it was being written; retry",
            ),
            other => unavailable(other),
        })?;
    Ok(stream_view(&to_value(patched)?, who))
}

/// Create or update one stream: the body is the whole editable state, `spec.stopped` is left
/// alone, and the answer is the stored object's view.
async fn put_stream(
    state: &AppState,
    who: &Identity,
    namespace: &str,
    name: &str,
    body: StreamWrite,
) -> Result<StreamView, Fail> {
    if namespace != crate::cluster::namespace() {
        return Err(not_found());
    }
    let planned = check_write(name, &body, who)
        .map_err(|message| Fail::new(StatusCode::BAD_REQUEST, message))?;
    match principal_ready(state, &body.owner_group).await? {
        Ok(()) => {}
        Err(message) => return Err(Fail::new(StatusCode::CONFLICT, message)),
    }
    let api = stream_api(state)?;

    let existing = match api.get(name).await {
        Ok(app) => Some(to_value(app)?),
        Err(kube::Error::Api(er)) if er.code == 404 => None,
        Err(other) => return Err(unavailable(other)),
    };
    if let Some(existing) = existing {
        ensure_nova_managed(&existing)?;
        return update_stream(&api, name, &planned, who).await;
    }

    let resource = stream_resource(name, &planned, &who.display_name);
    let object: kube::api::DynamicObject = serde_json::from_value(resource)
        .map_err(|e| resource_fail(ResourceError::Unavailable(e.to_string())))?;
    match api.create(&kube::api::PostParams::default(), &object).await {
        Ok(created) => Ok(stream_view(&to_value(created)?, who)),
        // Another writer won the race between the read and the create: take the update path once.
        Err(kube::Error::Api(er)) if er.code == 409 => {
            let existing = api.get(name).await.map_err(|e| match e {
                kube::Error::Api(er) if er.code == 404 => not_found(),
                other => unavailable(other),
            })?;
            ensure_nova_managed(&to_value(existing)?)?;
            update_stream(&api, name, &planned, who).await
        }
        Err(other) => Err(unavailable(other)),
    }
}

#[utoipa::path(
    post,
    operation_id = "streams_validate",
    path = "/api/streams/validate",
    tag = "public",
    description = "Check a stream write without writing it. The body is the body of `streams_put`: the stream's `name`, the `ownerGroup` the caller must belong to, `source` (`topic`, optional `connectionRef` naming a `Connection`) and `sink` (`catalog`, `table` as `schema.table`, `mode` `merge` or `append`, `mergeKeys`, optional `checkpointPrefix`), plus an optional `restartPolicy` (`always` or `on-failure`). Every rule a write runs is checked in order — the name, the body's name against the path's, the caller's groups, the topic, the connection reference, the catalog, the table, the mode, the merge keys, the checkpoint prefix, the restart policy — and then the group's ready `ServicePrincipal`. A rule that refuses is an answer, `ok: false` with one sentence in `errors`, status 200, never a 4xx; nothing is written. A cluster that does not answer is still 502 or 503, as on `pipelines_validate`. Answers `Cache-Control: no-store`.",
    request_body = StreamWrite,
    responses(
        (status = 200, description = "The verdict: accepted, or the one sentence that refuses it", body = StreamValidation),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn validate(signed: SignedIn, ApiJson(body): ApiJson<StreamWrite>) -> Response {
    answer(validate_stream(signed.state(), signed.identity(), body).await)
}

#[utoipa::path(
    put,
    operation_id = "streams_put",
    path = "/api/streams/{namespace}/{name}",
    tag = "public",
    description = "Create or update one stream (`StreamApp`, D42). The body is the body of `streams_validate`; `name` must be the path's name. Nova writes `runAs: pipeline-<groupId>` itself (never from the body), stamps its own labels and the creator's annotation, and never carries `stopped`: it defaults to `false` on create and an update leaves the stored value alone. Refusals, in this order: a name outside 1 to 40 lower-case letters, digits and dashes starting with a letter, a body name that differs from the path, a group the caller is not a member of, a topic outside 1 to 249 letters, digits, dot, underscore or dash, a bad connection reference, catalog, table, mode, merge key, checkpoint prefix or restart policy — all 400; a group without a ready `ServicePrincipal` 409 with the `principals` route's message; a stream Nova did not write 409 `this stream was created outside Nova; change it where it was created`; a stream another writer changed between the read and the write 409 `the stream changed while it was being written; retry`. A foreign namespace and a stream that vanished are 404. The answer is the stored stream's view. Answers `Cache-Control: no-store`.",
    params(
        ("namespace" = String, Path, description = "The stream's namespace; only Nova's own namespace"),
        ("name" = String, Path, description = "The stream's name"),
    ),
    request_body = StreamWrite,
    responses(
        (status = 200, description = "The stream as written", body = StreamView),
        (status = 400, description = "A rule of the body refuses the write, with its sentence", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "The namespace is not Nova's own", body = crate::openapi::ErrorResponse),
        (status = 409, description = "No ready principal, a stream written outside Nova, or a concurrent change", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn put(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
    ApiJson(body): ApiJson<StreamWrite>,
) -> Response {
    answer(put_stream(signed.state(), signed.identity(), &namespace, &name, body).await)
}

/// The stream routes, uncacheable like the pipelines ones.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/streams", get(list))
        .route("/streams/validate", post(validate))
        .route("/streams/{namespace}/{name}", patch_route(patch).put(put))
        .route_layer(axum::middleware::from_fn(crate::pipelines::no_store))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::{
        StreamSinkBody, StreamSourceBody, StreamWrite, check_write, patch_stream, router,
        stream_resource, stream_view, visible_streams,
    };
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

    /// A write body that every rule accepts, so a test can break exactly one field.
    fn body_named(name: &str) -> StreamWrite {
        StreamWrite {
            name: name.to_string(),
            owner_group: "sales".to_string(),
            source: StreamSourceBody {
                connection_ref: Some("pegelonline".to_string()),
                topic: "pegelonline.measurements".to_string(),
            },
            sink: StreamSinkBody {
                catalog: "shared".to_string(),
                table: "silver.pegelonline_measurements".to_string(),
                mode: None,
                merge_keys: vec!["station_id".to_string()],
                checkpoint_prefix: None,
            },
            restart_policy: None,
        }
    }

    #[test]
    fn a_write_needs_a_group_the_caller_is_in() {
        let error = check_write(
            "pegelonline-live",
            &body_named("pegelonline-live"),
            &who("u", &["marketing"], false),
        )
        .expect_err("a group the caller is not in is refused");
        assert_eq!(error, "you are not a member of the group sales");
    }

    #[test]
    fn the_body_name_must_match_the_path() {
        let error = check_write("a", &body_named("b"), &who("u", &["sales"], false))
            .expect_err("the body may not rename the stream");
        assert_eq!(error, "the body's name does not match the path");
    }

    #[test]
    fn a_merge_sink_needs_one_to_eight_merge_keys() {
        let caller = who("u", &["sales"], false);
        let mut none = body_named("a");
        none.sink.merge_keys = vec![];
        let error = check_write("a", &none, &caller).expect_err("no key is refused");
        assert!(error.contains("1 to 8 mergeKeys"), "{error}");

        let mut nine = body_named("a");
        nine.sink.merge_keys = (0..9).map(|i| format!("key_{i}")).collect();
        let error = check_write("a", &nine, &caller).expect_err("nine keys are refused");
        assert!(error.contains("1 to 8 mergeKeys"), "{error}");

        let mut two = body_named("a");
        two.sink.merge_keys = vec!["station_id".to_string(), "observed_at".to_string()];
        let planned = check_write("a", &two, &caller).expect("two keys are accepted");
        assert_eq!(
            planned.spec.pointer("/sink/mergeKeys"),
            Some(&json!(["station_id", "observed_at"]))
        );
    }

    #[test]
    fn an_append_sink_may_not_name_merge_keys() {
        let mut append = body_named("a");
        append.sink.mode = Some("append".to_string());
        let error = check_write("a", &append, &who("u", &["sales"], false))
            .expect_err("an append sink with a key is refused");
        assert_eq!(error, "an append sink may not name mergeKeys");

        append.sink.merge_keys = vec![];
        let planned = check_write("a", &append, &who("u", &["sales"], false))
            .expect("an append sink without keys is accepted");
        assert_eq!(
            planned.spec.pointer("/sink/mode"),
            Some(&json!("append")),
            "the body's mode is written"
        );
    }

    #[test]
    fn a_bad_topic_and_a_bad_table_are_refused() {
        let caller = who("u", &["sales"], false);
        let mut topic = body_named("a");
        topic.source.topic = "bad topic!".to_string();
        let error = check_write("a", &topic, &caller).expect_err("a bad topic is refused");
        assert_eq!(
            error,
            "a stream needs a topic of 1 to 249 characters: letters, digits, dot, underscore or dash"
        );

        let mut table = body_named("a");
        table.sink.table = "silver".to_string();
        let error =
            check_write("a", &table, &caller).expect_err("a table without a dot is refused");
        assert_eq!(
            error,
            "sink.table is schema.table, lower-case letters, digits and underscores"
        );
    }

    #[test]
    fn the_resource_carries_nova_labels_and_the_group_principal() {
        let planned = check_write("a", &body_named("a"), &who("u", &["sales"], false))
            .expect("the body is accepted");
        let resource = stream_resource("a", &planned, "Ada Lovelace");
        assert_eq!(
            resource.pointer("/spec/runAs"),
            Some(&json!("pipeline-sales"))
        );
        assert_eq!(
            resource.pointer("/metadata/labels/app.kubernetes.io~1managed-by"),
            Some(&json!("stardelt-nova"))
        );
        assert_eq!(
            resource.pointer("/metadata/labels/stardelt.io~1owner-group"),
            Some(&json!("sales"))
        );
        assert_eq!(
            resource.pointer("/metadata/annotations/stardelt.io~1created-by"),
            Some(&json!("Ada Lovelace"))
        );
        assert_eq!(resource.pointer("/spec/stopped"), None, "no `stopped`");
        assert_eq!(resource["kind"], json!("StreamApp"));
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
