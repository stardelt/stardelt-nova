//! The statement API, first slice (stardelt-nova#272): submit a statement, poll it, read its rows in
//! chunks, cancel it by id.
//!
//!   POST   /api/statements                     — submit `{sql, waitSeconds?}`
//!   GET    /api/statements/{id}                — the state; `?waitSeconds=0..30` waits for progress
//!   GET    /api/statements/{id}/chunks/{n}     — rows, 1 000 at a time
//!   DELETE /api/statements/{id}                — cancel (idempotent) and forget
//!
//! **No task runs between requests.** A task started at submit time would hold the submitter's bearer
//! token, which lives minutes, while the engine checks the token on every `nextUri` poll: any statement
//! longer than the token would fail half-way. Instead the store keeps the engine's current `nextUri` and
//! the pages read so far, and a request advances the cursor with the token *that request* carries
//! (long-polling for up to `waitSeconds`). The engine keeps the query running between polls; a client
//! that stops polling for the engine's own client timeout (5 minutes by default) has the query
//! cancelled by the engine, and the statement then reads `CANCELED`.
//!
//! Results live in this process's memory for one hour after the last request; a restart forgets them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::trino::{self, Column, ENGINE_UNREACHABLE, MAX_BYTES, TrinoResponse, row_bytes};

/// Rows in one chunk.
const CHUNK_ROWS: usize = 1_000;
/// Rows a statement keeps; past this the statement ends `SUCCEEDED` with `truncated: true`.
const MAX_ROWS: usize = 100_000;
/// Rough bytes of rows all statements together keep; while the store holds this much a new statement
/// is refused with `429`.
const MAX_STORE_BYTES: usize = 512 * 1024 * 1024;
/// Statements one person may hold at once.
const MAX_LIVE_PER_OWNER: usize = 10;
/// How long a statement lives after the last request that named it.
const KEEP: Duration = Duration::from_secs(3600);
/// The longest a request waits for progress.
const MAX_WAIT_SECONDS: u64 = 30;
/// The wait when the request names none.
const DEFAULT_WAIT_SECONDS: u64 = 5;
/// What the engine's `X-Trino-Source` and the query log call these statements.
const SOURCE: &str = "stardelt-api";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StatementState {
    /// Accepted, queued in the engine, nothing read yet.
    Pending,
    /// The engine is working on it; rows may already be readable.
    Running,
    /// Finished; all rows (or a prefix of them with `truncated`) are readable.
    Succeeded,
    /// Refused or failed in the engine; `error` says why.
    Failed,
    /// Cancelled by the person, or by the engine because nobody polled.
    Canceled,
}

impl StatementState {
    fn done(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }
}

struct Statement {
    state: StatementState,
    /// The engine's address for the next page; `None` when the engine has no more.
    next: Option<String>,
    columns: Vec<Column>,
    rows: Vec<Vec<Value>>,
    error: Option<String>,
    truncated: bool,
    /// Rough bytes of `rows` (see [`trino::row_bytes`]); a statement keeps at most [`MAX_BYTES`].
    bytes: usize,
    /// The same number, shared with the store so it can add up what every statement holds.
    held: Arc<AtomicUsize>,
}

