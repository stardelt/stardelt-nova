//! stardelt Nova — HTTP backend for the unified UI.
//!
//! Endpoints:
//!   GET  /api/me                            — current user + demo flag
//!   GET  /api/health                        — backend liveness + state-store status
//!   GET  /api/openapi.json                  — the public API description (unauthenticated, no data)
//!   GET  /api/auth/login                    — start OIDC sign-in (oidc mode)
//!   GET  /api/auth/callback                 — OIDC redirect target (oidc mode)
//!   POST /api/auth/logout                   — sign out locally and at the IdP
//!   GET  /api/trino/cluster                 — Trino /v1/info (or demo stub)
//!   GET  /api/catalog/warehouse             — resolved warehouse id+name
//!   GET  /api/catalog/catalogs              — the catalogs the caller may browse
//!   GET  /api/catalog/metrics               — schemas, tables, rows, bytes, last change per catalog
//!   GET  /api/catalog/namespaces            — list namespaces
//!   POST /api/catalog/namespaces            — create a schema
//!   GET  /api/catalog/namespaces/:ns/tables — list tables
//!   GET  /api/catalog/tables/:ns/:tbl       — table metadata
//!   GET/PUT /api/catalog/tables/:ns/:tbl/documentation — descriptions, tags, owner, domain
//!   PUT  /api/catalog/tables/:ns/:tbl/columns/:column/description — one column's description
//!   GET  /api/catalog/tables/:ns/:tbl/history — last 100 snapshots
//!   POST /api/catalog/tables/:ns/:tbl/rollback — roll back to a snapshot
//!   POST /api/query                         — run SQL
//!   GET  /api/jobs                          — recent activity
//!   GET  /api/saved-queries                 — the caller's saved queries (state store)
//!   POST /api/saved-queries                 — save one: {title, sql, warehouse?}
//!   DELETE /api/saved-queries/:id           — delete one of the caller's own
//!   GET  /api/settings                      — the caller's settings as one JSON object (state store)
//!   PUT  /api/settings/:key                 — set one (body = the JSON value)
//!   DELETE /api/settings/:key               — remove one
//!   GET  /api/apps                          — installed apps, reachable URLs, live images
//!   GET  /api/apps/:id                      — one app: workloads, facts, deep links
//!   GET  /api/workloads                     — workloads in Nova's namespace + their images
//!   GET  /api/admin/catalogs                — every catalog and its grants, administrators only
//!   PUT  /api/admin/catalogs/:name/access   — change owner, managed access and grants (administrators only)
//!   GET  /api/connections                   — the caller's connections (needs the secret store)
//!   POST /api/connections                   — create one: values go to the store, never back
//!   PUT  /api/connections/{scope}/…/secret  — rotate fields
//!   DELETE /api/connections/{scope}/…       — delete one
//!   GET  /api/pipelines                     — the caller's pipelines, with state and last run
//!   GET  /api/pipelines/{ns}/{name}         — one pipeline: latest runs, params, options
//!   POST /api/pipelines/check               — what the engine says about a statement (EXPLAIN)
//!   POST /api/pipelines/preview-schedule    — the next fire times of a cron in its zone
//!   GET  /api/pipelines/{ns}/{name}/runs/{run_id} — one run with its tasks
//!   GET  /api/pipelines/…/tasks/{task_id}/logs — one task's log (paged)
//!   POST /api/ingest/upload                 — load CSV/JSON into a table
//!   POST /api/transfer                      — copy table → table
//!   PUT  /api/notebooks/{group}/{name}      — publish a notebook (outputs stripped)
//!   GET  /api/notebooks                     — the caller's groups' notebooks
//!   GET  /api/notebooks/{group}/{name}/versions — one notebook's versions
//!   GET  /api/notebooks/{group}/{name}/parameters — one version's parameters (Schedule, #237)
//!
//! `NOVA_DEMO=1` serves an in-memory lakehouse so the UI runs with no cluster.
//! `NOVA_AUTH_MODE=oidc` puts every `/api/*` route except `/api/health` and
//! `/api/auth/*` behind OIDC sign-in (see `auth.rs`); `dev` (default) has no login.
//! `NOVA_DATABASE_URL` (a PostgreSQL URL) switches on the state store (`db.rs`); unset,
//! Nova behaves as before and `/api/health` says `"db":"disabled"`.
//! `NOVA_QUERY_TIMEOUT_SECS` bounds how long `POST /api/query` waits for Trino
//! (default 300 s): a slower query is cancelled server-side and answered
//! `400 {"error":"query exceeded the Ns limit and was cancelled"}`.
//! `NOVA_AIRFLOW_URL`, `NOVA_AIRFLOW_USER` and `NOVA_AIRFLOW_PASSWORD` (all three or
//! none) switch on the Airflow client (`airflow.rs`) that the pipeline views use.
//! `NOVA_CATALOG_TOKEN_URL`, `NOVA_CATALOG_CLIENT_ID` and `NOVA_CATALOG_CLIENT_SECRET`
//! (all three or none) make Nova send its own service token on every Lakekeeper call
//! (`catalog_token.rs`); unset, the catalog is called without one.
//! `NOVA_S3_URL`, `NOVA_S3_ACCESS_KEY`, `NOVA_S3_SECRET_KEY` and
//! `NOVA_S3_NOTEBOOK_BUCKET` (all four or none) switch on published notebooks
//! (`notebooks.rs`); unset, the routes answer 503.

mod admin_catalogs;
mod airflow;
mod api_json;
mod apps;
mod audit;
mod auth;
mod catalog;
mod catalog_docs;
mod catalog_metrics;
mod catalog_token;
mod cluster;
mod connection_routes;
mod connections;
mod data;
mod db;
mod demo;
mod embed;
mod jobs;
mod notebooks;
mod openapi;
mod people;
mod pipelines;
// Wired into the collector and the API by the follow-up query-log tickets (stardelt-nova#116).
#[allow(dead_code)]
mod query_log;
// Maps the engine's event payloads to log records; wired up by the collector ticket.
mod query_log_api;
mod query_log_collector;
#[allow(dead_code)]
mod query_log_event;
mod query_log_store;
mod saved_queries;
mod secret_store;
mod trino;
mod user_settings;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde::Serialize;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;
use url::Url;

