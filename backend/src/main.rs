//! stardelt Nova — HTTP backend for the unified UI.
//!
//! Endpoints:
//!   GET  /api/me                            — current user + demo flag
//!   GET  /api/health                        — backend liveness
//!   GET  /api/trino/cluster                 — Trino /v1/info (or demo stub)
//!   GET  /api/catalog/warehouse             — resolved warehouse id+name
//!   GET  /api/catalog/namespaces            — list namespaces
//!   POST /api/catalog/namespaces            — create a schema
//!   GET  /api/catalog/namespaces/:ns/tables — list tables
//!   GET  /api/catalog/tables/:ns/:tbl       — table metadata
//!   POST /api/query                         — run SQL
//!   GET  /api/jobs                          — recent activity
//!   GET  /api/apps                          — installed apps, reachable URLs, live images
//!   GET  /api/apps/:id                      — one app: workloads, facts, deep links
//!   GET  /api/workloads                     — workloads in Nova's namespace + their images
//!   POST /api/ingest/upload                 — load CSV/JSON into a table
//!   POST /api/transfer                      — copy table → table
//!
//! `NOVA_DEMO=1` serves an in-memory lakehouse so the UI runs with no cluster.

mod apps;
mod catalog;
mod cluster;
mod data;
mod demo;
mod jobs;
mod trino;

use std::sync::{Arc, Mutex, RwLock};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
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
    pub demo: bool,
    pub jobs: Mutex<Vec<jobs::Job>>,
    pub demo_state: RwLock<demo::DemoState>,
    /// Cluster client for the read-only workload view. `None` in demo mode, or when
    /// Nova runs outside a cluster / lacks RBAC — the UI then simply omits images.
    pub kube: Option<kube::Client>,
    pub workloads: cluster::WorkloadCache,
}

#[derive(Serialize)]
struct Me {
    user: String,
    auth_mode: &'static str,
    demo: bool,
    warehouse: String,
}

async fn me(State(s): State<Arc<AppState>>) -> impl IntoResponse {
    Json(Me {
        user: s.dev_user.clone(),
        auth_mode: "static-dev",
        demo: s.demo,
        warehouse: s.warehouse_name.clone(),
    })
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn trino_cluster(State(s): State<Arc<AppState>>) -> Response {
    if s.demo {
        return Json(serde_json::json!({
            "nodeVersion": { "version": "demo" },
            "state": "ACTIVE",
            "demo": true
        }))
        .into_response();
    }
    // Trino 480 dropped /v1/cluster + /v1/node; coordinator status lives at /v1/info.
    proxy_get(&s, format!("{}/v1/info", s.trino_url)).await
}

async fn proxy_get(s: &AppState, url: String) -> Response {
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

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let demo_mode = env_flag("NOVA_DEMO");
    let demo_state = if demo_mode {
        demo::DemoState::seed()
    } else {
        demo::DemoState::default()
    };

    // Read-only cluster access for the workload view. Absence is not an error: Nova
    // runs in demo mode and on laptops, and a cluster without the RBAC below simply
    // shows apps without images.
    let kube = if demo_mode {
        None
    } else {
        match kube::Client::try_default().await {
            Ok(client) => Some(client),
            Err(e) => {
                info!(error = %e, "no cluster client — workload images will be omitted");
                None
            }
        }
    };

    let state = Arc::new(AppState {
        dev_user: std::env::var("NOVA_DEV_USER").unwrap_or_else(|_| "stardelt-dev".into()),
        trino_url: std::env::var("NOVA_TRINO_URL")
            .unwrap_or_else(|_| "http://trino.stardelt.svc.cluster.local:8080".into()),
        lakekeeper_url: std::env::var("NOVA_LAKEKEEPER_URL")
            .unwrap_or_else(|_| "http://lakekeeper.stardelt.svc.cluster.local:8181".into()),
        warehouse_name: std::env::var("NOVA_WAREHOUSE_NAME").unwrap_or_else(|_| "warehouse".into()),
        warehouse_id: OnceCell::new(),
        http: reqwest::Client::builder().build()?,
        demo: demo_mode,
        jobs: Mutex::new(Vec::new()),
        demo_state: RwLock::new(demo_state),
        kube,
        workloads: cluster::WorkloadCache::default(),
    });

    let static_dir = std::env::var("NOVA_STATIC_DIR").unwrap_or_else(|_| "/app/static".into());

    let api = Router::new()
        .route("/me", get(me))
        .route("/health", get(health))
        .route("/trino/cluster", get(trino_cluster))
        .route("/catalog/warehouse", get(catalog::warehouse_info))
        .route(
            "/catalog/namespaces",
            get(catalog::list_namespaces).post(catalog::create_namespace),
        )
        .route("/catalog/namespaces/:ns/tables", get(catalog::list_tables))
        .route("/catalog/tables/:ns/:tbl", get(catalog::table_metadata))
        .route("/query", post(trino::run_query))
        .route("/jobs", get(jobs::list))
        .route("/apps", get(apps::list))
        .route("/apps/:id", get(apps::detail))
        .route("/workloads", get(cluster::list_handler))
        .route("/ingest/upload", post(data::upload))
        .route("/transfer", post(data::transfer));

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
        demo = state.demo,
        "nova backend starting"
    );
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
