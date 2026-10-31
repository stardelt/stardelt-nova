//! Lakekeeper catalog proxy, with an in-memory demo branch (`NOVA_DEMO=1`).
//!
//! Lakekeeper's Iceberg REST routes are prefixed by the warehouse UUID, not
//! the warehouse name. Nova maps catalog names to warehouse ids from the warehouse
//! list (`warehouse_id_for`) and caches that map.
//!
//! Every request to Lakekeeper carries Nova's own service token
//! (`catalog_token.rs`, stardelt-nova#222) when the platform configured the catalog
//! client. The caller's token is **not** forwarded to the catalog: the catalog sees one
//! identity, Nova's. **Which catalogs a person may browse is asked of Trino with that
//! person's own token** (`allowed_catalogs`): the engine's `SHOW CATALOGS` is filtered by
//! the platform's permission bridge, so Nova never re-implements the rule
//! (`design/multitenant-catalogs.md` §5.4). Nova is not the authority; it only fails
//! before the person tries.
//!
//! **The medallion layer.** The lakehouse keeps one catalog per layer — `bronze`, `silver`,
//! `gold` — next to the catalogs that carry none (stardelt-nova#109). The layer is the
//! `stardelt.io/layer` label on the `Catalog` resource, else the catalog's own name when it
//! is one of the three ([`catalog_layers`], cached for 60 s); the catalog list, the
//! Overview metrics and the administrator page all carry it and put bronze, silver, gold
//! first, then the rest alphabetically.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tracing::warn;

use crate::{
    AppState,
    api_json::ApiJson,
    auth::{self, Principal},
    data,
    trino::error_status,
};

/// Catalogs the engine always has and Nova never offers: `system`, the connectors'
/// information schemas, and the in-memory and TPC connectors. They answer `SHOW CATALOGS`
/// but are not lakehouse catalogs a person browses.
pub const HIDDEN_CATALOGS: [&str; 6] = [
    "system",
    "information_schema",
    "memory",
    "tpch",
    "tpcds",
    "jmx",
];

/// How long one person's catalog visibility is trusted before Trino is asked again.
const ACCESS_TTL: Duration = Duration::from_secs(30);

/// The medallion layers, in the order the platform reads them (`bronze` → `silver` →
/// `gold`). One catalog per layer, next to the catalogs that carry none
/// (stardelt-nova#109, `design/data-engineer-workflow.md`).
pub const LAYERS: [&str; 3] = ["bronze", "silver", "gold"];

/// The label on a `Catalog` resource that names its medallion layer. Labels are not
/// sensitive: the value is the same string this API answers for everyone.
pub const LAYER_LABEL: &str = "stardelt.io/layer";

/// How long a catalog's layer is trusted before the `Catalog` objects are listed again.
/// A label added or removed with `kubectl` shows up within a minute.
const LAYERS_TTL: Duration = Duration::from_secs(60);

/// The refusal when no catalog is open to a person. One sentence, shared with every route
/// that has to say it (`catalog_scope`, `catalog_metrics`), so the UI can match on it.
pub(crate) const NO_CATALOG_SENTENCE: &str =
    "no catalog is open to you yet; ask an administrator for access";

/// The `?catalog=` of every catalog route. Omitted means the default catalog: the
/// configured `NOVA_WAREHOUSE_NAME` when the caller may browse it, else the first one
/// they may.
#[derive(Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CatalogParam {
    /// Catalog to work in (`warehouse`, `finance`, …). Omitted, the default catalog is used.
    pub catalog: Option<String>,
}

/// OpenAPI shapes for the catalog routes (first slice of ticket #107).
///
/// Nova proxies these bodies — from Lakekeeper upstream, or from the
/// in-memory demo shapes in `demo.rs` — so the structs below mirror the
/// proxied Iceberg REST shapes rather than being constructed anywhere.
/// `#[schema(rename = …)]` keeps the hyphenated Iceberg field names.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct WarehouseInfo {
    /// The configured warehouse name (`NOVA_WAREHOUSE_NAME`).
    pub name: String,
    /// The Lakekeeper warehouse id, or `"demo"` in demo mode.
    pub id: String,
}

/// Iceberg `ListNamespaces` result: each namespace is a list of levels.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct NamespacesResponse {
    pub namespaces: Vec<Vec<String>>,
}

/// One Iceberg table identifier.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct TableIdentifier {
    pub namespace: Vec<String>,
    pub name: String,
}

/// Iceberg `ListTables` result.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct TablesResponse {
    pub identifiers: Vec<TableIdentifier>,
}

/// Iceberg `LoadTable` result: where the metadata lives plus the metadata
/// document itself (schema, partition specs, properties). The metadata body
/// is the catalog's own document — Nova never reshapes it.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct TableMetadataResponse {
    #[schema(rename = "metadata-location")]
    pub metadata_location: String,
    pub metadata: serde_json::Value,
}

/// The created schema's name.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct CreateNamespaceResponse {
    pub namespace: String,
}

/// One catalog the caller may browse, with its Lakekeeper warehouse id and its medallion
/// layer when it has one.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct CatalogInfo {
    /// Catalog name, as the engine and the catalog spell it (`warehouse`, `finance`, …).
    pub name: String,
    /// The Lakekeeper warehouse id behind that name.
    pub id: String,
    /// `bronze`, `silver` or `gold` when the catalog is a medallion layer — from the
    /// `stardelt.io/layer` label on its `Catalog` resource, else from its name. Absent for
    /// every other catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

/// `GET /api/catalog/catalogs`: the catalogs the caller may browse, the medallion layers
/// first.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct CatalogsResponse {
    /// Every catalog the caller may browse (`?catalog=` accepts any of these names),
    /// ordered bronze, silver, gold, then the rest alphabetically.
    pub catalogs: Vec<CatalogInfo>,
    /// The catalog a request without `?catalog=` works in.
    pub default: String,
}

#[derive(Deserialize)]
pub(crate) struct WarehouseListItem {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Deserialize)]
struct WarehouseList {
    warehouses: Vec<WarehouseListItem>,
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct CreateNamespaceRequest {
    /// Schema name to create (surrounding whitespace is trimmed).
    pub namespace: String,
}

/// Why the warehouse id could not be resolved.
#[derive(Debug)]
pub(crate) enum WarehouseError {
    /// Nova's own service token could not be obtained (`catalog_token.rs`): the token
    /// endpoint is the problem, not the catalog.
    Token(crate::catalog_token::TokenError),
    Unreachable,
    Malformed,
    NotFound(String),
}

impl WarehouseError {
    /// 502 when the catalog did not answer or answered nonsense; 503 when Nova is pointed at a warehouse that does not exist.
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::Token(_) | Self::Unreachable | Self::Malformed => StatusCode::BAD_GATEWAY,
            Self::NotFound(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl std::fmt::Display for WarehouseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(e) => e.fmt(f),
            Self::Unreachable => f.write_str("the catalog is not reachable"),
            Self::Malformed => f.write_str("the catalog returned an unexpected response"),
            Self::NotFound(name) => write!(f, "warehouse '{name}' not found"),
        }
    }
}

