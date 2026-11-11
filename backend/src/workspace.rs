//! The caller's own JupyterHub workspace (stardelt-nova#653, devx W3b).
//!
//! Nova is a JupyterHub *service* with exactly three powers: read the caller's server state,
//! start it, stop it. It never gets `access:servers`, so it can never open a person's server
//! or run code in it — the UI's "Open" button is a plain link to the hub, not an API call.
//!
//! The hub is addressed by the caller's own sign-in name (`Identity.display_name`, the
//! `preferred_username` the hub knows people by); there is no path parameter, so nobody can
//! ask about, start or stop somebody else's workspace.
//!
//! Every failure a reader can act on is answered as a *state*, not as an error: a missing
//! `NOVA_JUPYTERHUB_API_*` pair or an unreachable hub answers `200 {"state":"unavailable"}`,
//! so the Notebooks page always renders. Nothing here logs the service token.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{Identity, Principal, Secret};
use crate::connection_routes::Fail;

/// A call that has not answered within this time is a failure, not a hanging page.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The default idle stop when `NOVA_WORKSPACE_IDLE_SECONDS` is unset: the platform's hub
/// culls after an hour, and the card must not promise a longer stay than the hub gives.
const DEFAULT_IDLE_SECONDS: i64 = 3600;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Where the hub's API is and how Nova signs in to it. Absent (`None`) unless both the URL
/// and the token are set: the card then says `unavailable` instead of failing.
#[derive(Debug, Clone)]
pub struct WorkspaceConfig {
    /// API root, no trailing slash: `http://hub:8081/hub/api`.
    pub api: String,
    /// The hub service token (Secret `hub`, key `hub.services.nova.apiToken`). `Debug` prints
    /// `[redacted]`; it never reaches a log line, an error or a response.
    pub token: Secret,
    /// How long a server may sit idle before the hub culls it.
    pub idle_seconds: i64,
}

impl WorkspaceConfig {
    /// `NOVA_JUPYTERHUB_API_URL` and `NOVA_JUPYTERHUB_API_TOKEN`, plus the optional
    /// `NOVA_WORKSPACE_IDLE_SECONDS` (a value that is not a positive number means the default).
    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let api = get("NOVA_JUPYTERHUB_API_URL")?
            .trim()
            .trim_end_matches('/')
            .to_string();
        let token = get("NOVA_JUPYTERHUB_API_TOKEN")?;
        if api.is_empty() || token.is_empty() {
            return None;
        }
        let idle_seconds = get("NOVA_WORKSPACE_IDLE_SECONDS")
            .and_then(|value| value.trim().parse::<i64>().ok())
            .filter(|seconds| *seconds > 0)
            .unwrap_or(DEFAULT_IDLE_SECONDS);
        Some(Self {
            api,
            token: Secret::new(token),
            idle_seconds,
        })
    }

    pub fn from_env() -> Option<Self> {
        Self::from_env_with(|key| std::env::var(key).ok())
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// One workspace as the card shows it. `state` is one of `stopped`, `starting`, `running`,
/// `stopping` and `unavailable`; the three times are RFC 3339 in UTC, or `null` while the hub
/// reports none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    /// `stopped`, `starting`, `running`, `stopping` or `unavailable`.
    pub state: String,
    /// When the server was started (`server.started`).
    pub started_at: Option<String>,
    /// The hub's `last_activity` for the server.
    pub last_activity: Option<String>,
    /// `last_activity` plus `NOVA_WORKSPACE_IDLE_SECONDS`: when the hub culls an idle server.
    pub idle_stop_at: Option<String>,
}

impl WorkspaceView {
    fn state(state: &str) -> Self {
        Self {
            state: state.to_string(),
            started_at: None,
            last_activity: None,
            idle_stop_at: None,
        }
    }

    fn unavailable() -> Self {
        Self::state("unavailable")
    }
}

// ---------------------------------------------------------------------------
// Mapping a hub user document
// ---------------------------------------------------------------------------

