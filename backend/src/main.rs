//! stardelt Nova — HTTP backend for the unified UI.
//!
//! Endpoints:
//!   GET  /api/me                            — current user + demo flag
//!   GET  /api/health                        — backend liveness
//!   GET  /api/auth/login                    — start OIDC sign-in (oidc mode)
//!   GET  /api/auth/callback                 — OIDC redirect target (oidc mode)
//!   POST /api/auth/logout                   — sign out locally and at the IdP
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
//! `NOVA_AUTH_MODE=oidc` puts every `/api/*` route except `/api/health` and
//! `/api/auth/*` behind OIDC sign-in (see `auth.rs`); `dev` (default) has no login.

mod api_json;
mod apps;
mod audit;
mod auth;
mod catalog;
mod cluster;
mod data;
mod demo;
mod embed;
mod jobs;
mod trino;

use std::sync::{Arc, Mutex, RwLock};

use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode},
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
    /// Superset and JupyterHub are embedded in Nova's own UI; Nova reads their
    /// headers (and probes them) where they actually run.
    pub superset_url: String,
    pub jupyterhub_url: String,
    pub warehouse_name: String,
    pub warehouse_id: OnceCell<String>,
    pub http: reqwest::Client,
    pub demo: bool,
    /// Forward the caller's own token to Trino and the catalog, when the platform has
    /// configured them to accept it (`NOVA_FORWARD_IDENTITY=1`). Off by default: sending a
    /// bearer to a catalog that does not validate it buys nothing, and turning it on is a
    /// platform decision rather than a UI one.
    pub forward_identity: bool,
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

/// `/api/me` in `oidc` mode: claims, never tokens. Nova still talks to Trino and
/// Lakekeeper with its service connection, and says so (`identity_forwarding`).
#[derive(Serialize)]
struct OidcMe<'a> {
    user: &'a str,
    auth_mode: &'static str,
    demo: bool,
    warehouse: &'a str,
    sub: &'a str,
    email: Option<&'a str>,
    name: Option<&'a str>,
    groups: &'a [String],
    roles: &'a [String],
    groups_overage: bool,
    is_admin: bool,
    via: &'static str,
    identity_forwarding: bool,
}

async fn me(
    State(s): State<Arc<AppState>>,
    principal: Option<Extension<auth::Principal>>,
) -> Response {
    // The auth middleware only runs (and only inserts a principal) in oidc mode;
    // dev mode answers exactly as before.
    match principal {
        None => Json(Me {
            user: s.dev_user.clone(),
            auth_mode: "static-dev",
            demo: s.demo,
            warehouse: s.warehouse_name.clone(),
        })
        .into_response(),
        Some(Extension(p)) => {
            let id = &p.identity;
            Json(OidcMe {
                user: &id.display_name,
                auth_mode: "oidc",
                demo: s.demo,
                warehouse: &s.warehouse_name,
                sub: &id.sub,
                email: id.email.as_deref(),
                name: id.name.as_deref(),
                groups: &id.groups,
                roles: &id.roles,
                groups_overage: id.groups_overage,
                is_admin: id.is_admin,
                via: p.via,
                identity_forwarding: s.forward_identity,
            })
            .into_response()
        }
    }
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
        Err(e) => {
            // The reqwest error names the internal URL it tried. Log that, answer a
            // sentence: every signed-in user sees this body.
            tracing::warn!(error = %e, "upstream request failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "the upstream service is not reachable"})),
            )
                .into_response()
        }
    }
}

/// Any unmatched path under `/api` answers JSON, never the SPA's `index.html`.
///
/// Attached to the nested API router, i.e. outside the auth layer: a script that
/// mistypes a route gets `404 not found` rather than a `401` suggesting a session
/// problem, and the body says nothing about which routes do exist.
async fn api_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "not found"})),
    )
        .into_response()
}