impl Statement {
    fn new() -> Self {
        Self {
            state: StatementState::Pending,
            next: None,
            columns: Vec::new(),
            rows: Vec::new(),
            error: None,
            truncated: false,
            bytes: 0,
            held: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct Entry {
    owner: String,
    /// Rough bytes the statement holds, shared with the statement itself.
    held: Arc<AtomicUsize>,
    touched: Instant,
    /// One request at a time advances a statement: two polls of one `nextUri` would add a page twice.
    slot: Arc<tokio::sync::Mutex<Statement>>,
}

/// The statements this process holds, by the engine's query id.
#[derive(Default)]
pub struct StatementStore(Mutex<HashMap<String, Entry>>);

impl StatementStore {
    /// Forget statements nobody has named for an hour.
    fn sweep(&self, now: Instant) {
        let mut map = self.0.lock().expect("statement store poisoned");
        map.retain(|_, e| now.duration_since(e.touched) < KEEP);
    }

    /// Rough bytes of rows the store holds, all statements together.
    fn held_bytes(&self) -> usize {
        let map = self.0.lock().expect("statement store poisoned");
        map.values().map(|e| e.held.load(Ordering::Relaxed)).sum()
    }

    fn live_for(&self, owner: &str) -> usize {
        let map = self.0.lock().expect("statement store poisoned");
        map.values().filter(|e| e.owner == owner).count()
    }

    fn insert(
        &self,
        id: String,
        owner: String,
        statement: Statement,
    ) -> Arc<tokio::sync::Mutex<Statement>> {
        let held = statement.held.clone();
        let slot = Arc::new(tokio::sync::Mutex::new(statement));
        let entry = Entry {
            owner,
            held,
            touched: Instant::now(),
            slot: slot.clone(),
        };
        self.0
            .lock()
            .expect("statement store poisoned")
            .insert(id, entry);
        slot
    }

    /// The statement `id` when it exists **and is `owner`'s**; anyone else's answers like a missing one.
    fn find(&self, id: &str, owner: &str) -> Option<Arc<tokio::sync::Mutex<Statement>>> {
        let mut map = self.0.lock().expect("statement store poisoned");
        let entry = map.get_mut(id).filter(|e| e.owner == owner)?;
        entry.touched = Instant::now();
        Some(entry.slot.clone())
    }

    fn remove(&self, id: &str, owner: &str) {
        let mut map = self.0.lock().expect("statement store poisoned");
        if map.get(id).is_some_and(|e| e.owner == owner) {
            map.remove(id);
        }
    }
}

/// `POST /api/statements`.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    /// One SQL statement, run with the caller's identity.
    pub sql: String,
    /// How long the call waits for the statement to end before it answers `202`; 0 to 30, default 5.
    pub wait_seconds: Option<u64>,
}

/// `GET /api/statements/{id}`: the wait, in seconds.
#[derive(Deserialize)]
struct WaitQuery {
    #[serde(rename = "waitSeconds")]
    wait_seconds: Option<u64>,
}

/// A statement as the caller sees it.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatementView {
    /// The engine's query id; the handle for every other call.
    pub statement_id: String,
    pub state: StatementState,
    /// Known once the engine has sent them; empty before.
    pub columns: Vec<Column>,
    /// Rows read so far (at most 100 000).
    pub row_count: usize,
    /// Chunks readable now: every full chunk while the statement runs, the last partial one too once it ends.
    pub chunks: usize,
    /// True when the rows were cut (at 100 000 rows or at about 64 MiB of data): they are a prefix, not the whole answer.
    pub truncated: bool,
    /// The engine's one-line reason when `state` is `FAILED` or `CANCELED`.
    pub error: Option<String>,
    /// Chunk 0 when `state` is `SUCCEEDED`, so a short statement needs one call; absent otherwise.
    #[schema(value_type = Option<Vec<Vec<Value>>>)]
    pub rows: Option<Vec<Vec<Value>>>,
}

/// `GET /api/statements/{id}/chunks/{n}`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ChunkView {
    pub chunk: usize,
    /// One array per row, in column order.
    #[schema(value_type = Vec<Vec<Value>>)]
    pub rows: Vec<Vec<Value>>,
}

fn clamp_wait(requested: Option<u64>) -> Duration {
    Duration::from_secs(
        requested
            .unwrap_or(DEFAULT_WAIT_SECONDS)
            .min(MAX_WAIT_SECONDS),
    )
}

fn view(id: &str, s: &Statement) -> StatementView {
    let chunks = if s.state.done() {
        s.rows.len().div_ceil(CHUNK_ROWS)
    } else {
        s.rows.len() / CHUNK_ROWS
    };
    StatementView {
        statement_id: id.to_string(),
        state: s.state,
        columns: s.columns.clone(),
        row_count: s.rows.len(),
        chunks,
        truncated: s.truncated,
        error: s.error.clone(),
        rows: (s.state == StatementState::Succeeded)
            .then(|| s.rows.iter().take(CHUNK_ROWS).cloned().collect()),
    }
}

