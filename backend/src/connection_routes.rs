//! The Connections API: list, create, rotate and delete (stardelt-nova#171; decisions D23 and
//! D27).
//!
//! This module is the HTTP surface over the pure rules in `connections.rs` and the store client in
//! `secret_store.rs`. Two promises the whole file is built around:
//!
//! - **A secret value is written and never read back.** No response, error sentence or log line
//!   here carries a value: a 400 repeats a sentence from the rules (which names the field, not its
//!   content), an upstream failure repeats the store's own first line (which does not echo
//!   values), and the audit event carries the scope, the group, the name and the person.
//! - **An unreachable upstream is 502, never 500** (rule U12 of `design/ui-conformance.md`).
//!   Without a configured store every route answers 503 and says so; without a cluster client the
//!   group and managed routes answer 503 `no cluster connection`.
//!
//! The store is signed in with the caller's own token: Nova never holds a service identity, so the
//! store's policies — a person's own prefix and the prefixes of their groups, managed values
//! write-only — decide what is possible, and the store's audit log names the person.
//!
//! The route logic sits in plain `async fn`s that take their inputs, so the tests call them
//! without a router; each handler is the thin axum wrapper that checks sign-in and the store.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Path, State},
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use futures::stream::{self, StreamExt};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::api_json::ApiJson;
use crate::audit::{self, BreakGlassRecord};
use crate::auth::{Identity, Principal, Secret};
use crate::connections::{
    self, Action, BreakGlassNotice, ConnectionView, NewConnection, Scope, SyncView,
};
use crate::secret_store::{self, SecretMeta, StoreClient, StoreError, StoreScope, StoreToken};

/// The annotation a break-glass read leaves on the group's `Connection`. The listing turns it
/// into `breakGlass`; a rotation removes it.
pub const BREAK_GLASS_ANNOTATION: &str = "stardelt.io/break-glass";

/// At most this many attempted break-glass reads per administrator in [`BREAK_GLASS_WINDOW`].
const BREAK_GLASS_LIMIT: usize = 5;
const BREAK_GLASS_WINDOW: Duration = Duration::from_secs(10 * 60);

/// The in-memory per-admin rate limit (`AppState::break_glass_reads`).
pub type BreakGlassLimiter = Mutex<HashMap<String, VecDeque<Instant>>>;

/// The `Connection` resources of the platform namespace: the cluster, or memory in tests.
pub enum Resources {
    Kube(Api<DynamicObject>),
    #[cfg(test)]
    Memory {
        items: Arc<std::sync::Mutex<BTreeMap<String, Value>>>,
        /// Answer `Unavailable` from `delete`, as an API server that refuses would.
        fail_delete: bool,
        /// Answer `Unavailable` from `annotate`, as an API server that refuses would.
        fail_annotate: bool,
    },
}

#[derive(Debug)]
pub enum ResourceError {
    /// The name is taken: the answer is 409, never an overwrite.
    Exists,
    /// The cluster did not answer or refused.
    Unavailable(String),
}

impl Resources {
    /// The connection resources of Nova's namespace, through the cluster client.
    pub fn cluster(client: kube::Client) -> Self {
        let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "Connection",
        ));
        Self::Kube(Api::namespaced_with(
            client,
            &crate::cluster::namespace(),
            &ar,
        ))
    }

    /// A real create, not an apply: a name that already exists is [`ResourceError::Exists`], so a
    /// second person cannot silently take over a connection somebody else made.
    async fn create(&self, resource: Value) -> Result<(), ResourceError> {
        match self {
            Self::Kube(api) => {
                let object: DynamicObject = serde_json::from_value(resource)
                    .map_err(|e| ResourceError::Unavailable(format!("not a resource: {e}")))?;
                api.create(&kube::api::PostParams::default(), &object)
                    .await
                    .map(|_| ())
                    .map_err(|e| match e {
                        kube::Error::Api(er) if er.code == 409 => ResourceError::Exists,
                        other => ResourceError::Unavailable(other.to_string()),
                    })
            }
            #[cfg(test)]
            Self::Memory { items, .. } => {
                let mut items = items.lock().expect("memory resources poisoned");
                let name = resource_name(&resource).to_string();
                if items.contains_key(&name) {
                    return Err(ResourceError::Exists);
                }
                items.insert(name, resource);
                Ok(())
            }
        }
    }

    /// Delete one; a resource that is already gone is success.
    async fn delete(&self, name: &str) -> Result<(), ResourceError> {
        match self {
            Self::Kube(api) => match api.delete(name, &kube::api::DeleteParams::default()).await {
                Ok(_) => Ok(()),
                Err(kube::Error::Api(er)) if er.code == 404 => Ok(()),
                Err(e) => Err(ResourceError::Unavailable(e.to_string())),
            },
            #[cfg(test)]
            Self::Memory {
                items, fail_delete, ..
            } => {
                if *fail_delete {
                    return Err(ResourceError::Unavailable("the API server refused".into()));
                }
                items
                    .lock()
                    .expect("memory resources poisoned")
                    .remove(name);
                Ok(())
            }
        }
    }

    /// Set or remove one annotation on a `Connection`, through a JSON merge patch that touches
    /// `metadata.annotations` only. `None` removes the key. A failure here matters: a break-glass
    /// read answers nothing unless the owner group's notice could be written.
    pub async fn annotate(
        &self,
        name: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), ResourceError> {
        match self {
            Self::Kube(api) => {
                let patch = json!({ "metadata": { "annotations": { key: value } } });
                api.patch(name, &PatchParams::default(), &Patch::Merge(patch))
                    .await
                    .map(|_| ())
                    .map_err(|e| match e {
                        kube::Error::Api(er) if er.code == 404 => {
                            ResourceError::Unavailable("no such resource".into())
                        }
                        other => ResourceError::Unavailable(other.to_string()),
                    })
            }
            #[cfg(test)]
            Self::Memory {
                items,
                fail_annotate,
                ..
            } => {
                if *fail_annotate {
                    return Err(ResourceError::Unavailable("the API server refused".into()));
                }
                let mut items = items.lock().expect("memory resources poisoned");
                let Some(resource) = items.get_mut(name) else {
                    return Err(ResourceError::Unavailable("no such resource".into()));
                };
                let annotations = resource
                    .as_object_mut()
                    .and_then(|o| o.get_mut("metadata"))
                    .and_then(Value::as_object_mut)
                    .map(|m| {
                        m.entry("annotations")
                            .or_insert_with(|| Value::Object(Default::default()))
                    });
                let Some(Value::Object(annotations)) = annotations else {
                    return Err(ResourceError::Unavailable(
                        "the resource has no annotation map".into(),
                    ));
                };
                match value {
                    Some(value) => {
                        annotations.insert(key.to_string(), Value::String(value.to_string()));
                    }
                    None => {
                        annotations.remove(key);
                    }
                }
                Ok(())
            }
        }
    }

    /// Every `Connection` of the namespace. Administrators index the managed ones from here: a
    /// managed value cannot be read, so the resource is what says a connection exists.
    async fn list(&self) -> Result<Vec<Value>, ResourceError> {
        match self {
            Self::Kube(api) => {
                let objects = api
                    .list(&kube::api::ListParams::default())
                    .await
                    .map_err(|e| ResourceError::Unavailable(e.to_string()))?;
                objects
                    .items
                    .into_iter()
                    .map(|o| {
                        serde_json::to_value(o)
                            .map_err(|e| ResourceError::Unavailable(e.to_string()))
                    })
                    .collect()
            }
            #[cfg(test)]
            Self::Memory { items, .. } => Ok(items
                .lock()
                .expect("memory resources poisoned")
                .values()
                .cloned()
                .collect()),
        }
    }
}

fn resource_name(resource: &Value) -> &str {
    resource["metadata"]["name"].as_str().unwrap_or_default()
}

/// One non-2xx answer: a status and a sentence. The sentence never contains a value.
#[derive(Debug)]
pub(crate) struct Fail {
    status: StatusCode,
    message: String,
}

impl Fail {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// The status, for a test that inspects a refusal without serialising it.
    #[cfg(test)]
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    /// The sentence, for a test that inspects a refusal.
    #[cfg(test)]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

fn not_configured() -> Fail {
    Fail::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "the secret store is not configured",
    )
}

fn no_cluster() -> Fail {
    Fail::new(StatusCode::SERVICE_UNAVAILABLE, "no cluster connection")
}

fn conflict() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "a connection with this name already exists",
    )
}

fn not_found() -> Fail {
    Fail::new(StatusCode::NOT_FOUND, "no such connection")
}

fn sign_in_again() -> Fail {
    Fail::new(StatusCode::UNAUTHORIZED, "sign in again")
}

/// The status for a store error. An unreachable or refusing store is 502, never 500 (U12); an
/// absent one is 503 and says so.
fn store_fail(e: StoreError) -> Fail {
    match e {
        StoreError::NotConfigured => not_configured(),
        StoreError::NotFound => not_found(),
        StoreError::Invalid(m) => Fail::new(StatusCode::BAD_REQUEST, m),
        StoreError::Refused(m) => Fail::new(
            StatusCode::BAD_GATEWAY,
            format!("the secret store refused the request: {m}"),
        ),
        StoreError::Unavailable(m) => Fail::new(
            StatusCode::BAD_GATEWAY,
            format!("the secret store is unavailable: {m}"),
        ),
    }
}

/// The status for a cluster error: the sentence stays generic (a kube error names the API server),
/// the detail goes to the log.
fn cluster_fail(e: ResourceError) -> Fail {
    match e {
        ResourceError::Exists => conflict(),
        ResourceError::Unavailable(m) => {
            tracing::warn!(error = %m, "connection resource request failed");
            Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
        }
    }
}

/// `GET /api/connections` answer.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ConnectionsResponse {
    pub connections: Vec<ConnectionView>,
}

/// `PUT …/secret` body: the field values to write. Write-only, like the create body, and never
/// printed — no `Debug` on purpose.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct RotateRequest {
    /// The fields to rotate; a managed connection sends every field.
    #[schema(write_only)]
    pub secret: BTreeMap<String, String>,
}

/// The body extractor of the connection routes: the repo's [`ApiJson`] (a rejection is
/// `{"error": …}`, never plain text) with one status changed — a body that is valid JSON but not a
/// connection is 400, not 422, so a caller has one status to program against. The sentence comes
/// from serde and never echoes a value.
pub(crate) struct ConnectionJson<T>(pub T);

impl<S, T> FromRequest<S> for ConnectionJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = Fail;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match ApiJson::<T>::from_request(req, state).await {
            Ok(ApiJson(value)) => Ok(Self(value)),
            Err(e) => {
                let status = if e.status() == StatusCode::UNPROCESSABLE_ENTITY {
                    StatusCode::BAD_REQUEST
                } else {
                    e.status()
                };
                Err(Fail::new(status, e.message().to_string()))
            }
        }
    }
}

