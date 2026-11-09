//! Administrator view of every catalog and its grants (stardelt-nova#251 and #253).
//!
//! `GET /api/admin/catalogs` answers the `Catalog` resources of the platform
//! namespace (`stardelt.io/v1alpha1`) as rows: identity, owner, managed access,
//! the storage key prefix, the phase, and the grants and conditions the
//! access-sync job last wrote.
//!
//! `PUT /api/admin/catalogs/{name}/access` changes `spec.owner`,
//! `spec.managedAccess` and `spec.grants` as one JSON merge patch. The `Catalog`
//! resource stays the source of truth: Nova writes it with the `resourceVersion`
//! the page read, so a concurrent change is a 409 rather than an overwrite, and
//! the access-sync job (every two minutes) applies the new spec to Lakekeeper.
//! Nova never writes Lakekeeper rights itself and cannot trigger the sync; the
//! page polls the list until `status.observedGeneration` catches up.
//!
//! Administrator-only. `main` puts `auth::require_admin` in front of both routes
//! in `oidc` mode, exactly like `/api/apps`, so a non-administrator answers 403
//! `this view is for platform administrators`. In `dev` mode there is no IdP and
//! the single user is the operator.
//!
//! Permissions are catalog-wide today (`spec.grants[{group,verb}]` plus the one
//! `spec.owner` group, which holds `manage` and not `select`). Platform
//! administrators hold `project_admin` by role, so they are not listed in a
//! catalog's grants and can do everything in every catalog; the page says so in
//! one line. Table- and schema-level grants do not exist yet.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use kube::api::{
    Api, ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams, PostParams,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::auth::{Identity, Principal};
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
    /// The medallion layer (`bronze`, `silver`, `gold`) from the `stardelt.io/layer` label
    /// or the catalog's own name; absent for every other catalog (stardelt-nova#109).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
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
    /// The GitOps tool that owns this resource, or null. A Nova edit would be reverted by it.
    pub managed_by: Option<String>,
    /// `spec.grants`, in the order the resource declares them.
    pub grants: Vec<CatalogGrant>,
    /// `status.conditions`: what the access-sync job last reported.
    pub conditions: Vec<CatalogCondition>,
}

/// One declared grant: a group and one verb.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
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

/// The verbs a grant may carry, in the order the dialog shows them.
const VERBS: [&str; 5] = ["browse", "select", "write", "create", "manage"];

/// At most this many access changes per administrator in [`ACCESS_CHANGE_WINDOW`].
const ACCESS_CHANGE_LIMIT: usize = 30;
const ACCESS_CHANGE_WINDOW: Duration = Duration::from_secs(60 * 60);

/// At most this many team spaces per administrator in [`TEAM_CREATE_WINDOW`].
const TEAM_CREATE_LIMIT: usize = 10;
const TEAM_CREATE_WINDOW: Duration = Duration::from_secs(60 * 60);

/// The in-memory per-administrator rate limit (`AppState::catalog_access_changes`).
pub type AccessChangesLimiter = Mutex<HashMap<String, VecDeque<Instant>>>;

/// The in-memory per-administrator rate limit of `POST /api/admin/teams`
/// (`AppState::team_creates`): ten new team spaces an hour, each one three catalogs.
pub type TeamCreatesLimiter = Mutex<HashMap<String, VecDeque<Instant>>>;

/// One `PUT /api/admin/catalogs/{name}/access` body.
///
/// `resourceVersion` is the value the page read: the API server rejects the merge
/// patch with `409` when the object changed meanwhile, so two administrators
/// editing the same catalog cannot silently overwrite each other.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogAccessUpdate {
    /// `metadata.resourceVersion` from the row the page holds.
    pub resource_version: String,
    /// `spec.owner`: exactly one group.
    pub owner: String,
    /// `spec.managedAccess`: the owner cannot grant on this catalog.
    pub managed_access: bool,
    /// The whole `spec.grants` array, each `{group, verb}` once.
    pub grants: Vec<CatalogGrant>,
}

/// One `POST /api/admin/teams` body: a new team space.
///
/// The group comes from the company's sign-in provider; stardelt never creates groups,
/// so `group` must already exist there.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTeam {
    /// The team's short name: 2 to 21 lower-case letters and digits, starting with a letter.
    pub team: String,
    /// The group the three catalogs belong to.
    pub group: String,
    /// `spec.description` of the production catalog; the other two carry a fixed sentence.
    #[serde(default)]
    pub description: Option<String>,
}

/// What a created team space answers: its three catalogs and its pipeline principal.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTeamResponse {
    /// `<team>`, `<team>lab` and `<team>ref`, in that order.
    pub catalogs: Vec<String>,
    /// `pipeline-<group>`.
    pub principal: String,
    /// False when the principal already existed for another team of the same group.
    pub principal_created: bool,
}

#[utoipa::path(
    get,
    operation_id = "admin_catalogs_list",
    path = "/api/admin/catalogs",
    tag = "internal",
    description = "Every `Catalog` of the platform namespace, ordered bronze, silver, gold, then the rest alphabetically (`layer`: the `stardelt.io/layer` label, else the catalog's own name): identity, owner, managed access, the storage key prefix, phase and the grants and conditions the access-sync job last wrote. Administrator-only; a non-administrator answers 403 `this view is for platform administrators`. The write is `PUT /api/admin/catalogs/{name}/access`; this route never changes anything. A catalog the job has not observed yet answers `phase: \"Unknown\"` with no conditions. Without a cluster connection (demo mode, a local run) the answer is the one in-memory `warehouse` catalog. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "One row per catalog, medallion layers first, then by name", body = [AdminCatalog]),
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