pub struct AppState {
    pub dev_user: String,
    pub trino_url: String,
    pub lakekeeper_url: String,
    /// Nova's own catalog sign-in (`catalog_token.rs`, stardelt-nova#222). `None` unless
    /// `NOVA_CATALOG_TOKEN_URL`, `NOVA_CATALOG_CLIENT_ID` and `NOVA_CATALOG_CLIENT_SECRET`
    /// are all set: Lakekeeper is then called exactly as before, without a token.
    pub catalog_token: Option<catalog_token::CatalogToken>,
    /// Superset and JupyterHub are embedded in Nova's own UI; Nova reads their
    /// headers (and probes them) where they actually run.
    pub superset_url: String,
    pub jupyterhub_url: String,
    pub warehouse_name: String,
    /// Lakekeeper warehouse ids by catalog name, filled from the warehouse list on the
    /// first miss (`catalog::warehouse_id_for`). One catalog is one warehouse
    /// (`design/multitenant-catalogs.md` §2); this replaces the single id Nova used to
    /// cache, so any catalog can be addressed, not just the configured default.
    pub catalog_ids: RwLock<HashMap<String, String>>,
    /// Which catalogs a person may browse, in the engine's answer, cached for 30 s per
    /// subject (`catalog::allowed_catalogs`). Nova is never the authority: it asks Trino
    /// with the caller's own token and only fails **before** the person tries.
    pub catalog_access: Mutex<HashMap<String, (std::time::Instant, BTreeSet<String>)>>,
    /// What one catalog holds — schemas, tables, rows, bytes, newest commit — cached for
    /// 60 s per catalog (`catalog_metrics.rs`). Caller independent: the numbers describe
    /// the catalog, the caller only decides which catalogs are listed.
    pub catalog_metrics:
        Mutex<HashMap<String, (std::time::Instant, catalog_metrics::CatalogStats)>>,
    /// Which medallion layer each catalog carries (`bronze`, `silver`, `gold`), from the
    /// `stardelt.io/layer` label on the `Catalog` resources and cached for 60 s
    /// (`catalog::catalog_layers`). Caller independent: a layer describes the catalog, the
    /// caller only decides which catalogs are listed.
    pub catalog_layers: Mutex<Option<(std::time::Instant, HashMap<String, String>)>>,
    pub http: reqwest::Client,
    pub demo: bool,
    /// Forward the caller's own token to Trino, when the platform has configured it to
    /// accept it (`NOVA_FORWARD_IDENTITY=1`). Off by default: sending a bearer to an engine
    /// that does not validate it buys nothing, and turning it on is a platform decision
    /// rather than a UI one. The catalog is not in this: Lakekeeper always sees Nova's own
    /// service token (`catalog_token.rs`), because per-user catalog rights do not exist yet.
    pub forward_identity: bool,
    pub jobs: Mutex<Vec<jobs::Job>>,
    pub demo_state: RwLock<demo::DemoState>,
    /// Cluster client for the read-only workload view. `None` in demo mode, or when
    /// Nova runs outside a cluster / lacks RBAC — the UI then simply omits images.
    pub kube: Option<kube::Client>,
    pub workloads: cluster::WorkloadCache,
    /// The platform's secret store (`secret_store.rs`). `None` when `NOVA_SECRET_STORE_URL`
    /// is unset: the Connections API then answers 503 and says so, never panics. Its client
    /// is the shared upstream one, signed in per request with that caller's own token.
    pub store: Option<secret_store::StoreClient>,
    /// The Keycloak admin console for the configured realm, derived from
    /// `NOVA_OIDC_ISSUER` at startup. `None` in dev mode, for a non-Keycloak
    /// issuer, and for the master realm: Nova never links where it cannot manage
    /// users. The frontend only shows the link; the console enforces the roles.
    pub admin_console_url: Option<String>,
    /// The state store (`db.rs`). `None` when `NOVA_DATABASE_URL` is unset: every
    /// feature that needs it must answer 503 then, never panic.
    pub db: Option<sqlx::PgPool>,
    /// In-memory break-glass rate limit: at most five attempted reads per administrator in ten
    /// minutes. Refusals never reach it; only a restart forgets it.
    pub break_glass_reads: connection_routes::BreakGlassLimiter,
    /// In-memory run-start rate limit: at most ten started runs per user in a minute
    /// (stardelt-nova#196). Refusals before the limiter (unloaded, paused) never reach it;
    /// only a restart forgets it.
    pub run_starts: pipelines::RunStartsLimiter,
    /// In-memory create rate limit: at most ten scheduled pipelines per user in an hour
    /// (stardelt-nova#210). A refusal before the limiter (bad name, foreign group, missing
    /// principal, bad body, name taken) never consumes a slot; only a restart forgets it.
    pub pipeline_creates: pipelines::CreateLimiter,
    /// In-memory catalog-permission change limit: at most thirty changes per administrator in
    /// an hour (stardelt-nova#253). A refused body or a missing catalog never consumes a slot;
    /// only a restart forgets it.
    pub catalog_access_changes: admin_catalogs::AccessChangesLimiter,
    /// Airflow's REST API (`airflow.rs`), for the pipeline views. `None` unless
    /// `NOVA_AIRFLOW_URL`, `NOVA_AIRFLOW_USER` and `NOVA_AIRFLOW_PASSWORD` are all set.
    /// Nova signs in as one service user (built-in role `User`) and decides itself who may
    /// see what: Airflow cannot scope that credential to owner groups.
    pub airflow: Option<airflow::AirflowClient>,
    /// The versioned bucket published notebooks live in (`notebooks.rs`). `None` unless
    /// all four `NOVA_S3_*` variables are set: the notebook routes then answer 503 and
    /// say so, never panic.
    pub notebooks: Option<notebooks::NotebookStore>,
    /// One edit lock per table (`"{ns}.{tbl}"`) for the documentation writes. In-process,
    /// so it serialises one Nova replica; more than one replica needs a database lock (a
    /// later ticket). Lakekeeper 0.12.2 deadlocks on concurrent commits to one table
    /// instead of answering 409, which is why this exists (stardelt-nova#189).
    pub table_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// When each subject was last written to the person directory (`people::touch`):
    /// at most one write per person per ten minutes. In-process, like the other rate
    /// limits; a restart writes once more per person.
    pub people_touched: people::TouchLimiter,
}

#[derive(Serialize, utoipa::ToSchema)]
struct Me {
    user: String,
    auth_mode: &'static str,
    demo: bool,
    warehouse: String,
    /// `None` in `dev` mode: there is no IdP, so no console.
    admin_console_url: Option<String>,
}

/// `/api/me` in `oidc` mode: claims, never tokens. Nova still talks to Trino and
/// Lakekeeper with its service connection, and says so (`identity_forwarding`).
#[derive(Serialize, utoipa::ToSchema)]
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
    /// Realm admin console derived from the issuer; `None` for non-Keycloak issuers.
    admin_console_url: Option<&'a str>,
    /// The configured value behind this caller's administrator decision: the group
    /// matched, or the role held (and the configured precedence for a member). `None`
    /// when neither is configured (#213).
    admin_group: Option<&'a str>,
    /// `"group"` or `"role"`: which of the two `admin_group` is. `None` with it.
    admin_by: Option<&'static str>,
    /// Unix seconds at which the browser session can no longer be renewed and the
    /// person signs in again; `None` for an Authorization-header call, which has no
    /// session. Never the access token's own `exp`.
    session_expires_at: Option<u64>,
}