fn scope_word(scope: Scope) -> &'static str {
    match scope {
        Scope::Personal => "personal",
        Scope::Group => "group",
        Scope::Managed => "managed",
    }
}

fn scope_rank(scope: Scope) -> u8 {
    match scope {
        Scope::Personal => 0,
        Scope::Group => 1,
        Scope::Managed => 2,
    }
}

/// The rules' scope and the owner group for one store scope.
fn scope_of(scope: &StoreScope) -> (Scope, Option<&str>) {
    match scope {
        StoreScope::Personal { .. } => (Scope::Personal, None),
        StoreScope::Group { group } => (Scope::Group, Some(group.as_str())),
        StoreScope::Managed => (Scope::Managed, None),
    }
}

/// The resource row for one connection, when it is there.
fn resource_row<'a>(
    rows: &'a [Value],
    scope: Scope,
    group: Option<&str>,
    name: &str,
) -> Option<&'a Value> {
    rows.iter().find(|r| {
        resource_name(r) == name
            && r["spec"]["scope"].as_str() == Some(scope_word(scope))
            && group.is_none_or(|g| r["spec"]["ownerGroup"].as_str() == Some(g))
    })
}

/// `Ready` and its reason from the resource, for the row the caller asked about.
fn sync_in(rows: &[Value], scope: Scope, group: Option<&str>, name: &str) -> Option<SyncView> {
    connections::sync_from_resource(resource_row(rows, scope, group, name)?)
}

/// The groups a managed connection is usable by, as the resource carries them.
fn usable_by(resource: &Value) -> Vec<String> {
    resource["spec"]["usableBy"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What the store kept, rebuilt from a `Connection` resource whose secret is gone. A half-finished
/// delete leaves the resource behind; the row is what lets the person retry the delete.
fn meta_from_resource(resource: &Value) -> SecretMeta {
    let mut custom = BTreeMap::new();
    if let Some(t) = resource["spec"]["type"].as_str() {
        custom.insert("type".to_string(), t.to_string());
    }
    let keys = resource["spec"]["secret"]["keys"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    custom.insert("keys".to_string(), keys);
    if let Some(config) = resource["spec"]["config"].as_object() {
        for (k, v) in config {
            if let Some(v) = v.as_str() {
                custom.insert(format!("config.{k}"), v.to_string());
            }
        }
    }
    SecretMeta {
        version: 0,
        created_time: None,
        updated_time: None,
        custom_metadata: custom,
    }
}

/// The civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm, no dependency).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(month <= 2), month, day)
}

/// RFC 3339 in UTC, second precision: `2026-10-20T08:15:00Z`.
fn rfc3339_utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let day_secs = secs.rem_euclid(86_400);
    let (hour, minute, second) = (day_secs / 3_600, day_secs % 3_600 / 60, day_secs % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// `POST …/break-glass` body. No `Debug`: a reason can be sensitive and nothing here should ever
/// be printed by accident.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct BreakGlassRequest {
    pub reason: String,
}

/// The values, returned once. No `Debug`: the map holds the secret values and is never formatted,
/// logged or put in an error.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct BreakGlassResponse {
    pub fields: BTreeMap<String, String>,
    #[serde(rename = "readAt")]
    pub read_at: String,
}

impl IntoResponse for BreakGlassResponse {
    fn into_response(self) -> Response {
        (
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::PRAGMA, "no-cache"),
            ],
            Json(self),
        )
            .into_response()
    }
}