#[utoipa::path(
    put,
    operation_id = "admin_catalogs_access_update",
    path = "/api/admin/catalogs/{name}/access",
    tag = "internal",
    description = "Change one catalog's `spec.owner`, `spec.managedAccess` and `spec.grants` as a JSON merge patch on the `Catalog` resource, carrying the `resourceVersion` the page read. Administrator-only; a non-administrator answers 403 `this view is for platform administrators`. A malformed owner, group name, verb or a duplicate grant is a 400 in the words the form shows; a catalog that changed meanwhile is a 409 `this catalog changed since you opened it; reload`, and a catalog a GitOps tool owns (Argo CD or Flux, from its annotations or labels) is a 409 `<name> is managed by <tool>: change its grants in Git` with nothing patched; a name that is not a catalog or no longer exists is a 404; more than 30 changes per administrator in an hour is a 429. Without a cluster connection the answer is 503 `the cluster is not reachable from Nova` (demo mode changes memory instead). Answers the updated row, the same shape as the list, with the new `metadata.generation`; `status.observedGeneration` catches up when the access-sync job runs (about two minutes). Never carries a storage credential. `Cache-Control: no-store`.",
    params(("name" = String, Path, description = "The catalog's name")),
    request_body = CatalogAccessUpdate,
    responses(
        (status = 200, description = "The updated row", body = AdminCatalog),
        (status = 400, description = "The owner, a group name, a verb or the grant list is invalid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller is not a platform administrator", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The catalog changed since the page read it, or a GitOps tool owns it", body = crate::openapi::ErrorResponse),
        (status = 429, description = "More than 30 changes this hour", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer, or Nova may not patch catalogs", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Nova has no cluster client", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn update_access(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    principal: Option<axum::Extension<Principal>>,
    ApiJson(update): ApiJson<CatalogAccessUpdate>,
) -> Response {
    // A name that cannot be a catalog is answered before any cluster call, exactly like
    // the read routes: it is a 404, never an API-server detail.
    if !crate::catalog::valid_catalog(&name) {
        return no_such_catalog().into_response();
    }
    if let Err(fail) = validate_access(&update) {
        return fail.into_response();
    }
    let who = actor(principal, &state);

    let Some(client) = state.kube.clone() else {
        if state.demo {
            return demo_update(&state, &name, &update, &who);
        }
        return Fail::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the cluster is not reachable from Nova",
        )
        .into_response();
    };

    let api = catalogs_api(client);
    let before = match api.get_opt(&name).await {
        Ok(Some(object)) => object,
        Ok(None) => return no_such_catalog().into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "admin catalog read before change failed");
            return cluster_unavailable().into_response();
        }
    };
    let before = match serde_json::to_value(before) {
        Ok(value) => row_from_object(&value),
        Err(e) => {
            tracing::warn!(error = %e, "admin catalog object is not JSON");
            return cluster_unavailable().into_response();
        }
    };

    // A resource a GitOps tool owns is changed in Git, not here: accepting the write would
    // only have the tool put the old grants back minutes later (internal#106 decision 4).
    // Nothing is patched, so the refusal spends none of the thirty slots.
    if let Some(tool) = before.managed_by.clone() {
        crate::audit::catalog_access_refused(&who, &name, &tool);
        return Fail::new(
            StatusCode::CONFLICT,
            format!("{name} is managed by {tool}: change its grants in Git"),
        )
        .into_response();
    }

    // The slot is taken only for an attempt that reached a change (the catalog exists and
    // the body is valid): a typo never spends one of the thirty.
    if !take_access_change_slot(&state.catalog_access_changes, &who.sub) {
        return too_many_changes().into_response();
    }

    let patch = json!({
        "metadata": { "resourceVersion": update.resource_version },
        "spec": {
            "owner": update.owner,
            "managedAccess": update.managed_access,
            "grants": update.grants,
        }
    });
    let object = match api
        .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
        .await
    {
        Ok(object) => object,
        Err(kube::Error::Api(error)) if error.code == 409 => {
            return catalog_changed().into_response();
        }
        Err(kube::Error::Api(error)) if error.code == 404 => {
            return no_such_catalog().into_response();
        }
        Err(e) => {
            // The detail names the API server (and sometimes the ServiceAccount), so it
            // stays in the log; the answer is the same generic sentence as the list's.
            tracing::warn!(error = %e, "admin catalog change failed");
            return cluster_unavailable().into_response();
        }
    };
    let after = match serde_json::to_value(object) {
        Ok(value) => row_from_object(&value),
        Err(e) => {
            tracing::warn!(error = %e, "admin catalog object is not JSON");
            return cluster_unavailable().into_response();
        }
    };

    audit_change(
        &who,
        &name,
        before.owner.as_deref(),
        &update.owner,
        &before.grants,
        &update.grants,
    );
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(after),
    )
        .into_response()
}

#[utoipa::path(
    post,
    operation_id = "admin_teams_create",
    path = "/api/admin/teams",
    tag = "internal",
    description = "Create a new team space in one call: the `Catalog` resources `<team>` (production), `<team>lab` (the team's sandbox) and `<team>ref` (shared reference data), each owned by `group` with the four verbs `browse`, `select`, `write` and `create` for `group` and for `stardelt-admins`, plus the `ServicePrincipal` `pipeline-<group>` when it does not exist yet (it may already serve another team of the same group). The group must already exist in the company's sign-in provider — stardelt never creates groups. Administrator-only; a non-administrator answers 403 `this view is for platform administrators`. A team name outside 2 to 21 lower-case letters and digits starting with a letter, a group name outside the group rule, or a description over 500 characters is a 400; any of the three catalogs already existing is a 409 `the catalog <name> already exists` with nothing created, and a platform without any `Catalog` is a 409 `no lakehouse to put the catalogs in`; more than ten team spaces per administrator in an hour is a 429. When the API server refuses one of the creates the answer is a 502 naming what failed and what was created so far; nothing is rolled back, so a half-created space stays visible and can be finished by hand. Without a cluster connection the answer is 503 `the cluster is not reachable from Nova`. Answers 201 with the three catalog names and the principal, `Cache-Control: no-store`.",
    request_body = NewTeam,
    responses(
        (status = 201, description = "The three catalogs and the pipeline principal", body = NewTeamResponse),
        (status = 400, description = "The team name, the group name or the description is invalid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller is not a platform administrator", body = crate::openapi::ErrorResponse),
        (status = 409, description = "One of the three catalogs already exists, or there is no lakehouse", body = crate::openapi::ErrorResponse),
        (status = 429, description = "More than ten team spaces this hour", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer, or Nova may not create the resources", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Nova has no cluster client", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn create_team(
    State(state): State<Arc<AppState>>,
    principal: Option<axum::Extension<Principal>>,
    ApiJson(body): ApiJson<NewTeam>,
) -> Response {
    if let Err(fail) = validate_team(&body) {
        return fail.into_response();
    }
    // The same actor rule as `update_access`: `require_admin` ran in `oidc` mode, and in
    // `dev` mode the one operator is the actor with a stable limiter key.
    let who = actor(principal, &state);

    let Some(client) = state.kube.clone() else {
        return Fail::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the cluster is not reachable from Nova",
        )
        .into_response();
    };

    let catalogs = catalogs_api(client.clone());
    let existing = match catalogs.list(&kube::api::ListParams::default()).await {
        Ok(list) => list.items,
        Err(e) => {
            tracing::warn!(error = %e, "admin team catalog list failed");
            return cluster_unavailable().into_response();
        }
    };

    // All three names are checked against one list, before anything is created: either
    // the whole team space is new or nothing happens.
    let team = body.team.clone();
    let lab = format!("{team}lab");
    let reference = format!("{team}ref");
    for wanted in [&team, &lab, &reference] {
        if existing
            .iter()
            .any(|object| object.metadata.name.as_deref() == Some(wanted.as_str()))
        {
            return Fail::new(
                StatusCode::CONFLICT,
                format!("the catalog {wanted} already exists"),
            )
            .into_response();
        }
    }
    let lakehouse = existing
        .first()
        .and_then(|object| object.data.get("spec"))
        .and_then(|spec| spec.get("lakehouseRef"))
        .and_then(|reference| reference.get("name"))
        .and_then(Value::as_str);
    let Some(lakehouse) = lakehouse else {
        return Fail::new(StatusCode::CONFLICT, "no lakehouse to put the catalogs in")
            .into_response();
    };

    // The slot is taken only for an attempt that reached a change (the body is valid, the
    // names are free): a typo or a conflict never spends one of the ten.
    if !take_slot(
        &state.team_creates,
        &who.sub,
        TEAM_CREATE_LIMIT,
        TEAM_CREATE_WINDOW,
    ) {
        return Fail::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many new team spaces this hour; wait",
        )
        .into_response();
    }

    let title = capitalized(&team);
    let wanted = [
        (team.clone(), title.clone(), body.description.clone()),
        (
            lab,
            format!("{title} lab"),
            Some(format!("Work in progress for {team}.")),
        ),
        (
            reference,
            format!("{title} reference"),
            Some(format!("Shared reference data for {team}.")),
        ),
    ];

    let mut created: Vec<String> = Vec::with_capacity(wanted.len());
    for (name, display_name, description) in &wanted {
        let spec = catalog_spec(lakehouse, &body.group, display_name, description.as_deref());
        if let Err(e) = create_object(&catalogs, &catalog_resource(name, spec)).await {
            // The API server's words stay in the log; the answer says what failed and what
            // exists now, because nothing is rolled back.
            tracing::warn!(error = %e, catalog = %name, "admin team catalog create failed");
            return Fail::new(
                StatusCode::BAD_GATEWAY,
                format!(
                    "creating the catalog {name} failed; created so far: {}",
                    so_far(&created)
                ),
            )
            .into_response();
        }
        created.push(name.clone());
    }

    // The principal is shared by every team of the group, so it is created only when the
    // namespace has none by that name yet.
    let principal_name = format!("pipeline-{}", body.group);
    let principals = principals_api(client);
    let existing_principals = match principals.list(&kube::api::ListParams::default()).await {
        Ok(list) => list.items,
        Err(e) => {
            tracing::warn!(error = %e, "admin team principal list failed");
            return Fail::new(
                StatusCode::BAD_GATEWAY,
                format!(
                    "reading the principals failed; created so far: {}",
                    so_far(&created)
                ),
            )
            .into_response();
        }
    };
    let already_there = existing_principals
        .iter()
        .any(|object| object.metadata.name.as_deref() == Some(principal_name.as_str()));
    let mut principal_created = false;
    if !already_there {
        let resource = principal_resource(&principal_name, &body.group);
        if let Err(e) = create_object(&principals, &resource).await {
            tracing::warn!(error = %e, principal = %principal_name, "admin team principal create failed");
            return Fail::new(
                StatusCode::BAD_GATEWAY,
                format!(
                    "creating the principal {principal_name} failed; created so far: {}",
                    so_far(&created)
                ),
            )
            .into_response();
        }
        principal_created = true;
    }

    crate::audit::team_created(&who, &body.team, &body.group);

    let answer = NewTeamResponse {
        catalogs: created,
        principal: principal_name,
        principal_created,
    };
    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(answer),
    )
        .into_response()
}