#[utoipa::path(
    get,
    path = "/api/me",
    tag = "public",
    description = "The caller: static user in `dev` mode, verified claims (never tokens) in `oidc` mode. The `oidc` shape is the documented `OidcMe` superset of this one.",
    responses(
        (status = 200, description = "Current user, auth mode, demo flag and warehouse", body = Me),
    )
)]
pub(crate) async fn me(
    State(s): State<Arc<AppState>>,
    principal: Option<Extension<auth::Principal>>,
    facts: Option<Extension<auth::MeFacts>>,
) -> Response {
    // The auth middleware only runs (and only inserts a principal) in oidc mode;
    // dev mode answers exactly as before.
    match principal {
        None => Json(Me {
            user: s.dev_user.clone(),
            auth_mode: "static-dev",
            demo: s.demo,
            warehouse: s.warehouse_name.clone(),
            admin_console_url: None,
        })
        .into_response(),
        Some(Extension(p)) => {
            let id = &p.identity;
            // `require_auth` inserts the facts next to the principal; a router that
            // mounts the handler without it answers the claim fields as `null`.
            let facts = facts.map(|Extension(f)| f).unwrap_or_default();
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
                admin_console_url: s.admin_console_url.as_deref(),
                admin_group: facts.admin_group.as_deref(),
                admin_by: facts.admin_by,
                session_expires_at: facts.session_expires_at,
            })
            .into_response()
        }
    }
}

/// Always `200`: the kubelet's readiness probe must not take Trino queries offline because
/// the state store is down. The body says what is wrong: `db` is `up`, `down` or `disabled`.
async fn health(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "db": db::status(s.db.as_ref()).await,
    }))
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

/// Add the platform's internal CA to a client builder when the platform hands one over
/// (`NOVA_TRINO_CA_PEM`). Every upstream client Nova builds goes through this, so a caller
/// that adds its own options (a short timeout, a redirect policy) cannot lose the trust
/// settings the shared client has.
///
/// The Trino coordinator's in-cluster TLS certificate is signed by a
/// namespace-local CA (`manifests/trino-tls.yaml` in stardelt-platform), which
/// the public WebPKI roots of course do not contain — and pointing Nova at the
/// public hostname instead would hairpin every query through the ingress and
/// fail on an air-gapped install. The value is a Secret read through
/// `secretKeyRef` (Secret `trino-ca`, key `tls.crt`) and, like the other
/// identity variables, optional: a dev install without an IdP has no HTTPS
/// listener and no certificate. The CA is *added* to the public roots; nothing
/// disables verification, and a value that is not a certificate is a startup
/// error rather than a silently ignored variable.
pub(crate) fn add_platform_ca(
    mut builder: reqwest::ClientBuilder,
) -> anyhow::Result<reqwest::ClientBuilder> {
    if let Ok(pem) = std::env::var("NOVA_TRINO_CA_PEM")
        && !pem.trim().is_empty()
    {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("NOVA_TRINO_CA_PEM is not a certificate: {e}"))?;
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }
    Ok(builder)
}

/// The shared upstream HTTP client (`AppState.http`), built from [`add_platform_ca`].
fn http_client() -> anyhow::Result<reqwest::Client> {
    if let Ok(pem) = std::env::var("NOVA_TRINO_CA_PEM")
        && !pem.trim().is_empty()
    {
        info!(
            certificates = pem.matches("BEGIN CERTIFICATE").count(),
            "trusting the platform's internal CA (NOVA_TRINO_CA_PEM)"
        );
    }
    Ok(add_platform_ca(reqwest::Client::builder())?.build()?)
}

/// The realm's Keycloak admin console, derived from the configured issuer.
///
/// A Keycloak issuer ends in `/realms/<realm>` (optionally behind a base path,
/// e.g. `/auth/realms/<realm>`), so the console is `…/admin/<realm>/console/`.
/// Anything else — `dev` mode (no issuer), an external provider such as Entra ID,
/// the master realm, or an issuer with trailing segments — has no console Nova
/// could address, so the answer is `None` and the UI shows no Users link.
fn admin_console_url(issuer: &str) -> Option<String> {
    let mut url = Url::parse(issuer).ok()?;
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let realms_at = segments.iter().rposition(|s| *s == "realms")?;
    // Exactly `/realms/<realm>` at the end; a deeper path is not a realm issuer.
    if realms_at + 2 != segments.len() {
        return None;
    }
    let realm = segments[realms_at + 1];
    if realm == "master" {
        return None;
    }
    let base = segments[..realms_at].join("/");
    let path = if base.is_empty() {
        format!("/admin/{realm}/console/")
    } else {
        format!("/{base}/admin/{realm}/console/")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

/// Routes that stay outside sign-in even in `oidc` mode: the kubelet probe and
/// the API description. The document carries shapes, never data, so gating it
/// would buy nothing and cost every unauthenticated client (editors, CI) a
/// login. Everything else in `api_router` / `admin_router` requires a
/// principal in `oidc` mode.
fn public_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi::serve))
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
/// `/openapi.json` are added (unprotected) via `public_router()` in `main`,
/// and `/auth/*` is nested after them. The operator surfaces live in
/// `admin_router()` so they can carry an administrator check of their own.
fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me", get(me))
        .route("/catalog/warehouse", get(catalog::warehouse_info))
        .route("/catalog/catalogs", get(catalog::list_catalogs))
        // What is in each of them (stardelt-nova#249): one row per catalog, cached 60 s.
        .route("/catalog/metrics", get(catalog_metrics::get))
        .route(
            "/catalog/namespaces",
            get(catalog::list_namespaces).post(catalog::create_namespace),
        )
        .route("/catalog/namespaces/{ns}/tables", get(catalog::list_tables))
        .route("/catalog/tables/{ns}/{tbl}", get(catalog::table_metadata))
        .route(
            "/catalog/tables/{ns}/{tbl}/documentation",
            get(catalog_docs::read).put(catalog_docs::update),
        )
        .route(
            "/catalog/tables/{ns}/{tbl}/columns/{column}/description",
            put(catalog_docs::update_column),
        )
        .route(
            "/catalog/tables/{ns}/{tbl}/history",
            get(catalog::table_history),
        )
        .route(
            "/catalog/tables/{ns}/{tbl}/rollback",
            post(catalog::table_rollback),
        )
        .route("/query", post(trino::run_query))
        .route("/jobs", get(jobs::list))
        // The caller's own saved queries (state store; 503 while it is not configured).
        .route(
            "/saved-queries",
            get(saved_queries::list).post(saved_queries::create),
        )
        .route("/saved-queries/{id}", delete(saved_queries::remove))
        // The query log: the caller's own queries, everybody's for administrators (state store).
        .route("/query-log", get(query_log_api::list))
        .route("/query-log/{id}", get(query_log_api::detail))
        // The caller's own settings: one JSON value per key (state store).
        .route("/settings", get(user_settings::list))
        .route(
            "/settings/{key}",
            put(user_settings::put).delete(user_settings::remove),
        )
        // Every signed-in user: what the embedded pages need (see apps::embeds). The
        // operator routes live in admin_router() instead.
        .route("/embeds", get(apps::embeds))
        // Same audience, same rule: only what a person can open, never workload data.
        .route("/apps/links", get(apps::links))
        // Connections: a person's own, their groups', and administrators' managed ones. The
        // secret store is signed in with the caller's own token (503 while it is not configured).
        .route(
            "/connections",
            get(connection_routes::list).post(connection_routes::create),
        )
        .route(
            "/connections/personal/{name}/secret",
            put(connection_routes::rotate_personal),
        )
        .route(
            "/connections/group/{group}/{name}/secret",
            put(connection_routes::rotate_group),
        )
        .route(
            "/connections/managed/{name}/secret",
            put(connection_routes::rotate_managed),
        )
        .route(
            "/connections/personal/{name}",
            delete(connection_routes::delete_personal),
        )
        .route(
            "/connections/group/{group}/{name}",
            delete(connection_routes::delete_group),
        )
        .route(
            "/connections/managed/{name}",
            delete(connection_routes::delete_managed),
        )
        // Break-glass: administrators only, group connections only, every attempt audited.
        .route(
            "/connections/group/{group}/{name}/break-glass",
            post(connection_routes::break_glass_group),
        )
        .route(
            "/connections/personal/{name}/break-glass",
            post(connection_routes::break_glass_personal),
        )
        .route(
            "/connections/managed/{name}/break-glass",
            post(connection_routes::break_glass_managed),
        )
        .route(
            "/connections/break-glass-log",
            get(connection_routes::break_glass_log),
        )
        .route("/ingest/upload", post(data::upload))
        .route("/transfer", post(data::transfer))
        // Pipelines: only the caller's groups' pipelines, administrators see all. Nova checks
        // the resource before it asks Airflow; a hidden pipeline is 404. Uncacheable.
        .merge(pipelines::router())
        // Published notebooks: immutable versions, owner-group scoped (stardelt-nova#225).
        .merge(notebooks::router())
}