/// The `Authorization` value a Lakekeeper request carries: Nova's service token when the
/// platform configured the catalog client (`NOVA_CATALOG_*`), nothing otherwise — an
/// unconfigured install keeps calling the `allowall` catalog exactly as before.
pub(crate) async fn lakekeeper_authorization(
    state: &AppState,
) -> Result<Option<String>, crate::catalog_token::TokenError> {
    match &state.catalog_token {
        Some(token) => token.bearer().await.map(Some),
        None => Ok(None),
    }
}

/// A catalog name: 1 to 40 characters, `[a-z][a-z0-9]*`. The rule is what the platform's
/// catalogs are named; it also keeps the name safe to use as one SQL identifier (which
/// still goes through `quote_ident`) and as one URL query value.
pub fn valid_catalog(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 40
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// List every Lakekeeper warehouse, with Nova's own service token (`catalog_token.rs`).
///
/// `crate`-wide because the catalog-metrics refresher lists the same warehouses to decide
/// what to scan (`catalog_metrics.rs`, stardelt-nova#263).
pub(crate) async fn warehouse_list(
    state: &AppState,
) -> Result<Vec<WarehouseListItem>, WarehouseError> {
    let url = format!("{}/management/v1/warehouse", state.lakekeeper_url);
    let mut request = state.http.get(&url);
    if let Some(authorization) = lakekeeper_authorization(state)
        .await
        .map_err(WarehouseError::Token)?
    {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    let resp: WarehouseList = request
        .send()
        .await
        .map_err(|e| {
            warn!("lakekeeper unreachable: {e}");
            WarehouseError::Unreachable
        })?
        .json()
        .await
        .map_err(|e| {
            warn!("lakekeeper warehouse list malformed: {e}");
            WarehouseError::Malformed
        })?;
    Ok(resp.warehouses)
}

/// The medallion layer a label value names, or `None` for anything else.
///
/// The value is trimmed and compared case-insensitively: a hand-written manifest that says
/// `Gold` means gold. A value that is not one of the three is ignored — the next rule (the
/// name convention) still applies, and a typo never becomes a layer of its own.
pub fn layer_value(value: &str) -> Option<String> {
    let value = value.trim();
    LAYERS
        .iter()
        .find(|layer| layer.eq_ignore_ascii_case(value))
        .map(|layer| (*layer).to_string())
}

/// The layer a catalog *name* stands for: a catalog called `bronze`, `silver` or `gold` is
/// that layer. Every other name carries none.
pub fn layer_from_name(name: &str) -> Option<String> {
    LAYERS
        .iter()
        .find(|layer| **layer == name)
        .map(|layer| (*layer).to_string())
}

/// The layer a `Catalog` object declares: the `stardelt.io/layer` label when that names a
/// layer, else the name convention.
pub fn layer_of_object(object: &serde_json::Value) -> Option<String> {
    let metadata = object.get("metadata");
    let label = metadata
        .and_then(|metadata| metadata.get("labels"))
        .and_then(|labels| labels.get(LAYER_LABEL))
        .and_then(serde_json::Value::as_str);
    let name = metadata
        .and_then(|metadata| metadata.get("name"))
        .and_then(serde_json::Value::as_str);
    label
        .and_then(layer_value)
        .or_else(|| name.and_then(layer_from_name))
}

/// One catalog's layer: the label from [`catalog_layers`] when the resource carries one,
/// else the name convention.
pub fn layer_in(layers: &HashMap<String, String>, name: &str) -> Option<String> {
    layers.get(name).cloned().or_else(|| layer_from_name(name))
}

/// Where a catalog sorts: bronze, silver, gold, then everything without a layer.
pub fn layer_rank(layer: Option<&str>) -> usize {
    layer
        .and_then(|layer| LAYERS.iter().position(|known| *known == layer))
        .unwrap_or(LAYERS.len())
}

/// Sort catalogs by [`layer_rank`], then by name: the medallion layers first, in platform
/// order, then every other catalog alphabetically.
pub fn sort_by_layer<T>(
    rows: &mut [T],
    layer: impl Fn(&T) -> Option<&str>,
    name: impl Fn(&T) -> &str,
) {
    rows.sort_by(|a, b| {
        layer_rank(layer(a))
            .cmp(&layer_rank(layer(b)))
            .then_with(|| name(a).cmp(name(b)))
    });
}

/// Every catalog's medallion layer, from the `stardelt.io/layer` label on the `Catalog`
/// resources of Nova's namespace: one list per refresh, cached for [`LAYERS_TTL`], caller
/// independent (the layer describes the catalog, not the person).
///
/// The list is read with Nova's own service account — the same `get`/`list` on `catalogs`
/// the administrator page uses. A catalog the list does not cover gets no entry here; the
/// name convention still applies through [`layer_in`]. Without a cluster client — demo
/// mode, a local run, `dev` mode outside a cluster — there is nothing to read, so only the
/// name convention remains. A list that fails leaves the map empty and is retried after the
/// TTL: a missing label must not fail the catalog list.
pub async fn catalog_layers(state: &AppState) -> HashMap<String, String> {
    catalog_layers_within(state, LAYERS_TTL).await
}

/// [`catalog_layers`] with an explicit TTL. The tests use it instead of waiting a minute;
/// production always passes [`LAYERS_TTL`].
async fn catalog_layers_within(state: &AppState, ttl: Duration) -> HashMap<String, String> {
    let cached = state
        .catalog_layers
        .lock()
        .expect("catalog layers poisoned")
        .as_ref()
        .filter(|(at, _)| at.elapsed() < ttl)
        .map(|(_, layers)| layers.clone());
    if let Some(layers) = cached {
        return layers;
    }

    let mut layers = HashMap::new();
    if let Some(client) = state.kube.clone() {
        match crate::admin_catalogs::catalogs_api(client)
            .list(&kube::api::ListParams::default())
            .await
        {
            Ok(list) => {
                for object in list.items {
                    let name = object.metadata.name.unwrap_or_default();
                    let label = object
                        .metadata
                        .labels
                        .as_ref()
                        .and_then(|labels| labels.get(LAYER_LABEL));
                    let layer = label
                        .and_then(|label| layer_value(label))
                        .or_else(|| layer_from_name(&name));
                    if let Some(layer) = layer {
                        layers.insert(name, layer);
                    }
                }
            }
            // A label is a nicety: the name convention keeps working, and the next request
            // after the TTL asks again instead of caching a failure forever.
            Err(e) => warn!(error = %e, "catalog layer list failed"),
        }
    }
    state
        .catalog_layers
        .lock()
        .expect("catalog layers poisoned")
        .replace((Instant::now(), layers.clone()));
    layers
}

/// The Lakekeeper warehouse id for one catalog name.
///
/// Answers from the map; a miss lists the warehouses once (a new catalog appears without
/// a restart) and refills it, then answers or `NotFound(name)` — a name the catalog does
/// not know, never a stale id, except for a warehouse deleted after it was cached.
pub(crate) async fn warehouse_id_for(
    state: &AppState,
    name: &str,
) -> Result<String, WarehouseError> {
    if let Some(id) = state
        .catalog_ids
        .read()
        .expect("catalog ids poisoned")
        .get(name)
    {
        return Ok(id.clone());
    }
    let warehouses = warehouse_list(state).await?;
    let mut ids = state.catalog_ids.write().expect("catalog ids poisoned");
    for w in warehouses {
        ids.insert(w.name, w.id);
    }
    ids.get(name)
        .cloned()
        .ok_or_else(|| WarehouseError::NotFound(name.to_string()))
}

/// The configured default catalog's warehouse id (`NOVA_WAREHOUSE_NAME`): the dashboard
/// facts and `/api/catalog/warehouse` keep working on it, unchanged.
pub(crate) async fn resolve_warehouse_id(state: &AppState) -> Result<String, WarehouseError> {
    warehouse_id_for(state, &state.warehouse_name).await
}

async fn proxy(state: &AppState, path: String) -> Response {
    let url = format!("{}{}", state.lakekeeper_url, path);
    let mut request = state.http.get(&url);
    match lakekeeper_authorization(state).await {
        Ok(Some(header)) => request = request.header(reqwest::header::AUTHORIZATION, header),
        Ok(None) => {}
        Err(e) => {
            // The token endpoint failed, not the catalog: 502 for the same reason a
            // catalog that does not answer is one. The sentence is fixed text.
            warn!("catalog service token unavailable: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    }
    match request.send().await {
        Ok(r) => {
            let status = r.status();
            let body = r.bytes().await.unwrap_or_default();
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                [("content-type", "application/json")],
                body,
            )
                .into_response()
        }
        Err(e) => {
            // The reqwest error names the Lakekeeper URL it tried; log that and answer
            // a sentence, since every signed-in user sees this body.
            warn!("lakekeeper proxy error: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "the upstream service is not reachable"})),
            )
                .into_response()
        }
    }
}

/// One error sentence, the envelope every `/api/` failure uses.
fn sentence(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The catalogs `caller` may browse, **as the engine shows them to that caller**.
///
/// `SHOW CATALOGS` runs with the caller's own token through `trino::execute_from`, so the
/// platform's permission bridge filters it (stardelt-platform, authz 8/8); Nova only drops
/// the engine's own catalogs ([`HIDDEN_CATALOGS`]). The answer is cached for 30 s per
/// subject: a grant change shows up within half a minute, and browsing the catalog does
/// not run a statement per click.
///
/// Demo mode — and `dev` mode, which has no caller — has one catalog and no permission
/// bridge: the configured warehouse, exactly as before. An unreachable engine is the
/// `Err` string from `execute_from` (502 through [`error_status`]).
pub async fn allowed_catalogs(
    state: &AppState,
    caller: Option<&Principal>,
) -> Result<BTreeSet<String>, String> {
    let Some(caller) = caller.filter(|_| !state.demo) else {
        return Ok(BTreeSet::from([state.warehouse_name.clone()]));
    };

    let sub = caller.identity.sub.clone();
    let cached = state
        .catalog_access
        .lock()
        .expect("catalog access poisoned")
        .get(&sub)
        .and_then(|(at, names)| (at.elapsed() < ACCESS_TTL).then(|| names.clone()));
    if let Some(names) = cached {
        return Ok(names);
    }

    let result =
        crate::trino::execute_from(state, "SHOW CATALOGS".into(), Some(caller), "nova-catalog")
            .await?;
    let mut allowed = BTreeSet::new();
    for row in &result.rows {
        if let Some(name) = row.first().and_then(serde_json::Value::as_str)
            && !name.is_empty()
            && !HIDDEN_CATALOGS.contains(&name)
        {
            allowed.insert(name.to_string());
        }
    }
    state
        .catalog_access
        .lock()
        .expect("catalog access poisoned")
        .insert(sub, (Instant::now(), allowed.clone()));
    Ok(allowed)
}

/// The catalog one request works in, and its Lakekeeper warehouse id.
///
/// `requested` is the route's optional `?catalog=`. The answer is
/// `(catalog name, warehouse id)`; the `Err` is the response to send back: 400 for a name
/// that is not a catalog name, 403 when the caller may not browse it, 404 when no catalog
/// by that name exists, 502/503 when Trino or Lakekeeper does not answer. A catalog the
/// person may not browse is answered 403 and never proxied; the name of a catalog nobody
/// has is 404.
// The `Err` is the ready-made refusal every catalog handler returns, and the signature is
// fixed by the ticket; boxing it at eight call sites would only move the dereference.
#[allow(clippy::result_large_err)]
pub async fn catalog_scope(
    state: &AppState,
    caller: Option<&Principal>,
    requested: Option<&str>,
) -> Result<(String, String), Response> {
    let requested = requested.map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = requested
        && !valid_catalog(name)
    {
        return Err(sentence(
            StatusCode::BAD_REQUEST,
            "catalog name must be lower-case letters and digits",
        ));
    }

    let allowed = allowed_catalogs(state, caller)
        .await
        .map_err(|e| (error_status(&e), Json(serde_json::json!({ "error": e }))).into_response())?;

    let name = match requested {
        Some(name) => name.to_string(),
        None if allowed.is_empty() => {
            return Err(sentence(StatusCode::FORBIDDEN, NO_CATALOG_SENTENCE));
        }
        None if !allowed.contains(&state.warehouse_name) => {
            // The configured default is not open to this person: the first one that is.
            allowed
                .iter()
                .next()
                .cloned()
                .expect("a non-empty set has a first name")
        }
        None => state.warehouse_name.clone(),
    };

    // Resolve the id before refusing: a name no catalog knows is 404 and a catalog the
    // person may not browse is 403, whichever group owns it.
    let id = if state.demo {
        "demo".to_string()
    } else {
        match warehouse_id_for(state, &name).await {
            Ok(id) => id,
            Err(WarehouseError::NotFound(_)) => {
                return Err(sentence(
                    StatusCode::NOT_FOUND,
                    &format!("catalog '{name}' not found"),
                ));
            }
            Err(e) => {
                return Err(sentence(e.status(), &e.to_string()));
            }
        }
    };
    if !allowed.contains(&name) {
        return Err(sentence(
            StatusCode::FORBIDDEN,
            &format!("you cannot browse catalog '{name}'"),
        ));
    }
    Ok((name, id))
}

#[utoipa::path(
    get,
    operation_id = "catalogs_list",
    path = "/api/catalog/catalogs",
    tag = "public",
    description = "The catalogs the caller may browse, each with its Lakekeeper warehouse id and its medallion layer when it has one, plus the catalog a request without `?catalog=` uses. The order is bronze, silver, gold first (in that order), then the rest alphabetically; `layer` is `stardelt.io/layer` from the catalog's `Catalog` resource, else the catalog's own name when that is `bronze`, `silver` or `gold`, and absent otherwise. The list is what the engine's `SHOW CATALOGS` shows the caller (through the caller's own token), minus the engine's own catalogs (`system`, `information_schema`, `memory`, `tpch`, `tpcds`, `jmx`); it is cached for 30 s (the layer for 60 s). Every other `/api/catalog/*` route takes the same `?catalog=`; a catalog the caller may not browse answers 403 there and is never proxied. Empty when no catalog is open to the caller. `demo` in demo mode.",
    responses(
        (status = 200, description = "The browsable catalogs and the default one", body = CatalogsResponse),
        (status = 502, description = "The query engine or the catalog is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "A catalog the engine lists has no warehouse", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list_catalogs(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let allowed = match allowed_catalogs(&s, caller).await {
        Ok(allowed) => allowed,
        Err(e) => {
            return (error_status(&e), Json(serde_json::json!({ "error": e }))).into_response();
        }
    };
    let layers = catalog_layers(&s).await;
    let mut catalogs = Vec::with_capacity(allowed.len());
    for name in allowed {
        let id = if s.demo {
            "demo".to_string()
        } else {
            match warehouse_id_for(&s, &name).await {
                Ok(id) => id,
                Err(e) => {
                    return (
                        e.status(),
                        Json(serde_json::json!({"error": e.to_string()})),
                    )
                        .into_response();
                }
            }
        };
        let layer = layer_in(&layers, &name);
        catalogs.push(CatalogInfo { name, id, layer });
    }
    // The engine answers alphabetically; the medallion layers come first here.
    sort_by_layer(
        &mut catalogs,
        |catalog| catalog.layer.as_deref(),
        |catalog| catalog.name.as_str(),
    );
    let default = if catalogs.iter().any(|c| c.name == s.warehouse_name) {
        s.warehouse_name.clone()
    } else {
        catalogs
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| s.warehouse_name.clone())
    };
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(CatalogsResponse { catalogs, default }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/api/catalog/warehouse",
    tag = "public",
    description = "The resolved warehouse name and id (`demo` in demo mode).",
    responses(
        (status = 200, description = "Warehouse name and id", body = WarehouseInfo),
        (status = 502, description = "The catalog is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The configured warehouse does not exist", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn warehouse_info(State(s): State<Arc<AppState>>) -> Response {
    if s.demo {
        return Json(serde_json::json!({ "name": s.warehouse_name, "id": "demo" })).into_response();
    }
    match resolve_warehouse_id(&s).await {
        Ok(id) => Json(serde_json::json!({ "name": s.warehouse_name, "id": id })).into_response(),
        Err(e) => (
            e.status(),
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(
    get,
    operation_id = "namespaces_list",
    path = "/api/catalog/namespaces",
    tag = "public",
    description = "List one catalog's namespaces (schemas). Each namespace is a list of levels. `?catalog=` picks the catalog; omitted, the default one is used. A catalog the caller may not browse answers 403, a name no catalog knows 404.",
    params(CatalogParam),
    responses(
        (status = 200, description = "Namespaces in the catalog", body = NamespacesResponse),
        (status = 400, description = "The catalog name is not lower-case letters and digits", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The configured warehouse does not exist", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list_namespaces(
    State(s): State<Arc<AppState>>,
    Query(q): Query<CatalogParam>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let (_, id) = match catalog_scope(&s, caller, q.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return Json(demo.namespaces_json()).into_response();
    }
    proxy(&s, format!("/catalog/v1/{id}/namespaces")).await
}

#[utoipa::path(
    post,
    operation_id = "namespaces_create",
    path = "/api/catalog/namespaces",
    tag = "internal",
    description = "Create a schema in one catalog via Trino (`CREATE SCHEMA IF NOT EXISTS`). `?catalog=` picks the catalog; omitted, the default one is used. UI-only for now: not promised stable.",
    params(CatalogParam),
    request_body = CreateNamespaceRequest,
    responses(
        (status = 200, description = "The created schema name", body = CreateNamespaceResponse),
        (status = 400, description = "Missing name, invalid catalog name, or the engine refused it", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The query engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn create_namespace(
    State(s): State<Arc<AppState>>,
    Query(q): Query<CatalogParam>,
    caller: Option<axum::Extension<auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(req): ApiJson<CreateNamespaceRequest>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let (catalog, _) = match catalog_scope(&s, caller, q.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let ns = req.namespace.trim();
    if ns.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "namespace is required"})),
        )
            .into_response();
    }
    if s.demo {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "demo mode derives schemas from tables — upload a table into the schema instead"
            })),
        )
            .into_response();
    }
    let sql = format!(
        "CREATE SCHEMA IF NOT EXISTS {}.{}",
        data::quote_ident(&catalog),
        data::quote_ident(ns)
    );
    match crate::trino::execute(&s, sql, caller).await {
        Ok(_) => Json(serde_json::json!({ "namespace": ns })).into_response(),
        Err(e) => (error_status(&e), Json(serde_json::json!({"error": e}))).into_response(),
    }
}

#[utoipa::path(
    get,
    operation_id = "tables_list",
    path = "/api/catalog/namespaces/{ns}/tables",
    tag = "public",
    description = "List tables in one namespace of one catalog. `?catalog=` picks the catalog; omitted, the default one is used.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        CatalogParam,
    ),
    responses(
        (status = 200, description = "Table identifiers in the namespace", body = TablesResponse),
        (status = 400, description = "The name is not a single path segment, or the catalog name is invalid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list_tables(
    State(s): State<Arc<AppState>>,
    Path(ns): Path<String>,
    Query(q): Query<CatalogParam>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let (_, id) = match catalog_scope(&s, caller, q.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return Json(demo.tables_json(&ns)).into_response();
    }
    let Some(ns) = path_segment(&ns) else {
        return bad_segment(&ns);
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces/{ns}/tables")).await
}

#[utoipa::path(
    get,
    path = "/api/catalog/tables/{ns}/{tbl}",
    tag = "public",
    description = "Iceberg metadata for one table (metadata location plus the metadata document). `?catalog=` picks the catalog; omitted, the default one is used.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
        CatalogParam,
    ),
    responses(
        (status = 200, description = "The table metadata document", body = TableMetadataResponse),
        (status = 400, description = "A name is not a single path segment, or the catalog name is invalid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog or no such table", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn table_metadata(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    Query(q): Query<CatalogParam>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let (_, id) = match catalog_scope(&s, caller, q.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return match demo.table_metadata_json(&ns, &tbl) {
            Some(v) => Json(v).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("table {ns}.{tbl} not found")})),
            )
                .into_response(),
        };
    }
    let (Some(ns), Some(tbl)) = (path_segment(&ns), path_segment(&tbl)) else {
        return bad_segment(&format!("{ns}.{tbl}"));
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces/{ns}/tables/{tbl}")).await
}

/// The last 100 snapshots of a table, newest first (Trino 480 Iceberg syntax).
/// Names go through `quote_ident` only — never interpolated raw.
fn history_sql(catalog: &str, ns: &str, tbl: &str) -> String {
    format!(
        "SELECT committed_at, snapshot_id, parent_id, operation, summary FROM {}.{}.{} ORDER BY committed_at DESC LIMIT 100",
        crate::data::quote_ident(catalog),
        crate::data::quote_ident(ns),
        crate::data::quote_ident(&format!("{tbl}$snapshots")),
    )
}

/// Move the table's current pointer to an earlier snapshot. The snapshot id is
/// an `i64`, so it is interpolated as an integer — never as a quoted string.
fn rollback_sql(catalog: &str, ns: &str, tbl: &str, snapshot_id: i64) -> String {
    format!(
        "ALTER TABLE {}.{}.{} EXECUTE rollback_to_snapshot({snapshot_id})",
        crate::data::quote_ident(catalog),
        crate::data::quote_ident(ns),
        crate::data::quote_ident(tbl),
    )
}

/// Accept the snapshot id as a JSON number or as a string of digits.
///
/// Iceberg snapshot ids are 64-bit (e.g. `5972469351597084165`), past JS's
/// `Number.MAX_SAFE_INTEGER`. The History tab therefore keeps them as strings
/// end-to-end (see `stringify_snapshot_ids`): a UI that parsed them as numbers
/// would roll back to a rounded id Trino does not know. A string is parsed
/// here and still interpolated as an integer — never as SQL text.
fn deserialize_snapshot_id<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Unexpected};
    struct Visit;
    impl serde::de::Visitor<'_> for Visit {
        type Value = i64;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a snapshot id as an integer or a string of digits")
        }
        fn visit_i64<E: Error>(self, v: i64) -> Result<i64, E> {
            Ok(v)
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<i64, E> {
            i64::try_from(v).map_err(|_| Error::invalid_value(Unexpected::Unsigned(v), &self))
        }
        fn visit_str<E: Error>(self, v: &str) -> Result<i64, E> {
            v.parse::<i64>()
                .map_err(|_| Error::invalid_value(Unexpected::Str(v), &self))
        }
    }
    deserializer.deserialize_any(Visit)
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RollbackRequest {
    /// The snapshot to return to, as a string of digits (an Iceberg id has 64 bits; a JSON
    /// number is accepted too but a browser rounds it).
    #[serde(deserialize_with = "deserialize_snapshot_id")]
    #[schema(value_type = String)]
    pub snapshot_id: i64,
}

/// Render `snapshot_id`/`parent_id` as strings so the JSON keeps all 64 bits.
///
/// `serde_json` holds the full `i64` from Trino, but `JSON.parse` in the
/// browser rounds past 2^53 — and a rounded id rolls back to
/// `Cannot roll back to unknown snapshot id`. Strings survive intact; the
/// History tab validates the digits before using them as an integer literal.
fn stringify_snapshot_ids(mut r: crate::trino::QueryResult) -> crate::trino::QueryResult {
    let mut snap = None;
    let mut parent = None;
    for (i, c) in r.columns.iter().enumerate() {
        if c.name == "snapshot_id" && snap.is_none() {
            snap = Some(i);
        } else if c.name == "parent_id" && parent.is_none() {
            parent = Some(i);
        }
    }
    for row in r.rows.iter_mut() {
        for idx in [snap, parent].into_iter().flatten() {
            if let Some(cell) = row.get_mut(idx)
                && let serde_json::Value::Number(n) = &*cell
            {
                *cell = serde_json::Value::String(n.to_string());
            }
        }
    }
    r
}

#[utoipa::path(
    get,
    operation_id = "table_history",
    path = "/api/catalog/tables/{ns}/{tbl}/history",
    tag = "public",
    description = "The table's last 100 snapshots, newest first: `committed_at`, `snapshot_id`, `parent_id`, `operation`, `summary`. Snapshot and parent ids are strings so that all 64 bits survive JSON in a browser.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
        CatalogParam,
    ),
    responses(
        (status = 200, description = "One row per snapshot", body = crate::trino::QueryResult),
        (status = 400, description = "A name is not a single path segment, or the engine refused the statement", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn table_history(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    Query(q): Query<CatalogParam>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let (catalog, _) = match catalog_scope(&s, caller, q.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if s.demo {
        return Json(serde_json::json!({"columns": [], "rows": []})).into_response();
    }
    if path_segment(&ns).is_none() || path_segment(&tbl).is_none() {
        return bad_segment(&format!("{ns}.{tbl}"));
    }
    match crate::trino::execute(&s, history_sql(&catalog, &ns, &tbl), caller).await {
        Ok(r) => Json(stringify_snapshot_ids(r)).into_response(),
        Err(e) => (error_status(&e), Json(serde_json::json!({"error": e}))).into_response(),
    }
}

#[utoipa::path(
    post,
    operation_id = "table_rollback",
    path = "/api/catalog/tables/{ns}/{tbl}/rollback",
    tag = "public",
    description = "Move the table's current pointer back to an earlier snapshot (`ALTER TABLE … EXECUTE rollback_to_snapshot`), run as the caller, so the engine's permissions decide. Nothing is deleted: the newer snapshots stay until they are expired.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
        CatalogParam,
    ),
    request_body = RollbackRequest,
    responses(
        (status = 200, description = "The engine's answer to the statement", body = crate::trino::QueryResult),
        (status = 400, description = "A name or the body is not valid, the snapshot is unknown, or the engine refused the statement", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn table_rollback(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    Query(q): Query<CatalogParam>,
    caller: Option<axum::Extension<auth::Principal>>,
    ApiJson(req): ApiJson<RollbackRequest>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let (catalog, _) = match catalog_scope(&s, caller, q.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if s.demo {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "rollback is not available in demo mode"})),
        )
            .into_response();
    }
    if path_segment(&ns).is_none() || path_segment(&tbl).is_none() {
        return bad_segment(&format!("{ns}.{tbl}"));
    }
    match crate::trino::execute(
        &s,
        rollback_sql(&catalog, &ns, &tbl, req.snapshot_id),
        caller,
    )
    .await
    {
        Ok(r) => Json(r).into_response(),
        Err(e) => (error_status(&e), Json(serde_json::json!({"error": e}))).into_response(),
    }
}

/// Percent-encode one URL path segment for the Lakekeeper proxy. Axum has already
/// decoded the request path, so a namespace like `a%2F..%2Fb` arrives as `a/../b`;
/// interpolated raw, the URL parser would collapse the dot segments and send the GET
/// to another Lakekeeper endpoint. Encoding keeps it one segment; a bare `.` or `..`
/// (which parsers collapse even when encoded as unreserved characters) or an empty
/// segment is refused.
pub(crate) fn path_segment(raw: &str) -> Option<String> {
    if raw.is_empty() || raw == "." || raw == ".." {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    Some(out)
}

fn bad_segment(what: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": format!("invalid name '{what}'")})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{
        Extension, Router,
        http::HeaderMap,
        routing::{get, post},
    };
    use serde_json::{Value, json};

    use super::*;

    // -----------------------------------------------------------------------
    // Fakes: one Trino that answers `SHOW CATALOGS` per person, one Lakekeeper
    // -----------------------------------------------------------------------

    /// Answers `SHOW CATALOGS` with the named person's catalogs, as the permission
    /// bridge does: `X-Trino-User` is the display name (`trino::upstream_headers`).
    #[derive(Clone, Default)]
    struct FakeTrino {
        by_user: HashMap<String, Vec<String>>,
        /// bearer token -> person key: with a forwarded token the engine sees only
        /// the Authorization header (platform#311/#259).
        tokens: HashMap<String, String>,
        calls: Arc<AtomicUsize>,
    }

    async fn show_catalogs(State(fake): State<FakeTrino>, headers: HeaderMap) -> Json<Value> {
        fake.calls.fetch_add(1, Ordering::SeqCst);
        let user = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .and_then(|token| fake.tokens.get(token).cloned())
            .or_else(|| {
                headers
                    .get("x-trino-user")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let rows: Vec<Value> = fake
            .by_user
            .get(&user)
            .map(|names| names.iter().map(|name| json!([name])).collect())
            .unwrap_or_default();
        Json(json!({
            "id": "query_1",
            "columns": [{"name": "Catalog", "type": "varchar"}],
            "data": rows,
            "stats": { "state": "FINISHED" }
        }))
    }

    async fn fake_trino(by_user: &[(&str, &[&str])]) -> (String, Arc<AtomicUsize>) {
        let fake = FakeTrino {
            by_user: by_user
                .iter()
                .map(|(user, names)| {
                    (
                        user.to_string(),
                        names.iter().map(|name| name.to_string()).collect(),
                    )
                })
                .collect(),
            tokens: by_user
                .iter()
                .map(|(user, _)| (format!("token-{user}"), user.to_string()))
                .collect(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let calls = Arc::clone(&fake.calls);
        let app = Router::new()
            .route("/v1/statement", post(show_catalogs))
            .with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), calls)
    }

    /// The four warehouses the platform seeds (`stardelt-platform` #309).
    async fn fake_lakekeeper() -> String {
        fake_lakekeeper_names(&["warehouse", "finance", "product", "sandbox"]).await
    }

    /// One Lakekeeper warehouse per name, so the layer tests can list any catalog.
    async fn fake_lakekeeper_names(names: &[&str]) -> String {
        let warehouses: Vec<Value> = names
            .iter()
            .map(|name| json!({"id": format!("wid-{name}"), "name": name}))
            .collect();
        let app = Router::new().route(
            "/management/v1/warehouse",
            get(move || {
                let warehouses = warehouses.clone();
                async move { Json(json!({ "warehouses": warehouses })) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// A `Catalog` object as the API server hands it over: a name, and optionally the
    /// `stardelt.io/layer` label.
    fn catalog_object(name: &str, label: Option<&str>) -> Value {
        let mut object = json!({
            "apiVersion": "stardelt.io/v1alpha1",
            "kind": "Catalog",
            "metadata": { "name": name }
        });
        if let Some(label) = label {
            object["metadata"]["labels"] = json!({ "stardelt.io/layer": label });
        }
        object
    }

    /// An API server that answers one `Catalog` list and counts the calls, so the layer
    /// cache's "one list per refresh" is observable (`cache_ttl`).
    mod fake_catalog_api {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::{Json, Router, extract::State, routing::get};
        use serde_json::{Value, json};

        #[derive(Default)]
        pub struct Fake {
            pub items: Vec<Value>,
            pub lists: AtomicUsize,
        }

        async fn list(State(fake): State<Arc<Fake>>) -> Json<Value> {
            fake.lists.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "CatalogList",
                "metadata": { "resourceVersion": "1" },
                "items": fake.items,
            }))
        }

        pub async fn start(items: Vec<Value>) -> (kube::Client, Arc<Fake>) {
            let fake = Arc::new(Fake {
                items,
                ..Fake::default()
            });
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/catalogs",
                    get(list),
                )
                .with_state(Arc::clone(&fake));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let url = format!("http://{addr}").parse().unwrap();
            let client =
                kube::Client::try_from(kube::Config::new(url)).expect("a client for the fake API");
            (client, fake)
        }
    }

    /// The freshly built test state, owned, so one field can be changed.
    fn own_state(state: Arc<AppState>) -> AppState {
        Arc::try_unwrap(state).unwrap_or_else(|_| panic!("a fresh state"))
    }

    /// A state that talks to the fakes and has a cluster client: the catalog list reads
    /// its layers from `Catalog` objects.
    fn state_with_catalogs(trino: &str, lakekeeper: &str, kube: kube::Client) -> Arc<AppState> {
        let mut state = own_state(crate::router_tests::catalog_scope_test_state(
            trino, lakekeeper,
        ));
        state.kube = Some(kube);
        Arc::new(state)
    }

    async fn json_body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    fn person(name: &str) -> Principal {
        Principal {
            identity: crate::auth::Identity {
                sub: name.into(),
                display_name: name.into(),
                email: None,
                name: None,
                groups: Vec::new(),
                roles: Vec::new(),
                groups_overage: false,
                is_admin: false,
            },
            via: "session",
            access_token: Some(crate::auth::Secret::new(format!("token-{name}"))),
        }
    }

    async fn error_sentence(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let value: Value = serde_json::from_slice(&bytes).expect("json");
        (
            status,
            value["error"]
                .as_str()
                .expect("an error sentence")
                .to_string(),
        )
    }

    #[test]
    fn warehouse_errors_carry_upstream_statuses() {
        assert_eq!(
            WarehouseError::Unreachable.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(WarehouseError::Malformed.status(), StatusCode::BAD_GATEWAY);
        // A token endpoint that does not answer is the same kind of failure as a catalog
        // that does not: 502, never 500.
        assert_eq!(
            WarehouseError::Token(crate::catalog_token::TokenError::Unreachable).status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            WarehouseError::NotFound("warehouse".into()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn warehouse_errors_keep_their_sentences() {
        assert_eq!(
            WarehouseError::Unreachable.to_string(),
            "the catalog is not reachable"
        );
        assert_eq!(
            WarehouseError::Malformed.to_string(),
            "the catalog returned an unexpected response"
        );
        assert_eq!(
            WarehouseError::NotFound("warehouse".into()).to_string(),
            "warehouse 'warehouse' not found"
        );
        assert_eq!(
            WarehouseError::Token(crate::catalog_token::TokenError::Malformed).to_string(),
            "the catalog token endpoint answered with an unexpected body"
        );
    }

    #[test]
    fn valid_catalog_is_lower_case_letters_and_digits() {
        assert!(valid_catalog("finance"));
        assert!(valid_catalog("a"));
        assert!(valid_catalog(&"a".repeat(40)), "40 characters are allowed");
        assert!(!valid_catalog("System"), "upper case is refused");
        assert!(!valid_catalog("a-b"), "a dash is not a catalog name");
        assert!(!valid_catalog(""), "an empty name is refused");
        assert!(
            !valid_catalog(&"a".repeat(41)),
            "41 characters are too long"
        );
        assert!(
            !valid_catalog("1finance"),
            "the first character is a letter"
        );
    }

    #[tokio::test]
    async fn each_person_sees_only_their_catalogs() {
        let (trino, calls) = fake_trino(&[
            ("ada", &["warehouse", "finance", "system", "tpch"]),
            ("bob", &["warehouse", "product"]),
            ("outsider", &["warehouse"]),
        ])
        .await;
        let lakekeeper = fake_lakekeeper().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();

        // The finance member sees her group's catalog plus the open one; the engine's own
        // catalogs (`system`, `tpch`) are dropped even though `SHOW CATALOGS` lists them.
        let ada = person("ada");
        assert_eq!(
            allowed_catalogs(&state, Some(&ada))
                .await
                .expect("the engine answers"),
            set(&["finance", "warehouse"])
        );
        // A member of the product group sees product, not finance.
        let bob = person("bob");
        assert_eq!(
            allowed_catalogs(&state, Some(&bob))
                .await
                .expect("the engine answers"),
            set(&["product", "warehouse"])
        );
        // An outsider sees exactly what is left open to everybody.
        let outsider = person("outsider");
        assert_eq!(
            allowed_catalogs(&state, Some(&outsider))
                .await
                .expect("the engine answers"),
            set(&["warehouse"])
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3, "one statement per person");

        // Within 30 s the cached answer is used: a second call asks nobody.
        assert_eq!(
            allowed_catalogs(&state, Some(&ada))
                .await
                .expect("the engine answers"),
            set(&["finance", "warehouse"])
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "a second call within 30 s must not run SHOW CATALOGS again"
        );
    }

    #[tokio::test]
    async fn no_caller_gets_the_configured_warehouse_without_asking_the_engine() {
        let (trino, calls) = fake_trino(&[]).await;
        let lakekeeper = fake_lakekeeper().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        assert_eq!(
            allowed_catalogs(&state, None).await.expect("one catalog"),
            BTreeSet::from(["warehouse".to_string()])
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "dev mode runs no statement"
        );

        let (name, id) = catalog_scope(&state, None, None)
            .await
            .expect("the default catalog resolves");
        assert_eq!((name.as_str(), id.as_str()), ("warehouse", "wid-warehouse"));
    }

    #[tokio::test]
    async fn catalog_scope_names_the_reason_for_a_refusal() {
        let (trino, _) = fake_trino(&[("ada", &["warehouse", "finance", "system", "tpch"])]).await;
        let lakekeeper = fake_lakekeeper().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);
        let ada = person("ada");

        let (name, id) = catalog_scope(&state, Some(&ada), Some("finance"))
            .await
            .expect("a catalog she may browse");
        assert_eq!((name.as_str(), id.as_str()), ("finance", "wid-finance"));

        // Known to the catalog, not in her list: 403 with the sentence, never proxied.
        let (status, sentence) = error_sentence(
            catalog_scope(&state, Some(&ada), Some("product"))
                .await
                .expect_err("not hers"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(sentence, "you cannot browse catalog 'product'");

        // No catalog by that name: 404.
        let (status, sentence) = error_sentence(
            catalog_scope(&state, Some(&ada), Some("nope"))
                .await
                .expect_err("unknown"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(sentence, "catalog 'nope' not found");

        // A hidden engine catalog is not offered and no warehouse carries it: 404.
        let (status, _) = error_sentence(
            catalog_scope(&state, Some(&ada), Some("system"))
                .await
                .expect_err("hidden"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, sentence) = error_sentence(
            catalog_scope(&state, Some(&ada), Some("System"))
                .await
                .expect_err("invalid"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            sentence,
            "catalog name must be lower-case letters and digits"
        );
    }

    #[tokio::test]
    async fn a_person_with_no_catalog_is_told_whom_to_ask() {
        let (trino, _) = fake_trino(&[("nobody", &["system", "tpch"])]).await;
        let lakekeeper = fake_lakekeeper().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);
        let nobody = person("nobody");

        let (status, sentence) = error_sentence(
            catalog_scope(&state, Some(&nobody), None)
                .await
                .expect_err("nothing to default to"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            sentence,
            "no catalog is open to you yet; ask an administrator for access"
        );
    }

    #[tokio::test]
    async fn an_unreachable_engine_or_catalog_is_502() {
        let lakekeeper = fake_lakekeeper().await;
        let engine_down =
            crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", &lakekeeper);
        let ada = person("ada");
        let (status, sentence) = error_sentence(
            catalog_scope(&engine_down, Some(&ada), None)
                .await
                .expect_err("no engine"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(sentence, "the query engine is not reachable");

        let (trino, _) = fake_trino(&[("ada", &["warehouse", "finance"])]).await;
        // Nothing listens on that port: the warehouse list cannot be read.
        let catalog_down =
            crate::router_tests::catalog_scope_test_state(&trino, "http://127.0.0.1:1");
        let (status, sentence) = error_sentence(
            catalog_scope(&catalog_down, Some(&ada), Some("finance"))
                .await
                .expect_err("no catalog"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(sentence, "the catalog is not reachable");
    }

    #[tokio::test]
    async fn the_catalog_list_is_the_defaults_and_nothing_else_in_dev_mode() {
        let (trino, _) = fake_trino(&[]).await;
        let lakekeeper = fake_lakekeeper().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        let response = list_catalogs(State(Arc::clone(&state)), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body,
            json!({
                "catalogs": [{"name": "warehouse", "id": "wid-warehouse"}],
                "default": "warehouse"
            })
        );
    }

    // -----------------------------------------------------------------------
    // Medallion layers (stardelt-nova#109): label first, name second, layers first
    // -----------------------------------------------------------------------

    #[test]
    fn layer_from_label() {
        assert_eq!(LAYER_LABEL, "stardelt.io/layer");
        assert_eq!(
            layer_of_object(&catalog_object("shared", Some("gold"))).as_deref(),
            Some("gold")
        );
        // The label counts on any catalog, and it wins over the name.
        assert_eq!(
            layer_of_object(&catalog_object("bronze", Some("silver"))).as_deref(),
            Some("silver")
        );
        assert_eq!(
            layer_of_object(&catalog_object("finance", Some("bronze"))).as_deref(),
            Some("bronze")
        );
        // A hand-written `Gold` is gold; the API answers the lower-case spelling.
        assert_eq!(
            layer_of_object(&catalog_object("shared", Some(" Gold "))).as_deref(),
            Some("gold")
        );
    }

    #[test]
    fn layer_from_name_when_no_label() {
        for layer in LAYERS {
            assert_eq!(
                layer_of_object(&catalog_object(layer, None)).as_deref(),
                Some(layer)
            );
        }
        assert_eq!(layer_of_object(&catalog_object("warehouse", None)), None);
        assert_eq!(layer_of_object(&catalog_object("shared", None)), None);
    }

    #[test]
    fn invalid_label_ignored() {
        assert_eq!(
            layer_of_object(&catalog_object("shared", Some("platinum"))),
            None
        );
        assert_eq!(layer_of_object(&catalog_object("shared", Some(""))), None);
        assert_eq!(
            layer_of_object(&catalog_object("shared", Some("gold, silver"))),
            None
        );
        // The name convention is the second rule, not a fallback value of the label.
        assert_eq!(
            layer_of_object(&catalog_object("silver", Some("platinum"))).as_deref(),
            Some("silver")
        );
    }

    #[tokio::test]
    async fn sorted_bronze_silver_gold_then_alphabetical() {
        let names = ["warehouse", "shared", "gold", "alpha", "bronze", "silver"];
        let (trino, _) = fake_trino(&[("ada", &names)]).await;
        let lakekeeper = fake_lakekeeper_names(&names).await;
        // `shared` is labelled gold; `warehouse` carries no layer; the rest follow the name.
        let (kube, _) = fake_catalog_api::start(vec![
            catalog_object("shared", Some("gold")),
            catalog_object("warehouse", None),
        ])
        .await;
        let state = state_with_catalogs(&trino, &lakekeeper, kube);

        let response = list_catalogs(State(state), Some(Extension(person("ada")))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        let listed: Vec<(&str, Option<&str>)> = body["catalogs"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|catalog| {
                (
                    catalog["name"].as_str().expect("a name"),
                    catalog["layer"].as_str(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("bronze", Some("bronze")),
                ("silver", Some("silver")),
                ("gold", Some("gold")),
                ("shared", Some("gold")),
                ("alpha", None),
                ("warehouse", None),
            ]
        );
        assert_eq!(body["default"], "warehouse");
    }

    #[tokio::test]
    async fn cache_ttl() {
        let (kube, fake) = fake_catalog_api::start(vec![
            catalog_object("bronze", None),
            catalog_object("shared", Some("gold")),
            catalog_object("finance", Some("platinum")),
        ])
        .await;
        let state = crate::router_tests::test_state_with_kube(kube);

        let first = catalog_layers_within(&state, LAYERS_TTL).await;
        assert_eq!(
            first,
            HashMap::from([
                ("bronze".to_string(), "bronze".to_string()),
                ("shared".to_string(), "gold".to_string()),
            ]),
            "the label first, the name convention second, an invalid label never"
        );
        assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
        // Inside the TTL the map answers and the API server is not asked again.
        assert_eq!(catalog_layers_within(&state, LAYERS_TTL).await, first);
        assert_eq!(fake.lists.load(Ordering::SeqCst), 1);
        // Expired (an explicit zero TTL, rather than a minute's wait): one list again.
        assert_eq!(catalog_layers_within(&state, Duration::ZERO).await, first);
        assert_eq!(fake.lists.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn demo_mode_names_only() {
        let mut state = own_state(crate::router_tests::test_state());
        state.warehouse_name = "gold".into();
        let state = Arc::new(state);

        // No cluster client: nothing to label, only the name convention.
        let layers = catalog_layers(&state).await;
        assert!(layers.is_empty());
        assert_eq!(layer_in(&layers, "gold").as_deref(), Some("gold"));
        assert_eq!(layer_in(&layers, "shared"), None);

        let response = list_catalogs(State(Arc::clone(&state)), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await,
            json!({
                "catalogs": [{"name": "gold", "id": "demo", "layer": "gold"}],
                "default": "gold"
            })
        );
    }

    #[test]
    fn plain_names_pass_through() {
        assert_eq!(
            path_segment("nyc_taxi-2024").as_deref(),
            Some("nyc_taxi-2024")
        );
    }

    #[test]
    fn separators_and_specials_are_encoded() {
        assert_eq!(path_segment("a/../b").as_deref(), Some("a%2F..%2Fb"));
        assert_eq!(path_segment("x?y#z").as_deref(), Some("x%3Fy%23z"));
        // Iceberg REST joins nested namespace levels with the unit separator.
        assert_eq!(path_segment("a\u{1f}b").as_deref(), Some("a%1Fb"));
        assert_eq!(path_segment("ä").as_deref(), Some("%C3%A4"));
    }

    #[test]
    fn dot_segments_and_empty_are_refused() {
        assert_eq!(path_segment(""), None);
        assert_eq!(path_segment("."), None);
        assert_eq!(path_segment(".."), None);
    }

    #[test]
    fn history_sql_quotes_the_metadata_table() {
        let sql = history_sql("warehouse", "demo", "t");
        assert!(
            sql.contains("\"t$snapshots\""),
            "metadata table must stay quoted: {sql}"
        );
        assert!(sql.contains("LIMIT 100"), "history is bounded: {sql}");
        assert!(sql.contains("\"demo\""), "namespace must be quoted: {sql}");
        assert!(
            sql.contains("\"warehouse\""),
            "catalog must be quoted: {sql}"
        );
    }

    #[test]
    fn rollback_sql_embeds_only_an_integer() {
        assert_eq!(
            rollback_sql("finance", "a", "t", 42),
            "ALTER TABLE \"finance\".\"a\".\"t\" EXECUTE rollback_to_snapshot(42)"
        );
    }

    #[test]
    fn sql_builders_escape_double_quotes() {
        let sql = history_sql("warehouse", "a\"b", "t");
        assert!(sql.contains("\"a\"\"b\""), "quote must be doubled: {sql}");
        let sql = rollback_sql("fin\"ance", "a", "x\"y", 7);
        assert!(sql.contains("\"x\"\"y\""), "quote must be doubled: {sql}");
        assert!(
            sql.contains("\"fin\"\"ance\""),
            "the catalog is an identifier too: {sql}"
        );
        // No raw interpolation: the doubled quote is the only quoting present.
        assert!(!sql.contains("\"y\"\""), "unexpected quoting: {sql}");
    }

    #[test]
    fn rollback_accepts_a_string_snapshot_id() {
        // The UI keeps 64-bit ids as strings (past JS safe-integer range).
        let req: super::RollbackRequest =
            serde_json::from_value(serde_json::json!({"snapshot_id": "5972469351597084165"}))
                .expect("string id");
        assert_eq!(req.snapshot_id, 5_972_469_351_597_084_165);
        let req: super::RollbackRequest =
            serde_json::from_value(serde_json::json!({"snapshot_id": 42})).expect("number id");
        assert_eq!(req.snapshot_id, 42);
        assert!(
            serde_json::from_value::<super::RollbackRequest>(
                serde_json::json!({"snapshot_id": "not-an-id"})
            )
            .is_err(),
            "a non-numeric string must not become SQL"
        );
    }

    #[test]
    fn history_stringifies_snapshot_ids_without_losing_bits() {
        use serde_json::json;
        let r = crate::trino::QueryResult {
            columns: vec![
                crate::trino::Column {
                    name: "committed_at".into(),
                    r#type: "timestamp".into(),
                },
                crate::trino::Column {
                    name: "snapshot_id".into(),
                    r#type: "bigint".into(),
                },
                crate::trino::Column {
                    name: "parent_id".into(),
                    r#type: "bigint".into(),
                },
            ],
            rows: vec![vec![
                json!("2026-10-17 19:58:01.577 UTC"),
                json!(5_972_469_351_597_084_165_i64),
                json!(null),
            ]],
            row_count: 1,
            query_id: None,
            stats: None,
            truncated: false,
        };
        let out = super::stringify_snapshot_ids(r);
        assert_eq!(
            out.rows[0][1],
            json!("5972469351597084165"),
            "the id must survive JSON.parse in the browser"
        );
    }
}