/// Browser hardening for everything Nova serves. Nova is an authenticated console with
/// mutating actions (SQL, loads, transfers) and `/api/*` carries per-user data, so it
/// must not be framed by another origin, sniffed into another type, or kept by anything
/// in between a person and the cluster.
///
/// `or_insert` leaves a header a handler set itself untouched: the sign-in redirects
/// already send their own `cache-control: no-store`. No `script-src`/`default-src` here
/// on purpose — Nova frames Superset and JupyterHub, and a full CSP needs the platform's
/// base domain and a ticket of its own.
async fn security_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.entry("x-content-type-options")
        .or_insert(HeaderValue::from_static("nosniff"));
    h.entry("x-frame-options")
        .or_insert(HeaderValue::from_static("SAMEORIGIN"));
    h.entry("content-security-policy")
        .or_insert(HeaderValue::from_static("frame-ancestors 'self'"));
    h.entry("referrer-policy")
        .or_insert(HeaderValue::from_static("same-origin"));
    if is_api {
        h.entry("cache-control")
            .or_insert(HeaderValue::from_static("no-store"));
    }
    res
}

/// The whole app: the JSON API under `/api`, the SPA everywhere else, and the response
/// headers both share. `main` supplies the real state and static directory; the tests in
/// `router_tests` build the same shape, so a routing or header regression fails in CI
/// instead of at the ingress.
fn app(state: Arc<AppState>, api: Router<Arc<AppState>>, static_dir: &str) -> Router {
    Router::new()
        // The fallback sits on the nested router; see `api_not_found`.
        .nest("/api", api.fallback(api_not_found))
        .fallback_service(
            ServeDir::new(static_dir).fallback(tower_http::services::ServeFile::new(format!(
                "{static_dir}/index.html"
            ))),
        )
        .layer(axum::middleware::from_fn(security_headers))
        // Spans carry the path only: query strings (e.g. the OIDC callback's `code`)
        // stay out of logs.
        .layer(TraceLayer::new_for_http().make_span_with(
            |req: &axum::http::Request<axum::body::Body>| {
                tracing::debug_span!("request", method = %req.method(), path = %req.uri().path())
            },
        ))
        .with_state(state)
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

/// The API route table.
///
/// Deliberately separate from `main`: axum validates path syntax when the router is
/// built and panics on anything it rejects, so a table that only exists inside `main`
/// cannot be checked by any test. That is exactly how the axum 0.8 `:param` → `{param}`
/// change reached a published image and crash-looped the Deployment while CI stayed
/// green — `api_router_builds` below now fails in CI instead.
///
/// These are the routes that sit behind sign-in in oidc mode; `/health` and
/// `/auth/*` are added (unprotected) in `main`, and the operator surfaces live in
/// `admin_router()` so they can carry an administrator check of their own.
fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me", get(me))
        .route("/catalog/warehouse", get(catalog::warehouse_info))
        .route(
            "/catalog/namespaces",
            get(catalog::list_namespaces).post(catalog::create_namespace),
        )
        .route("/catalog/namespaces/{ns}/tables", get(catalog::list_tables))
        .route("/catalog/tables/{ns}/{tbl}", get(catalog::table_metadata))
        .route("/query", post(trino::run_query))
        .route("/jobs", get(jobs::list))
        // Every signed-in user: what the embedded pages need (see apps::embeds). The
        // operator routes live in admin_router() instead.
        .route("/embeds", get(apps::embeds))
        // Same audience, same rule: only what a person can open, never workload data.
        .route("/apps/links", get(apps::links))
        .route("/ingest/upload", post(data::upload))
        .route("/transfer", post(data::transfer))
}

