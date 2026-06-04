//! Lakekeeper catalog proxy.
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

use crate::AppState;

#[derive(Deserialize)]
struct WarehouseListItem {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct WarehouseList {
    warehouses: Vec<WarehouseListItem>,
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

async fn warehouse_id_or_500(state: &AppState) -> Result<String, Response> {
    resolve_warehouse_id(state).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response()
    })
}

pub async fn warehouse_info(State(s): State<Arc<AppState>>) -> Response {
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
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces")).await
}

pub async fn list_tables(State(s): State<Arc<AppState>>, Path(ns): Path<String>) -> Response {
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces/{ns}/tables")).await
}

pub async fn table_metadata(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
) -> Response {
    let id = match warehouse_id_or_500(&s).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    proxy(&s, format!("/catalog/v1/{id}/namespaces/{ns}/tables/{tbl}")).await
}