/// The team-space body rules, in the order the form shows them.
fn validate_team(body: &NewTeam) -> Result<(), Fail> {
    if !valid_team_name(&body.team) {
        return Err(bad_request(
            "a team name is 2 to 21 lower-case letters and digits, starting with a letter",
        ));
    }
    if !valid_group(&body.group) {
        return Err(bad_request(GROUP_SENTENCE));
    }
    if body
        .description
        .as_deref()
        .is_some_and(|description| description.chars().count() > 500)
    {
        return Err(bad_request("the description is at most 500 characters"));
    }
    Ok(())
}

/// A team name: `^[a-z][a-z0-9]{1,20}$`.
fn valid_team_name(team: &str) -> bool {
    let bytes = team.as_bytes();
    (2..=21).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The team name with its first letter capitalised: `mobility` is `Mobility`.
fn capitalized(team: &str) -> String {
    let mut chars = team.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `spec` of one of the three catalogs: the owner group, the lakehouse, the display name,
/// the description when there is one, and the four verbs for the group and for the
/// administrators. No `spec.storage`: the catalog takes the lakehouse's defaults.
fn catalog_spec(
    lakehouse: &str,
    group: &str,
    display_name: &str,
    description: Option<&str>,
) -> Value {
    let mut grants = Vec::with_capacity(8);
    for holder in [group, "stardelt-admins"] {
        for verb in ["browse", "select", "write", "create"] {
            grants.push(json!({ "group": holder, "verb": verb }));
        }
    }
    let mut spec = serde_json::Map::new();
    spec.insert("owner".into(), json!(group));
    spec.insert("lakehouseRef".into(), json!({ "name": lakehouse }));
    spec.insert("displayName".into(), json!(display_name));
    if let Some(description) = description {
        spec.insert("description".into(), json!(description));
    }
    spec.insert("grants".into(), Value::Array(grants));
    Value::Object(spec)
}

/// One `Catalog` resource of Nova's namespace, ready for a create.
fn catalog_resource(name: &str, spec: Value) -> DynamicObject {
    DynamicObject::new(name, &catalog_api_resource()).data(json!({ "spec": spec }))
}

/// The `Catalog` API resource, so a create can name its kind without a second `Api`.
fn catalog_api_resource() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk("stardelt.io", "v1alpha1", "Catalog"))
}

/// The `ServicePrincipal` resources of Nova's namespace (`pipelines.rs` reads the same
/// kind). Here only the team space creates one.
fn principals_api(client: kube::Client) -> Api<DynamicObject> {
    Api::namespaced_with(
        client,
        &crate::cluster::namespace(),
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "ServicePrincipal",
        )),
    )
}

/// One `pipeline-<group>` principal: the owner group and the `pipeline` purpose. The
/// audiences keep the CRD's default (both engines).
fn principal_resource(name: &str, group: &str) -> DynamicObject {
    DynamicObject::new(
        name,
        &ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "ServicePrincipal",
        )),
    )
    .data(json!({
        "spec": { "ownerGroup": group, "purpose": "pipeline" }
    }))
}

/// A real create: a name that already exists is an error, never an overwrite. The namespace
/// is Nova's own, so a team space never lands anywhere else.
async fn create_object(
    api: &Api<DynamicObject>,
    object: &DynamicObject,
) -> Result<(), kube::Error> {
    let mut object = object.clone();
    object.metadata.namespace = Some(crate::cluster::namespace());
    api.create(&PostParams::default(), &object)
        .await
        .map(|_| ())
}

/// The names created so far, for the 502 that says where the half-created space stops.
fn so_far(created: &[String]) -> String {
    if created.is_empty() {
        "none".to_string()
    } else {
        created.join(", ")
    }
}