/// `GET /api/connections/break-glass-log`: the last 200 reads and refusals, newest first. It has
/// no values by construction (see [`audit::BreakGlassRecord`]).
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct BreakGlassLogResponse {
    pub events: Vec<BreakGlassLogRow>,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct BreakGlassLogRow {
    pub at: String,
    pub admin: String,
    #[serde(rename = "ownerGroup")]
    pub owner_group: String,
    pub connection: String,
    pub reason: String,
    pub outcome: String,
}

/// The result of one break-glass attempt: the answer and the audit record for it. The record is
/// kept aside so the test can prove what was written; the handler only looks at `answer`.
struct BreakGlassResult {
    answer: Result<BreakGlassResponse, Fail>,
    /// The tests inspect what was recorded; production has already written the line and the row.
    #[allow(dead_code)]
    record: BreakGlassRecord,
}

/// Validate a reason: trimmed, 20 to 500 characters, no control character except newline.
fn break_glass_reason(reason: &str) -> Option<&str> {
    let reason = reason.trim();
    let count = reason.chars().count();
    if (20..=500).contains(&count) && reason.chars().all(|c| c == '\n' || !c.is_control()) {
        Some(reason)
    } else {
        None
    }
}

/// The reason as the audit record keeps it: trimmed and bounded, but even a refused reason is
/// recorded (it must never become a value-sized column).
fn audit_reason(reason: &str) -> String {
    reason.trim().chars().take(500).collect()
}

/// Take one slot for `sub`, or report that the window is full. Refusals do not reach this; every
/// attempted read does, whether it succeeds, is refused by the store or fails at the notice.
fn take_break_glass_slot(limiter: &BreakGlassLimiter, sub: &str) -> bool {
    let mut map = limiter.lock().expect("break-glass limiter poisoned");
    let now = Instant::now();
    let slots = map.entry(sub.to_string()).or_default();
    while slots
        .front()
        .is_some_and(|then| now.duration_since(*then) >= BREAK_GLASS_WINDOW)
    {
        slots.pop_front();
    }
    if slots.len() >= BREAK_GLASS_LIMIT {
        false
    } else {
        slots.push_back(now);
        true
    }
}

/// Finish one attempt: set the outcome, write the audit line (and the state-store row when one is
/// configured) and hand the answer back. A failed insert is logged and never changes the answer.
async fn finish_break_glass(
    db: Option<&sqlx::PgPool>,
    mut record: BreakGlassRecord,
    outcome: &str,
    answer: Result<BreakGlassResponse, Fail>,
) -> BreakGlassResult {
    record.outcome = outcome.to_string();
    audit::break_glass_event(&record);
    if let Some(pool) = db
        && let Err(e) = sqlx::query(
            "INSERT INTO break_glass_read (admin, admin_sub, owner_group, connection, reason, outcome, request_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&record.admin)
        .bind(&record.admin_sub)
        .bind(&record.owner_group)
        .bind(&record.connection)
        .bind(&record.reason)
        .bind(&record.outcome)
        .bind(record.request_id.as_deref())
        .execute(pool)
        .await
    {
        tracing::warn!(error = %e, "could not record a break-glass read in the state store");
    }
    BreakGlassResult { answer, record }
}

// ---------------------------------------------------------------------------
// The route logic. These take their inputs, so the tests call them directly.
// ---------------------------------------------------------------------------

/// `POST /api/connections`: validate, authorize, write the value, create the resource, and answer
/// the row. A resource failure undoes the store entry so a retry starts from a clean slate.
async fn create_connection(
    store: &StoreClient,
    resources: Option<&Resources>,
    who: &Identity,
    access: &Secret,
    req: NewConnection,
) -> Result<(StatusCode, ConnectionView), Fail> {
    connections::validate_new(&req).map_err(|m| Fail::new(StatusCode::BAD_REQUEST, m))?;
    let scope = connections::authorize(who, req.scope, req.owner_group.as_deref(), Action::Create)
        .map_err(|d| Fail::new(StatusCode::FORBIDDEN, d.0))?;

    // A group or managed connection *is* its resource: without a cluster Nova cannot create one,
    // and says so rather than writing a value nobody syncs.
    let resource = connections::connection_resource(&req, &crate::cluster::namespace());
    if resource.is_some() && resources.is_none() {
        return Err(no_cluster());
    }

    let path = secret_store::secret_path(&scope, &req.name).map_err(store_fail)?;
    let token = store.login(access).await.map_err(store_fail)?;
    match store.metadata(&token, &path).await {
        Ok(_) => return Err(conflict()),
        Err(StoreError::NotFound) => {}
        Err(e) => return Err(store_fail(e)),
    }

    let (fields, meta) = connections::store_payload(&req);
    store
        .put(&token, &path, &fields, &meta)
        .await
        .map_err(store_fail)?;

    if let Some(resource) = resource {
        // Checked above; the pairing keeps the compiler from asking again.
        let Some(resources) = resources else {
            return Err(no_cluster());
        };
        if let Err(e) = resources.create(resource).await {
            // Destroy the half-written state, then answer the first error: a failed rollback is
            // logged and never hides what actually went wrong.
            if let Err(rollback) = store.destroy(&token, &path).await {
                tracing::warn!(
                    error = %rollback,
                    "rolling back a connection secret after the resource failed"
                );
            }
            return Err(cluster_fail(e));
        }
    }

    let meta = store.metadata(&token, &path).await.map_err(store_fail)?;
    audit::connection_event(
        "create",
        who,
        scope_word(req.scope),
        req.owner_group.as_deref(),
        &req.name,
        None,
    );
    Ok((
        StatusCode::CREATED,
        connections::view_from_store(
            &req.name,
            req.scope,
            req.owner_group.as_deref(),
            req.usable_by.clone(),
            &meta,
            // The operator has not synced the new resource yet; the next read shows its status.
            None,
            // `authorize` only lets a caller create in a scope they belong to.
            true,
        ),
    ))
}

/// A rotation: merge the patch into the current fields, write a new version, answer the row.
async fn rotate_connection(
    store: &StoreClient,
    resources: Option<&Resources>,
    who: &Identity,
    access: &Secret,
    scope: StoreScope,
    name: &str,
    patch: BTreeMap<String, String>,
) -> Result<ConnectionView, Fail> {
    let (view_scope, group) = scope_of(&scope);
    connections::authorize(who, view_scope, group, Action::Rotate)
        .map_err(|d| Fail::new(StatusCode::FORBIDDEN, d.0))?;
    if view_scope != Scope::Personal && resources.is_none() {
        return Err(no_cluster());
    }

    let path = secret_store::secret_path(&scope, name).map_err(store_fail)?;
    let token = store.login(access).await.map_err(store_fail)?;
    let meta = store.metadata(&token, &path).await.map_err(store_fail)?;
    let keys: BTreeSet<String> = meta
        .custom_metadata
        .get("keys")
        .map(|k| {
            k.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // A managed value cannot be read by anybody, so it is never read here either: the caller
    // sends every field (the rules enforce that).
    let existing = match view_scope {
        Scope::Managed => None,
        _ => Some(store.read(&token, &path).await.map_err(store_fail)?),
    };
    let merged = connections::merge_rotation(view_scope, existing.as_ref(), &keys, &patch)
        .map_err(|m| Fail::new(StatusCode::BAD_REQUEST, m))?;
    store
        .put(&token, &path, &merged, &BTreeMap::new())
        .await
        .map_err(store_fail)?;

    let meta = store.metadata(&token, &path).await.map_err(store_fail)?;
    // The credential has been replaced, which is the answer to a break-glass notice: clear it.
    // A cluster that refuses this must not fail a rotation that already happened.
    if view_scope == Scope::Group
        && let Some(resources) = resources
        && let Err(e) = resources.annotate(name, BREAK_GLASS_ANNOTATION, None).await
    {
        tracing::warn!(error = ?e, "could not clear the break-glass notice after a rotation");
    }
    let rows = resource_rows(resources, view_scope).await;
    audit::connection_event("rotate", who, scope_word(view_scope), group, name, None);
    Ok(connections::view_from_store(
        name,
        view_scope,
        group,
        resource_row(&rows, view_scope, group, name)
            .map(usable_by)
            .unwrap_or_default(),
        &meta,
        sync_in(&rows, view_scope, group, name),
        // A rotation is members-only for a group and administrators-only for a managed one.
        true,
    ))
}

/// Delete the value first, then the resource: if the second step fails the value is gone and the
/// resource stays visible as not-ready, which is retryable. A member and an administrator who is
/// not one both remove the resource; the audit line says when it was the administrator.
async fn delete_connection(
    store: &StoreClient,
    resources: Option<&Resources>,
    who: &Identity,
    access: &Secret,
    scope: StoreScope,
    name: &str,
) -> Result<(), Fail> {
    let (view_scope, group) = scope_of(&scope);
    connections::authorize(who, view_scope, group, Action::Delete)
        .map_err(|d| Fail::new(StatusCode::FORBIDDEN, d.0))?;
    if view_scope != Scope::Personal && resources.is_none() {
        return Err(no_cluster());
    }
    // Personal and managed connections are always the caller's own business; a group connection is
    // this person's only when they are a member.
    let member = group.is_none_or(|g| who.groups.iter().any(|own| own == g));

    let path = secret_store::secret_path(&scope, name).map_err(store_fail)?;
    let token = store.login(access).await.map_err(store_fail)?;
    store.destroy(&token, &path).await.map_err(store_fail)?;

    if view_scope != Scope::Personal
        && let Some(resources) = resources
    {
        resources.delete(name).await.map_err(cluster_fail)?;
    }

    audit::connection_event(
        "delete",
        who,
        scope_word(view_scope),
        group,
        name,
        if member { None } else { Some("administrator") },
    );
    Ok(())
}

/// `GET /api/connections`: personal, then group, then managed, each sorted by name. An
/// administrator sees every group's connections, not only their own groups': the incident case is
/// exactly the one where the owning group is unavailable (ticket #203, D27).
async fn list_connections(
    store: &StoreClient,
    resources: Option<&Resources>,
    who: &Identity,
    access: &Secret,
) -> Result<Vec<ConnectionView>, Fail> {
    let token = store.login(access).await.map_err(store_fail)?;
    let mut rows = Vec::new();

    let personal = StoreScope::Personal {
        sub: who.sub.clone(),
    };
    rows.extend(rows_from_store(store, &token, &personal, Scope::Personal, None, &[], true).await?);

    // The resources are an index and a status hint. For an administrator a failed read is fatal
    // (the managed section would silently disappear); for everybody else it only means no `sync`.
    let cluster_rows = match resources {
        Some(r) if who.is_admin => r.list().await.map_err(cluster_fail)?,
        Some(r) if !who.groups.is_empty() => r.list().await.unwrap_or_default(),
        _ => Vec::new(),
    };

    for group in &who.groups {
        let scope = StoreScope::Group {
            group: group.clone(),
        };
        // A group name that cannot be a store path is not a group Nova can list.
        if secret_store::scope_prefix(&scope).is_err() {
            continue;
        }
        rows.extend(
            rows_from_store(
                store,
                &token,
                &scope,
                Scope::Group,
                Some(group),
                &cluster_rows,
                true,
            )
            .await?,
        );
    }

    // An administrator also sees the connections of groups they are not a member of. The store
    // names them (its `connections-admin` policy grants `list` on every scope's metadata); a
    // member's token is refused and never asks.
    if who.is_admin {
        for group in store.list_group_names(&token).await.map_err(store_fail)? {
            if who.groups.iter().any(|own| own == &group) {
                continue;
            }
            let scope = StoreScope::Group {
                group: group.clone(),
            };
            if secret_store::scope_prefix(&scope).is_err() {
                continue;
            }
            rows.extend(
                rows_from_store(
                    store,
                    &token,
                    &scope,
                    Scope::Group,
                    Some(&group),
                    &cluster_rows,
                    // Seen as an administrator, not as a member: read-only except delete.
                    false,
                )
                .await?,
            );
        }
    }

    // Managed: administrators only; a managed value cannot be read, so the resources are the index.
    if who.is_admin {
        for resource in cluster_rows
            .iter()
            .filter(|r| r["spec"]["scope"] == "managed")
        {
            let name = resource_name(resource);
            let Ok(path) = secret_store::secret_path(&StoreScope::Managed, name) else {
                continue;
            };
            let meta = match store.metadata(&token, &path).await {
                Ok(meta) => meta,
                // The value is gone but the resource is not (a half-finished delete): show the
                // row, not-ready, so the delete can be retried.
                Err(StoreError::NotFound) => meta_from_resource(resource),
                Err(e) => return Err(store_fail(e)),
            };
            rows.push(connections::view_from_store(
                name,
                Scope::Managed,
                None,
                usable_by(resource),
                &meta,
                connections::sync_from_resource(resource),
                true,
            ));
        }
    }

    rows.sort_by(|a, b| (scope_rank(a.scope), &a.name).cmp(&(scope_rank(b.scope), &b.name)));
    Ok(rows)
}

/// The emergency read of a group connection: administrator, a written reason, a rate limit, the
/// store's `breakglass` role signed in with the administrator's **own** token, the notice on the
/// `Connection`, then the values once. Every exit is audited and the record cannot hold a value.
#[allow(clippy::too_many_arguments)]
async fn break_glass_group_at(
    store: &StoreClient,
    resources: Option<&Resources>,
    limiter: &BreakGlassLimiter,
    db: Option<&sqlx::PgPool>,
    who: &Identity,
    access: &Secret,
    group: &str,
    name: &str,
    reason: &str,
    request_id: Option<&str>,
) -> BreakGlassResult {
    let mut record = BreakGlassRecord {
        admin: who.display_name.clone(),
        admin_sub: who.sub.clone(),
        owner_group: group.to_string(),
        connection: name.to_string(),
        scope: "group".to_string(),
        reason: audit_reason(reason),
        outcome: String::new(),
        request_id: request_id.map(str::to_string),
    };

    if !who.is_admin {
        return finish_break_glass(
            db,
            record,
            "refused:not-admin",
            Err(Fail::new(
                StatusCode::FORBIDDEN,
                "only administrators may do this",
            )),
        )
        .await;
    }

    let Some(reason) = break_glass_reason(reason) else {
        return finish_break_glass(
            db,
            record,
            "refused:reason",
            Err(Fail::new(
                StatusCode::BAD_REQUEST,
                "give a reason of at least 20 characters",
            )),
        )
        .await;
    };
    record.reason = reason.to_string();

    if !take_break_glass_slot(limiter, &who.sub) {
        return finish_break_glass(
            db,
            record,
            "refused:rate",
            Err(Fail::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many break-glass reads; wait",
            )),
        )
        .await;
    }

    let scope = StoreScope::Group {
        group: group.to_string(),
    };
    let path = match secret_store::secret_path(&scope, name) {
        Ok(path) => path,
        Err(e) => {
            return finish_break_glass(db, record, "refused:request", Err(store_fail(e))).await;
        }
    };

    let token = match store.login_as(store.breakglass_role(), access).await {
        Ok(token) => token,
        Err(StoreError::Refused(_)) => {
            return finish_break_glass(
                db,
                record,
                "refused:store",
                Err(Fail::new(
                    StatusCode::FORBIDDEN,
                    "the store refused the break-glass sign-in",
                )),
            )
            .await;
        }
        Err(e) => {
            return finish_break_glass(db, record, "refused:store", Err(store_fail(e))).await;
        }
    };

    let fields = match store.read(&token, &path).await {
        Ok(fields) => fields,
        Err(StoreError::NotFound) => {
            let _ = store.revoke(&token).await;
            drop(token);
            return finish_break_glass(db, record, "not-found", Err(not_found())).await;
        }
        Err(e) => {
            let _ = store.revoke(&token).await;
            drop(token);
            return finish_break_glass(db, record, "unavailable", Err(store_fail(e))).await;
        }
    };
    let _ = store.revoke(&token).await;
    drop(token);

    let at = rfc3339_utc(SystemTime::now());
    let notice = BreakGlassNotice {
        by: who.display_name.clone(),
        at: at.clone(),
        reason: reason.to_string(),
    };
    let value = serde_json::to_string(&notice).expect("the break-glass notice serializes");
    let Some(resources) = resources else {
        return finish_break_glass(
            db,
            record,
            "notice-failed",
            Err(Fail::new(
                StatusCode::BAD_GATEWAY,
                "could not record the notice; nothing was returned",
            )),
        )
        .await;
    };
    if let Err(e) = resources
        .annotate(name, BREAK_GLASS_ANNOTATION, Some(&value))
        .await
    {
        tracing::warn!(error = ?e, "could not write the break-glass notice");
        return finish_break_glass(
            db,
            record,
            "notice-failed",
            Err(Fail::new(
                StatusCode::BAD_GATEWAY,
                "could not record the notice; nothing was returned",
            )),
        )
        .await;
    }

    finish_break_glass(
        db,
        record,
        "read",
        Ok(BreakGlassResponse {
            fields,
            read_at: at,
        }),
    )
    .await
}

/// A personal or managed connection is never readable, even by an administrator: the route exists
/// only to refuse (after the administrator check) and to leave an audit row.
async fn break_glass_refused_scope(
    db: Option<&sqlx::PgPool>,
    who: &Identity,
    scope: &str,
    name: &str,
    reason: &str,
    request_id: Option<&str>,
) -> BreakGlassResult {
    let record = BreakGlassRecord {
        admin: who.display_name.clone(),
        admin_sub: who.sub.clone(),
        owner_group: String::new(),
        connection: name.to_string(),
        scope: scope.to_string(),
        reason: audit_reason(reason),
        outcome: String::new(),
        request_id: request_id.map(str::to_string),
    };
    if !who.is_admin {
        return finish_break_glass(
            db,
            record,
            "refused:not-admin",
            Err(Fail::new(
                StatusCode::FORBIDDEN,
                "only administrators may do this",
            )),
        )
        .await;
    }
    finish_break_glass(
        db,
        record,
        "refused:scope",
        Err(Fail::new(
            StatusCode::FORBIDDEN,
            "break-glass covers group connections only",
        )),
    )
    .await
}

/// The rows of one store scope. Values are read for nothing here: the listing is metadata only.
/// `member` is the caller's membership of the scope, carried onto every row.
#[allow(clippy::too_many_arguments)]
async fn rows_from_store(
    store: &StoreClient,
    token: &StoreToken,
    scope: &StoreScope,
    view: Scope,
    group: Option<&str>,
    cluster_rows: &[Value],
    member: bool,
) -> Result<Vec<ConnectionView>, Fail> {
    let prefix = secret_store::scope_prefix(scope).map_err(store_fail)?;
    let names = match store.list(token, &prefix).await {
        Ok(names) => names,
        // A group the store has no listing for, or refuses — the store's policy, not an error.
        Err(StoreError::Refused(_)) if view == Scope::Group => Vec::new(),
        Err(e) => return Err(store_fail(e)),
    };

    // At most eight metadata reads in flight: a person with many connections must not turn a
    // listing into a burst the store has to absorb.
    let fetched: Vec<(String, Result<SecretMeta, StoreError>)> = stream::iter(names)
        .map(|name| async move {
            let meta = match secret_store::secret_path(scope, &name) {
                Ok(path) => store.metadata(token, &path).await,
                Err(e) => Err(e),
            };
            (name, meta)
        })
        .buffer_unordered(8)
        .collect()
        .await;

    let mut rows = Vec::new();
    for (name, meta) in fetched {
        match meta {
            Ok(meta) => {
                let mut row = connections::view_from_store(
                    &name,
                    view,
                    group,
                    Vec::new(),
                    &meta,
                    sync_in(cluster_rows, view, group, &name),
                    member,
                );
                if view == Scope::Group {
                    row.break_glass = resource_row(cluster_rows, Scope::Group, group, &name)
                        .and_then(connections::break_glass_notice);
                }
                rows.push(row);
            }
            // Gone between the listing and the read: nothing to show.
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(store_fail(e)),
        }
    }
    Ok(rows)
}

/// The resource rows for a write that already succeeded: a failed read only means no `sync`/`usableBy`,
/// never an error for a write the caller did complete.
async fn resource_rows(resources: Option<&Resources>, scope: Scope) -> Vec<Value> {
    match (resources, scope) {
        (Some(_), Scope::Personal) => Vec::new(),
        (Some(r), _) => r.list().await.unwrap_or_default(),
        (None, _) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The handlers: sign-in, the store, the caller's token, then one of the functions above.
// ---------------------------------------------------------------------------

/// The signed-in caller and the store, checked **before** the body is parsed: a route that cannot
/// serve this caller says so (`503` without a store, `401` without a token) rather than complaining
/// about a body it was never going to use. Demo mode and `static-dev` have no principal at all.
pub(crate) struct SignedIn {
    state: Arc<AppState>,
    identity: Identity,
    /// The caller's own token, the one the store is signed in with — never a service identity.
    access: Secret,
    /// The ingress/proxy request id, recorded with a break-glass read when present.
    request_id: Option<String>,
}

impl SignedIn {
    /// The store. Present: the extractor refused the request otherwise, and state never changes.
    fn store(&self) -> &StoreClient {
        self.state
            .store
            .as_ref()
            .expect("the extractor checked the store")
    }

    /// The application state, for a route (like the pipelines one) that needs another client.
    pub(crate) fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// Who is signed in; the pipelines routes check the owner group against these groups.
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }
}

impl FromRequestParts<Arc<AppState>> for SignedIn {
    type Rejection = Fail;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Some(principal) = parts.extensions.get::<Principal>().cloned() else {
            return Err(not_configured());
        };
        if state.store.is_none() {
            return Err(not_configured());
        }
        let Some(access) = principal.access_token else {
            return Err(sign_in_again());
        };
        let request_id = parts
            .headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.chars().take(200).collect());
        Ok(Self {
            state: Arc::clone(state),
            identity: principal.identity,
            access,
            request_id,
        })
    }
}

/// The resource client for this request; `None` without a cluster connection.
fn resources(state: &AppState) -> Option<Resources> {
    state.kube.clone().map(Resources::cluster)
}

#[utoipa::path(
    get,
    path = "/api/connections",
    tag = "public",
    description = "The caller's connections: their own, their groups', and — for administrators — the managed ones. Values are never part of an answer, only the field names.",
    responses(
        (status = 200, description = "Personal, then group, then managed connections, each sorted by name", body = ConnectionsResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(signed: SignedIn) -> Response {
    let resources = resources(&signed.state);
    match list_connections(
        signed.store(),
        resources.as_ref(),
        &signed.identity,
        &signed.access,
    )
    .await
    {
        Ok(connections) => Json(ConnectionsResponse { connections }).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/api/connections/group/{group}/{name}/break-glass",
    tag = "public",
    description = "Emergency read of a group connection by an administrator: a written reason, a notice on the Connection for the owner group, an audit row, and the values returned **once**. The answer is never stored and carries `Cache-Control: no-store` and `Pragma: no-cache`. Personal and managed connections are never readable.",
    params(
        ("group" = String, Path, description = "Owner group"),
        ("name" = String, Path, description = "Connection name"),
    ),
    request_body = BreakGlassRequest,
    responses(
        (status = 200, description = "The connection's fields, returned once and never stored; the owner group's notice is already written", body = BreakGlassResponse),
        (status = 400, description = "The reason is shorter than 20 characters; never a value", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not an administrator, or the store refused the break-glass sign-in", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 429, description = "More than five break-glass reads by this administrator in ten minutes", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store did not answer, or the notice on the Connection could not be written (nothing is returned then)", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn break_glass_group(
    signed: SignedIn,
    Path((group, name)): Path<(String, String)>,
    ConnectionJson(req): ConnectionJson<BreakGlassRequest>,
) -> Response {
    let resources = resources(&signed.state);
    let result = break_glass_group_at(
        signed.store(),
        resources.as_ref(),
        &signed.state.break_glass_reads,
        signed.state.db.as_ref(),
        &signed.identity,
        &signed.access,
        &group,
        &name,
        &req.reason,
        signed.request_id.as_deref(),
    )
    .await;
    match result.answer {
        Ok(response) => response.into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/api/connections/personal/{name}/break-glass",
    tag = "public",
    description = "Always refused: break-glass covers group connections only. An administrator gets a 403 and an audit row; nothing is read.",
    params(
        ("name" = String, Path, description = "Connection name"),
    ),
    request_body = BreakGlassRequest,
    responses(
        (status = 403, description = "Break-glass covers group connections only, or the caller is not an administrator", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn break_glass_personal(
    signed: SignedIn,
    Path(name): Path<String>,
    ConnectionJson(req): ConnectionJson<BreakGlassRequest>,
) -> Response {
    let result = break_glass_refused_scope(
        signed.state.db.as_ref(),
        &signed.identity,
        "personal",
        &name,
        &req.reason,
        signed.request_id.as_deref(),
    )
    .await;
    match result.answer {
        Ok(response) => response.into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/api/connections/managed/{name}/break-glass",
    tag = "public",
    description = "Always refused: break-glass covers group connections only. An administrator gets a 403 and an audit row; nothing is read.",
    params(
        ("name" = String, Path, description = "Connection name"),
    ),
    request_body = BreakGlassRequest,
    responses(
        (status = 403, description = "Break-glass covers group connections only, or the caller is not an administrator", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn break_glass_managed(
    signed: SignedIn,
    Path(name): Path<String>,
    ConnectionJson(req): ConnectionJson<BreakGlassRequest>,
) -> Response {
    let result = break_glass_refused_scope(
        signed.state.db.as_ref(),
        &signed.identity,
        "managed",
        &name,
        &req.reason,
        signed.request_id.as_deref(),
    )
    .await;
    match result.answer {
        Ok(response) => response.into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/api/connections/break-glass-log",
    tag = "public",
    description = "The last 200 break-glass reads and refusals, newest first, for administrators. The rows carry the administrator, group, connection, reason and outcome — never a value. `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "The most recent events, newest first", body = BreakGlassLogResponse),
        (status = 403, description = "Not an administrator", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn break_glass_log(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<Principal>>,
) -> Response {
    // In dev mode there is no principal and one operator, who is the administrator.
    let is_admin = principal
        .as_ref()
        .is_none_or(|axum::Extension(p)| p.identity.is_admin);
    if !is_admin {
        return Fail::new(StatusCode::FORBIDDEN, "only administrators may do this").into_response();
    }
    let Some(db) = state.db.as_ref() else {
        return Fail::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the state store is not configured",
        )
        .into_response();
    };
    let rows = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "SELECT to_char(at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), admin, \
            owner_group, connection, reason, outcome \
         FROM break_glass_read ORDER BY at DESC LIMIT 200",
    )
    .fetch_all(db)
    .await;
    match rows {
        Ok(rows) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(BreakGlassLogResponse {
                events: rows
                    .into_iter()
                    .map(
                        |(at, admin, owner_group, connection, reason, outcome)| BreakGlassLogRow {
                            at,
                            admin,
                            owner_group,
                            connection,
                            reason,
                            outcome,
                        },
                    )
                    .collect(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "could not read the break-glass log");
            Fail::new(StatusCode::BAD_GATEWAY, "the state store did not answer").into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/connections",
    tag = "public",
    description = "Create a connection. The values in `secret` are written to the platform's secret store with the caller's own token and are never returned by any route.",
    request_body = NewConnection,
    responses(
        (status = 201, description = "The created connection, without values", body = ConnectionView),
        (status = 400, description = "A sentence from the connection rules; never a value", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not allowed for this scope", body = crate::openapi::ErrorResponse),
        (status = 409, description = "A connection with this name already exists", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured, or no cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn create(
    signed: SignedIn,
    ConnectionJson(req): ConnectionJson<NewConnection>,
) -> Response {
    let resources = resources(&signed.state);
    match create_connection(
        signed.store(),
        resources.as_ref(),
        &signed.identity,
        &signed.access,
        req,
    )
    .await
    {
        Ok((status, view)) => (status, Json(view)).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/api/connections/personal/{name}/secret",
    tag = "public",
    description = "Rotate some fields of one of the caller's own connections; the fields that are not sent keep their value.",
    params(
        ("name" = String, Path, description = "Connection name"),
    ),
    request_body = RotateRequest,
    responses(
        (status = 200, description = "The connection after the rotation, without values", body = ConnectionView),
        (status = 400, description = "A sentence from the connection rules; never a value", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn rotate_personal(
    signed: SignedIn,
    Path(name): Path<String>,
    ConnectionJson(body): ConnectionJson<RotateRequest>,
) -> Response {
    rotate_at(
        &signed,
        |who| StoreScope::Personal {
            sub: who.sub.clone(),
        },
        &name,
        body,
    )
    .await
}

#[utoipa::path(
    put,
    path = "/api/connections/group/{group}/{name}/secret",
    tag = "public",
    description = "Rotate some fields of one of a group's connections. Asked to write the fields that are sent; the others keep their value.",
    params(
        ("group" = String, Path, description = "Owner group"),
        ("name" = String, Path, description = "Connection name"),
    ),
    request_body = RotateRequest,
    responses(
        (status = 200, description = "The connection after the rotation, without values", body = ConnectionView),
        (status = 400, description = "A sentence from the connection rules; never a value", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not a member of the group", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured, or no cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn rotate_group(
    signed: SignedIn,
    Path((group, name)): Path<(String, String)>,
    ConnectionJson(body): ConnectionJson<RotateRequest>,
) -> Response {
    rotate_at(&signed, move |_| StoreScope::Group { group }, &name, body).await
}

#[utoipa::path(
    put,
    path = "/api/connections/managed/{name}/secret",
    tag = "public",
    description = "Rotate an administrator-managed connection. Nobody can read it back, so every field must be sent.",
    params(
        ("name" = String, Path, description = "Connection name"),
    ),
    request_body = RotateRequest,
    responses(
        (status = 200, description = "The connection after the rotation, without values", body = ConnectionView),
        (status = 400, description = "A sentence from the connection rules; never a value", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Managed connections are administrators' business", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured, or no cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn rotate_managed(
    signed: SignedIn,
    Path(name): Path<String>,
    ConnectionJson(body): ConnectionJson<RotateRequest>,
) -> Response {
    rotate_at(&signed, |_| StoreScope::Managed, &name, body).await
}

async fn rotate_at(
    signed: &SignedIn,
    scope: impl FnOnce(&Identity) -> StoreScope,
    name: &str,
    body: RotateRequest,
) -> Response {
    let scope = scope(&signed.identity);
    let resources = resources(&signed.state);
    match rotate_connection(
        signed.store(),
        resources.as_ref(),
        &signed.identity,
        &signed.access,
        scope,
        name,
        body.secret,
    )
    .await
    {
        Ok(view) => Json(view).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/api/connections/personal/{name}",
    tag = "public",
    description = "Delete one of the caller's own connections.",
    params(
        ("name" = String, Path, description = "Connection name"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn delete_personal(signed: SignedIn, Path(name): Path<String>) -> Response {
    delete_at(
        &signed,
        |who| StoreScope::Personal {
            sub: who.sub.clone(),
        },
        &name,
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/api/connections/group/{group}/{name}",
    tag = "public",
    description = "Delete one of a group's connections. The value goes first, then the resource; if the resource delete fails the entry stays visible as not-ready and the delete can be retried.",
    params(
        ("group" = String, Path, description = "Owner group"),
        ("name" = String, Path, description = "Connection name"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not a member of the group", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured, or no cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn delete_group(
    signed: SignedIn,
    Path((group, name)): Path<(String, String)>,
) -> Response {
    delete_at(&signed, move |_| StoreScope::Group { group }, &name).await
}

#[utoipa::path(
    delete,
    path = "/api/connections/managed/{name}",
    tag = "public",
    description = "Delete an administrator-managed connection. The value goes first, then the resource; if the resource delete fails the entry stays visible as not-ready and the delete can be retried.",
    params(
        ("name" = String, Path, description = "Connection name"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No token to sign in to the secret store with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Managed connections are administrators' business", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such connection", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The secret store or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The secret store is not configured, or no cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn delete_managed(signed: SignedIn, Path(name): Path<String>) -> Response {
    delete_at(&signed, |_| StoreScope::Managed, &name).await
}

async fn delete_at(
    signed: &SignedIn,
    scope: impl FnOnce(&Identity) -> StoreScope,
    name: &str,
) -> Response {
    let scope = scope(&signed.identity);
    let resources = resources(&signed.state);
    match delete_connection(
        signed.store(),
        resources.as_ref(),
        &signed.identity,
        &signed.access,
        scope,
        name,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::{get, post},
    };
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::secret_store::fake;

    /// A value that must never appear in an answer.
    const CANARY: &str = "canary-7f3a-DO-NOT-LEAK";

    /// A reason long enough for every test that only needs a valid one.
    const REASON: &str = "incident-4711: paged at 03:12, rotate after";

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

    /// A `POST /api/connections` body, with `patch` applied (`null` removes a key).
    fn body(patch: Value) -> NewConnection {
        let mut v = json!({
            "name": "crm-db", "type": "sqlserver", "scope": "group", "ownerGroup": "analytics",
            "config": { "host": "crm.example.com" },
            "secret": { "user": "etl", "password": "p1" }
        });
        for (k, val) in patch.as_object().unwrap() {
            if val.is_null() {
                v.as_object_mut().unwrap().remove(k);
            } else {
                v[k] = val.clone();
            }
        }
        serde_json::from_value(v).unwrap()
    }

    fn personal() -> NewConnection {
        body(json!({ "scope": "personal", "ownerGroup": null }))
    }

    /// An in-memory cluster with its items, so a test can look at what was written.
    fn memory() -> (Resources, Arc<Mutex<BTreeMap<String, Value>>>) {
        let items = Arc::new(Mutex::new(BTreeMap::new()));
        (
            Resources::Memory {
                items: Arc::clone(&items),
                fail_delete: false,
                fail_annotate: false,
            },
            items,
        )
    }

    fn stored(fake: &fake::Shared, path: &str) -> Option<BTreeMap<String, String>> {
        fake.lock().unwrap().data.get(path).cloned()
    }

    fn admin() -> Identity {
        who("sub-admin", &["stardelt-admins"], true)
    }

    fn limiter() -> BreakGlassLimiter {
        Mutex::new(HashMap::new())
    }

    /// The refusal and the audit record of one attempt. (`BreakGlassResponse` has no `Debug` on
    /// purpose, so the tests cannot use `Result::expect_err`.)
    fn expect_fail(result: BreakGlassResult) -> (Fail, BreakGlassRecord) {
        match result.answer {
            Ok(_) => panic!("expected a refusal"),
            Err(e) => (e, result.record),
        }
    }

    fn seed_group(fake: &fake::Shared) {
        fake.lock().unwrap().seed(
            "connections/group/analytics/crm-db",
            &[("user", "etl"), ("password", CANARY)],
            &[("type", "sqlserver"), ("keys", "password,user")],
        );
    }

    /// A connection of a group the administrator is **not** a member of.
    fn seed_finance(fake: &fake::Shared) {
        fake.lock().unwrap().seed(
            "connections/group/finance/ledger",
            &[("user", "etl"), ("password", CANARY)],
            &[("type", "postgresql"), ("keys", "password,user")],
        );
    }

    fn group_resource() -> Value {
        json!({
            "metadata": { "name": "crm-db" },
            "spec": { "scope": "group", "ownerGroup": "analytics" },
        })
    }

    fn memory_with(items: &[(&str, Value)]) -> (Resources, Arc<Mutex<BTreeMap<String, Value>>>) {
        let (resources, stored) = memory();
        for (name, value) in items {
            stored
                .lock()
                .unwrap()
                .insert((*name).to_string(), value.clone());
        }
        (resources, stored)
    }

    #[test]
    fn rfc3339_is_utc() {
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339_utc(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            "2023-11-14T22:13:20Z"
        );
    }

    #[tokio::test]
    async fn create_personal_writes_the_store_and_no_resource() {
        let (store, fake) = fake::start().await;
        let (resources, items) = memory();
        let alice = who("sub-alice", &["analytics"], false);
        let req = body(json!({
            "scope": "personal", "ownerGroup": null,
            "secret": { "user": "etl", "password": CANARY },
        }));

        let (status, view) =
            create_connection(&store, Some(&resources), &alice, &Secret::new("good"), req)
                .await
                .expect("created");

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(view.scope, Scope::Personal);
        assert_eq!(view.secret_keys, ["password", "user"]);
        let path = "connections/user/sub-alice/crm-db";
        let fields = stored(&fake, path).expect("the value is in the store");
        assert_eq!(fields["password"], CANARY);
        assert_eq!(fields["user"], "etl");
        let custom = &fake.lock().unwrap().custom[path];
        assert_eq!(custom["type"], "sqlserver");
        assert_eq!(custom["keys"], "password,user");
        assert_eq!(custom["config.host"], "crm.example.com");
        assert!(
            items.lock().unwrap().is_empty(),
            "a personal connection has no resource"
        );
        let answer = serde_json::to_string(&view).unwrap();
        assert!(answer.contains("secretKeys"), "{answer}");
        assert!(!answer.contains(CANARY), "no value in the answer: {answer}");
    }

    #[tokio::test]
    async fn create_group_writes_store_then_resource() {
        let (store, fake) = fake::start().await;
        let (resources, items) = memory();
        let alice = who("sub-alice", &["analytics"], false);
        let req = body(json!({ "secret": { "user": "etl", "password": CANARY } }));

        let (status, view) =
            create_connection(&store, Some(&resources), &alice, &Secret::new("good"), req)
                .await
                .expect("created");

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(view.scope, Scope::Group);
        assert_eq!(view.owner_group.as_deref(), Some("analytics"));
        assert!(
            stored(&fake, "connections/group/analytics/crm-db").is_some(),
            "the value is in the store"
        );
        let items = items.lock().unwrap();
        let resource = items.get("crm-db").expect("the resource exists");
        assert!(resource["spec"]["secret"].get("remoteRef").is_none());
        let text = serde_json::to_string(resource).unwrap();
        assert!(
            !text.contains(CANARY) && !text.contains("etl"),
            "no value in the resource: {text}"
        );
        assert_eq!(
            resource["spec"]["secret"]["keys"],
            json!(["password", "user"])
        );
    }

    #[tokio::test]
    async fn create_group_rolls_the_store_back_when_the_resource_exists() {
        let (store, fake) = fake::start().await;
        let (resources, items) = memory();
        items
            .lock()
            .unwrap()
            .insert("crm-db".into(), json!({ "metadata": { "name": "crm-db" } }));
        let alice = who("sub-alice", &["analytics"], false);

        let err = create_connection(
            &store,
            Some(&resources),
            &alice,
            &Secret::new("good"),
            body(json!({})),
        )
        .await
        .expect_err("the resource exists");

        assert_eq!(err.status, StatusCode::CONFLICT);
        let fake = fake.lock().unwrap();
        assert!(
            !fake.data.contains_key("connections/group/analytics/crm-db"),
            "the store entry is gone again"
        );
        assert!(
            !fake
                .custom
                .contains_key("connections/group/analytics/crm-db")
        );
    }

    #[tokio::test]
    async fn create_twice_is_a_conflict_and_does_not_overwrite() {
        let (store, _fake) = fake::start().await;
        let alice = who("sub-alice", &[], false);
        let first = body(json!({
            "scope": "personal", "ownerGroup": null,
            "secret": { "user": "etl", "password": "first" },
        }));
        create_connection(&store, None, &alice, &Secret::new("good"), first)
            .await
            .expect("created");

        let second = body(json!({
            "scope": "personal", "ownerGroup": null,
            "secret": { "user": "etl", "password": "second" },
        }));
        let err = create_connection(&store, None, &alice, &Secret::new("good"), second)
            .await
            .expect_err("the name is taken");
        assert_eq!(err.status, StatusCode::CONFLICT);

        let token = store.login(&Secret::new("good")).await.unwrap();
        let current = store
            .read(&token, "connections/user/sub-alice/crm-db")
            .await
            .unwrap();
        assert_eq!(
            current["password"], "first",
            "the second create must not overwrite"
        );
    }

    #[tokio::test]
    async fn a_member_cannot_create_in_another_group() {
        let (store, fake) = fake::start().await;
        let alice = who("sub-alice", &["analytics"], false);
        let req = body(json!({ "ownerGroup": "finance" }));

        let err = create_connection(&store, None, &alice, &Secret::new("good"), req)
            .await
            .expect_err("not a member");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(
            fake.lock().unwrap().data.is_empty(),
            "the store is untouched"
        );
    }

    #[tokio::test]
    async fn an_administrator_cannot_create_in_it() {
        let (store, fake) = fake::start().await;
        let admin = who("sub-admin", &["stardelt-admins"], true);
        let req = body(json!({ "ownerGroup": "finance" }));

        let err = create_connection(&store, None, &admin, &Secret::new("admin"), req)
            .await
            .expect_err("an administrator is not a member");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        let fake = fake.lock().unwrap();
        assert!(fake.data.is_empty(), "the store is untouched");
        assert_eq!(fake.requests, 0, "not even a sign-in");
    }

    #[tokio::test]
    async fn only_admins_create_managed() {
        let (store, fake) = fake::start().await;
        let managed = || {
            body(json!({
                "scope": "managed", "ownerGroup": null, "usableBy": ["analytics"],
                "secret": { "user": "u", "password": "p" },
            }))
        };

        let alice = who("sub-alice", &["analytics"], false);
        let err = create_connection(&store, None, &alice, &Secret::new("good"), managed())
            .await
            .expect_err("only administrators");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(fake.lock().unwrap().data.is_empty());

        let (resources, items) = memory();
        let admin = who("sub-admin", &["stardelt-admins"], true);
        let (status, view) = create_connection(
            &store,
            Some(&resources),
            &admin,
            &Secret::new("good"),
            managed(),
        )
        .await
        .expect("an administrator creates one");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(view.scope, Scope::Managed);
        assert_eq!(view.usable_by, ["analytics"]);
        assert!(items.lock().unwrap().contains_key("crm-db"));
    }

    #[tokio::test]
    async fn rotate_keeps_the_other_fields_of_a_personal_connection() {
        let (store, fake) = fake::start().await;
        fake.lock().unwrap().seed(
            "connections/user/sub-alice/crm-db",
            &[("user", "etl"), ("password", "p1")],
            &[("type", "sqlserver"), ("keys", "user,password")],
        );
        let alice = who("sub-alice", &[], false);
        let patch: BTreeMap<String, String> = [("password".to_string(), "p2".to_string())].into();

        let view = rotate_connection(
            &store,
            None,
            &alice,
            &Secret::new("good"),
            StoreScope::Personal {
                sub: "sub-alice".into(),
            },
            "crm-db",
            patch,
        )
        .await
        .expect("rotated");

        assert_eq!(view.version, 2);
        let fake = fake.lock().unwrap();
        let path = "connections/user/sub-alice/crm-db";
        assert_eq!(fake.data[path]["user"], "etl", "the other field is kept");
        assert_eq!(fake.data[path]["password"], "p2");
        assert_eq!(fake.custom[path]["type"], "sqlserver");
    }

    #[tokio::test]
    async fn rotate_managed_needs_every_field() {
        let (store, fake) = fake::start().await;
        fake.lock().unwrap().seed(
            "connections/managed/crm-db",
            &[("user", "u"), ("password", "p")],
            &[("type", "sqlserver"), ("keys", "user,password")],
        );
        let (resources, _items) = memory();
        let admin = who("sub-admin", &["stardelt-admins"], true);
        let patch: BTreeMap<String, String> = [("password".to_string(), "p2".to_string())].into();

        let err = rotate_connection(
            &store,
            Some(&resources),
            &admin,
            &Secret::new("good"),
            StoreScope::Managed,
            "crm-db",
            patch,
        )
        .await
        .expect_err("one field of two");

        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("every field"), "{}", err.message);
        assert_eq!(
            fake.lock().unwrap().data["connections/managed/crm-db"]["password"],
            "p",
            "nothing was written"
        );
    }

    #[tokio::test]
    async fn delete_removes_store_then_resource() {
        let (store, fake) = fake::start().await;
        let (resources, items) = memory();
        fake.lock().unwrap().seed(
            "connections/group/analytics/crm-db",
            &[("user", "u"), ("password", "p")],
            &[("type", "sqlserver"), ("keys", "user,password")],
        );
        items
            .lock()
            .unwrap()
            .insert("crm-db".into(), json!({ "metadata": { "name": "crm-db" } }));
        let alice = who("sub-alice", &["analytics"], false);

        delete_connection(
            &store,
            Some(&resources),
            &alice,
            &Secret::new("good"),
            StoreScope::Group {
                group: "analytics".into(),
            },
            "crm-db",
        )
        .await
        .expect("deleted");

        assert!(
            !fake
                .lock()
                .unwrap()
                .data
                .contains_key("connections/group/analytics/crm-db")
        );
        assert!(items.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_when_the_resource_delete_fails_keeps_the_resource() {
        let (store, fake) = fake::start().await;
        let items = Arc::new(Mutex::new(BTreeMap::new()));
        items
            .lock()
            .unwrap()
            .insert("crm-db".into(), json!({ "metadata": { "name": "crm-db" } }));
        let resources = Resources::Memory {
            items: Arc::clone(&items),
            fail_delete: true,
            fail_annotate: false,
        };
        fake.lock().unwrap().seed(
            "connections/group/analytics/crm-db",
            &[("user", "u"), ("password", "p")],
            &[("type", "sqlserver"), ("keys", "user,password")],
        );
        let alice = who("sub-alice", &["analytics"], false);

        let err = delete_connection(
            &store,
            Some(&resources),
            &alice,
            &Secret::new("good"),
            StoreScope::Group {
                group: "analytics".into(),
            },
            "crm-db",
        )
        .await
        .expect_err("the cluster refused");

        assert_eq!(err.status, StatusCode::BAD_GATEWAY);
        assert!(
            !fake
                .lock()
                .unwrap()
                .data
                .contains_key("connections/group/analytics/crm-db"),
            "the value went first"
        );
        assert!(
            items.lock().unwrap().contains_key("crm-db"),
            "the resource stays, so the delete can be retried"
        );
    }

    #[tokio::test]
    async fn list_never_contains_a_value() {
        let (store, fake) = fake::start().await;
        fake.lock().unwrap().seed(
            "connections/user/sub-alice/crm-db",
            &[("user", "etl"), ("password", CANARY)],
            &[
                ("type", "sqlserver"),
                ("keys", "password,user"),
                ("config.host", "crm.example.com"),
            ],
        );
        let alice = who("sub-alice", &[], false);

        let rows = list_connections(&store, None, &alice, &Secret::new("good"))
            .await
            .expect("listed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "crm-db");
        assert_eq!(rows[0].secret_keys, ["password", "user"]);
        let answer = serde_json::to_string(&rows).unwrap();
        assert!(answer.contains("secretKeys"), "{answer}");
        assert!(
            !answer.contains(CANARY) && !answer.contains("etl"),
            "no value in the listing: {answer}"
        );
    }

    #[tokio::test]
    async fn list_hides_managed_from_non_admins() {
        let (store, fake) = fake::start().await;
        fake.lock().unwrap().seed(
            "connections/managed/shared-db",
            &[("user", "u"), ("password", CANARY)],
            &[("type", "sqlserver"), ("keys", "password,user")],
        );
        let (resources, items) = memory();
        items.lock().unwrap().insert(
            "shared-db".into(),
            json!({ "metadata": { "name": "shared-db" }, "spec": { "scope": "managed" } }),
        );
        let alice = who("sub-alice", &[], false);

        let rows = list_connections(&store, Some(&resources), &alice, &Secret::new("good"))
            .await
            .expect("listed");

        assert!(
            rows.is_empty(),
            "a non-administrator sees no managed connection"
        );
    }

    #[tokio::test]
    async fn list_skips_a_group_without_entries() {
        let (store, fake) = fake::start().await;
        fake.lock().unwrap().seed(
            "connections/group/analytics/crm-db",
            &[("user", "u"), ("password", "p")],
            &[("type", "sqlserver"), ("keys", "user,password")],
        );
        // `data` has no listing; `finance` is a path the fake store refuses, as the real one would.
        let alice = who("sub-alice", &["analytics", "data", "finance"], false);

        let rows = list_connections(&store, None, &alice, &Secret::new("good"))
            .await
            .expect("listed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "crm-db");
        assert_eq!(rows[0].owner_group.as_deref(), Some("analytics"));
    }

    /// An administrator is a member of `analytics` and not of `finance`; both have a connection.
    #[tokio::test]
    async fn an_administrator_sees_the_connections_of_every_group() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        seed_finance(&fake);
        let (resources, _items) = memory_with(&[
            ("crm-db", group_resource()),
            (
                "ledger",
                json!({
                    "metadata": { "name": "ledger" },
                    "spec": { "scope": "group", "ownerGroup": "finance" },
                    "status": { "conditions": [
                        { "type": "Ready", "status": "False", "reason": "StoreUnavailable" }
                    ]},
                }),
            ),
        ]);
        let admin = who("sub-admin", &["analytics", "stardelt-admins"], true);

        let rows = list_connections(&store, Some(&resources), &admin, &Secret::new("admin"))
            .await
            .expect("listed");

        assert_eq!(
            rows.len(),
            2,
            "one row per connection, the member group not twice: {rows:#?}"
        );
        let analytics = rows
            .iter()
            .find(|r| r.owner_group.as_deref() == Some("analytics"))
            .expect("the caller's own group");
        let finance = rows
            .iter()
            .find(|r| r.owner_group.as_deref() == Some("finance"))
            .expect("the group the caller is not a member of");
        assert!(analytics.member, "their own group");
        assert!(
            !finance.member,
            "seen only because they are an administrator"
        );
        assert_eq!(
            finance.sync.as_ref().map(|s| s.reason.as_str()),
            Some("StoreUnavailable"),
            "the sync still comes from the resources fetched for administrators"
        );
        let answer = serde_json::to_string(&rows).unwrap();
        assert!(answer.contains("\"member\":false"), "{answer}");
        assert!(
            !answer.contains(CANARY) && !answer.contains("etl"),
            "no value in the listing: {answer}"
        );
    }

    #[tokio::test]
    async fn a_normal_person_still_sees_only_their_groups() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        seed_finance(&fake);
        let alice = who("sub-alice", &["analytics"], false);

        let rows = list_connections(&store, None, &alice, &Secret::new("good"))
            .await
            .expect("listed");

        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].name, "crm-db");
        assert!(rows[0].member);
        let lists = fake.lock().unwrap().lists.clone();
        assert!(
            !lists.iter().any(|path| path == "connections/group"),
            "a member never asks the store to name the groups: {lists:?}"
        );
    }

    #[tokio::test]
    async fn an_administrator_may_delete_a_group_connection_of_a_group_they_are_not_in() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let (store, fake) = fake::start().await;
        seed_finance(&fake);
        let (resources, items) = memory_with(&[(
            "ledger",
            json!({
                "metadata": { "name": "ledger" },
                "spec": { "scope": "group", "ownerGroup": "finance" },
            }),
        )]);
        let admin = who("sub-admin", &["stardelt-admins"], true);

        delete_connection(
            &store,
            Some(&resources),
            &admin,
            &Secret::new("admin"),
            StoreScope::Group {
                group: "finance".into(),
            },
            "ledger",
        )
        .await
        .expect("an administrator destroys it in an incident");

        assert!(
            stored(&fake, "connections/group/finance/ledger").is_none(),
            "the value is gone"
        );
        assert!(
            !items.lock().unwrap().contains_key("ledger"),
            "the resource is gone exactly like a member's delete"
        );
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("via=administrator"),
            "the audit line says how the right was held: {text}"
        );
        assert!(
            text.contains("action=\"delete\"") && text.contains("name=ledger"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn an_administrator_cannot_rotate_it() {
        let (store, fake) = fake::start().await;
        seed_finance(&fake);
        let (resources, _items) = memory();
        let admin = who("sub-admin", &["stardelt-admins"], true);
        let patch: BTreeMap<String, String> = [("password".to_string(), "p2".to_string())].into();

        let err = rotate_connection(
            &store,
            Some(&resources),
            &admin,
            &Secret::new("admin"),
            StoreScope::Group {
                group: "finance".into(),
            },
            "ledger",
            patch,
        )
        .await
        .expect_err("an administrator cannot write into a group");

        assert_eq!(err.status, StatusCode::FORBIDDEN);
        let fake = fake.lock().unwrap();
        assert_eq!(
            fake.requests, 0,
            "refused before the store was called, not even to sign in"
        );
        assert_eq!(
            fake.data["connections/group/finance/ledger"]["password"], CANARY,
            "the value is untouched"
        );
    }

    /// A signed-in caller, as `require_auth` would have put it in the request.
    fn principal() -> Principal {
        Principal {
            identity: who("sub-alice", &[], false),
            via: "session",
            access_token: Some(Secret::new("good")),
        }
    }

    fn admin_principal() -> Principal {
        Principal {
            identity: admin(),
            via: "session",
            access_token: Some(Secret::new("good")),
        }
    }

    /// The routes a body test needs, with `store` configured or not and a caller (`None` =
    /// no principal at all, as in demo mode and `static-dev`).
    fn app(store: Option<StoreClient>, caller: Option<Principal>) -> Router {
        let router = Router::new()
            .route("/connections", get(list).post(create))
            .route(
                "/connections/personal/{name}/break-glass",
                post(break_glass_personal),
            )
            .route(
                "/connections/managed/{name}/break-glass",
                post(break_glass_managed),
            )
            .route("/connections/break-glass-log", get(break_glass_log))
            .with_state(test_state(store));
        match caller {
            Some(principal) => router.layer(axum::Extension(principal)),
            None => router,
        }
    }

    fn test_state(store: Option<StoreClient>) -> Arc<AppState> {
        test_state_db(store, None)
    }

    fn test_state_db(store: Option<StoreClient>, db: Option<sqlx::PgPool>) -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: "http://127.0.0.1:1".into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            // No catalog client in these tests: Lakekeeper is called as before (#222).
            catalog_token: None,
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            catalog_ids: std::sync::RwLock::new(std::collections::HashMap::new()),
            catalog_access: std::sync::Mutex::new(std::collections::HashMap::new()),
            catalog_metrics: std::sync::Mutex::new(std::collections::HashMap::new()),
            catalog_layers: std::sync::Mutex::new(None),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(crate::demo::DemoState::default()),
            kube: None,
            workloads: crate::cluster::WorkloadCache::default(),
            admin_console_url: None,
            db,
            store,
            break_glass_reads: Mutex::new(HashMap::new()),
            run_starts: std::sync::Mutex::new(std::collections::HashMap::new()),
            pipeline_creates: Mutex::new(std::collections::HashMap::new()),
            catalog_access_changes: Mutex::new(std::collections::HashMap::new()),
            airflow: None,
            table_locks: Mutex::new(HashMap::new()),
            people_touched: Mutex::new(HashMap::new()),
            notebooks: None,
        })
    }

    #[tokio::test]
    async fn bad_body_does_not_echo_the_secret() {
        let request = Request::builder()
            .method("POST")
            .uri("/connections")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"name":"x","type":"generic","scope":"personal","secret":{{"password":{{"x":"{CANARY}"}}}}}}"#
            )))
            .unwrap();
        let (store, _fake) = fake::start().await;
        let response = app(Some(store), Some(principal()))
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !text.contains(CANARY),
            "the rejection must not echo the value: {text}"
        );
        let body: Value = serde_json::from_str(&text).expect("a JSON error");
        assert!(body["error"].is_string(), "{body}");
    }

    #[tokio::test]
    async fn store_unreachable_is_502_and_not_500() {
        let store = StoreClient::new(
            reqwest::Client::new(),
            secret_store::StoreConfig {
                url: "http://127.0.0.1:9".into(),
                role: "platform-users".into(),
                breakglass_role: "breakglass".into(),
                mount: "secret".into(),
            },
        );
        let alice = who("sub-alice", &[], false);

        let err = create_connection(&store, None, &alice, &Secret::new("good"), personal())
            .await
            .expect_err("the store is down");
        assert_eq!(err.status, StatusCode::BAD_GATEWAY, "{err:?}");

        let err = list_connections(&store, None, &alice, &Secret::new("good"))
            .await
            .expect_err("the store is down");
        assert_eq!(err.status, StatusCode::BAD_GATEWAY, "{err:?}");
    }

    #[tokio::test]
    async fn not_configured_is_503() {
        // A signed-in caller with a store missing, and a caller with no principal at all (demo
        // mode): both answer the same sentence, and before the body is looked at.
        for caller in [Some(principal()), None] {
            let app = app(None, caller);
            for method in ["GET", "POST"] {
                let request = Request::builder()
                    .method(method)
                    .uri("/connections")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"x"}"#))
                    .unwrap();
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::SERVICE_UNAVAILABLE,
                    "{method}"
                );
                let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(value["error"], "the secret store is not configured");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Break-glass: the read, its refusals, the notice and the audit record.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn break_glass_returns_the_fields_once_and_announces() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, items) = memory_with(&[("crm-db", group_resource())]);
        let admin = admin();
        let limiter = limiter();

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter,
            None,
            &admin,
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            Some("req-1"),
        )
        .await;

        let response = result.answer.expect("the read succeeded");
        assert_eq!(response.fields["password"], CANARY);
        assert_eq!(response.fields["user"], "etl");
        assert!(response.read_at.ends_with('Z'), "{}", response.read_at);

        let items = items.lock().unwrap();
        let annotation = items["crm-db"]["metadata"]["annotations"][BREAK_GLASS_ANNOTATION]
            .as_str()
            .expect("the notice is on the resource");
        let notice: Value = serde_json::from_str(annotation).expect("the notice is JSON");
        assert_eq!(notice["by"], "sub-admin");
        assert_eq!(notice["reason"], REASON);
        assert!(notice["at"].as_str().unwrap().ends_with('Z'));
        assert!(
            !annotation.contains(CANARY),
            "the notice never carries the value: {annotation}"
        );

        assert_eq!(result.record.outcome, "read");
        assert_eq!(result.record.admin, "sub-admin");
        assert_eq!(result.record.admin_sub, "sub-admin");
        assert_eq!(result.record.owner_group, "analytics");
        assert_eq!(result.record.connection, "crm-db");
        assert_eq!(result.record.scope, "group");
        assert_eq!(result.record.request_id.as_deref(), Some("req-1"));
        assert!(
            !format!("{:?}", result.record).contains(CANARY),
            "the audit record cannot hold a value"
        );
        assert!(
            !serde_json::to_string(&result.record)
                .unwrap()
                .contains(CANARY),
            "the audit record's JSON cannot hold a value"
        );
    }

    #[tokio::test]
    async fn break_glass_needs_an_administrator() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);
        let alice = who("sub-alice", &["analytics"], false);

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &alice,
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;

        let (err, record) = expect_fail(result);
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.message, "only administrators may do this");
        assert_eq!(record.outcome, "refused:not-admin");
        assert_eq!(
            fake.lock().unwrap().requests,
            0,
            "the store is not called for a refusal"
        );
    }

    #[tokio::test]
    async fn break_glass_needs_a_reason() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);
        let admin = admin();
        let limiter = limiter();
        let short = "a".repeat(19);
        let spaces = " ".repeat(30);

        for reason in ["", short.as_str(), spaces.as_str()] {
            let result = break_glass_group_at(
                &store,
                Some(&resources),
                &limiter,
                None,
                &admin,
                &Secret::new("good"),
                "analytics",
                "crm-db",
                reason,
                None,
            )
            .await;
            let (err, record) = expect_fail(result);
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{reason:?}");
            assert_eq!(err.message, "give a reason of at least 20 characters");
            assert_eq!(record.outcome, "refused:reason");
        }
        assert_eq!(
            fake.lock().unwrap().requests,
            0,
            "the store is not called for a refused reason"
        );

        // Twenty characters is enough.
        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter,
            None,
            &admin,
            &Secret::new("good"),
            "analytics",
            "crm-db",
            &"a".repeat(20),
            None,
        )
        .await;
        assert!(result.answer.is_ok(), "{:?}", result.record.outcome);
        assert_eq!(result.record.outcome, "read");
    }

    #[tokio::test]
    async fn break_glass_is_refused_for_personal_and_managed() {
        let (store, fake) = fake::start().await;
        let app = app(Some(store), Some(admin_principal()));
        for uri in [
            "/connections/personal/crm-db/break-glass",
            "/connections/managed/crm-db/break-glass",
        ] {
            let request = Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"reason":"{REASON}"}}"#)))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"], "break-glass covers group connections only");
        }
        assert_eq!(
            fake.lock().unwrap().requests,
            0,
            "the store is never called for personal or managed"
        );

        // The record says why, and the scope is named.
        for (scope, expected) in [("personal", "personal"), ("managed", "managed")] {
            let result =
                break_glass_refused_scope(None, &admin(), scope, "crm-db", REASON, None).await;
            assert_eq!(result.record.outcome, "refused:scope");
            assert_eq!(result.record.scope, expected);
        }
    }

    #[tokio::test]
    async fn break_glass_uses_the_breakglass_role_and_the_admins_token() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);

        break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await
        .answer
        .expect("the read succeeded");

        let fake = fake.lock().unwrap();
        assert_eq!(
            fake.logins,
            [("breakglass".to_string(), "good".to_string())],
            "one sign-in, with the breakglass role and the administrator's own token"
        );
        assert_eq!(
            fake.reads,
            ["connections/group/analytics/crm-db"],
            "one read, of exactly the group path"
        );
        assert_eq!(fake.revokes, ["tok-alice"], "the store token is revoked");
    }

    #[tokio::test]
    async fn break_glass_fails_closed_when_the_notice_cannot_be_written() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let items = Arc::new(Mutex::new(BTreeMap::new()));
        items
            .lock()
            .unwrap()
            .insert("crm-db".into(), group_resource());
        let resources = Resources::Memory {
            items: Arc::clone(&items),
            fail_delete: false,
            fail_annotate: true,
        };

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;

        let (err, record) = expect_fail(result);
        assert_eq!(err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            err.message,
            "could not record the notice; nothing was returned"
        );
        assert!(!err.message.contains(CANARY), "no value in the error");
        assert_eq!(record.outcome, "notice-failed");
        assert!(
            items.lock().unwrap()["crm-db"]["metadata"]
                .get("annotations")
                .is_none(),
            "nothing was announced, so nothing is written"
        );
    }

    #[tokio::test]
    async fn break_glass_unknown_connection_is_404() {
        let (store, _fake) = fake::start().await;
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;

        let (err, record) = expect_fail(result);
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "no such connection");
        assert_eq!(record.outcome, "not-found");
    }

    #[tokio::test]
    async fn break_glass_store_refusal_is_403() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("issued-to-another-client"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;

        let (err, record) = expect_fail(result);
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.message, "the store refused the break-glass sign-in");
        assert_eq!(record.outcome, "refused:store");
        assert!(
            fake.lock().unwrap().reads.is_empty(),
            "a refused sign-in never reads"
        );
    }

    #[tokio::test]
    async fn break_glass_store_down_is_502_not_500() {
        let store = StoreClient::new(
            reqwest::Client::new(),
            secret_store::StoreConfig {
                url: "http://127.0.0.1:9".into(),
                role: "platform-users".into(),
                breakglass_role: "breakglass".into(),
                mount: "secret".into(),
            },
        );
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;

        let (err, record) = expect_fail(result);
        assert_eq!(err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(record.outcome, "refused:store");
    }

    #[tokio::test]
    async fn break_glass_is_rate_limited() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);
        let limiter = limiter();
        let admin = admin();

        for _ in 0..BREAK_GLASS_LIMIT {
            let result = break_glass_group_at(
                &store,
                Some(&resources),
                &limiter,
                None,
                &admin,
                &Secret::new("good"),
                "analytics",
                "crm-db",
                REASON,
                None,
            )
            .await;
            assert!(result.answer.is_ok(), "{}", result.record.outcome);
        }

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter,
            None,
            &admin,
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;
        let (err, record) = expect_fail(result);
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.message, "too many break-glass reads; wait");
        assert_eq!(record.outcome, "refused:rate");
        assert_eq!(
            fake.lock().unwrap().reads.len(),
            BREAK_GLASS_LIMIT,
            "the refused attempt never reached the store"
        );
    }

    #[tokio::test]
    async fn rotate_clears_the_break_glass_notice() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let notice = json!({
            "by": "sub-admin",
            "at": "2026-10-20T08:00:00Z",
            "reason": REASON,
        })
        .to_string();
        let resource = json!({
            "metadata": {
                "name": "crm-db",
                "annotations": { BREAK_GLASS_ANNOTATION: notice },
            },
            "spec": { "scope": "group", "ownerGroup": "analytics" },
        });
        let (resources, items) = memory_with(&[("crm-db", resource)]);
        let alice = who("sub-alice", &["analytics"], false);
        let patch: BTreeMap<String, String> = [("password".to_string(), "p2".to_string())].into();

        rotate_connection(
            &store,
            Some(&resources),
            &alice,
            &Secret::new("good"),
            StoreScope::Group {
                group: "analytics".into(),
            },
            "crm-db",
            patch,
        )
        .await
        .expect("rotated");

        assert!(
            items.lock().unwrap()["crm-db"]["metadata"]["annotations"]
                .get(BREAK_GLASS_ANNOTATION)
                .is_none(),
            "a rotation answers the notice"
        );

        // A cluster that refuses the annotation patch does not fail the rotation.
        let resource = json!({
            "metadata": {
                "name": "crm-db",
                "annotations": { BREAK_GLASS_ANNOTATION: notice },
            },
            "spec": { "scope": "group", "ownerGroup": "analytics" },
        });
        let items = Arc::new(Mutex::new(BTreeMap::new()));
        items.lock().unwrap().insert("crm-db".into(), resource);
        let resources = Resources::Memory {
            items: Arc::clone(&items),
            fail_delete: false,
            fail_annotate: true,
        };
        let patch: BTreeMap<String, String> = [("password".to_string(), "p3".to_string())].into();
        rotate_connection(
            &store,
            Some(&resources),
            &alice,
            &Secret::new("good"),
            StoreScope::Group {
                group: "analytics".into(),
            },
            "crm-db",
            patch,
        )
        .await
        .expect("the rotation still succeeded");
        assert!(
            items.lock().unwrap()["crm-db"]["metadata"]["annotations"]
                .get(BREAK_GLASS_ANNOTATION)
                .is_some(),
            "the failed clear left the notice (and only logged it)"
        );
    }

    #[tokio::test]
    async fn list_shows_the_notice_for_group_rows_only() {
        let (store, fake) = fake::start().await;
        fake.lock().unwrap().seed(
            "connections/user/sub-alice/personal-db",
            &[("token", "t")],
            &[("type", "generic"), ("keys", "token")],
        );
        seed_group(&fake);
        fake.lock().unwrap().seed(
            "connections/managed/shared-db",
            &[("user", "u"), ("password", "p")],
            &[("type", "sqlserver"), ("keys", "password,user")],
        );
        let notice = json!({
            "by": "sub-admin",
            "at": "2026-10-20T08:00:00Z",
            "reason": REASON,
        })
        .to_string();
        let (resources, _items) = memory_with(&[
            (
                "crm-db",
                json!({
                    "metadata": {
                        "name": "crm-db",
                        "annotations": { BREAK_GLASS_ANNOTATION: notice },
                    },
                    "spec": { "scope": "group", "ownerGroup": "analytics" },
                }),
            ),
            (
                "shared-db",
                json!({
                    "metadata": {
                        "name": "shared-db",
                        "annotations": { BREAK_GLASS_ANNOTATION: notice },
                    },
                    "spec": { "scope": "managed" },
                }),
            ),
        ]);
        let alice = who("sub-alice", &["analytics"], true);

        // An administrator's own token carries the `connections-admin` policy, which is what lets
        // the listing name every group (see `list_group_names`).
        let rows = list_connections(&store, Some(&resources), &alice, &Secret::new("admin"))
            .await
            .expect("listed");

        let group = rows.iter().find(|r| r.scope == Scope::Group).unwrap();
        let notice = group.break_glass.as_ref().expect("the group row shows it");
        assert_eq!(notice.by, "sub-admin");
        assert_eq!(notice.reason, REASON);
        assert!(
            rows.iter()
                .filter(|r| r.scope != Scope::Group)
                .all(|r| r.break_glass.is_none()),
            "personal and managed rows never carry it"
        );
    }

    #[tokio::test]
    async fn break_glass_response_is_not_cacheable() {
        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);

        let result = break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await;
        let response = result.answer.expect("the read succeeded").into_response();
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        assert_eq!(response.headers().get("pragma").unwrap(), "no-cache");
    }

    /// A shared buffer a tracing subscriber can write into.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn the_log_has_no_value() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(captured.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let (store, fake) = fake::start().await;
        seed_group(&fake);
        let (resources, _items) = memory_with(&[("crm-db", group_resource())]);

        break_glass_group_at(
            &store,
            Some(&resources),
            &limiter(),
            None,
            &admin(),
            &Secret::new("good"),
            "analytics",
            "crm-db",
            REASON,
            None,
        )
        .await
        .answer
        .expect("the read succeeded");

        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            !text.contains(CANARY),
            "no log line may carry the value: {text}"
        );
        assert!(
            text.contains("break-glass"),
            "the audit line was captured, so the check means something: {text}"
        );
    }

    #[tokio::test]
    async fn the_log_needs_an_administrator_and_a_state_store() {
        let sandbox = app(None, Some(principal()));
        let response = sandbox
            .oneshot(
                Request::builder()
                    .uri("/connections/break-glass-log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let sandbox = app(None, Some(admin_principal()));
        let response = sandbox
            .oneshot(
                Request::builder()
                    .uri("/connections/break-glass-log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "the state store is not configured");
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn break_glass_log_round_trips() {
        let pool = crate::db::test_support::migrated_pool().await;
        let record = |outcome: &str| BreakGlassRecord {
            admin: "sub-admin".into(),
            admin_sub: "sub-admin".into(),
            owner_group: "analytics".into(),
            connection: "crm-db".into(),
            scope: "group".into(),
            reason: REASON.into(),
            outcome: outcome.into(),
            request_id: Some("req-1".into()),
        };

        finish_break_glass(
            Some(&pool),
            record("read"),
            "read",
            Ok(BreakGlassResponse {
                fields: BTreeMap::new(),
                read_at: "2026-10-20T08:00:00Z".into(),
            }),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        finish_break_glass(
            Some(&pool),
            record("refused:scope"),
            "refused:scope",
            Err(Fail::new(StatusCode::FORBIDDEN, "refused")),
        )
        .await;

        let response = break_glass_log(
            State(test_state_db(None, Some(pool))),
            Some(axum::Extension(admin_principal())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let events = body["events"].as_array().expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["outcome"], "refused:scope", "newest first");
        assert_eq!(events[1]["outcome"], "read");
        assert_eq!(events[0]["ownerGroup"], "analytics");
        assert_eq!(events[0]["admin"], "sub-admin");
        assert!(!body.to_string().contains(CANARY));
    }
}
