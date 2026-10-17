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

use crate::{AppState, data};

#[derive(Deserialize)]
struct WarehouseListItem {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct WarehouseList {
    warehouses: Vec<WarehouseListItem>,
}

#[derive(Deserialize)]
pub struct CreateNamespaceRequest {
    pub namespace: String,
}

async fn resolve_warehouse_id(state: &AppState) -> Result<String, String> {
    state
        .warehouse_id
        .get_or_try_init(|| async {
            let url = format!("{}/management/v1/warehouse", state.lakekeeper_url);
            let resp: WarehouseList = state
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("lakekeeper unreachable: {e}"))?
                .json()
                .await
                .map_err(|e| format!("lakekeeper warehouse list malformed: {e}"))?;
            resp.warehouses
                .into_iter()
                .find(|w| w.name == state.warehouse_name)
                .map(|w| w.id)
                .ok_or_else(|| format!("warehouse '{}' not found", state.warehouse_name))
        })
        .await
        .cloned()
}

async fn proxy(state: &AppState, path: String) -> Response {
    let url = format!("{}{}", state.lakekeeper_url, path);
    match state.http.get(&url).send().await {
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
            warn!("lakekeeper proxy error: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": e.to_string()})),
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

pub async fn list_namespaces(State(s): State<Arc<AppState>>) -> Response {
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return Json(demo.namespaces_json()).into_response();
    }
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces")).await
}

pub async fn create_namespace(
    State(s): State<Arc<AppState>>,
    Json(req): Json<CreateNamespaceRequest>,
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
    match crate::trino::execute(&s, sql).await {
        Ok(_) => Json(serde_json::json!({ "namespace": ns })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

pub async fn list_tables(State(s): State<Arc<AppState>>, Path(ns): Path<String>) -> Response {
    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return Json(demo.tables_json(&ns)).into_response();
    }
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces/{ns}/tables")).await
}

pub async fn table_metadata(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
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
    proxy(&s, format!("/catalog/v1/{id}/namespaces/{ns}/tables/{tbl}")).await
}
