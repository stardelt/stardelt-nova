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

/// The last 100 snapshots of a table, newest first (Trino 480 Iceberg syntax).
/// Names go through `quote_ident` only — never interpolated raw.
fn history_sql(ns: &str, tbl: &str) -> String {
    format!(
        "SELECT committed_at, snapshot_id, parent_id, operation, summary FROM warehouse.{}.{} ORDER BY committed_at DESC LIMIT 100",
        crate::data::quote_ident(ns),
        crate::data::quote_ident(&format!("{tbl}$snapshots")),
    )
}

/// Move the table's current pointer to an earlier snapshot. The snapshot id is
/// an `i64`, so it is interpolated as an integer — never as a quoted string.
fn rollback_sql(ns: &str, tbl: &str, snapshot_id: i64) -> String {
    format!(
        "ALTER TABLE warehouse.{}.{} EXECUTE rollback_to_snapshot({snapshot_id})",
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

#[derive(Deserialize)]
pub struct RollbackRequest {
    #[serde(deserialize_with = "deserialize_snapshot_id")]
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

pub async fn table_history(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    if s.demo {
        return Json(serde_json::json!({"columns": [], "rows": []})).into_response();
    }
    if path_segment(&ns).is_none() || path_segment(&tbl).is_none() {
        return bad_segment(&format!("{ns}.{tbl}"));
    }
    match crate::trino::execute(&s, history_sql(&ns, &tbl), caller.as_ref().map(|c| &c.0)).await {
        Ok(r) => Json(stringify_snapshot_ids(r)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

pub async fn table_rollback(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    caller: Option<axum::Extension<auth::Principal>>,
    ApiJson(req): ApiJson<RollbackRequest>,
) -> Response {
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
        rollback_sql(&ns, &tbl, req.snapshot_id),
        caller.as_ref().map(|c| &c.0),
    )
    .await
    {
        Ok(r) => Json(r).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
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
    use super::{history_sql, path_segment, rollback_sql};

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
        let sql = history_sql("demo", "t");
        assert!(
            sql.contains("\"t$snapshots\""),
            "metadata table must stay quoted: {sql}"
        );
        assert!(sql.contains("LIMIT 100"), "history is bounded: {sql}");
        assert!(sql.contains("\"demo\""), "namespace must be quoted: {sql}");
    }

    #[test]
    fn rollback_sql_embeds_only_an_integer() {
        assert_eq!(
            rollback_sql("a", "t", 42),
            "ALTER TABLE warehouse.\"a\".\"t\" EXECUTE rollback_to_snapshot(42)"
        );
    }

    #[test]
    fn sql_builders_escape_double_quotes() {
        let sql = history_sql("a\"b", "t");
        assert!(sql.contains("\"a\"\"b\""), "quote must be doubled: {sql}");
        let sql = rollback_sql("a", "x\"y", 7);
        assert!(sql.contains("\"x\"\"y\""), "quote must be doubled: {sql}");
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