/// `last_activity + idle_seconds`, in the same RFC 3339 UTC shape the hub reports. A value
/// that is not a timestamp is passed through rather than dropping the whole view.
fn idle_stop_at(last_activity: &str, idle_seconds: i64) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(last_activity).ok()?;
    let stop = parsed.with_timezone(&chrono::Utc) + chrono::Duration::seconds(idle_seconds);
    Some(stop.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// The state of the caller's server from the hub's user document
/// (`GET /hub/api/users/{name}`):
///
/// - no `servers[""]` → `stopped`;
/// - `pending == "spawn"` → `starting`;
/// - `pending == "stop"` → `stopping`;
/// - `ready == true` → `running`;
/// - a server that is neither ready nor pending is still coming up → `starting`.
fn view_of(user: &Value, idle_seconds: i64) -> WorkspaceView {
    let server = user.get("servers").and_then(|servers| servers.get(""));
    let Some(server) = server.filter(|server| !server.is_null()) else {
        return WorkspaceView::state("stopped");
    };
    let text = |key: &str| {
        server
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let state = match server.get("pending").and_then(Value::as_str) {
        Some("spawn") => "starting",
        Some("stop") => "stopping",
        _ if server.get("ready").and_then(Value::as_bool) == Some(true) => "running",
        _ => "starting",
    };
    let last_activity = text("last_activity");
    WorkspaceView {
        state: state.to_string(),
        started_at: text("started"),
        idle_stop_at: last_activity
            .as_deref()
            .and_then(|value| idle_stop_at(value, idle_seconds)),
        last_activity,
    }
}

/// A fixed, running workspace for demo mode: the page shows its three states without a hub.
fn demo_view(idle_seconds: i64) -> WorkspaceView {
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let started = now - chrono::Duration::minutes(42);
    let active = now - chrono::Duration::minutes(3);
    let last_activity = active.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    WorkspaceView {
        state: "running".to_string(),
        started_at: Some(started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        idle_stop_at: idle_stop_at(&last_activity, idle_seconds),
        last_activity: Some(last_activity),
    }
}

// ---------------------------------------------------------------------------
// The hub calls
// ---------------------------------------------------------------------------

/// Percent-encode one path segment: only unreserved characters (`A-Z a-z 0-9 - . _ ~`)
/// survive, so a sign-in name with `@` or a space addresses the right hub user.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A hub answer: the status and, for the sentences Nova needs to read, the body.
struct HubAnswer {
    status: StatusCode,
    body: String,
}

/// One call against the hub, always as `Authorization: token <service token>` and always
/// bounded by [`REQUEST_TIMEOUT`]. Any transport failure (DNS, refused, timeout) is `None`.
async fn hub_call(
    state: &AppState,
    cfg: &WorkspaceConfig,
    method: reqwest::Method,
    path: &str,
) -> Option<HubAnswer> {
    let response = state
        .http
        .request(method, format!("{}{path}", cfg.api))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("token {}", cfg.token.expose()),
        )
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .ok()?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Some(HubAnswer {
        status: StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
        body,
    })
}

/// Who the caller is. `dev` mode has no IdP: the single user is `NOVA_DEV_USER`, exactly like
/// `compute::actor` and `warehouses::actor`.
fn actor(principal: Option<Extension<Principal>>, state: &AppState) -> Identity {
    match principal {
        Some(Extension(principal)) => principal.identity,
        None => Identity {
            sub: state.dev_user.clone(),
            display_name: state.dev_user.clone(),
            email: None,
            name: None,
            groups: Vec::new(),
            roles: Vec::new(),
            groups_overage: false,
            is_admin: true,
        },
    }
}

fn not_configured() -> Fail {
    Fail::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "the workspace is not configured",
    )
}

fn no_hub_user() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "Open JupyterLab once to create your workspace.",
    )
}

/// Whether the hub's `400` says the server already exists (JupyterHub phrases it "User <name>
/// is already running" or "already has a server running").
fn already_running(body: &str) -> bool {
    let text = body.to_ascii_lowercase();
    text.contains("already")
}

