//! stardelt Nova — HTTP backend for the unified UI.
//!
//! Endpoints (MVP first draft):
//!   GET  /api/me                            — static dev user
//!   GET  /api/health                        — backend liveness
//!   GET  /api/trino/cluster                 — proxy Trino /v1/cluster
//!   GET  /api/catalog/warehouse             — resolved Lakekeeper warehouse id+name
//!   GET  /api/catalog/namespaces            — list namespaces
//!   GET  /api/catalog/namespaces/:ns/tables — list tables in a namespace
//!   GET  /api/catalog/tables/:ns/:tbl       — table metadata (schema, partition spec, ...)
//!   POST /api/query                         — submit SQL to Trino, return all rows JSON
//!
//! Static UI assets are served at / (NOVA_STATIC_DIR, default /app/static).
//!
//! Configuration via env:
//!   NOVA_TRINO_URL       (default: http://trino.stardelt.svc.cluster.local:8080)
//!   NOVA_LAKEKEEPER_URL  (default: http://lakekeeper.stardelt.svc.cluster.local:8181)
//!   NOVA_WAREHOUSE_NAME  (default: "warehouse")
//!   NOVA_DEV_USER        (default: "stardelt-dev")
//!   NOVA_BIND_ADDR       (default: 0.0.0.0:8080)
//!   NOVA_STATIC_DIR      (default: /app/static)

mod catalog;
mod trino;

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::{get, post}};
use serde::Serialize;
use tokio::sync::OnceCell;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;

pub struct AppState {
    pub dev_user: String,
    pub trino_url: String,
    pub lakekeeper_url: String,
    pub warehouse_name: String,
    pub warehouse_id: OnceCell<String>,
    pub http: reqwest::Client,
}

#[derive(Serialize)]
struct Me {
    user: String,
    auth_mode: &'static str,
}

async fn me(State(s): State<Arc<AppState>>) -> impl IntoResponse {
    Json(Me {
        user: s.dev_user.clone(),
        auth_mode: "static-dev",
    })
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn trino_cluster(State(s): State<Arc<AppState>>) -> impl IntoResponse {
    // Trino 480 dropped /v1/cluster + /v1/node; coordinator status lives at /v1/info.
    proxy_get(&s, format!("{}/v1/info", s.trino_url)).await
}

async fn proxy_get(s: &AppState, url: String) -> axum::response::Response {
    match s
        .http
        .get(&url)
        .header("X-Trino-User", &s.dev_user)
        .send()
        .await
    {
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
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let state = Arc::new(AppState {
        dev_user: std::env::var("NOVA_DEV_USER").unwrap_or_else(|_| "stardelt-dev".into()),
        trino_url: std::env::var("NOVA_TRINO_URL")
            .unwrap_or_else(|_| "http://trino.stardelt.svc.cluster.local:8080".into()),
        lakekeeper_url: std::env::var("NOVA_LAKEKEEPER_URL")
            .unwrap_or_else(|_| "http://lakekeeper.stardelt.svc.cluster.local:8181".into()),
        warehouse_name: std::env::var("NOVA_WAREHOUSE_NAME").unwrap_or_else(|_| "warehouse".into()),
        warehouse_id: OnceCell::new(),
        http: reqwest::Client::builder().build()?,
    });

    let static_dir = std::env::var("NOVA_STATIC_DIR").unwrap_or_else(|_| "/app/static".into());

    let api = Router::new()
        .route("/me", get(me))
        .route("/health", get(health))
        .route("/trino/cluster", get(trino_cluster))
        .route("/catalog/warehouse", get(catalog::warehouse_info))
        .route("/catalog/namespaces", get(catalog::list_namespaces))
        .route("/catalog/namespaces/:ns/tables", get(catalog::list_tables))
        .route("/catalog/tables/:ns/:tbl", get(catalog::table_metadata))
        .route("/query", post(trino::run_query));

    let app = Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(&static_dir).fallback(
            tower_http::services::ServeFile::new(format!("{static_dir}/index.html")),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let bind = std::env::var("NOVA_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    info!(
        bind = %bind,
        trino = %state.trino_url,
        lakekeeper = %state.lakekeeper_url,
        warehouse = %state.warehouse_name,
        static_dir = %static_dir,
        "nova backend starting"
    );
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