/// The operator surfaces: the service inventory, the workload list and the engine
/// details behind them. Separate from `api_router` because they are the one part of the
/// API an ordinary user has no business reading — `main` puts the administrator check in
/// front of them in `oidc` mode (see `auth::require_admin`).
fn admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/apps", get(apps::list))
        .route("/apps/{id}", get(apps::detail))
        .route("/workloads", get(cluster::list_handler))
        .route("/trino/cluster", get(trino_cluster))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Fail closed: a half-configured oidc mode refuses to start.
    let auth_mode = auth::AuthMode::from_env()
        .map_err(|e| anyhow::anyhow!("refusing to start: auth configuration: {e}"))?;

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
        match kube::Config::infer().await {
            Ok(mut config) => {
                // A half-reachable API server (accepts TCP, never answers) would otherwise
                // stall every workload read for the client's 30 s default — enough to make
                // the UI feel hung on a screen a person is watching. Short timeouts turn
                // that into a fast, honest "the cluster did not answer".
                config.connect_timeout = Some(std::time::Duration::from_secs(3));
                config.read_timeout = Some(std::time::Duration::from_secs(5));
                config.write_timeout = Some(std::time::Duration::from_secs(5));
                match kube::Client::try_from(config) {
                    Ok(client) => Some(client),
                    Err(e) => {
                        info!(error = %e, "no cluster client — workload images will be omitted");
                        None
                    }
                }
            }
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
        superset_url: std::env::var("NOVA_SUPERSET_URL")
            .unwrap_or_else(|_| "http://superset:8088".into()),
        jupyterhub_url: std::env::var("NOVA_JUPYTERHUB_URL")
            .unwrap_or_else(|_| "http://proxy-public:80".into()),
        warehouse_name: std::env::var("NOVA_WAREHOUSE_NAME").unwrap_or_else(|_| "warehouse".into()),
        warehouse_id: OnceCell::new(),
        http: reqwest::Client::builder().build()?,
        demo: demo_mode,
        forward_identity: env_flag("NOVA_FORWARD_IDENTITY"),
        jobs: Mutex::new(Vec::new()),
        demo_state: RwLock::new(demo_state),
        kube,
        workloads: cluster::WorkloadCache::default(),
    });

    let auth = match auth_mode {
        auth::AuthMode::Dev => None,
        auth::AuthMode::Oidc(cfg) => {
            if !cfg.secure_cookies() {
                tracing::warn!(public_url = %cfg.public_url, "NOVA_PUBLIC_URL is not https — session cookies are not Secure");
            }
            let a = auth::AuthState::new(*cfg, reqwest::Client::builder().build()?);
            // Check the IdP now so a wrong issuer is a startup error. An unreachable IdP
            // is not: Nova starts, sign-in fails (closed) and discovery is retried.
            match a.provider.discovery().await {
                Ok(d) => info!(issuer = %d.issuer, "oidc discovery ok"),
                Err(e @ auth::AuthError::Misconfigured(_)) => {
                    anyhow::bail!("refusing to start: {e}")
                }
                Err(e) => {
                    tracing::warn!(reason = %e, "oidc discovery failed — sign-in unavailable until the identity provider answers")
                }
            }
            Some(a)
        }
    };

    let static_dir = std::env::var("NOVA_STATIC_DIR").unwrap_or_else(|_| "/app/static".into());

    let protected = api_router();
    // Everything except /health and /auth/* requires a principal in oidc mode; the
    // admin table additionally requires an administrator. In `dev` there is no IdP and
    // one user, so both tables are served as they always were.
    let api = match &auth {
        Some(a) => {
            // Each layer is applied to the routes already in the router: /health stays
            // outside them (the kubelet probe has no session) and /auth/* is nested after
            // them (sign-in must be reachable unauthenticated). Within the admin table,
            // `require_auth` is applied last and therefore runs first — `require_admin`
            // reads the identity it put in the request extensions.
            // Order, outer to inner: `require_auth` (who are you) → `require_admin`
            // (may you) → `audit` (what did you do). The audit layer is innermost on
            // purpose: it needs the principal, and a refused request never reaches it —
            // refusals are recorded by the two middlewares themselves, with the actor
            // when they know it.
            let authed = protected
                .route_layer(axum::middleware::from_fn(audit::record_mutations))
                .route_layer(axum::middleware::from_fn_with_state(
                    a.clone(),
                    auth::require_auth,
                ))
                .merge(
                    admin_router()
                        .route_layer(axum::middleware::from_fn(audit::record_mutations))
                        .route_layer(axum::middleware::from_fn(auth::require_admin))
                        .route_layer(axum::middleware::from_fn_with_state(
                            a.clone(),
                            auth::require_auth,
                        )),
                );
            Router::new()
                .route("/health", get(health))
                .merge(authed)
                .nest("/auth", auth::router(a.clone()))
        }
        None => Router::new()
            .route("/health", get(health))
            .merge(protected)
            .merge(admin_router())
            .layer(axum::middleware::from_fn(audit::record_mutations))
            .nest("/auth", auth::dev_router()),
    };

    let app = app(state.clone(), api, &static_dir);

    let bind = std::env::var("NOVA_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    info!(
        bind = %bind,
        trino = %state.trino_url,
        lakekeeper = %state.lakekeeper_url,
        warehouse = %state.warehouse_name,
        static_dir = %static_dir,
        demo = state.demo,
        auth = if auth.is_some() { "oidc" } else { "dev" },
        "nova backend starting"
    );
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod router_tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// An `AppState` for the routing and header tests: no cluster, no upstream. Nothing
    /// here is reached by the paths under test.
    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: "http://127.0.0.1:1".into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            warehouse_id: OnceCell::new(),
            http: reqwest::Client::new(),
            demo: true,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: RwLock::new(demo::DemoState::default()),
            kube: None,
            workloads: cluster::WorkloadCache::default(),
        })
    }

    /// A static directory holding only `index.html`, so the SPA fallback behaves the way
    /// the image's `/app/static` does. Removed when the test ends.
    struct StaticDir(std::path::PathBuf);

    impl StaticDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("nova-test-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp static dir");
            std::fs::write(dir.join("index.html"), "<!doctype html><title>nova</title>")
                .expect("index.html");
            Self(dir)
        }

        fn path(&self) -> &str {
            self.0.to_str().expect("utf-8 temp path")
        }
    }

    impl Drop for StaticDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_app(tag: &str) -> (Router, StaticDir) {
        let dir = StaticDir::new(tag);
        let app = app(test_state(), api_router(), dir.path());
        (app, dir)
    }

    async fn get(app: Router, uri: &str) -> Response {
        app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    #[test]
    fn api_router_builds() {
        let _ = api_router();
        let _ = admin_router();
    }

    /// A typo in an API path must not reach the SPA's `index.html`: a caller cannot tell
    /// a 200 HTML page from a result document. One rule: unknown `/api/*` is 404 JSON.
    #[tokio::test]
    async fn unknown_api_path_is_json_404() {
        for uri in [
            "/api/nope",
            "/api",
            "/api/catalog/",
            "/api/auth/nonexistent",
        ] {
            let (app, _dir) = test_app("api-404");
            let response = get(app, uri).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(
                header(&response, "content-type"),
                Some("application/json"),
                "{uri} must answer JSON"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["error"], "not found", "{uri}");
        }
    }

    /// Client-side routes are not API routes: a deep link still gets the SPA.
    #[tokio::test]
    async fn spa_routes_still_serve_index_html() {
        for uri in ["/", "/catalog"] {
            let (app, _dir) = test_app("spa");
            let response = get(app, uri).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert!(
                header(&response, "content-type").is_some_and(|v| v.starts_with("text/html")),
                "{uri} must serve the SPA"
            );
        }
    }

    #[tokio::test]
    async fn security_headers_are_set_on_api_responses() {
        let (app, _dir) = test_app("headers-api");
        let response = get(app, "/api/me").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&response, "x-frame-options"), Some("SAMEORIGIN"));
        assert_eq!(
            header(&response, "content-security-policy"),
            Some("frame-ancestors 'self'")
        );
        assert_eq!(header(&response, "referrer-policy"), Some("same-origin"));
        // `/api/me` carries the signed-in person's sub, email and groups.
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
    }

    #[tokio::test]
    async fn security_headers_are_set_on_the_spa_without_no_store() {
        let (app, _dir) = test_app("headers-spa");
        let response = get(app, "/").await;
        assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&response, "x-frame-options"), Some("SAMEORIGIN"));
        assert_eq!(
            header(&response, "content-security-policy"),
            Some("frame-ancestors 'self'")
        );
        assert_eq!(header(&response, "referrer-policy"), Some("same-origin"));
        // Static assets are cacheable; only the API is `no-store`.
        assert_eq!(header(&response, "cache-control"), None);
    }

    /// The headers also ride on the error path the fallback exists for.
    #[tokio::test]
    async fn unknown_api_path_carries_security_headers() {
        let (app, _dir) = test_app("headers-404");
        let response = get(app, "/api/nope").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
        assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
    }
}