/// Answer with a status and a view, or with the refusal.
fn answer<T: Serialize>(status: StatusCode, value: T) -> Response {
    (status, Json(value)).into_response()
}

/// `GET /api/workspace`: the caller's own server state.
#[utoipa::path(
    get,
    operation_id = "workspace_get",
    path = "/api/workspace",
    tag = "public",
    description = "The state of the caller's own JupyterHub server: `stopped` (no server), `starting` (the hub is spawning it), `running` (`ready`) or `stopping` (the hub is stopping it), with `startedAt`, `lastActivity` and `idleStopAt` (`lastActivity` plus the platform's idle limit: when the hub culls an idle server). Orders no server and never reads another person's: the hub user is the caller's own sign-in name. A person the hub has never seen answers `stopped`. Answers `200 {\"state\":\"unavailable\"}` when Nova has no hub credentials or the hub does not answer, so the page still renders. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "The caller's workspace, or `unavailable`", body = WorkspaceView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn get(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let who = actor(principal, &state);
    let view = match &state.workspace {
        Some(cfg) if state.demo => demo_view(cfg.idle_seconds),
        Some(cfg) => {
            let path = format!("/users/{}", encode_segment(&who.display_name));
            match hub_call(&state, cfg, reqwest::Method::GET, &path).await {
                Some(answer) if answer.status == StatusCode::OK => {
                    match serde_json::from_str::<Value>(&answer.body) {
                        Ok(user) => view_of(&user, cfg.idle_seconds),
                        Err(_) => WorkspaceView::unavailable(),
                    }
                }
                // The hub has never seen this person (they never opened it): no server.
                Some(answer) if answer.status == StatusCode::NOT_FOUND => {
                    WorkspaceView::state("stopped")
                }
                Some(_) | None => WorkspaceView::unavailable(),
            }
        }
        None => WorkspaceView::unavailable(),
    };
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(view),
    )
        .into_response()
}