fn fail(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

fn no_such_statement() -> Response {
    fail(StatusCode::NOT_FOUND, "no such statement")
}

/// Who owns a statement: the person's `sub`, or the dev user when nobody is signed in.
fn owner_of(s: &AppState, caller: Option<&crate::auth::Principal>) -> String {
    caller
        .map(|c| c.identity.sub.clone())
        .unwrap_or_else(|| s.dev_user.clone())
}

/// Take one page of the engine's answer into `s`. Returns the address to cancel when the rows were cut.
fn absorb(s: &mut Statement, mut page: TrinoResponse) -> Option<String> {
    if let Some(err) = page.error.take() {
        s.state = StatementState::Failed;
        s.error = Some(trino::client_error(&err));
        s.next = None;
        return None;
    }
    if s.columns.is_empty()
        && let Some(columns) = page.columns.take()
    {
        s.columns = columns
            .into_iter()
            .map(|c| Column {
                name: c.name,
                r#type: c.ty,
            })
            .collect();
    }
    if let Some(data) = page.data.take() {
        for row in data {
            let size = row_bytes(&row);
            if s.rows.len() >= MAX_ROWS || s.bytes + size > MAX_BYTES {
                s.truncated = true;
                s.next = None;
                s.state = StatementState::Succeeded;
                s.held.store(s.bytes, Ordering::Relaxed);
                return page.next_uri.take();
            }
            s.bytes += size;
            s.rows.push(row);
        }
        s.held.store(s.bytes, Ordering::Relaxed);
    }
    s.next = page.next_uri.take();
    s.state = if s.next.is_none() {
        StatementState::Succeeded
    } else if page
        .stats
        .as_ref()
        .and_then(|stats| stats.get("state"))
        .and_then(Value::as_str)
        == Some("QUEUED")
    {
        StatementState::Pending
    } else {
        StatementState::Running
    };
    None
}

enum Step {
    /// The engine did not answer.
    Unreachable,
    /// The engine refused the token (it expired, or the engine does not accept it).
    Unauthorized,
    Malformed(String),
}

impl Step {
    fn response(self) -> Response {
        match self {
            Step::Unreachable => fail(StatusCode::BAD_GATEWAY, ENGINE_UNREACHABLE),
            Step::Unauthorized => fail(
                StatusCode::UNAUTHORIZED,
                "the query engine refused the sign-in; sign in again and poll the statement again",
            ),
            Step::Malformed(m) => fail(StatusCode::BAD_GATEWAY, m),
        }
    }
}

/// Poll the engine, with the token of the request in hand, until the statement ends or `wait` is up.
/// At least one page is read when the statement is not finished.
async fn advance(
    state: &AppState,
    caller: Option<&crate::auth::Principal>,
    s: &mut Statement,
    wait: Duration,
) -> Result<(), Step> {
    let deadline = Instant::now() + wait;
    let mut polls = 0u64;
    while !s.state.done() {
        let Some(next) = s.next.clone() else {
            s.state = StatementState::Succeeded;
            break;
        };
        let response = trino::trino_request(state, caller, state.http.get(&next))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "trino statement poll failed");
                Step::Unreachable
            })?;
        match response.status() {
            StatusCode::NOT_FOUND => {
                // The engine dropped the query: nobody polled it for its client timeout.
                s.state = StatementState::Canceled;
                s.next = None;
                s.error = Some(
                    "the engine no longer has this statement (it was not polled in time)".into(),
                );
                break;
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Step::Unauthorized),
            _ => {}
        }
        let page: TrinoResponse = response
            .json()
            .await
            .map_err(|e| Step::Malformed(format!("malformed Trino paginated response: {e}")))?;
        if let Some(cut) = absorb(s, page) {
            // Rows past the cap: the engine need not finish the rest.
            let _ = trino::trino_request(state, caller, state.http.delete(&cut))
                .send()
                .await;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let delay = Duration::from_millis((50 + polls * 25).min(500));
        polls += 1;
        sleep(delay.min(deadline - now)).await;
    }
    Ok(())
}