/// The administrator the audit line names and the rate limit counts. In `oidc` mode the
/// principal is always there (`require_admin` ran); in `dev` mode the one operator is the
/// actor, with `sub` the configured dev user so the limiter has a stable key.
fn actor(principal: Option<axum::Extension<Principal>>, state: &AppState) -> Identity {
    match principal {
        Some(axum::Extension(principal)) => principal.identity,
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

/// The body rules the `Catalog` CRD (and its CEL rule) enforces, repeated as 400 sentences
/// so the dialog can show the same words before the request leaves the browser.
fn validate_access(update: &CatalogAccessUpdate) -> Result<(), Fail> {
    if update.owner.is_empty() {
        return Err(bad_request("the owner is one group name"));
    }
    if !valid_group(&update.owner) {
        return Err(bad_request(GROUP_SENTENCE));
    }
    if update.grants.len() > 50 {
        return Err(bad_request("at most 50 grants"));
    }
    let mut seen = HashSet::new();
    for grant in &update.grants {
        if !valid_group(&grant.group) {
            return Err(bad_request(GROUP_SENTENCE));
        }
        if !VERBS.contains(&grant.verb.as_str()) {
            return Err(bad_request(format!(
                "unknown verb {}; use browse, select, write, create or manage",
                grant.verb
            )));
        }
        if !seen.insert((grant.group.as_str(), grant.verb.as_str())) {
            return Err(bad_request(format!(
                "{} appears twice with {}",
                grant.group, grant.verb
            )));
        }
    }
    Ok(())
}

const GROUP_SENTENCE: &str =
    "a group name is letters, digits, dots, dashes and underscores (up to 64)";

/// The CRD's group rule: 1 to 64 characters, a leading letter or digit, then letters,
/// digits, dots, dashes and underscores.
fn valid_group(group: &str) -> bool {
    let bytes = group.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn bad_request(message: impl Into<String>) -> Fail {
    Fail::new(StatusCode::BAD_REQUEST, message)
}

fn no_such_catalog() -> Fail {
    Fail::new(StatusCode::NOT_FOUND, "no such catalog")
}

fn catalog_changed() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "this catalog changed since you opened it; reload",
    )
}

fn too_many_changes() -> Fail {
    Fail::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too many catalog changes this hour; wait",
    )
}

fn cluster_unavailable() -> Fail {
    Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
}

/// True when this administrator may make another change now. Old slots fall out of the
/// window; a refused attempt never reaches this.
fn take_access_change_slot(limiter: &AccessChangesLimiter, sub: &str) -> bool {
    take_slot(limiter, sub, ACCESS_CHANGE_LIMIT, ACCESS_CHANGE_WINDOW)
}

/// One slot of a per-administrator window, shared by both limits here: the catalog
/// changes (thirty an hour) and the team spaces (ten an hour).
fn take_slot(
    limiter: &Mutex<HashMap<String, VecDeque<Instant>>>,
    sub: &str,
    limit: usize,
    window: Duration,
) -> bool {
    let mut changes = limiter.lock().expect("catalog change limiter poisoned");
    let now = Instant::now();
    let slots = changes.entry(sub.to_string()).or_default();
    while slots
        .front()
        .is_some_and(|taken| now.duration_since(*taken) >= window)
    {
        slots.pop_front();
    }
    if slots.len() >= limit {
        return false;
    }
    slots.push_back(now);
    true
}

/// The change in memory, for a Nova with no cluster (demo mode). The demo lakehouse is one
/// catalog named `warehouse`, so any other name is a 404 exactly as in the cluster.
fn demo_update(
    state: &AppState,
    name: &str,
    update: &CatalogAccessUpdate,
    who: &Identity,
) -> Response {
    let mut demo = state.demo_state.write().expect("demo state poisoned");
    let mut row = demo.admin_catalog.clone().unwrap_or_else(demo_row);
    if row.name != name {
        return no_such_catalog().into_response();
    }
    let current_version = row
        .resource_version
        .clone()
        .unwrap_or_else(|| "1".to_string());
    if update.resource_version != current_version {
        return catalog_changed().into_response();
    }
    if !take_access_change_slot(&state.catalog_access_changes, &who.sub) {
        return too_many_changes().into_response();
    }

    let owner_before = row.owner.clone();
    let grants_before = row.grants.clone();
    let generation = row.generation.unwrap_or(1) + 1;
    row.owner = Some(update.owner.clone());
    row.managed_access = update.managed_access;
    row.grants = update.grants.clone();
    row.generation = Some(generation);
    row.observed_generation = Some(generation);
    row.resource_version = Some(next_resource_version(&current_version));
    row.phase = "Ready".to_string();
    row.conditions = demo_conditions();
    demo.admin_catalog = Some(row.clone());

    audit_change(
        who,
        name,
        owner_before.as_deref(),
        &update.owner,
        &grants_before,
        &update.grants,
    );
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(row),
    )
        .into_response()
}

fn next_resource_version(current: &str) -> String {
    current
        .parse::<u64>()
        .map(|version| (version + 1).to_string())
        .unwrap_or_else(|_| "2".to_string())
}

/// The audit line for one change: who, which catalog, the owner before and after, and the
/// added and removed `(group, verb)` pairs. Group names and verbs are permissions, not
/// secrets; no body, description or credential can reach the record.
fn audit_change(
    who: &Identity,
    catalog: &str,
    owner_before: Option<&str>,
    owner_after: &str,
    before: &[CatalogGrant],
    after: &[CatalogGrant],
) {
    let before_pairs: std::collections::BTreeSet<(String, String)> = before
        .iter()
        .map(|grant| (grant.group.clone(), grant.verb.clone()))
        .collect();
    let after_pairs: std::collections::BTreeSet<(String, String)> = after
        .iter()
        .map(|grant| (grant.group.clone(), grant.verb.clone()))
        .collect();
    let added: Vec<(String, String)> = after_pairs.difference(&before_pairs).cloned().collect();
    let removed: Vec<(String, String)> = before_pairs.difference(&after_pairs).cloned().collect();
    let unchanged = before_pairs.intersection(&after_pairs).count();
    crate::audit::catalog_access_changed(
        who,
        catalog,
        owner_before,
        owner_after,
        &added,
        &removed,
        unchanged,
    );
}