/// `POST /api/workspace/start`: ask the hub to spawn the caller's server.
#[utoipa::path(
    post,
    operation_id = "workspace_start",
    path = "/api/workspace/start",
    tag = "public",
    description = "Spawn the caller's own JupyterHub server: `POST /hub/api/users/{name}/server` with Nova's service token. Answers `202 {\"state\":\"starting\"}` when the hub accepted the spawn, `200 {\"state\":\"running\"}` when a server was already running, and `409` when the hub has never seen this person (they must open JupyterHub once to create their workspace) or when the hub's stored sign-in for them has expired (only the hub's login can renew it: open JupyterLab once). `503` when Nova has no hub credentials, `502` when the hub does not answer or refuses another way.",
    responses(
        (status = 202, description = "The hub is spawning the server", body = WorkspaceView),
        (status = 200, description = "A server was already running", body = WorkspaceView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The hub has no such user, or their hub sign-in has expired", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The hub did not answer or refused", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No hub credentials", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn start(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let who = actor(principal, &state);
    let Some(cfg) = &state.workspace else {
        return not_configured().into_response();
    };
    if state.demo {
        return answer(StatusCode::OK, demo_view(cfg.idle_seconds));
    }
    let path = format!("/users/{}/server", encode_segment(&who.display_name));
    match hub_call(&state, cfg, reqwest::Method::POST, &path).await {
        Some(hub) if hub.status == StatusCode::CREATED || hub.status == StatusCode::ACCEPTED => {
            answer(StatusCode::ACCEPTED, WorkspaceView::state("starting"))
        }
        Some(hub) if hub.status == StatusCode::BAD_REQUEST && already_running(&hub.body) => {
            answer(StatusCode::OK, WorkspaceView::state("running"))
        }
        Some(hub) if hub.status == StatusCode::NOT_FOUND => no_hub_user().into_response(),
        // The hub's own words: its stored OAuth token for this person has aged out, and only the
        // hub's login page can refresh it (the spawn route does it through the SSO session).
        Some(hub) if hub.status == StatusCode::FORBIDDEN => Fail::new(
            StatusCode::CONFLICT,
            "JupyterHub needs you to sign in again: open JupyterLab once, then Start",
        )
        .into_response(),
        Some(_) | None => Fail::new(
            StatusCode::BAD_GATEWAY,
            "the hub did not start your workspace",
        )
        .into_response(),
    }
}

/// `POST /api/workspace/stop`: ask the hub to stop the caller's server.
#[utoipa::path(
    post,
    operation_id = "workspace_stop",
    path = "/api/workspace/stop",
    tag = "public",
    description = "Stop the caller's own JupyterHub server: `DELETE /hub/api/users/{name}/server` with Nova's service token. Answers `202 {\"state\":\"stopping\"}`; a person the hub has never seen answers `200 {\"state\":\"stopped\"}` (there is nothing to stop). `503` when Nova has no hub credentials, `502` when the hub does not answer or refuses.",
    responses(
        (status = 202, description = "The hub is stopping the server", body = WorkspaceView),
        (status = 200, description = "There was no server to stop", body = WorkspaceView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The hub did not answer or refused", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No hub credentials", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn stop(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let who = actor(principal, &state);
    let Some(cfg) = &state.workspace else {
        return not_configured().into_response();
    };
    if state.demo {
        return answer(StatusCode::OK, WorkspaceView::state("stopped"));
    }
    let path = format!("/users/{}/server", encode_segment(&who.display_name));
    match hub_call(&state, cfg, reqwest::Method::DELETE, &path).await {
        Some(hub) if hub.status.is_success() => {
            answer(StatusCode::ACCEPTED, WorkspaceView::state("stopping"))
        }
        Some(hub) if hub.status == StatusCode::NOT_FOUND => {
            answer(StatusCode::OK, WorkspaceView::state("stopped"))
        }
        Some(_) | None => Fail::new(
            StatusCode::BAD_GATEWAY,
            "the hub did not stop your workspace",
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{Router, http::StatusCode, routing::get};
    use serde_json::{Value, json};

    use super::{WorkspaceConfig, WorkspaceView, encode_segment, idle_stop_at, view_of};

    fn user(server: Option<Value>) -> Value {
        let mut servers = serde_json::Map::new();
        if let Some(server) = server {
            servers.insert(String::new(), server);
        }
        json!({ "name": "alice", "servers": servers })
    }

    const STARTED: &str = "2026-11-09T09:12:00Z";
    const ACTIVE: &str = "2026-11-09T09:55:00Z";

    #[test]
    fn a_person_without_a_server_is_stopped() {
        assert_eq!(view_of(&user(None), 3600).state, "stopped");
        // The hub also writes an explicit `null` for a person who never spawned.
        assert_eq!(
            view_of(&json!({"servers": {"": null}}), 3600).state,
            "stopped"
        );
        assert_eq!(
            view_of(&user(None), 3600),
            WorkspaceView::state("stopped"),
            "a stopped view carries no times"
        );
    }

    #[test]
    fn a_pending_spawn_is_starting() {
        let view = view_of(
            &user(Some(
                json!({"pending": "spawn", "ready": false, "started": STARTED}),
            )),
            3600,
        );
        assert_eq!(view.state, "starting");
        assert_eq!(view.started_at.as_deref(), Some(STARTED));
    }

    #[test]
    fn a_pending_stop_is_stopping() {
        let view = view_of(
            &user(Some(
                json!({"pending": "stop", "ready": true, "started": STARTED}),
            )),
            3600,
        );
        assert_eq!(view.state, "stopping");
    }

    #[test]
    fn a_ready_server_is_running_with_its_idle_stop() {
        let view = view_of(
            &user(Some(json!({
                "ready": true,
                "started": STARTED,
                "last_activity": ACTIVE,
                "pending": null
            }))),
            3600,
        );
        assert_eq!(view.state, "running");
        assert_eq!(view.started_at.as_deref(), Some(STARTED));
        assert_eq!(view.last_activity.as_deref(), Some(ACTIVE));
        // 09:55 + one hour, seconds only.
        assert_eq!(view.idle_stop_at.as_deref(), Some("2026-11-09T10:55:00Z"));
    }

    #[test]
    fn a_server_that_is_neither_ready_nor_pending_is_still_starting() {
        assert_eq!(
            view_of(&user(Some(json!({"ready": false}))), 3600).state,
            "starting"
        );
    }

    #[test]
    fn the_idle_limit_comes_from_the_configuration() {
        assert_eq!(
            idle_stop_at(ACTIVE, 600).as_deref(),
            Some("2026-11-09T10:05:00Z")
        );
        // An unreadable timestamp is no idle-stop time, never a wrong one.
        assert_eq!(idle_stop_at("soon", 600), None);
    }

    #[test]
    fn the_configuration_needs_both_the_url_and_the_token() {
        let get = |vars: Vec<(&'static str, &'static str)>| {
            move |key: &str| {
                vars.iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_string())
            }
        };
        assert!(WorkspaceConfig::from_env_with(get(vec![])).is_none());
        assert!(
            WorkspaceConfig::from_env_with(get(vec![(
                "NOVA_JUPYTERHUB_API_URL",
                "http://hub:8081/hub/api"
            )]))
            .is_none()
        );
        let both = WorkspaceConfig::from_env_with(get(vec![
            ("NOVA_JUPYTERHUB_API_URL", "http://hub:8081/hub/api/"),
            ("NOVA_JUPYTERHUB_API_TOKEN", "hub-token"),
        ]))
        .expect("configured");
        // The trailing slash is trimmed, and the token is never printed.
        assert_eq!(both.api, "http://hub:8081/hub/api");
        assert_eq!(both.idle_seconds, 3600);
        assert!(!format!("{both:?}").contains("hub-token"));
        let idle = WorkspaceConfig::from_env_with(get(vec![
            ("NOVA_JUPYTERHUB_API_URL", "http://hub:8081/hub/api"),
            ("NOVA_JUPYTERHUB_API_TOKEN", "hub-token"),
            ("NOVA_WORKSPACE_IDLE_SECONDS", "600"),
        ]))
        .expect("configured");
        assert_eq!(idle.idle_seconds, 600);
    }

    #[test]
    fn a_sign_in_name_is_one_path_segment() {
        assert_eq!(encode_segment("alice"), "alice");
        assert_eq!(encode_segment("alice@example.com"), "alice%40example.com");
        assert_eq!(encode_segment("a b"), "a%20b");
    }

    /// A fake hub: every call is recorded (method, path, `Authorization`), and the answer for
    /// a path is what the test set up.
    mod fake_hub {
        use std::sync::{Arc, Mutex};

        use axum::{
            Router,
            extract::{Request, State},
            http::StatusCode,
            response::{IntoResponse, Response},
        };
        use serde_json::Value;

        #[derive(Default)]
        pub struct Fake {
            /// `"GET /users/alice token hub-token"`, in order: the `Authorization` header verbatim.
            pub calls: Vec<String>,
            /// What `GET /users/{name}` answers.
            pub user: Option<(StatusCode, Value)>,
            /// What a spawn and a stop answer.
            pub spawn: Option<(StatusCode, Value)>,
            pub stop: Option<(StatusCode, Value)>,
        }

        pub type Shared = Arc<Mutex<Fake>>;

        async fn any(State(state): State<Shared>, request: Request) -> Response {
            let method = request.method().clone();
            let path = request.uri().path().to_string();
            let token = request
                .headers()
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let mut fake = state.lock().expect("fake hub poisoned");
            fake.calls.push(format!("{method} {path} {token}"));
            let answer = if method == axum::http::Method::GET {
                fake.user.clone()
            } else if method == axum::http::Method::DELETE {
                fake.stop.clone()
            } else {
                fake.spawn.clone()
            };
            match answer {
                Some((status, body)) => (status, axum::Json(body)).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }

        pub async fn start(fake: Fake) -> (String, Shared) {
            let shared: Shared = Arc::new(Mutex::new(fake));
            let app = Router::new().fallback(any).with_state(shared.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (format!("http://{addr}"), shared)
        }

        pub fn body(status: StatusCode, body: Value) -> Option<(StatusCode, Value)> {
            Some((status, body))
        }

        pub fn json(body: Value) -> Option<(StatusCode, Value)> {
            Some((StatusCode::OK, body))
        }
    }

    /// `GET /api/workspace` on a test state whose hub is the fake at `api`.
    async fn get_view(
        api: &str,
        idle_seconds: i64,
        who: Option<crate::auth::Identity>,
    ) -> (StatusCode, Value) {
        use tower::ServiceExt;

        let state = crate::router_tests::test_state_with_workspace(api, idle_seconds);
        let router = Router::new()
            .route("/workspace", get(super::get))
            .with_state(state);
        let router = match who {
            Some(identity) => router.layer(axum::Extension(crate::auth::Principal {
                identity,
                via: "session",
                access_token: None,
            })),
            None => router,
        };
        let request = axum::http::Request::builder()
            .uri("/workspace")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    fn alice() -> crate::auth::Identity {
        crate::auth::Identity {
            sub: "alice".to_string(),
            display_name: "alice@example.com".to_string(),
            email: None,
            name: None,
            groups: Vec::new(),
            roles: Vec::new(),
            groups_overage: false,
            is_admin: false,
        }
    }

    #[tokio::test]
    async fn the_view_asks_the_hub_for_the_callers_own_name() {
        let (api, fake) = fake_hub::start(fake_hub::Fake {
            user: fake_hub::json(user(Some(json!({
                "ready": true,
                "started": STARTED,
                "last_activity": ACTIVE
            })))),
            ..fake_hub::Fake::default()
        })
        .await;
        let (status, body) = get_view(&api, 3600, Some(alice())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "running");
        assert_eq!(body["idleStopAt"], "2026-11-09T10:55:00Z");
        // The caller's own name, percent-encoded, and the service token as a `token` header.
        assert_eq!(
            fake.lock().unwrap().calls,
            ["GET /users/alice%40example.com token hub-token"]
        );
    }

    #[tokio::test]
    async fn a_person_the_hub_never_saw_is_stopped() {
        let (api, _) = fake_hub::start(fake_hub::Fake::default()).await;
        let (status, body) = get_view(&api, 3600, Some(alice())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "stopped");
    }

    #[tokio::test]
    async fn an_unreachable_hub_answers_unavailable_with_200() {
        let (status, body) = get_view("http://127.0.0.1:1", 3600, Some(alice())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "unavailable");
    }

    #[tokio::test]
    async fn demo_mode_answers_a_fixed_running_view() {
        use tower::ServiceExt;

        let mut state = crate::router_tests::test_state();
        Arc::get_mut(&mut state).expect("a fresh state").workspace = Some(WorkspaceConfig {
            api: "http://127.0.0.1:1".to_string(),
            token: crate::auth::Secret::new("hub-token"),
            idle_seconds: 3600,
        });
        let router = Router::new()
            .route("/workspace", get(super::get))
            .with_state(state);
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/workspace")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["state"], "running");
        assert!(body["startedAt"].is_string());
        assert!(body["idleStopAt"].is_string());
    }

    #[tokio::test]
    async fn no_configuration_answers_unavailable_with_200() {
        use tower::ServiceExt;

        let state = crate::router_tests::test_state();
        let router = Router::new()
            .route("/workspace", get(super::get))
            .with_state(state);
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/workspace")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["state"], "unavailable");
    }

    /// `POST /api/workspace/start` and `/stop` against the fake hub, `who` signed in.
    async fn act(api: &str, path: &str, who: Option<crate::auth::Identity>) -> (StatusCode, Value) {
        use tower::ServiceExt;

        let state = crate::router_tests::test_state_with_workspace(api, 3600);
        let router = Router::new()
            .route("/workspace/start", axum::routing::post(super::start))
            .route("/workspace/stop", axum::routing::post(super::stop))
            .with_state(state);
        let router = match who {
            Some(identity) => router.layer(axum::Extension(crate::auth::Principal {
                identity,
                via: "session",
                access_token: None,
            })),
            None => router,
        };
        let request = axum::http::Request::builder()
            .method(axum::http::Method::POST)
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn start_asks_the_hub_to_spawn_the_callers_own_server() {
        let (api, fake) = fake_hub::start(fake_hub::Fake {
            spawn: fake_hub::body(StatusCode::ACCEPTED, json!({"name": "alice"})),
            ..fake_hub::Fake::default()
        })
        .await;
        let (status, body) = act(&api, "/workspace/start", Some(alice())).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["state"], "starting");
        assert_eq!(
            fake.lock().unwrap().calls,
            ["POST /users/alice%40example.com/server token hub-token"]
        );
    }

    #[tokio::test]
    async fn start_of_a_running_server_answers_running() {
        let (api, _) = fake_hub::start(fake_hub::Fake {
            spawn: fake_hub::body(
                StatusCode::BAD_REQUEST,
                json!({"status": 400, "message": "User alice already has a server running"}),
            ),
            ..fake_hub::Fake::default()
        })
        .await;
        let (status, body) = act(&api, "/workspace/start", Some(alice())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "running");
    }

    #[tokio::test]
    async fn start_without_a_hub_user_says_to_open_jupyterlab_once() {
        let (api, _) = fake_hub::start(fake_hub::Fake::default()).await;
        let (status, body) = act(&api, "/workspace/start", Some(alice())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"],
            "Open JupyterLab once to create your workspace."
        );
    }

    #[tokio::test]
    async fn start_with_an_expired_hub_sign_in_says_to_open_jupyterlab_once() {
        let (api, _) = fake_hub::start(fake_hub::Fake {
            spawn: fake_hub::body(
                StatusCode::FORBIDDEN,
                json!({"status": 403, "message": "auth has expired for alice@example.com, login again"}),
            ),
            ..fake_hub::Fake::default()
        })
        .await;
        let (status, body) = act(&api, "/workspace/start", Some(alice())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"],
            "JupyterHub needs you to sign in again: open JupyterLab once, then Start"
        );
    }

    #[tokio::test]
    async fn start_with_a_broken_hub_is_a_bad_gateway() {
        let (api, _) = fake_hub::start(fake_hub::Fake {
            spawn: fake_hub::body(StatusCode::INTERNAL_SERVER_ERROR, json!({})),
            ..fake_hub::Fake::default()
        })
        .await;
        let (status, body) = act(&api, "/workspace/start", Some(alice())).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "the hub did not start your workspace");
    }

    #[tokio::test]
    async fn stop_asks_the_hub_to_stop_the_callers_own_server() {
        let (api, fake) = fake_hub::start(fake_hub::Fake {
            stop: fake_hub::body(StatusCode::ACCEPTED, json!({})),
            ..fake_hub::Fake::default()
        })
        .await;
        let (status, body) = act(&api, "/workspace/stop", Some(alice())).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["state"], "stopping");
        assert_eq!(
            fake.lock().unwrap().calls,
            ["DELETE /users/alice%40example.com/server token hub-token"]
        );
    }

    #[tokio::test]
    async fn stop_of_a_person_the_hub_never_saw_is_already_stopped() {
        let (api, _) = fake_hub::start(fake_hub::Fake::default()).await;
        let (status, body) = act(&api, "/workspace/stop", Some(alice())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "stopped");
    }

    #[tokio::test]
    async fn an_unconfigured_hub_answers_503_for_start_and_stop() {
        use tower::ServiceExt;

        let state = crate::router_tests::test_state();
        let router = Router::new()
            .route("/workspace/start", axum::routing::post(super::start))
            .with_state(state);
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .method(axum::http::Method::POST)
                    .uri("/workspace/start")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