#[utoipa::path(
    post,
    operation_id = "statements_submit",
    path = "/api/statements",
    tag = "public",
    description = "Submit one SQL statement as the caller and get a handle to it. The call waits up to `waitSeconds` (0 to 30, default 5): when the statement ends in that time the answer is `200` and carries the first chunk of rows, otherwise it is `202` and the caller polls `statements_get`. A statement that fails in the engine is still `200` with `state: FAILED` and `error`.",
    request_body = SubmitRequest,
    responses(
        (status = 200, description = "The statement ended within the wait", body = StatementView),
        (status = 202, description = "The statement is still running; poll it", body = StatementView),
        (status = 400, description = "The statement is empty", body = crate::openapi::ErrorResponse),
        (status = 429, description = "The caller already holds ten statements; read or cancel one", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The query engine is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Not available in demo mode", body = crate::openapi::ErrorResponse),
    )
)]
async fn submit(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    ApiJson(req): ApiJson<SubmitRequest>,
) -> Response {
    if s.demo {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "statements are not available in demo mode",
        );
    }
    if req.sql.trim().is_empty() {
        return fail(StatusCode::BAD_REQUEST, "the statement is empty");
    }
    let caller = caller.as_ref().map(|c| &c.0);
    let owner = owner_of(&s, caller);
    s.statements.sweep(Instant::now());
    if s.statements.live_for(&owner) >= MAX_LIVE_PER_OWNER {
        return fail(
            StatusCode::TOO_MANY_REQUESTS,
            "you already hold ten statements; read or cancel one first",
        );
    }

    if s.statements.held_bytes() >= MAX_STORE_BYTES {
        return fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Nova holds as many results as it can right now; read or cancel a statement, or try again in a few minutes",
        );
    }

    let initial = match trino::statement_post(&s, caller, SOURCE)
        .body(req.sql)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "trino statement submit failed");
            return Step::Unreachable.response();
        }
    };
    let page: TrinoResponse = match initial.json().await {
        Ok(p) => p,
        Err(e) => return Step::Malformed(format!("malformed Trino response: {e}")).response(),
    };
    let Some(id) = page.id.clone() else {
        return Step::Malformed("the engine named no query id".into()).response();
    };

    let mut statement = Statement::new();
    let cut = absorb(&mut statement, page);
    if let Some(cut) = cut {
        let _ = trino::trino_request(&s, caller, s.http.delete(&cut))
            .send()
            .await;
    }
    let slot = s.statements.insert(id.clone(), owner, statement);
    let mut statement = slot.lock().await;
    if let Err(step) = advance(&s, caller, &mut statement, clamp_wait(req.wait_seconds)).await {
        return step.response();
    }
    let status = if statement.state.done() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    (status, Json(view(&id, &statement))).into_response()
}

#[utoipa::path(
    get,
    operation_id = "statements_get",
    path = "/api/statements/{id}",
    tag = "public",
    description = "The state of one of the caller's statements. Each call reads the engine's next pages with the caller's current token, for up to `waitSeconds` (0 to 30, default 0); the rows are read with `statements_chunk_get`. A statement that nobody polls for the engine's client timeout (5 minutes by default) is cancelled by the engine.",
    params(
        ("id" = String, Path, description = "The statement id from `statements_submit`"),
        ("waitSeconds" = Option<u64>, Query, description = "Wait up to this many seconds (0 to 30) for the statement to end"),
    ),
    responses(
        (status = 200, description = "The statement", body = StatementView),
        (status = 401, description = "The engine refused the caller's token", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such statement of the caller's", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The query engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
async fn poll(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let Some(slot) = s.statements.find(&id, &owner_of(&s, caller)) else {
        return no_such_statement();
    };
    let mut statement = slot.lock().await;
    let wait = Duration::from_secs(q.wait_seconds.unwrap_or(0).min(MAX_WAIT_SECONDS));
    if let Err(step) = advance(&s, caller, &mut statement, wait).await {
        return step.response();
    }
    Json(view(&id, &statement)).into_response()
}

#[utoipa::path(
    get,
    operation_id = "statements_chunk_get",
    path = "/api/statements/{id}/chunks/{n}",
    tag = "public",
    description = "One chunk (1 000 rows, the last one fewer) of a statement's rows, counted from 0. A chunk exists once it is full or the statement has ended; the call does not wait.",
    params(
        ("id" = String, Path, description = "The statement id"),
        ("n" = usize, Path, description = "The chunk number, from 0"),
    ),
    responses(
        (status = 200, description = "The rows of the chunk", body = ChunkView),
        (status = 404, description = "No such statement of the caller's, or no such chunk (yet)", body = crate::openapi::ErrorResponse),
    )
)]
async fn chunk(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    Path((id, n)): Path<(String, usize)>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let Some(slot) = s.statements.find(&id, &owner_of(&s, caller)) else {
        return no_such_statement();
    };
    let statement = slot.lock().await;
    let start = n.saturating_mul(CHUNK_ROWS);
    let end = start.saturating_add(CHUNK_ROWS);
    let readable = if statement.state.done() {
        start < statement.rows.len()
    } else {
        end <= statement.rows.len()
    };
    if !readable {
        return fail(StatusCode::NOT_FOUND, "no such chunk");
    }
    let rows = statement.rows[start..end.min(statement.rows.len())].to_vec();
    Json(ChunkView { chunk: n, rows }).into_response()
}

