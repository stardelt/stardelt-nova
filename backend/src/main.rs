//! stardelt Nova — HTTP backend for the unified UI.
//!
//! Endpoints:
//!   GET  /api/me                            — authenticated user (401 if no session when SSO on)
//!   GET  /api/health                        — backend liveness
//!   GET  /api/trino/cluster                 — proxy Trino /v1/info
//!   GET  /api/catalog/warehouse             — resolved Lakekeeper warehouse id+name
//!   GET  /api/catalog/namespaces            — list namespaces
//!   GET  /api/catalog/namespaces/:ns/tables — list tables in a namespace
//!   GET  /api/catalog/tables/:ns/:tbl       — table metadata
//!   POST /api/query                         — submit SQL to Trino
//!   GET  /auth/login                        — redirect to Keycloak (when SSO configured)
//!   GET  /auth/callback                     — exchange code, set session cookie
//!   GET  /auth/logout                       — clear session cookie
//!
//! Static UI assets are served at / (NOVA_STATIC_DIR, default /app/static).
//!
//! Configuration via env:
//!   NOVA_TRINO_URL, NOVA_LAKEKEEPER_URL, NOVA_WAREHOUSE_NAME, NOVA_DEV_USER,
//!   NOVA_BIND_ADDR, NOVA_STATIC_DIR, NOVA_OIDC_ISSUER, NOVA_OIDC_CLIENT_ID,
//!   NOVA_OIDC_CLIENT_SECRET, NOVA_PUBLIC_URL

mod auth;
mod catalog;
mod trino;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::sync::OnceCell;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;

pub struct AppState {
    pub dev_user: String,
    pub sso_enabled: bool,
    pub trino_url: String,
    pub lakekeeper_url: String,
    pub warehouse_name: String,
    pub warehouse_id: OnceCell<String>,
    pub http: reqwest::Client,
}

/// GET /api/me — returns the session user. When SSO is on and there is no
/// session, returns 401 + `{login}` so the SPA can redirect to Keycloak.
async fn me(headers: axum::http::HeaderMap, State(s): State<Arc<AppState>>) -> Response {
    match auth::current_user(&headers) {
        Some(u) => Json(u).into_response(),
        None if s.sso_enabled => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "login": "/auth/login" })),
        )
            .into_response(),
        None => Json(serde_json::json!({
            "sub": s.dev_user,
            "name": s.dev_user,
            "auth_mode": "static-dev",
        }))
        .into_response(),
    }
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn trino_cluster(State(s): State<Arc<AppState>>) -> impl IntoResponse {
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
                [(header::CONTENT_TYPE, "application/json")],
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

    let oidc_cfg = auth::OidcConfig::from_env();
    let oidc_state = auth::OidcState::new(oidc_cfg.is_some());
    if let Some(cfg) = oidc_cfg.clone() {
        oidc_state.spawn_discovery(cfg);
    }

    let state = Arc::new(AppState {
        dev_user: std::env::var("NOVA_DEV_USER").unwrap_or_else(|_| "stardelt-dev".into()),
        sso_enabled: oidc_cfg.is_some(),
        trino_url: std::env::var("NOVA_TRINO_URL")
            .unwrap_or_else(|_| "http://trino.stardelt.svc.cluster.local:8080".into()),
        lakekeeper_url: std::env::var("NOVA_LAKEKEEPER_URL")
            .unwrap_or_else(|_| "http://lakekeeper.stardelt.svc.cluster.local:8181".into()),
        warehouse_name: std::env::var("NOVA_WAREHOUSE_NAME").unwrap_or_else(|_| "warehouse".into()),
        warehouse_id: OnceCell::new(),
        http: reqwest::Client::builder().build()?,
    });

    let static_dir = std::env::var("NOVA_STATIC_DIR").unwrap_or_else(|_| "/app/static".into());

    // Public: health (k8s probes) + me (SPA auth check / returns 401+login).
    let public_api = Router::new()
        .route("/health", get(health))
        .route("/me", get(me));

    // Protected: data endpoints — 401 when SSO on and no session.
    let protected_api = Router::new()
        .route("/trino/cluster", get(trino_cluster))
        .route("/catalog/warehouse", get(catalog::warehouse_info))
        .route("/catalog/namespaces", get(catalog::list_namespaces))
        .route("/catalog/namespaces/:ns/tables", get(catalog::list_tables))
        .route("/catalog/tables/:ns/:tbl", get(catalog::table_metadata))
        .route("/query", post(trino::run_query))
        .layer(axum::middleware::from_fn_with_state(
            oidc_state.sso_configured,
            auth::require_auth,
        ));

    let api = public_api.merge(protected_api);

    // Auth routes (login / callback / logout) — only registered when SSO configured.
    // Cookies are raw Set-Cookie response headers; no CookieManagerLayer needed.
    let mut app = Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(&static_dir).fallback(
            tower_http::services::ServeFile::new(format!("{static_dir}/index.html")),
        ))
        .with_state(state.clone());

    if oidc_state.sso_configured {
        app = app.merge(
            Router::new()
                .route("/auth/login", get(auth::login))
                .route("/auth/callback", get(auth::callback))
                .route("/auth/logout", get(auth::logout))
                .with_state(oidc_state),
        );
    }

    let app = app.layer(TraceLayer::new_for_http());

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
