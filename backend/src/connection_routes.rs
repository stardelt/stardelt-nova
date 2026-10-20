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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Path},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use futures::stream::{self, StreamExt};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind};
use serde::Deserialize;
use serde_json::Value;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::audit;
use crate::auth::{Identity, Principal, Secret};
use crate::connections::{self, Action, ConnectionView, NewConnection, Scope, SyncView};
use crate::secret_store::{self, SecretMeta, StoreClient, StoreError, StoreScope, StoreToken};

/// The `Connection` resources of the platform namespace: the cluster, or memory in tests.
pub enum Resources {
    Kube(Api<DynamicObject>),
    #[cfg(test)]
    Memory {
        items: Arc<std::sync::Mutex<BTreeMap<String, Value>>>,
        /// Answer `Unavailable` from `delete`, as an API server that refuses would.
        fail_delete: bool,
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
            Self::Memory { items, fail_delete } => {
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
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
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
    let rows = resource_rows(resources, view_scope).await;
    audit::connection_event("rotate", who, scope_word(view_scope), group, name);
    Ok(connections::view_from_store(
        name,
        view_scope,
        group,
        resource_row(&rows, view_scope, group, name)
            .map(usable_by)
            .unwrap_or_default(),
        &meta,
        sync_in(&rows, view_scope, group, name),
    ))
}

/// Delete the value first, then the resource: if the second step fails the value is gone and the
/// resource stays visible as not-ready, which is retryable.
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

    let path = secret_store::secret_path(&scope, name).map_err(store_fail)?;
    let token = store.login(access).await.map_err(store_fail)?;
    store.destroy(&token, &path).await.map_err(store_fail)?;

    if view_scope != Scope::Personal
        && let Some(resources) = resources
    {
        resources.delete(name).await.map_err(cluster_fail)?;
    }

    audit::connection_event("delete", who, scope_word(view_scope), group, name);
    Ok(())
}

/// `GET /api/connections`: personal, then group, then managed, each sorted by name.
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
    rows.extend(rows_from_store(store, &token, &personal, Scope::Personal, None, &[]).await?);

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
            )
            .await?,
        );
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
            ));
        }
    }

    rows.sort_by(|a, b| (scope_rank(a.scope), &a.name).cmp(&(scope_rank(b.scope), &b.name)));
    Ok(rows)
}

/// The rows of one store scope. Values are read for nothing here: the listing is metadata only.
async fn rows_from_store(
    store: &StoreClient,
    token: &StoreToken,
    scope: &StoreScope,
    view: Scope,
    group: Option<&str>,
    cluster_rows: &[Value],
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
            Ok(meta) => rows.push(connections::view_from_store(
                &name,
                view,
                group,
                Vec::new(),
                &meta,
                sync_in(cluster_rows, view, group, &name),
            )),
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
}

impl SignedIn {
    /// The store. Present: the extractor refused the request otherwise, and state never changes.
    fn store(&self) -> &StoreClient {
        self.state
            .store
            .as_ref()
            .expect("the extractor checked the store")
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
        Ok(Self {
            state: Arc::clone(state),
            identity: principal.identity,
            access,
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
        routing::get,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::secret_store::fake;

    /// A value that must never appear in an answer.
    const CANARY: &str = "canary-7f3a";

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
            },
            items,
        )
    }

    fn stored(fake: &fake::Shared, path: &str) -> Option<BTreeMap<String, String>> {
        fake.lock().unwrap().data.get(path).cloned()
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
    async fn an_admin_cannot_create_in_a_group_they_are_not_in() {
        let (store, fake) = fake::start().await;
        let admin = who("sub-admin", &["stardelt-admins"], true);
        let req = body(json!({ "ownerGroup": "analytics" }));

        let err = create_connection(&store, None, &admin, &Secret::new("good"), req)
            .await
            .expect_err("an administrator is not a member");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(
            fake.lock().unwrap().data.is_empty(),
            "the store is untouched"
        );
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

    /// A signed-in caller, as `require_auth` would have put it in the request.
    fn principal() -> Principal {
        Principal {
            identity: who("sub-alice", &[], false),
            via: "session",
            access_token: Some(Secret::new("good")),
        }
    }

    /// The two routes a body test needs, with `store` configured or not and a caller (`None` =
    /// no principal at all, as in demo mode and `static-dev`).
    fn app(store: Option<StoreClient>, caller: Option<Principal>) -> Router {
        let router = Router::new()
            .route("/connections", get(list).post(create))
            .with_state(test_state(store));
        match caller {
            Some(principal) => router.layer(axum::Extension(principal)),
            None => router,
        }
    }

    fn test_state(store: Option<StoreClient>) -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: "http://127.0.0.1:1".into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            warehouse_id: tokio::sync::OnceCell::new(),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(crate::demo::DemoState::default()),
            kube: None,
            workloads: crate::cluster::WorkloadCache::default(),
            admin_console_url: None,
            db: None,
            store,
            airflow: None,
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
}
