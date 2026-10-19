//! Lakekeeper catalog proxy, with an in-memory demo branch (`NOVA_DEMO=1`).
//!
//! Lakekeeper's Iceberg REST routes are prefixed by the warehouse UUID, not
//! the warehouse name. We look up the UUID at first request and cache it.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tracing::warn;

use crate::{AppState, api_json::ApiJson, auth, data};

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

#[derive(Deserialize)]
struct WarehouseListItem {
    id: String,
    name: String,
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

pub(crate) async fn resolve_warehouse_id(state: &AppState) -> Result<String, String> {
    state
        .warehouse_id
        .get_or_try_init(|| async {
            let url = format!("{}/management/v1/warehouse", state.lakekeeper_url);
            let resp: WarehouseList = state
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| {
                    warn!("lakekeeper unreachable: {e}");
                    "the catalog is not reachable".to_string()
                })?
                .json()
                .await
                .map_err(|e| {
                    warn!("lakekeeper warehouse list malformed: {e}");
                    "the catalog returned an unexpected response".to_string()
                })?;
            resp.warehouses
                .into_iter()
                .find(|w| w.name == state.warehouse_name)
                .map(|w| w.id)
                .ok_or_else(|| format!("warehouse '{}' not found", state.warehouse_name))
        })
        .await
        .cloned()
}

async fn proxy(state: &AppState, path: String, authorization: Option<&str>) -> Response {
    let url = format!("{}{}", state.lakekeeper_url, path);
    let mut request = state.http.get(&url);
    if let Some(header) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, header);
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

/// Resolve the warehouse id, or the 500 to send back. The error is boxed: an
/// unboxed `Response` in the `Err` variant trips `clippy::result_large_err`, and the
/// happy path is what this function is for.
async fn warehouse_id_or_500(state: &AppState) -> Result<String, Box<Response>> {
    resolve_warehouse_id(state).await.map_err(|e| {
        Box::new(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
                .into_response(),
        )
    })
}

#[utoipa::path(
    get,
    path = "/api/catalog/warehouse",
    tag = "public",
    description = "The resolved warehouse name and id (`demo` in demo mode).",
    responses(
        (status = 200, description = "Warehouse name and id", body = WarehouseInfo),
        (status = 500, description = "The catalog is not reachable or has no such warehouse", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn warehouse_info(State(s): State<Arc<AppState>>) -> Response {
    if s.demo {
        return Json(serde_json::json!({ "name": s.warehouse_name, "id": "demo" })).into_response();
    }
    match resolve_warehouse_id(&s).await {
        Ok(id) => Json(serde_json::json!({ "name": s.warehouse_name, "id": id })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/api/catalog/namespaces",
    tag = "public",
    description = "List catalog namespaces (schemas). Each namespace is a list of levels.",
    responses(
        (status = 200, description = "Namespaces in the warehouse", body = NamespacesResponse),
        (status = 500, description = "The catalog is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list_namespaces(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return Json(demo.namespaces_json()).into_response();
    }
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    proxy(
        &s,
        format!("/catalog/v1/{id}/namespaces"),
        auth::upstream_authorization(s.forward_identity, caller.as_ref().map(|c| &c.0)).as_deref(),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/catalog/namespaces",
    tag = "internal",
    description = "Create a schema via Trino (`CREATE SCHEMA IF NOT EXISTS`). UI-only for now: not promised stable.",
    request_body = CreateNamespaceRequest,
    responses(
        (status = 200, description = "The created schema name", body = CreateNamespaceResponse),
        (status = 400, description = "Missing name, or the engine refused it", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn create_namespace(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(req): ApiJson<CreateNamespaceRequest>,
) -> Response {
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
        "CREATE SCHEMA IF NOT EXISTS warehouse.{}",
        data::quote_ident(ns)
    );
    match crate::trino::execute(&s, sql, caller.as_ref().map(|c| &c.0)).await {
        Ok(_) => Json(serde_json::json!({ "namespace": ns })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/api/catalog/namespaces/{ns}/tables",
    tag = "public",
    description = "List tables in one namespace.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
    ),
    responses(
        (status = 200, description = "Table identifiers in the namespace", body = TablesResponse),
        (status = 400, description = "The name is not a single path segment", body = crate::openapi::ErrorResponse),
        (status = 500, description = "The catalog is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list_tables(
    State(s): State<Arc<AppState>>,
    Path(ns): Path<String>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return Json(demo.tables_json(&ns)).into_response();
    }
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let Some(ns) = path_segment(&ns) else {
        return bad_segment(&ns);
    };
    proxy(
        &s,
        format!("/catalog/v1/{id}/namespaces/{ns}/tables"),
        auth::upstream_authorization(s.forward_identity, caller.as_ref().map(|c| &c.0)).as_deref(),
    )
    .await
}

#[utoipa::path(
    get,
    path = "/api/catalog/tables/{ns}/{tbl}",
    tag = "public",
    description = "Iceberg metadata for one table (metadata location plus the metadata document).",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
    ),
    responses(
        (status = 200, description = "The table metadata document", body = TableMetadataResponse),
        (status = 400, description = "A name is not a single path segment", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such table", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn table_metadata(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
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
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let (Some(ns), Some(tbl)) = (path_segment(&ns), path_segment(&tbl)) else {
        return bad_segment(&format!("{ns}.{tbl}"));
    };
    proxy(
        &s,
        format!("/catalog/v1/{id}/namespaces/{ns}/tables/{tbl}"),
        auth::upstream_authorization(s.forward_identity, caller.as_ref().map(|c| &c.0)).as_deref(),
    )
    .await
}

/// Percent-encode one URL path segment for the Lakekeeper proxy. Axum has already
/// decoded the request path, so a namespace like `a%2F..%2Fb` arrives as `a/../b`;
/// interpolated raw, the URL parser would collapse the dot segments and send the GET
/// to another Lakekeeper endpoint. Encoding keeps it one segment; a bare `.` or `..`
/// (which parsers collapse even when encoded as unreserved characters) or an empty
/// segment is refused.
fn path_segment(raw: &str) -> Option<String> {
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
    use super::path_segment;

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
}