#[utoipa::path(
    delete,
    operation_id = "statements_cancel",
    path = "/api/statements/{id}",
    tag = "public",
    description = "Cancel one of the caller's statements in the engine and forget it. Idempotent: it answers `204` whether or not the statement exists or is the caller's (it cancels only the caller's own).",
    params(("id" = String, Path, description = "The statement id")),
    responses((status = 204, description = "Cancelled, or nothing to cancel")),
)]
async fn cancel(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    Path(id): Path<String>,
) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let owner = owner_of(&s, caller);
    if let Some(slot) = s.statements.find(&id, &owner) {
        let mut statement = slot.lock().await;
        if let Some(next) = statement.next.take() {
            // The engine cancels a query on DELETE of its next address.
            let _ = trino::trino_request(&s, caller, s.http.delete(&next))
                .send()
                .await;
        }
        if !statement.state.done() {
            statement.state = StatementState::Canceled;
        }
    }
    s.statements.remove(&id, &owner);
    StatusCode::NO_CONTENT.into_response()
}

/// The four routes, all uncacheable (the answer carries a person's rows).
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/statements", post(submit))
        .route("/statements/{id}", get(poll).delete(cancel))
        .route("/statements/{id}/chunks/{n}", get(chunk))
        .route_layer(axum::middleware::from_fn(crate::notebooks::no_store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Identity, Principal};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A pretend engine: `POST /v1/statement` answers page 0 and `GET .../{n}` answers page n.
    /// The last page has no `nextUri`; with `endless` every page has one.
    struct FakeTrino {
        base: String,
        /// `(state, columns, rows)` per page.
        pages: Vec<(&'static str, bool, Vec<Vec<Value>>)>,
        endless: bool,
        deletes: Mutex<usize>,
        sources: Mutex<Vec<String>>,
        /// Answer every poll with this status instead of a page.
        poll_status: Mutex<Option<StatusCode>>,
    }

    impl FakeTrino {
        fn page(&self, n: usize) -> Value {
            let index = n.min(self.pages.len() - 1);
            let (state, with_columns, rows) = &self.pages[index];
            let last = index == self.pages.len() - 1 && !self.endless;
            let mut page = json!({ "id": "q1", "stats": { "state": state } });
            if !last {
                page["nextUri"] = json!(format!("{}/v1/statement/q1/{}", self.base, n + 1));
            }
            if *with_columns {
                page["columns"] = json!([{ "name": "x", "type": "integer" }]);
            }
            if !rows.is_empty() {
                page["data"] = json!(rows);
            }
            page
        }
    }

    async fn serve(
        pages: Vec<(&'static str, bool, Vec<Vec<Value>>)>,
        endless: bool,
    ) -> Arc<FakeTrino> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let fake = Arc::new(FakeTrino {
            base,
            pages,
            endless,
            deletes: Mutex::new(0),
            sources: Mutex::new(Vec::new()),
            poll_status: Mutex::new(None),
        });
        let app = Router::new()
            .route(
                "/v1/statement",
                post(
                    |State(f): State<Arc<FakeTrino>>, headers: axum::http::HeaderMap| async move {
                        let source = headers
                            .get("x-trino-source")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        f.sources.lock().unwrap().push(source);
                        Json(f.page(0))
                    },
                ),
            )
            .route(
                "/v1/statement/q1/{n}",
                get(
                    |State(f): State<Arc<FakeTrino>>, Path(n): Path<usize>| async move {
                        if let Some(status) = *f.poll_status.lock().unwrap() {
                            return status.into_response();
                        }
                        Json(f.page(n)).into_response()
                    },
                )
                .delete(|State(f): State<Arc<FakeTrino>>| async move {
                    *f.deletes.lock().unwrap() += 1;
                    StatusCode::NO_CONTENT
                }),
            )
            .with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        fake
    }

    fn person(sub: &str) -> Principal {
        Principal {
            identity: Identity {
                sub: sub.into(),
                display_name: sub.into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: false,
            },
            via: "bearer",
            access_token: None,
        }
    }

    fn app(state: Arc<AppState>, who: &str) -> Router {
        Router::new()
            .nest("/api", router())
            .layer(axum::Extension(person(who)))
            .with_state(state)
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(b) => {
                request = request.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 26)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    fn rows(n: usize) -> Vec<Vec<Value>> {
        (0..n).map(|i| vec![json!(i)]).collect()
    }

    #[tokio::test]
    async fn a_short_statement_ends_within_the_wait_and_carries_its_first_chunk() {
        let fake = serve(
            vec![
                ("QUEUED", false, vec![]),
                ("RUNNING", true, rows(3)),
                ("FINISHED", false, vec![]),
            ],
            false,
        )
        .await;
        let app = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        let (status, view) = call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["statementId"], "q1");
        assert_eq!(view["state"], "SUCCEEDED");
        assert_eq!(view["columns"], json!([{ "name": "x", "type": "integer" }]));
        assert_eq!(
            (view["rowCount"].clone(), view["chunks"].clone()),
            (json!(3), json!(1))
        );
        assert_eq!(view["rows"], json!([[0], [1], [2]]));
        assert_eq!(
            *fake.sources.lock().unwrap(),
            vec!["stardelt-api".to_string()]
        );
    }

    #[tokio::test]
    async fn a_slow_statement_is_202_and_a_poll_with_a_wait_finishes_it() {
        let fake = serve(
            vec![
                ("QUEUED", false, vec![]),
                ("RUNNING", true, vec![]),
                ("RUNNING", false, rows(2)),
                ("FINISHED", false, vec![]),
            ],
            false,
        )
        .await;
        let app = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        let (status, view) = call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x", "waitSeconds": 0 })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(
            view["state"] == "PENDING" || view["state"] == "RUNNING",
            "{view}"
        );
        assert!(view["rows"].is_null(), "no rows before the end: {view}");
        let (status, view) = call(&app, "GET", "/api/statements/q1?waitSeconds=5", None).await;
        assert_eq!(
            (status, view["state"].clone(), view["rowCount"].clone()),
            (StatusCode::OK, json!("SUCCEEDED"), json!(2))
        );
    }

    #[tokio::test]
    async fn cancel_sends_one_delete_forgets_the_statement_and_is_idempotent() {
        let fake = serve(vec![("RUNNING", true, vec![])], true).await;
        let app = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        let (status, _) = call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x", "waitSeconds": 0 })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, _) = call(&app, "DELETE", "/api/statements/q1", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(*fake.deletes.lock().unwrap(), 1);
        assert_eq!(
            call(&app, "GET", "/api/statements/q1", None).await.0,
            StatusCode::NOT_FOUND
        );
        let (status, _) = call(&app, "DELETE", "/api/statements/q1", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(*fake.deletes.lock().unwrap(), 1, "nothing left to cancel");
    }

    #[tokio::test]
    async fn rows_are_read_in_chunks_of_a_thousand() {
        let fake = serve(vec![("FINISHED", true, rows(2_500))], false).await;
        let app = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        let (_, view) = call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x" })),
        )
        .await;
        assert_eq!(
            (view["rowCount"].clone(), view["chunks"].clone()),
            (json!(2_500), json!(3))
        );
        assert_eq!(view["rows"].as_array().unwrap().len(), 1_000);
        let (status, last) = call(&app, "GET", "/api/statements/q1/chunks/2", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(last["rows"].as_array().unwrap().len(), 500);
        assert_eq!(last["rows"][0], json!([2_000]));
        assert_eq!(
            call(&app, "GET", "/api/statements/q1/chunks/3", None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_result_past_the_cap_is_cut_and_says_so() {
        let fake = serve(vec![("FINISHED", true, rows(120_000))], false).await;
        let app = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        let (_, view) = call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x" })),
        )
        .await;
        assert_eq!(view["state"], "SUCCEEDED");
        assert_eq!(
            (
                view["rowCount"].clone(),
                view["chunks"].clone(),
                view["truncated"].clone()
            ),
            (json!(100_000), json!(100), json!(true))
        );
    }

    #[tokio::test]
    async fn another_persons_statement_is_a_404_and_is_not_cancelled_by_them() {
        let fake = serve(vec![("RUNNING", true, vec![])], true).await;
        let state = crate::router_tests::test_state_with_trino(&fake.base);
        let alice = app(state.clone(), "alice");
        let bob = app(state, "bob");
        call(
            &alice,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x", "waitSeconds": 0 })),
        )
        .await;
        assert_eq!(
            call(&bob, "GET", "/api/statements/q1", None).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&bob, "GET", "/api/statements/q1/chunks/0", None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&bob, "DELETE", "/api/statements/q1", None).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(*fake.deletes.lock().unwrap(), 0, "bob cancelled nothing");
        assert_eq!(
            call(&alice, "GET", "/api/statements/q1", None).await.0,
            StatusCode::OK
        );
    }

    #[test]
    fn an_engine_error_is_a_failed_statement_with_one_clean_line() {
        let page: TrinoResponse = serde_json::from_value(json!({
            "id": "q1",
            "error": {
                "errorName": "SYNTAX_ERROR",
                "message": "line 1:1: mismatched input",
                "failureInfo": { "stack": ["io.trino.X"] }
            }
        }))
        .unwrap();
        let mut statement = Statement::new();
        absorb(&mut statement, page);
        assert_eq!(statement.state, StatementState::Failed);
        assert_eq!(
            statement.error.as_deref(),
            Some("SYNTAX_ERROR: line 1:1: mismatched input")
        );
        assert!(statement.next.is_none());
        let shown = serde_json::to_string(&view("q1", &statement)).unwrap();
        assert!(!shown.contains("io.trino"), "no stack in the view: {shown}");
    }

    #[tokio::test]
    async fn the_engine_dropping_the_query_reads_as_canceled() {
        let fake = serve(vec![("RUNNING", true, vec![])], true).await;
        let app = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x", "waitSeconds": 0 })),
        )
        .await;
        *fake.poll_status.lock().unwrap() = Some(StatusCode::NOT_FOUND);
        let (status, view) = call(&app, "GET", "/api/statements/q1", None).await;
        assert_eq!(
            (status, view["state"].clone()),
            (StatusCode::OK, json!("CANCELED"))
        );
        assert!(
            view["error"]
                .as_str()
                .unwrap()
                .contains("not polled in time")
        );
        // Canceled is final: a later poll does not ask the engine again (it would now answer 401).
        *fake.poll_status.lock().unwrap() = Some(StatusCode::UNAUTHORIZED);
        assert_eq!(
            call(&app, "GET", "/api/statements/q1", None).await.0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_refused_token_is_a_401_and_an_unreachable_engine_a_502() {
        let fake = serve(vec![("RUNNING", true, vec![])], true).await;
        let app_live = app(
            crate::router_tests::test_state_with_trino(&fake.base),
            "alice",
        );
        call(
            &app_live,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x", "waitSeconds": 0 })),
        )
        .await;
        *fake.poll_status.lock().unwrap() = Some(StatusCode::UNAUTHORIZED);
        assert_eq!(
            call(&app_live, "GET", "/api/statements/q1", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let dead = app(
            crate::router_tests::test_state_with_trino("http://127.0.0.1:1"),
            "alice",
        );
        let (status, body) = call(
            &dead,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x" })),
        )
        .await;
        assert_eq!(
            (status, body["error"].clone()),
            (StatusCode::BAD_GATEWAY, json!(ENGINE_UNREACHABLE))
        );
    }

    #[tokio::test]
    async fn an_empty_statement_is_a_400_and_demo_mode_a_503() {
        let live = app(
            crate::router_tests::test_state_with_trino("http://127.0.0.1:1"),
            "alice",
        );
        let (status, _) = call(
            &live,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "  " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let demo = app(crate::router_tests::test_state(), "alice");
        let (status, _) = call(
            &demo,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select 1" })),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_person_holds_at_most_ten_statements() {
        let fake = serve(vec![("RUNNING", true, vec![])], true).await;
        let state = crate::router_tests::test_state_with_trino(&fake.base);
        for n in 0..MAX_LIVE_PER_OWNER {
            state
                .statements
                .insert(format!("old{n}"), "alice".into(), Statement::new());
        }
        let app = app(state, "alice");
        let (status, body) = call(
            &app,
            "POST",
            "/api/statements",
            Some(json!({ "sql": "select x" })),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    }

    fn page(value: Value) -> TrinoResponse {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn rows_past_the_byte_budget_are_cut_and_the_statement_says_so() {
        let wide = json!("x".repeat(1024 * 1024));
        let rows: Vec<Vec<Value>> = (0..70).map(|_| vec![wide.clone()]).collect();
        let mut s = Statement::new();
        let cancel = absorb(
            &mut s,
            page(json!({ "id": "q1", "nextUri": "http://engine/next", "data": rows })),
        );
        assert_eq!(cancel.as_deref(), Some("http://engine/next"));
        assert_eq!((s.state, s.truncated), (StatementState::Succeeded, true));
        assert_eq!(s.rows.len(), MAX_BYTES / row_bytes(&[wide]));
        assert!(s.bytes <= MAX_BYTES);
        assert_eq!(s.held.load(Ordering::Relaxed), s.bytes);
    }

    #[test]
    fn a_small_page_is_kept_whole_and_counted() {
        let mut s = Statement::new();
        let cancel = absorb(
            &mut s,
            page(json!({ "id": "q1", "nextUri": "http://engine/next", "data": [[1], [2], [3]] })),
        );
        assert_eq!(cancel, None);
        assert_eq!((s.rows.len(), s.truncated), (3, false));
        assert!(s.bytes > 0);
        assert_eq!(s.held.load(Ordering::Relaxed), s.bytes);
    }

    #[test]
    fn the_store_adds_up_what_its_statements_hold() {
        let store = StatementStore::default();
        for (id, bytes) in [("a", 10), ("b", 20)] {
            let statement = Statement::new();
            statement.held.store(bytes, Ordering::Relaxed);
            store.insert(id.into(), "alice".into(), statement);
        }
        assert_eq!(store.held_bytes(), 30);
        store.remove("a", "alice");
        assert_eq!(store.held_bytes(), 20);
    }

    #[tokio::test]
    async fn a_store_that_holds_its_budget_refuses_a_new_statement_until_one_is_forgotten() {
        let fake = serve(vec![("FINISHED", true, rows(3))], false).await;
        let state = crate::router_tests::test_state_with_trino(&fake.base);
        let full = Statement::new();
        full.held.store(MAX_STORE_BYTES, Ordering::Relaxed);
        state.statements.insert("big".into(), "bob".into(), full);
        let app = app(state.clone(), "alice");
        let submit = Some(json!({ "sql": "select x" }));
        let (status, body) = call(&app, "POST", "/api/statements", submit.clone()).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        state.statements.remove("big", "bob");
        let (status, body) = call(&app, "POST", "/api/statements", submit).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[test]
    fn the_wait_is_clamped_to_thirty_seconds_and_defaults_to_five() {
        assert_eq!(clamp_wait(None), Duration::from_secs(5));
        assert_eq!(clamp_wait(Some(0)), Duration::ZERO);
        assert_eq!(clamp_wait(Some(999)), Duration::from_secs(30));
    }

    #[test]
    fn statements_nobody_named_for_an_hour_are_forgotten() {
        let store = StatementStore::default();
        store.insert("a".into(), "alice".into(), Statement::new());
        store.sweep(Instant::now());
        assert!(store.find("a", "alice").is_some());
        store.sweep(Instant::now() + KEEP + Duration::from_secs(1));
        assert!(store.find("a", "alice").is_none());
    }
}