/// The rows the page shows: from the cluster, or the demo one when Nova has no
/// cluster client (demo mode, or a local run outside a cluster).
async fn catalog_rows(state: &AppState) -> Result<Vec<AdminCatalog>, Fail> {
    let Some(client) = state.kube.clone() else {
        return Ok(vec![demo_row_for(state)]);
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
    sort_by_layer(&mut rows);
    Ok(rows)
}

/// The `Catalog` resources of Nova's namespace, through the cluster client. Also what the
/// catalog-layer reader lists (`catalog::catalog_layers`).
pub(crate) fn catalogs_api(client: kube::Client) -> Api<DynamicObject> {
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
pub(crate) fn row_from_object(object: &Value) -> AdminCatalog {
    let spec = object.get("spec").unwrap_or(&Value::Null);
    let status = object
        .get("status")
        .filter(|status| !status.is_null())
        .unwrap_or(&Value::Null);
    let metadata = object.get("metadata").unwrap_or(&Value::Null);

    AdminCatalog {
        name: text(metadata.get("name")).unwrap_or_default(),
        layer: crate::catalog::layer_of_object(object),
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
        managed_by: gitops_owner(metadata),
        grants: grants_of(spec),
        conditions: conditions_of(status),
    }
}

/// The GitOps tool that owns a `Catalog`, read from its `metadata`, or `None` when Nova is
/// the only writer. `Argo CD` when the resource carries its tracking id, or its instance
/// label beside one of its sync or compare options; `Flux` when a kustomize or helm
/// controller label names it. A tool that owns the resource puts the old grants back
/// minutes after a Nova edit, so the page says who owns it and the write is refused.
pub fn gitops_owner(metadata: &Value) -> Option<String> {
    let annotations = metadata.get("annotations");
    let labels = metadata.get("labels");
    let has_annotation = |key: &str| annotations.is_some_and(|a| a.get(key).is_some());
    let has_label = |key: &str| labels.is_some_and(|l| l.get(key).is_some());

    if has_annotation("argocd.argoproj.io/tracking-id")
        || (has_label("app.kubernetes.io/instance")
            && (has_annotation("argocd.argoproj.io/sync-options")
                || has_annotation("argocd.argoproj.io/compare-options")))
    {
        return Some("Argo CD".to_string());
    }
    if has_label("kustomize.toolkit.fluxcd.io/name") || has_label("helm.toolkit.fluxcd.io/name") {
        return Some("Flux".to_string());
    }
    None
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

fn sort_by_layer(rows: &mut [AdminCatalog]) {
    crate::catalog::sort_by_layer(rows, |row| row.layer.as_deref(), |row| row.name.as_str());
}

/// What a Nova without a cluster client answers: the in-memory demo lakehouse,
/// which is the one catalog `GET /api/catalog/catalogs` lists in demo mode.
/// It is `Ready` by construction — there is no `Catalog` resource and no sync
/// job behind it. The generations start at 1 so an edit through the dialog can
/// poll the same way it does against a cluster; a change is kept in
/// `demo_state.admin_catalog`.
fn demo_row_for(state: &AppState) -> AdminCatalog {
    state
        .demo_state
        .read()
        .expect("demo state poisoned")
        .admin_catalog
        .clone()
        .unwrap_or_else(demo_row)
}

fn demo_row() -> AdminCatalog {
    AdminCatalog {
        name: "warehouse".to_string(),
        // The demo catalog is `warehouse`: not one of the three layer names.
        layer: None,
        display_name: None,
        description: None,
        owner: Some("stardelt-admins".to_string()),
        managed_access: false,
        key_prefix: None,
        phase: "Ready".to_string(),
        warehouse_id: None,
        generation: Some(1),
        observed_generation: Some(1),
        resource_version: Some("1".to_string()),
        managed_by: None,
        grants: ["browse", "select", "write", "create"]
            .into_iter()
            .map(|verb| CatalogGrant {
                group: "stardelt-users".to_string(),
                verb: verb.to_string(),
            })
            .collect(),
        conditions: Vec::new(),
    }
}

/// The conditions a demo edit reports: there is no sync job, so the change is applied by
/// construction. `GrantsReady` carries the same shape the page reads from a cluster.
fn demo_conditions() -> Vec<CatalogCondition> {
    [
        (
            "CatalogReady",
            "True",
            "Ready",
            "catalog and its grants are ready",
        ),
        ("GrantsReady", "True", "Ready", "grants match the resource"),
        ("Drift", "False", "Clean", "no foreign grants"),
    ]
    .into_iter()
    .map(|(r#type, status, reason, message)| CatalogCondition {
        r#type: r#type.to_string(),
        status: status.to_string(),
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    })
    .collect()
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

    /// A row whose `Catalog` object carries the `stardelt.io/layer` label.
    fn layered(name: &str, layer: &str) -> AdminCatalog {
        let mut object = catalog_object(name, Some("Ready"));
        object["metadata"]["labels"] = json!({ crate::catalog::LAYER_LABEL: layer });
        row_from_object(&object)
    }

    #[test]
    fn maps_a_catalog_object_to_a_row() {
        let mapped = row_from_object(&catalog_object("finance", Some("Ready")));
        assert_eq!(
            mapped,
            AdminCatalog {
                name: "finance".into(),
                layer: None,
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
                managed_by: None,
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
    fn rows_sorted_by_layer_then_name() {
        // `warehouse` is labelled bronze, `gold` is gold by its name, `silver` by its name;
        // `product` and `shared` carry no layer.
        let mut rows = vec![
            row("product"),
            layered("gold", "gold"),
            row("shared"),
            layered("warehouse", "bronze"),
            row("silver"),
        ];
        sort_by_layer(&mut rows);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["warehouse", "silver", "gold", "product", "shared"]);
        assert_eq!(rows[0].layer.as_deref(), Some("bronze"));
        assert_eq!(rows[2].layer.as_deref(), Some("gold"));
        assert_eq!(rows[3].layer, None);
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
    /// detail (the API server's own message) must not reach the browser. The same fake
    /// serves one catalog (GET) and records a merge patch (PATCH) for the write test.
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
            pub principals: Vec<Value>,
            pub status: StatusCode,
            pub patch_status: StatusCode,
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
                    "reason": if code == StatusCode::CONFLICT { "Conflict" } else { "Forbidden" },
                    "code": code.as_u16()
                })),
            )
        }

        async fn list(
            State(state): State<Shared>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let state = state.lock().unwrap();
            if state.status != StatusCode::OK {
                return Err(failure(
                    state.status,
                    "catalogs.stardelt.io is forbidden: User \"system:serviceaccount:stardelt:nova\" cannot list resource \"catalogs\"",
                ));
            }
            Ok(Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "CatalogList",
                "metadata": { "resourceVersion": "1" },
                "items": state.items
            })))
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
                .ok_or_else(|| {
                    failure(
                        StatusCode::NOT_FOUND,
                        "catalogs.stardelt.io \"x\" not found",
                    )
                })
        }

        async fn patch_one(
            State(state): State<Shared>,
            Path((_namespace, name)): Path<(String, String)>,
            body: String,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let body: Value = serde_json::from_str(&body)
                .map_err(|_| failure(StatusCode::BAD_REQUEST, "the patch is not JSON"))?;
            let mut state = state.lock().unwrap();
            state.patches.push(body.clone());
            if state.patch_status != StatusCode::OK {
                return Err(failure(
                    state.patch_status,
                    "Operation cannot be fulfilled on catalogs.stardelt.io \"finance\": the object has been modified",
                ));
            }
            let item = state
                .items
                .iter_mut()
                .find(|item| item["metadata"]["name"] == name)
                .ok_or_else(|| failure(StatusCode::NOT_FOUND, "not found"))?;
            if let Some(spec) = body.get("spec") {
                item["spec"] = spec.clone();
            }
            let generation = item["metadata"]["generation"].as_i64().unwrap_or(0) + 1;
            item["metadata"]["generation"] = json!(generation);
            item["metadata"]["resourceVersion"] = json!("4212");
            Ok(Json(item.clone()))
        }

        async fn create_catalog(
            State(state): State<Shared>,
            Path(namespace): Path<String>,
            body: String,
        ) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
            create_in(state, namespace, &body, true)
        }

        async fn create_principal(
            State(state): State<Shared>,
            Path(namespace): Path<String>,
            body: String,
        ) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
            create_in(state, namespace, &body, false)
        }

        /// A real create of the fake API: a name that exists is a `409`, and the stored
        /// object carries the namespace and a `resourceVersion`.
        fn create_in(
            state: Shared,
            namespace: String,
            body: &str,
            catalog: bool,
        ) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
            let mut object: Value = serde_json::from_str(body)
                .map_err(|_| failure(StatusCode::BAD_REQUEST, "the object is not JSON"))?;
            let name = object["metadata"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                return Err(failure(StatusCode::BAD_REQUEST, "the object has no name"));
            }
            let mut state = state.lock().unwrap();
            let list = if catalog {
                &mut state.items
            } else {
                &mut state.principals
            };
            if list.iter().any(|item| item["metadata"]["name"] == name) {
                return Err(failure(
                    StatusCode::CONFLICT,
                    &format!("{name} already exists"),
                ));
            }
            object["metadata"]["namespace"] = json!(namespace);
            object["metadata"]["resourceVersion"] = json!("500");
            list.push(object.clone());
            Ok((StatusCode::CREATED, Json(object)))
        }

        async fn list_principals(State(state): State<Shared>) -> Json<Value> {
            let state = state.lock().unwrap();
            Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "ServicePrincipalList",
                "metadata": { "resourceVersion": "1" },
                "items": state.principals
            }))
        }

        pub async fn start(items: Vec<Value>, status: StatusCode) -> kube::Client {
            start_with_patch(items, status, StatusCode::OK).await.0
        }

        /// A fake with no cluster error: the `Catalog` list and the creates both answer, and
        /// `principals` is what the namespace already holds (`ServicePrincipal`).
        pub async fn start_with_principals(
            items: Vec<Value>,
            principals: Vec<Value>,
        ) -> (kube::Client, Shared) {
            start_all(items, principals, StatusCode::OK, StatusCode::OK).await
        }

        pub async fn start_with_patch(
            items: Vec<Value>,
            status: StatusCode,
            patch_status: StatusCode,
        ) -> (kube::Client, Shared) {
            start_all(items, Vec::new(), status, patch_status).await
        }

        async fn start_all(
            items: Vec<Value>,
            principals: Vec<Value>,
            status: StatusCode,
            patch_status: StatusCode,
        ) -> (kube::Client, Shared) {
            let shared = Arc::new(Mutex::new(Fake {
                items,
                principals,
                status,
                patch_status,
                patches: Vec::new(),
            }));
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/catalogs",
                    get(list).post(create_catalog),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/catalogs/{name}",
                    get(one).patch(patch_one),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/serviceprincipals",
                    get(list_principals).post(create_principal),
                )
                .with_state(shared.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let url = format!("http://{addr}").parse().unwrap();
            let client =
                kube::Client::try_from(kube::Config::new(url)).expect("a client for the fake API");
            (client, shared)
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

    fn wanted_update() -> CatalogAccessUpdate {
        CatalogAccessUpdate {
            resource_version: "4211".into(),
            owner: "stardelt-finops".into(),
            managed_access: false,
            grants: vec![
                CatalogGrant {
                    group: "stardelt-finops".into(),
                    verb: "browse".into(),
                },
                CatalogGrant {
                    group: "stardelt-analytics".into(),
                    verb: "browse".into(),
                },
            ],
        }
    }

    fn access_request(name: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(format!("/admin/catalogs/{name}/access"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn non_admin_403() {
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

        async fn status_for(
            identity: Option<crate::auth::Identity>,
            body: &Value,
        ) -> (StatusCode, String) {
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
                .oneshot(access_request("warehouse", body))
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, String::from_utf8_lossy(&body).into_owned())
        }

        let body = json!({
            "resourceVersion": "1",
            "owner": "stardelt-finops",
            "managedAccess": false,
            "grants": [{ "group": "stardelt-users", "verb": "browse" }]
        });

        let (status, refused) = status_for(Some(identity(false)), &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(refused.contains("this view is for platform administrators"));

        let (status, _) = status_for(None, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // An administrator reaches the handler; in demo mode it changes memory.
        let (status, answer) = status_for(Some(identity(true)), &body).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert!(answer.contains("\"warehouse\""));
    }

    #[tokio::test]
    async fn merge_patch_body_exact_with_resource_version() {
        let (client, fake) = fake_kube::start_with_patch(
            vec![catalog_object("finance", Some("Ready"))],
            StatusCode::OK,
            StatusCode::OK,
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        let update = wanted_update();
        let response = update_access(
            State(state),
            Path("finance".to_string()),
            None,
            ApiJson(update.clone()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let after: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(after["name"], "finance");
        assert_eq!(
            after["generation"], 8,
            "the new generation from the API server"
        );

        let patches = fake.lock().unwrap().patches.clone();
        assert_eq!(
            patches,
            vec![json!({
                "metadata": { "resourceVersion": "4211" },
                "spec": {
                    "owner": "stardelt-finops",
                    "managedAccess": false,
                    "grants": [
                        { "group": "stardelt-finops", "verb": "browse" },
                        { "group": "stardelt-analytics", "verb": "browse" }
                    ]
                }
            })]
        );
    }

    #[test]
    fn gitops_owner_from_argo_flux_or_none() {
        assert_eq!(
            gitops_owner(&json!({
                "annotations": {
                    "argocd.argoproj.io/tracking-id": "test:stardelt.io/Catalog:stardelt/retailref"
                }
            })),
            Some("Argo CD".to_string())
        );
        // The other Argo CD shape: the instance label beside a sync or compare option.
        assert_eq!(
            gitops_owner(&json!({
                "labels": { "app.kubernetes.io/instance": "apps" },
                "annotations": { "argocd.argoproj.io/sync-options": "Prune=false" }
            })),
            Some("Argo CD".to_string())
        );
        assert_eq!(
            gitops_owner(&json!({
                "labels": {
                    "kustomize.toolkit.fluxcd.io/name": "apps",
                    "kustomize.toolkit.fluxcd.io/namespace": "flux-system"
                }
            })),
            Some("Flux".to_string())
        );
        assert_eq!(
            gitops_owner(&json!({ "labels": { "helm.toolkit.fluxcd.io/name": "charts" } })),
            Some("Flux".to_string())
        );
        assert_eq!(gitops_owner(&json!({ "name": "finance" })), None);
        // An instance label alone is not an owner: many Deployments carry one.
        assert_eq!(
            gitops_owner(&json!({ "labels": { "app.kubernetes.io/instance": "nova" } })),
            None
        );

        // The row carries the tool, so the page can say who owns the catalog.
        let mut object = catalog_object("finance", Some("Ready"));
        object["metadata"]["annotations"] = json!({
            "argocd.argoproj.io/tracking-id": "test:stardelt.io/Catalog:stardelt/finance"
        });
        assert_eq!(
            row_from_object(&object).managed_by.as_deref(),
            Some("Argo CD")
        );
    }

    #[tokio::test]
    async fn gitops_owned_catalog_is_409_and_never_patched() {
        let mut object = catalog_object("finance", Some("Ready"));
        object["metadata"]["annotations"] = json!({
            "argocd.argoproj.io/tracking-id": "test:stardelt.io/Catalog:stardelt/finance"
        });
        let (client, fake) =
            fake_kube::start_with_patch(vec![object], StatusCode::OK, StatusCode::OK).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = update_access(
            State(state),
            Path("finance".to_string()),
            None,
            ApiJson(wanted_update()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("finance is managed by Argo CD: change its grants in Git"),
            "{body}"
        );
        assert!(
            fake.lock().unwrap().patches.is_empty(),
            "a managed catalog is never patched"
        );
    }

    #[test]
    fn invalid_group_verb_duplicate_rejected() {
        fn message(update: &CatalogAccessUpdate) -> (StatusCode, String) {
            let fail = validate_access(update).expect_err("refused");
            (fail.status(), fail.message().to_string())
        }

        let mut update = wanted_update();
        update.owner = String::new();
        assert_eq!(
            message(&update),
            (
                StatusCode::BAD_REQUEST,
                "the owner is one group name".into()
            )
        );

        update.owner = "bad group".into();
        assert_eq!(
            message(&update),
            (StatusCode::BAD_REQUEST, GROUP_SENTENCE.into())
        );

        let mut update = wanted_update();
        update.grants[0].group = "bad group".into();
        assert_eq!(
            message(&update),
            (StatusCode::BAD_REQUEST, GROUP_SENTENCE.into())
        );

        let mut update = wanted_update();
        update.grants[0].verb = "use".into();
        assert_eq!(
            message(&update),
            (
                StatusCode::BAD_REQUEST,
                "unknown verb use; use browse, select, write, create or manage".into()
            )
        );

        let mut update = wanted_update();
        update.grants = vec![
            CatalogGrant {
                group: "stardelt-users".into(),
                verb: "browse".into(),
            },
            CatalogGrant {
                group: "stardelt-users".into(),
                verb: "browse".into(),
            },
        ];
        assert_eq!(
            message(&update),
            (
                StatusCode::BAD_REQUEST,
                "stardelt-users appears twice with browse".into()
            )
        );

        let mut update = wanted_update();
        update.grants = (0..51)
            .map(|index| CatalogGrant {
                group: format!("group{index}"),
                verb: "browse".into(),
            })
            .collect();
        assert_eq!(
            message(&update),
            (StatusCode::BAD_REQUEST, "at most 50 grants".into())
        );
    }

    #[tokio::test]
    async fn conflict_maps_to_409() {
        let (client, fake) = fake_kube::start_with_patch(
            vec![catalog_object("finance", Some("Ready"))],
            StatusCode::OK,
            StatusCode::CONFLICT,
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = update_access(
            State(state),
            Path("finance".to_string()),
            None,
            ApiJson(wanted_update()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("this catalog changed since you opened it; reload"),
            "{body}"
        );
        // The API server's own words (and the object name) stay out of the answer.
        assert!(!body.contains("Operation cannot be fulfilled"), "{body}");
        assert_eq!(fake.lock().unwrap().patches.len(), 1);
    }

    #[tokio::test]
    async fn rate_limit_429() {
        let (client, fake) = fake_kube::start_with_patch(
            vec![catalog_object("finance", Some("Ready"))],
            StatusCode::OK,
            StatusCode::OK,
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        for _ in 0..ACCESS_CHANGE_LIMIT {
            let response = update_access(
                State(state.clone()),
                Path("finance".to_string()),
                None,
                ApiJson(wanted_update()),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response = update_access(
            State(state),
            Path("finance".to_string()),
            None,
            ApiJson(wanted_update()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("too many catalog changes this hour; wait")
        );
        assert_eq!(
            fake.lock().unwrap().patches.len(),
            ACCESS_CHANGE_LIMIT,
            "the refused attempt never reached the API server"
        );
    }

    #[tokio::test]
    async fn audit_has_pairs_and_no_body() {
        use crate::auth::{Identity, Principal};

        const CANARY: &str = "canary-7f3a-DO-NOT-LEAK";
        let state = crate::router_tests::test_state();
        let app = axum::Router::new()
            .route(
                "/admin/catalogs/{name}/access",
                axum::routing::put(update_access),
            )
            .layer(axum::middleware::from_fn(
                |mut req: Request<Body>, next: axum::middleware::Next| async move {
                    req.extensions_mut().insert(Principal {
                        identity: Identity {
                            sub: "catalog-audit-test".into(),
                            display_name: "ada".into(),
                            email: None,
                            name: None,
                            groups: vec![],
                            roles: vec![],
                            groups_overage: false,
                            is_admin: true,
                        },
                        via: "session",
                        access_token: None,
                    });
                    next.run(req).await
                },
            ))
            .with_state(state);
        let body = json!({
            "resourceVersion": "1",
            "owner": "stardelt-finops",
            "managedAccess": true,
            "grants": [
                { "group": "stardelt-analytics", "verb": "select" },
                { "group": "stardelt-finops", "verb": "browse" }
            ],
            // An unknown field is not part of the change; the audit must not copy it.
            "description": CANARY
        });
        let response = app
            .oneshot(access_request("warehouse", &body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let records = crate::audit::catalog_test_sink::take_for("catalog-audit-test");
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.catalog, "warehouse");
        assert_eq!(record.owner_before.as_deref(), Some("stardelt-admins"));
        assert_eq!(record.owner_after, "stardelt-finops");
        assert_eq!(
            record.added,
            vec![
                ("stardelt-analytics".into(), "select".into()),
                ("stardelt-finops".into(), "browse".into())
            ]
        );
        assert_eq!(
            record.removed,
            ["browse", "create", "select", "write"]
                .into_iter()
                .map(|verb| ("stardelt-users".to_string(), verb.to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(record.unchanged, 0);
        assert_eq!(record.actor, "ada");
        assert!(!format!("{record:?}").contains(CANARY));
    }

    #[tokio::test]
    async fn demo_mode_roundtrip() {
        let state = crate::router_tests::test_state();
        let update = CatalogAccessUpdate {
            resource_version: "1".into(),
            owner: "stardelt-finops".into(),
            managed_access: true,
            grants: vec![CatalogGrant {
                group: "stardelt-analytics".into(),
                verb: "select".into(),
            }],
        };
        let response = update_access(
            State(state.clone()),
            Path("warehouse".to_string()),
            None,
            ApiJson(update),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let row: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(row["owner"], "stardelt-finops");
        assert_eq!(row["managedAccess"], true);
        assert_eq!(row["grants"][0]["group"], "stardelt-analytics");
        assert_eq!(row["generation"], 2);
        assert_eq!(row["observedGeneration"], 2);

        // The change is what the list now answers, not a one-request echo.
        let rows = catalog_rows(&state).await.expect("the demo row");
        assert_eq!(rows[0].owner.as_deref(), Some("stardelt-finops"));
        assert_eq!(
            rows[0].grants,
            vec![CatalogGrant {
                group: "stardelt-analytics".into(),
                verb: "select".into()
            }]
        );

        // The page's old resourceVersion is refused rather than overwriting.
        let stale = CatalogAccessUpdate {
            resource_version: "1".into(),
            owner: "stardelt-users".into(),
            managed_access: false,
            grants: vec![],
        };
        let response = update_access(
            State(state.clone()),
            Path("warehouse".to_string()),
            None,
            ApiJson(stale),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        // A real run without a cluster is a 503, never a panic or a memory change.
        let no_cluster = crate::router_tests::catalog_test_state("http://127.0.0.1:1", None);
        let response = update_access(
            State(no_cluster),
            Path("warehouse".to_string()),
            None,
            ApiJson(wanted_update()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("the cluster is not reachable from Nova"));
    }

    #[tokio::test]
    async fn unknown_catalog_is_404_without_a_cluster_call() {
        let state = crate::router_tests::test_state();
        let response = update_access(
            State(state),
            Path("System".to_string()),
            None,
            ApiJson(wanted_update()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The `POST /api/admin/teams` request body.
    fn new_team(team: &str, group: &str) -> NewTeam {
        NewTeam {
            team: team.into(),
            group: group.into(),
            description: Some("Energy trading: consumption, prices and forecasts.".into()),
        }
    }

    fn principal_object(name: &str, group: &str) -> Value {
        json!({
            "apiVersion": "stardelt.io/v1alpha1",
            "kind": "ServicePrincipal",
            "metadata": { "name": name, "namespace": "stardelt", "resourceVersion": "77" },
            "spec": { "ownerGroup": group, "purpose": "pipeline" }
        })
    }

    /// One `Catalog` of the fake, by name.
    fn stored_catalog(fake: &fake_kube::Fake, name: &str) -> Value {
        fake.items
            .iter()
            .find(|item| item["metadata"]["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no catalog {name} was created"))
    }

    #[tokio::test]
    async fn create_team_creates_three_catalogs_and_one_principal() {
        let (client, fake) = fake_kube::start_with_principals(
            vec![catalog_object("warehouse", Some("Ready"))],
            Vec::new(),
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = create_team(
            State(state),
            None,
            ApiJson(new_team("energy", "energy-analytics")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let answer: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            answer,
            json!({
                "catalogs": ["energy", "energylab", "energyref"],
                "principal": "pipeline-energy-analytics",
                "principalCreated": true
            })
        );

        let fake = fake.lock().unwrap();
        let names: Vec<&str> = fake
            .items
            .iter()
            .filter_map(|item| item["metadata"]["name"].as_str())
            .collect();
        assert_eq!(names.len(), 4, "the warehouse plus the three: {names:?}");

        let production = stored_catalog(&fake, "energy");
        assert_eq!(production["spec"]["owner"], "energy-analytics");
        assert_eq!(production["spec"]["lakehouseRef"]["name"], "default");
        assert_eq!(production["spec"]["displayName"], "Energy");
        assert_eq!(
            production["spec"]["description"],
            "Energy trading: consumption, prices and forecasts."
        );
        assert!(
            production["spec"].get("storage").is_none(),
            "no spec.storage: the lakehouse's defaults"
        );
        let grants = production["spec"]["grants"].as_array().unwrap();
        assert_eq!(grants.len(), 8);
        assert_eq!(
            grants[0],
            json!({ "group": "energy-analytics", "verb": "browse" })
        );
        assert_eq!(
            grants[3],
            json!({ "group": "energy-analytics", "verb": "create" })
        );
        assert_eq!(
            grants[4],
            json!({ "group": "stardelt-admins", "verb": "browse" })
        );
        assert_eq!(
            grants[7],
            json!({ "group": "stardelt-admins", "verb": "create" })
        );

        let lab = stored_catalog(&fake, "energylab");
        assert_eq!(lab["spec"]["displayName"], "Energy lab");
        assert_eq!(lab["spec"]["description"], "Work in progress for energy.");
        assert_eq!(lab["spec"]["owner"], "energy-analytics");

        let reference = stored_catalog(&fake, "energyref");
        assert_eq!(reference["spec"]["displayName"], "Energy reference");
        assert_eq!(
            reference["spec"]["description"],
            "Shared reference data for energy."
        );

        assert_eq!(fake.principals.len(), 1);
        assert_eq!(
            fake.principals[0]["metadata"]["name"],
            "pipeline-energy-analytics"
        );
        assert_eq!(fake.principals[0]["spec"]["ownerGroup"], "energy-analytics");
        assert_eq!(fake.principals[0]["spec"]["purpose"], "pipeline");
    }

    #[tokio::test]
    async fn create_team_keeps_an_existing_principal() {
        let (client, fake) = fake_kube::start_with_principals(
            vec![catalog_object("warehouse", Some("Ready"))],
            vec![principal_object(
                "pipeline-energy-analytics",
                "energy-analytics",
            )],
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = create_team(
            State(state),
            None,
            ApiJson(new_team("energy", "energy-analytics")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let answer: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(answer["principalCreated"], false);

        // The principal that was already there is untouched, and no second one was created.
        let fake = fake.lock().unwrap();
        assert_eq!(fake.principals.len(), 1);
        assert_eq!(fake.principals[0]["metadata"]["resourceVersion"], "77");
        assert_eq!(fake.items.len(), 4);
    }

    #[tokio::test]
    async fn create_team_refuses_a_taken_catalog_and_an_empty_platform() {
        // `energylab` exists: the whole team space is refused, nothing is created.
        let (client, fake) = fake_kube::start_with_principals(
            vec![
                catalog_object("warehouse", Some("Ready")),
                catalog_object("energylab", Some("Ready")),
            ],
            Vec::new(),
        )
        .await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = create_team(
            State(state),
            None,
            ApiJson(new_team("energy", "energy-analytics")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("the catalog energylab already exists"),
            "{}",
            String::from_utf8_lossy(&body)
        );
        {
            let fake = fake.lock().unwrap();
            assert_eq!(fake.items.len(), 2, "nothing was created");
            assert!(fake.principals.is_empty(), "no principal either");
        }

        // No `Catalog` at all: there is no lakehouse to put the three in.
        let (client, fake) = fake_kube::start_with_principals(Vec::new(), Vec::new()).await;
        let state = crate::router_tests::test_state_with_kube(client);
        let response = create_team(
            State(state),
            None,
            ApiJson(new_team("energy", "energy-analytics")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("no lakehouse to put the catalogs in"),
            "{}",
            String::from_utf8_lossy(&body)
        );
        let fake = fake.lock().unwrap();
        assert!(fake.items.is_empty());
        assert!(fake.principals.is_empty());
    }

    #[tokio::test]
    async fn create_team_rejects_a_bad_team_name() {
        for team in ["Energy", "e", "1energy", "energy-1", "energy space"] {
            let fail = validate_team(&new_team(team, "energy-analytics")).expect_err("refused");
            assert_eq!(
                (fail.status(), fail.message()),
                (
                    StatusCode::BAD_REQUEST,
                    "a team name is 2 to 21 lower-case letters and digits, starting with a letter"
                ),
                "{team}"
            );
        }
        assert!(validate_team(&new_team("energy", "energy-analytics")).is_ok());

        // Long, and answered before any cluster call (this state has no cluster client).
        let too_long = "a".repeat(22);
        let state = crate::router_tests::test_state();
        let response = create_team(
            State(state),
            None,
            ApiJson(new_team(&too_long, "energy-analytics")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // The group rule and the description length are answered too.
        let fail = validate_team(&NewTeam {
            team: "energy".into(),
            group: "bad group".into(),
            description: None,
        })
        .expect_err("refused");
        assert_eq!(
            (fail.status(), fail.message()),
            (StatusCode::BAD_REQUEST, GROUP_SENTENCE)
        );
        let fail = validate_team(&NewTeam {
            team: "energy".into(),
            group: "energy-analytics".into(),
            description: Some("x".repeat(501)),
        })
        .expect_err("refused");
        assert_eq!(
            (fail.status(), fail.message()),
            (
                StatusCode::BAD_REQUEST,
                "the description is at most 500 characters"
            )
        );
    }

    #[tokio::test]
    async fn create_team_is_admin_only() {
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
                        .method("POST")
                        .uri("/admin/teams")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({ "team": "energy", "group": "energy-analytics" }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, String::from_utf8_lossy(&body).into_owned())
        }

        let (status, refused) = status_for(Some(identity(false))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(refused.contains("this view is for platform administrators"));

        // No principal is refused too (the dev router has no check, oidc always has one).
        let (status, _) = status_for(None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