/// The operator surfaces: the service inventory, the workload list, the engine
/// details and the catalog permissions view. Separate from `api_router` because
/// they are the one part of the API an ordinary user has no business reading —
/// `main` puts the administrator check in front of them in `oidc` mode (see
/// `auth::require_admin`).
fn admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/apps", get(apps::list))
        .route("/apps/{id}", get(apps::detail))
        .route("/workloads", get(cluster::list_handler))
        .route("/trino/cluster", get(trino_cluster))
        // Every `Catalog` of the platform and the grants on it (stardelt-nova#251):
        // the same administrator gate as the rest of this table, in `oidc` mode.
        .route("/admin/catalogs", get(admin_catalogs::list))
        // ...and the one write: a merge patch on the resource, never Lakekeeper (#253).
        .route(
            "/admin/catalogs/{name}/access",
            put(admin_catalogs::update_access),
        )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Fail closed: a half-configured oidc mode refuses to start.
    let auth_mode = auth::AuthMode::from_env()
        .map_err(|e| anyhow::anyhow!("refusing to start: auth configuration: {e}"))?;
    // Derived once, from the same issuer tokens are validated against. The frontend
    // never hard-codes a Keycloak host.
    let admin_console = match &auth_mode {
        auth::AuthMode::Oidc(cfg) => admin_console_url(&cfg.issuer),
        auth::AuthMode::Dev => None,
    };

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

    // Optional state store. Unset = disabled. Set = must connect and migrate, or Nova
    // refuses to start (the URL holds the password and is never logged).
    let db = match db::url_from_env() {
        None => {
            info!("NOVA_DATABASE_URL is not set — the state store is disabled");
            None
        }
        Some(url) => {
            let pool = db::connect(&url)
                .await
                .map_err(|e| anyhow::anyhow!("refusing to start: {e}"))?;
            db::migrate(&pool)
                .await
                .map_err(|e| anyhow::anyhow!("refusing to start: {e}"))?;
            info!("state store connected, migrations applied");
            Some(pool)
        }
    };

    // One client for every upstream call, so the platform CA is trusted everywhere. The secret
    // store is plain HTTP in-cluster, so it adds no trust configuration of its own.
    let http = http_client()?;
    let store = secret_store::StoreConfig::from_env()
        .map(|cfg| secret_store::StoreClient::new(http.clone(), cfg));
    if store.is_none() {
        info!("NOVA_SECRET_STORE_URL is not set — the Connections API answers 503");
    }

    // Airflow is plain HTTP in-cluster and adds no trust configuration of its own. All three
    // variables or none: a half-configured client is a platform mistake, not a runtime guess.
    let airflow = airflow::AirflowConfig::from_env()
        .map(|cfg| airflow::AirflowClient::new(http.clone(), cfg));
    if airflow.is_none() {
        info!(
            "NOVA_AIRFLOW_URL, NOVA_AIRFLOW_USER and NOVA_AIRFLOW_PASSWORD are not all set — the pipelines API is disabled"
        );
    }

    // Nova's own sign-in at the catalog (stardelt-nova#222). All three variables or none;
    // a partial one refuses to start rather than calling Lakekeeper unauthenticated. The
    // client secret and every token stay in `Secret` — nothing here logs either.
    let catalog_token = match catalog_token::CatalogTokenConfig::from_env() {
        Ok(None) => {
            info!("NOVA_CATALOG_TOKEN_URL is not set — catalog calls carry no service token");
            None
        }
        Ok(Some(cfg)) => {
            info!(
                token_url = %cfg.token_url,
                client_id = %cfg.client_id,
                "catalog service token configured"
            );
            Some(catalog_token::CatalogToken::new(http.clone(), cfg))
        }
        Err(e) => anyhow::bail!("refusing to start: catalog token configuration: {e}"),
    };

    // Published notebooks (stardelt-nova#225). All four variables or none; a partial one
    // refuses to start rather than answering 503 for a bucket the platform meant to wire.
    // The secret key is a `Secret` and never printed.
    let notebooks = match notebooks::NotebookConfig::from_env() {
        Ok(None) => {
            info!("NOVA_S3_URL is not set — notebook publishing answers 503");
            None
        }
        Ok(Some(cfg)) => {
            info!(bucket = %cfg.bucket, endpoint = %cfg.endpoint, "notebook storage configured");
            Some(
                notebooks::NotebookStore::new(http.clone(), cfg)
                    .map_err(|e| anyhow::anyhow!("refusing to start: notebook storage: {e}"))?,
            )
        }
        Err(e) => anyhow::bail!("refusing to start: notebook storage configuration: {e}"),
    };

    let state = Arc::new(AppState {
        dev_user: std::env::var("NOVA_DEV_USER").unwrap_or_else(|_| "stardelt-dev".into()),
        trino_url: std::env::var("NOVA_TRINO_URL")
            .unwrap_or_else(|_| "http://trino.stardelt.svc.cluster.local:8080".into()),
        lakekeeper_url: std::env::var("NOVA_LAKEKEEPER_URL")
            .unwrap_or_else(|_| "http://lakekeeper.stardelt.svc.cluster.local:8181".into()),
        catalog_token,
        superset_url: std::env::var("NOVA_SUPERSET_URL")
            .unwrap_or_else(|_| "http://superset:8088".into()),
        jupyterhub_url: std::env::var("NOVA_JUPYTERHUB_URL")
            .unwrap_or_else(|_| "http://proxy-public:80".into()),
        warehouse_name: std::env::var("NOVA_WAREHOUSE_NAME").unwrap_or_else(|_| "warehouse".into()),
        catalog_ids: RwLock::new(HashMap::new()),
        catalog_access: Mutex::new(HashMap::new()),
        catalog_metrics: Mutex::new(HashMap::new()),
        catalog_layers: Mutex::new(None),
        http,
        demo: demo_mode,
        forward_identity: env_flag("NOVA_FORWARD_IDENTITY"),
        jobs: Mutex::new(Vec::new()),
        demo_state: RwLock::new(demo_state),
        kube,
        workloads: cluster::WorkloadCache::default(),
        admin_console_url: admin_console,
        db,
        store,
        break_glass_reads: Mutex::new(std::collections::HashMap::new()),
        run_starts: Mutex::new(std::collections::HashMap::new()),
        pipeline_creates: Mutex::new(std::collections::HashMap::new()),
        catalog_access_changes: Mutex::new(std::collections::HashMap::new()),
        airflow,
        table_locks: Mutex::new(HashMap::new()),
        people_touched: Mutex::new(HashMap::new()),
        notebooks,
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
    // Everything except /health, /openapi.json and /auth/* requires a principal
    // in oidc mode; the admin table additionally requires an administrator. In `dev` there is no IdP and
    // one user, so both tables are served as they always were.
    let api = match &auth {
        Some(a) => {
            let auth_context = auth::AuthContext {
                auth: a.clone(),
                app: state.clone(),
            };
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
                    auth_context.clone(),
                    auth::require_auth,
                ))
                .merge(
                    admin_router()
                        .route_layer(axum::middleware::from_fn(audit::record_mutations))
                        .route_layer(axum::middleware::from_fn(auth::require_admin))
                        .route_layer(axum::middleware::from_fn_with_state(
                            auth_context.clone(),
                            auth::require_auth,
                        )),
                );
            Router::new()
                .merge(public_router())
                .merge(authed)
                .nest("/auth", auth::router(a.clone()))
        }
        None => Router::new()
            .merge(public_router())
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
        store = state.store.is_some(),
        catalog_token = state.catalog_token.is_some(),
        notebooks = state.notebooks.is_some(),
        "nova backend starting"
    );
    // Second listener for the engine's event feed; off unless NOVA_INTERNAL_ADDR is set.
    query_log_collector::start(state.db.clone()).await?;
    if let Some(pool) = state.db.clone() {
        query_log_store::spawn_retention(pool);
    }
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
    /// here is reached by the paths under test. Shared with `auth.rs`, whose `/api/me`
    /// tests go through the real handler.
    pub(crate) fn test_state() -> Arc<AppState> {
        test_state_with_db(None)
    }

    pub(crate) fn test_state_with_db(db: Option<sqlx::PgPool>) -> Arc<AppState> {
        build_test_state(
            db,
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            true,
            None,
            None,
        )
    }

    /// An `AppState` with a cluster client (`admin_catalogs.rs`): the `Catalog` rows come
    /// from the API server the client points at, not from the demo branch.
    pub(crate) fn test_state_with_kube(kube: kube::Client) -> Arc<AppState> {
        build_test_state(
            None,
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            false,
            None,
            Some(kube),
        )
    }

    /// An `AppState` for the catalog-token tests (`catalog_token.rs`): not demo, so the
    /// handlers really call Lakekeeper, with the fakes' URLs and the service-token client
    /// under test (or `None` for today's unauthenticated behaviour).
    pub(crate) fn catalog_test_state(
        lakekeeper_url: &str,
        catalog_token: Option<catalog_token::CatalogToken>,
    ) -> Arc<AppState> {
        build_test_state(
            None,
            "http://127.0.0.1:1",
            lakekeeper_url,
            false,
            catalog_token,
            None,
        )
    }

    /// An `AppState` for the catalog tests (`catalog.rs`): not demo, both upstreams are
    /// fakes, so `SHOW CATALOGS` and the warehouse list are answered by the test.
    pub(crate) fn catalog_scope_test_state(trino_url: &str, lakekeeper_url: &str) -> Arc<AppState> {
        build_test_state(None, trino_url, lakekeeper_url, false, None, None)
    }

    fn build_test_state(
        db: Option<sqlx::PgPool>,
        trino_url: &str,
        lakekeeper_url: &str,
        demo_mode: bool,
        catalog_token: Option<catalog_token::CatalogToken>,
        kube: Option<kube::Client>,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: trino_url.into(),
            lakekeeper_url: lakekeeper_url.into(),
            catalog_token,
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            catalog_ids: RwLock::new(HashMap::new()),
            catalog_access: Mutex::new(HashMap::new()),
            catalog_metrics: Mutex::new(HashMap::new()),
            catalog_layers: Mutex::new(None),
            http: reqwest::Client::new(),
            demo: demo_mode,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: RwLock::new(demo::DemoState::default()),
            kube,
            workloads: cluster::WorkloadCache::default(),
            admin_console_url: admin_console_url("https://id.example.com/realms/stardelt"),
            db,
            store: None,
            break_glass_reads: Mutex::new(std::collections::HashMap::new()),
            run_starts: Mutex::new(std::collections::HashMap::new()),
            pipeline_creates: Mutex::new(std::collections::HashMap::new()),
            catalog_access_changes: Mutex::new(std::collections::HashMap::new()),
            airflow: None,
            table_locks: Mutex::new(std::collections::HashMap::new()),
            people_touched: Mutex::new(std::collections::HashMap::new()),
            notebooks: None,
        })
    }

    #[test]
    fn admin_console_url_from_keycloak_issuer() {
        assert_eq!(
            admin_console_url("https://id.example.com/realms/stardelt").as_deref(),
            Some("https://id.example.com/admin/stardelt/console/")
        );
        // A trailing slash on the issuer is normal in Keycloak discovery documents.
        assert_eq!(
            admin_console_url("https://id.example.com/realms/stardelt/").as_deref(),
            Some("https://id.example.com/admin/stardelt/console/")
        );
        // Keycloak behind a base path keeps it.
        assert_eq!(
            admin_console_url("https://example.com/auth/realms/stardelt").as_deref(),
            Some("https://example.com/auth/admin/stardelt/console/")
        );
        // A realm console, not realm ownership, and not another provider.
        assert_eq!(
            admin_console_url("https://id.example.com/realms/master"),
            None
        );
        assert_eq!(
            admin_console_url("https://login.microsoftonline.com/tenant/v2.0"),
            None
        );
        assert_eq!(
            admin_console_url("https://id.example.com/realms/stardelt/extra"),
            None
        );
        assert_eq!(admin_console_url("not a url"), None);
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

    /// `GET /api/catalog/catalogs` is wired next to `/catalog/warehouse` and answers the
    /// shape the UI reads: the demo catalog, its id and the default, never cached.
    #[tokio::test]
    async fn the_catalog_list_route_answers_the_demo_catalog() {
        let (app, _dir) = test_app("catalog-list");
        let response = get(app, "/api/catalog/catalogs").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["default"], "warehouse");
        assert_eq!(
            body["catalogs"],
            serde_json::json!([{ "name": "warehouse", "id": "demo" }])
        );
    }

    /// Demo mode has one catalog: a name it does not have is refused before any upstream
    /// is called, and an engine catalog name is refused as invalid.
    #[tokio::test]
    async fn a_catalog_the_caller_may_not_browse_is_refused_before_the_catalog() {
        let (app, _dir) = test_app("catalog-scope");
        let response = get(app.clone(), "/api/catalog/namespaces?catalog=finance").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = get(app.clone(), "/api/catalog/namespaces?catalog=System").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = get(app, "/api/catalog/namespaces").await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the default catalog still answers in demo mode"
        );
    }

    /// `GET /api/openapi.json` is served outside sign-in and carries the
    /// generated document with the crate version — shapes, never data.
    #[tokio::test]
    async fn openapi_document_is_public_and_versioned() {
        let dir = StaticDir::new("openapi");
        // Same shape as `main` in dev mode: public routes merged next to the
        // API table, no auth layer anywhere.
        let api = Router::new()
            .merge(public_router())
            .merge(api_router())
            .merge(admin_router());
        let app = app(test_state(), api, dir.path());
        let response = get(app, "/api/openapi.json").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header(&response, "content-type"),
            Some("application/json"),
            "the document is JSON"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["info"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "the version field follows the crate version"
        );
        assert!(
            value["paths"].get("/api/me").is_some(),
            "the document describes /api/me"
        );
    }

    /// Without `NOVA_DATABASE_URL` the probe still answers 200 and says why.
    #[tokio::test]
    async fn health_reports_a_disabled_state_store() {
        let dir = StaticDir::new("health");
        // `/health` lives in `public_router()`, not in `api_router()`.
        let api = Router::new().merge(public_router()).merge(api_router());
        let app = app(test_state(), api, dir.path());
        let response = get(app, "/api/health").await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value, serde_json::json!({"status": "ok", "db": "disabled"}));
    }

    /// One request through `app`; the answer's status and JSON body (`null` when empty).
    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    /// Without `NOVA_DATABASE_URL` the saved-query routes say so, in JSON; a path that
    /// cannot be an id is a plain 404.
    #[tokio::test]
    async fn saved_queries_answer_503_without_a_state_store() {
        let (app, _dir) = test_app("saved-503");
        let unavailable = serde_json::json!({"error": "the state store is not configured"});
        let (status, body) = call(&app, "GET", "/api/saved-queries", None).await;
        assert_eq!(
            (status, body),
            (StatusCode::SERVICE_UNAVAILABLE, unavailable.clone())
        );
        let post = r#"{"title":"t","sql":"select 1"}"#;
        let (status, body) = call(&app, "POST", "/api/saved-queries", Some(post)).await;
        assert_eq!(
            (status, body),
            (StatusCode::SERVICE_UNAVAILABLE, unavailable.clone())
        );
        let (status, body) = call(&app, "DELETE", "/api/saved-queries/7", None).await;
        assert_eq!(
            (status, body),
            (StatusCode::SERVICE_UNAVAILABLE, unavailable)
        );
        let (status, body) = call(&app, "DELETE", "/api/saved-queries/abc", None).await;
        assert_eq!(
            (status, body),
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({"error": "not found"})
            )
        );
        // A malformed body is still the JSON 4xx every route answers.
        let (status, body) = call(&app, "POST", "/api/saved-queries", Some("{bad")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].is_string());
    }

    /// A signed-in caller, as `require_auth` would have put it in the request.
    fn signed_in(sub: &str) -> auth::Principal {
        auth::Principal {
            identity: auth::Identity {
                sub: sub.into(),
                display_name: sub.into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: false,
            },
            via: "session",
            access_token: None,
        }
    }

    /// The API on a real database, with `who` signed in (`None` = `dev` mode).
    fn saved_query_app(pool: sqlx::PgPool, who: Option<&str>, dir: &StaticDir) -> Router {
        let mut api = api_router();
        if let Some(sub) = who {
            api = api.layer(Extension(signed_in(sub)));
        }
        app(test_state_with_db(Some(pool)), api, dir.path())
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn saved_queries_belong_to_their_owner() {
        let pool = db::test_support::migrated_pool().await;
        let dir = StaticDir::new("saved-owner");
        let alice = saved_query_app(pool.clone(), Some("alice-sub"), &dir);
        let bob = saved_query_app(pool.clone(), Some("bob-sub"), &dir);
        let dev = saved_query_app(pool, None, &dir);

        // Alice saves one: 201 and the stored row, trimmed.
        let post = r#"{"title":" Daily trips ","sql":"select count(*) from trips","warehouse":"warehouse"}"#;
        let (status, row) = call(&alice, "POST", "/api/saved-queries", Some(post)).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(row["title"], "Daily trips");
        assert_eq!(row["sql"], "select count(*) from trips");
        assert_eq!(row["warehouse"], "warehouse");
        assert!(row["created_at"].as_i64().unwrap() > 1_700_000_000_000);
        let id = row["id"].as_i64().unwrap();

        // She sees it; Bob and the dev user do not.
        let (status, list) = call(&alice, "GET", "/api/saved-queries", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], id);
        assert_eq!(
            call(&bob, "GET", "/api/saved-queries", None).await.1,
            serde_json::json!([])
        );
        assert_eq!(
            call(&dev, "GET", "/api/saved-queries", None).await.1,
            serde_json::json!([])
        );

        // Bob cannot delete it, and cannot tell it exists: the same 404 as a missing id.
        let uri = format!("/api/saved-queries/{id}");
        let (status, body) = call(&bob, "DELETE", &uri, None).await;
        assert_eq!(
            (status, body),
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({"error": "not found"})
            )
        );
        let (status, _) = call(&alice, "GET", "/api/saved-queries", None).await;
        assert_eq!(status, StatusCode::OK);

        // Alice can; a second delete is a 404.
        assert_eq!(
            call(&alice, "DELETE", &uri, None).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&alice, "DELETE", &uri, None).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&alice, "GET", "/api/saved-queries", None).await.1,
            serde_json::json!([])
        );
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn saved_queries_in_dev_mode_belong_to_the_dev_user_and_are_validated() {
        let pool = db::test_support::migrated_pool().await;
        let dir = StaticDir::new("saved-dev");
        let app = saved_query_app(pool, None, &dir);

        let (status, body) = call(
            &app,
            "POST",
            "/api/saved-queries",
            Some(r#"{"title":" ","sql":"select 1"}"#),
        )
        .await;
        assert_eq!(
            (status, body),
            (
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "title must not be empty"})
            )
        );
        let (status, body) =
            call(&app, "POST", "/api/saved-queries", Some(r#"{"title":"t"}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body["error"].is_string());

        let (status, _) = call(
            &app,
            "POST",
            "/api/saved-queries",
            Some(r#"{"title":"one","sql":"select 1"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, second) = call(
            &app,
            "POST",
            "/api/saved-queries",
            Some(r#"{"title":"two","sql":"select 2"}"#),
        )
        .await;
        // Newest first.
        let (_, list) = call(&app, "GET", "/api/saved-queries", None).await;
        assert_eq!(list[0]["id"], second["id"]);
        assert_eq!(list[0]["warehouse"], "");
        assert_eq!(list.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn settings_answer_503_without_a_state_store() {
        let (app, _dir) = test_app("settings-503");
        let unavailable = serde_json::json!({"error": "the state store is not configured"});
        for (method, uri, body) in [
            ("GET", "/api/settings", None),
            ("PUT", "/api/settings/theme", Some(r#""dark""#)),
            ("DELETE", "/api/settings/theme", None),
        ] {
            let (status, answer) = call(&app, method, uri, body).await;
            assert_eq!(
                (status, answer),
                (StatusCode::SERVICE_UNAVAILABLE, unavailable.clone()),
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn settings_are_per_person_and_validated() {
        let pool = db::test_support::migrated_pool().await;
        let dir = StaticDir::new("settings");
        let alice = saved_query_app(pool.clone(), Some("alice-sub"), &dir);
        let bob = saved_query_app(pool, Some("bob-sub"), &dir);

        assert_eq!(
            call(&alice, "GET", "/api/settings", None).await.1,
            serde_json::json!({})
        );
        let (status, body) = call(&alice, "PUT", "/api/settings/theme", Some(r#""dark""#)).await;
        assert_eq!(
            (status, body),
            (
                StatusCode::OK,
                serde_json::json!({"key": "theme", "value": "dark"})
            )
        );
        // Replace, and store a structured value.
        call(&alice, "PUT", "/api/settings/theme", Some(r#""light""#)).await;
        call(
            &alice,
            "PUT",
            "/api/settings/query.page",
            Some(r#"{"size":50}"#),
        )
        .await;
        let (_, all) = call(&alice, "GET", "/api/settings", None).await;
        assert_eq!(
            all,
            serde_json::json!({"query.page": {"size": 50}, "theme": "light"})
        );

        // Bob has none of it, and cannot delete Alice's.
        assert_eq!(
            call(&bob, "GET", "/api/settings", None).await.1,
            serde_json::json!({})
        );
        assert_eq!(
            call(&bob, "DELETE", "/api/settings/theme", None).await.0,
            StatusCode::NOT_FOUND
        );

        // Validation: key shape and value size, both 400 JSON.
        let (status, body) = call(&alice, "PUT", "/api/settings/Bad%20Key", Some("1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().starts_with("key must be"));
        let big = format!("\"{}\"", "x".repeat(16 * 1024));
        let (status, body) = call(&alice, "PUT", "/api/settings/big", Some(&big)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("value must be at most")
        );

        // Delete: 204, then 404.
        assert_eq!(
            call(&alice, "DELETE", "/api/settings/theme", None).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(&alice, "DELETE", "/api/settings/theme", None).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&alice, "GET", "/api/settings", None).await.1,
            serde_json::json!({"query.page": {"size": 50}})
        );
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

    fn signed_in_admin(sub: &str) -> auth::Principal {
        let mut p = signed_in(sub);
        p.identity.is_admin = true;
        p
    }

    /// The API on a real database, with `who` signed in (`None` = `dev` mode).
    fn log_app(pool: sqlx::PgPool, who: Option<auth::Principal>, dir: &StaticDir) -> Router {
        let mut api = api_router();
        if let Some(p) = who {
            api = api.layer(Extension(p));
        }
        app(test_state_with_db(Some(pool)), api, dir.path())
    }

    /// Two queries of alice (a select on warehouse `default`, a create-table-as on `analytics`)
    /// and one failed query of bob. Returns their ids in that order.
    async fn seed_query_log(pool: &sqlx::PgPool) -> [String; 3] {
        let mut ids = Vec::new();
        for (body, warehouse) in [
            (
                include_str!("testdata/query_log/completed_select.json"),
                "default",
            ),
            (
                include_str!("testdata/query_log/completed_ctas.json"),
                "analytics",
            ),
            (
                include_str!("testdata/query_log/completed_failed.json"),
                "default",
            ),
        ] {
            let record = query_log_event::parse(body.as_bytes()).expect("a fixture");
            ids.push(record.query_id.clone());
            query_log_store::upsert(
                pool,
                &query_log_collector::Accepted {
                    record,
                    warehouse: warehouse.into(),
                },
            )
            .await
            .expect("store a fixture");
        }
        [ids[0].clone(), ids[1].clone(), ids[2].clone()]
    }

    fn item_ids(list: &serde_json::Value) -> Vec<String> {
        list["items"]
            .as_array()
            .expect("an items array")
            .iter()
            .map(|i| i["id"].as_str().expect("an id").to_string())
            .collect()
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn the_query_log_shows_people_their_own_queries_and_administrators_everything() {
        let pool = db::test_support::migrated_pool().await;
        let [select, ctas, failed] = seed_query_log(&pool).await;
        let dir = StaticDir::new("query-log");
        let alice = log_app(pool.clone(), Some(signed_in("alice")), &dir);
        let bob = log_app(pool.clone(), Some(signed_in("bob")), &dir);
        let admin = log_app(pool.clone(), Some(signed_in_admin("root")), &dir);

        // Alice sees her two queries, newest first, and nothing of Bob's.
        let (status, list) = call(&alice, "GET", "/api/query-log", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(item_ids(&list), [ctas.clone(), select.clone()]);
        assert_eq!(list["items"][0]["user"], "alice");
        assert_eq!(list["items"][1]["state"], "FINISHED");
        assert_eq!(list["items"][1]["wallMs"], 2324);
        assert!(list["nextCursor"].is_null());

        // Bob sees his failed one with the engine's error code.
        let (_, list) = call(&bob, "GET", "/api/query-log", None).await;
        assert_eq!(item_ids(&list), vec![failed.clone()]);
        assert_eq!(list["items"][0]["errorCode"], "TABLE_NOT_FOUND");

        // The all scope is for administrators; the team scope is empty for now.
        let (status, body) = call(&alice, "GET", "/api/query-log?scope=all", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("administrators"));
        let (_, team) = call(&alice, "GET", "/api/query-log?scope=team", None).await;
        assert_eq!(item_ids(&team), Vec::<String>::new());

        // An administrator sees everything with scope=all, and nothing of their own.
        let (_, all) = call(&admin, "GET", "/api/query-log?scope=all", None).await;
        assert_eq!(item_ids(&all).len(), 3);
        let (_, mine) = call(&admin, "GET", "/api/query-log", None).await;
        assert_eq!(item_ids(&mine), Vec::<String>::new());

        // Filters.
        let uri = |q: &str| format!("/api/query-log?scope=all&{q}");
        let (_, failed_only) = call(&admin, "GET", &uri("state=failed"), None).await;
        assert_eq!(item_ids(&failed_only), vec![failed.clone()]);
        let (_, analytics) = call(&admin, "GET", &uri("warehouse=analytics"), None).await;
        assert_eq!(item_ids(&analytics), vec![ctas.clone()]);
        let (_, nation) = call(&admin, "GET", &uri("q=nation"), None).await;
        assert_eq!(item_ids(&nation), vec![select.clone()]);
        let (_, by_id) = call(&admin, "GET", &uri(&format!("q={failed}")), None).await;
        assert_eq!(item_ids(&by_id), vec![failed.clone()]);
        let (_, wildcard) = call(&admin, "GET", &uri("q=%25"), None).await;
        assert_eq!(
            item_ids(&wildcard),
            Vec::<String>::new(),
            "a percent sign is a literal"
        );

        // Paging one at a time visits every row once.
        let mut seen = Vec::new();
        let mut cursor = String::new();
        for _ in 0..5 {
            let (_, page) = call(&admin, "GET", &uri(&format!("limit=1{cursor}")), None).await;
            seen.extend(item_ids(&page));
            match page["nextCursor"].as_str() {
                Some(next) => cursor = format!("&cursor={next}"),
                None => break,
            }
        }
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(seen.len(), 3);
        assert_eq!(sorted.len(), 3, "no row twice: {seen:?}");

        // Detail: the owner and administrators, not a stranger; a stranger gets a plain 404.
        let (status, detail) = call(&alice, "GET", &format!("/api/query-log/{select}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            detail["sql"],
            "select 'alice-secret-literal' as c, count(*) from tpch.tiny.nation"
        );
        assert_eq!(detail["timing"]["wallMs"], 2324);
        assert_eq!(
            detail["tablesRead"],
            serde_json::json!(["tpch.tiny.nation"])
        );
        assert!(detail["error"].is_null());
        let (status, body) = call(&bob, "GET", &format!("/api/query-log/{select}"), None).await;
        assert_eq!(
            (status, body),
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({"error": "not found"})
            )
        );
        let (status, detail) = call(&bob, "GET", &format!("/api/query-log/{failed}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["error"]["code"], "TABLE_NOT_FOUND");
        assert_eq!(
            call(&admin, "GET", &format!("/api/query-log/{select}"), None)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(&alice, "GET", "/api/query-log/no-such-query", None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );

        // A bad parameter is a sentence, not a stack trace.
        let (status, body) = call(&alice, "GET", "/api/query-log?limit=0", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("limit"));
    }

    /// A fixture stored with an arbitrary engine user: the shape once authz 8/8 runs every
    /// query as the person's `sub`.
    async fn seed_query_log_as(pool: &sqlx::PgPool, principal: &str, fixture: &str) -> String {
        let mut record = query_log_event::parse(fixture.as_bytes()).expect("a fixture");
        record.principal = principal.to_string();
        let id = record.query_id.clone();
        query_log_store::upsert(
            pool,
            &query_log_collector::Accepted {
                record,
                warehouse: "default".into(),
            },
        )
        .await
        .expect("store a fixture");
        id
    }

    fn signed_in_as(sub: &str, username: &str) -> auth::Principal {
        let mut p = signed_in(sub);
        p.identity.display_name = username.into();
        p
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn query_log_mine_matches_the_subject_and_shows_the_directory_name() {
        const ALICE_SUB: &str = "0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6e";
        const BOB_SUB: &str = "1c7c2f1f-5a1f-4e5f-9f4b-2a3b4c5d6e7f";
        const UNKNOWN_SUB: &str = "2d8d3a20-6b20-4f60-a05c-3b4c5d6e7f80";

        let pool = db::test_support::migrated_pool().await;
        // Alice is in the directory (name from the token's `name`); Bob and the third
        // subject are not, so their rows must fall back to the raw engine user.
        let directory = test_state_with_db(Some(pool.clone()));
        people::touch(
            &directory,
            &auth::Identity {
                sub: ALICE_SUB.into(),
                display_name: "alice".into(),
                email: Some("alice@example.com".into()),
                name: Some("Alice Example".into()),
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: false,
            },
        )
        .await;

        let alice_query = seed_query_log_as(
            &pool,
            ALICE_SUB,
            include_str!("testdata/query_log/completed_select.json"),
        )
        .await;
        let bob_query = seed_query_log_as(
            &pool,
            BOB_SUB,
            include_str!("testdata/query_log/completed_ctas.json"),
        )
        .await;
        let unknown_query = seed_query_log_as(
            &pool,
            UNKNOWN_SUB,
            include_str!("testdata/query_log/completed_failed.json"),
        )
        .await;

        let dir = StaticDir::new("query-log-sub");
        let alice = log_app(pool.clone(), Some(signed_in_as(ALICE_SUB, "alice")), &dir);

        // "Mine" is the subject: the row the engine ran as Alice's sub is hers, Bob's is not.
        let (status, list) = call(&alice, "GET", "/api/query-log", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(item_ids(&list), vec![alice_query.clone()]);
        assert_eq!(list["items"][0]["user"], ALICE_SUB, "the raw engine user");
        assert_eq!(
            list["items"][0]["userName"], "Alice Example",
            "the directory resolves the subject to a name"
        );
        assert!(
            !list.to_string().contains(BOB_SUB),
            "somebody else's query is not in the mine scope"
        );

        // The sign-in name in the same token is not a second scope: Bob's row stays out.
        let bob = log_app(pool.clone(), Some(signed_in_as(BOB_SUB, "bob")), &dir);
        let (_, bob_list) = call(&bob, "GET", "/api/query-log", None).await;
        assert_eq!(item_ids(&bob_list), vec![bob_query.clone()]);
        assert_eq!(bob_list["items"][0]["user"], BOB_SUB);
        assert!(
            bob_list["items"][0]["userName"].is_null(),
            "an unknown subject falls back to the raw value"
        );

        // The administrator sees every row and still gets names only where a person exists.
        let admin = log_app(pool.clone(), Some(signed_in_admin("root")), &dir);
        let (_, all) = call(&admin, "GET", "/api/query-log?scope=all", None).await;
        let by_id = |list: &serde_json::Value, id: &str| {
            list["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["id"] == id)
                .cloned()
                .expect("the row")
        };
        assert_eq!(all["items"].as_array().unwrap().len(), 3);
        assert_eq!(by_id(&all, &alice_query)["userName"], "Alice Example");
        assert!(by_id(&all, &bob_query)["userName"].is_null());
        assert!(by_id(&all, &unknown_query)["userName"].is_null());

        // The detail carries the same name and the raw value, and a stranger cannot open it.
        let (status, detail) = call(
            &alice,
            "GET",
            &format!("/api/query-log/{alice_query}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["user"], ALICE_SUB);
        assert_eq!(detail["userName"], "Alice Example");
        let (status, _) = call(&alice, "GET", &format!("/api/query-log/{bob_query}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, bob_detail) =
            call(&admin, "GET", &format!("/api/query-log/{bob_query}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(bob_detail["userName"].is_null());

        // Nothing in either answer is a credential: no token field, no bearer, no session.
        for body in [list.to_string(), detail.to_string(), bob_detail.to_string()] {
            for word in ["token", "accessToken", "Bearer", "session"] {
                assert!(
                    !body.contains(word),
                    "a query-log answer leaked {word}: {body}"
                );
            }
        }
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn in_dev_mode_the_one_user_is_the_operator_and_sees_all() {
        let pool = db::test_support::migrated_pool().await;
        seed_query_log(&pool).await;
        let dir = StaticDir::new("query-log-dev");
        let dev = log_app(pool, None, &dir);
        let (_, all) = call(&dev, "GET", "/api/query-log?scope=all", None).await;
        assert_eq!(item_ids(&all).len(), 3);
        let (_, mine) = call(&dev, "GET", "/api/query-log", None).await;
        assert_eq!(
            item_ids(&mine),
            Vec::<String>::new(),
            "the dev user ran none of them"
        );
    }

    #[tokio::test]
    async fn the_query_log_answers_503_without_a_state_store() {
        let dir = StaticDir::new("query-log-503");
        let app = app(test_state(), api_router(), dir.path());
        assert_eq!(
            call(&app, "GET", "/api/query-log", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            call(&app, "GET", "/api/query-log/x", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
