//! Submit SQL to Trino, follow the nextUri pagination, return all rows JSON.
//!
//! Trino's HTTP statement protocol returns an initial response with a
//! `nextUri`; the client polls that until the response has no `nextUri`,
//! collecting `data` arrays along the way. We aggregate everything into one
//! response — fine for MVP, fine for queries that return up to a few-thousand
//! rows. Real streaming/cursor support is a future enhancement.

use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::sleep;

use crate::{AppState, api_json::ApiJson, jobs};

#[derive(Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Serialize, Clone)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub query_id: Option<String>,
    pub stats: Option<Value>,
    /// True when rows were cut at [`MAX_ROWS`]: the result is a prefix, not the
    /// whole answer. The UI says so instead of showing a silent partial table.
    pub truncated: bool,
}

#[derive(Serialize, Clone)]
pub struct Column {
    pub name: String,
    pub r#type: String,
}

#[derive(Deserialize)]
struct TrinoColumn {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Deserialize)]
struct TrinoResponse {
    id: Option<String>,
    #[serde(rename = "nextUri")]
    next_uri: Option<String>,
    columns: Option<Vec<TrinoColumn>>,
    data: Option<Vec<Vec<Value>>>,
    error: Option<Value>,
    stats: Option<Value>,
}

/// Rows Nova will hold in memory for one query. The page granularity means a
/// result is cut to exactly this many rows, not approximately.
const MAX_ROWS: usize = 50_000;

/// Cut `rows` to [`MAX_ROWS`]; true when anything was cut.
fn apply_row_cap(rows: &mut Vec<Vec<Value>>) -> bool {
    if rows.len() > MAX_ROWS {
        rows.truncate(MAX_ROWS);
        true
    } else {
        false
    }
}

/// Headers that identify the caller to Trino; identical on the statement POST
/// and every nextUri poll.
///
/// `X-Trino-User` is what Trino records in its query events, so it should name
/// the person who asked — the token identifies them for authentication, this
/// identifies them to the audit trail. Falls back to the dev user when nobody
/// is signed in.
fn upstream_headers(
    dev_user: &str,
    forward: bool,
    caller: Option<&crate::auth::Principal>,
) -> Vec<(reqwest::header::HeaderName, String)> {
    let user = caller
        .map(|c| c.identity.display_name.as_str())
        .unwrap_or(dev_user);
    let mut h = vec![(
        reqwest::header::HeaderName::from_static("x-trino-user"),
        user.to_string(),
    )];
    if let Some(v) = crate::auth::upstream_authorization(forward, caller) {
        h.push((reqwest::header::AUTHORIZATION, v));
    }
    h
}

/// Attach the caller's identity to a Trino request: the initial POST, every
/// `nextUri` poll and the cancel DELETE all carry the same `X-Trino-User` and
/// the same forwarded `Authorization` (when the platform asked for it).
fn trino_request(
    state: &AppState,
    caller: Option<&crate::auth::Principal>,
    rb: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut req = rb;
    for (k, v) in &upstream_headers(&state.dev_user, state.forward_identity, caller) {
        req = req.header(k, v);
    }
    req
}

/// How long `execute` waits for Trino before giving up (`NOVA_QUERY_TIMEOUT_SECS`,
/// default 300 s). The ingress cuts browser connections at 60 s regardless; the
/// default only matters when that timeout is raised or for in-cluster callers.
fn query_timeout_secs_from(get: impl Fn(&str) -> Option<String>) -> u64 {
    get("NOVA_QUERY_TIMEOUT_SECS")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(300)
}

fn query_timeout() -> Duration {
    Duration::from_secs(query_timeout_secs_from(|k| std::env::var(k).ok()))
}

/// Cancels the Trino query when it is dropped with a `nextUri` still armed.
///
/// `Drop` cannot be async, so the DELETE is spawned: a dropped future (client
/// disconnect, query timeout) still cancels the query server-side instead of
/// leaving it on the single Trino worker until Trino times out the abandoned
/// client itself. Disarm on every terminal exit — a DELETE against a finished
/// query is a harmless but noisy 404.
struct CancelOnDrop {
    http: reqwest::Client,
    next: Option<String>,
    user: String,
    auth: Option<String>,
}

impl CancelOnDrop {
    fn arm(&mut self, uri: String) {
        self.next = Some(uri);
    }

    fn disarm(&mut self) {
        self.next = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(uri) = self.next.take() {
            let http = self.http.clone();
            let user = self.user.clone();
            let auth = self.auth.clone();
            tokio::spawn(async move {
                let mut req = http.delete(uri).header("X-Trino-User", user);
                if let Some(a) = auth {
                    req = req.header(reqwest::header::AUTHORIZATION, a);
                }
                let _ = req.send().await;
            });
        }
    }
}

/// The engine's `source` for statements Nova runs on its own behalf (loads, transfers); the query
/// log shows it so a person can tell where a statement came from.
const SOURCE_NOVA: &str = "nova";

/// The statement POST: the caller's identity plus where the statement comes from. The engine records
/// `source` and the client tag in its query events, and the query log keeps them.
fn statement_post(
    state: &AppState,
    caller: Option<&crate::auth::Principal>,
    source: &str,
) -> reqwest::RequestBuilder {
    trino_request(
        state,
        caller,
        state
            .http
            .post(format!("{}/v1/statement", state.trino_url))
            .header("Content-Type", "text/plain")
            .header("X-Trino-Source", source)
            .header("X-Trino-Client-Tags", "nova"),
    )
}

/// Run SQL and return the aggregated result. Used by the query handler and by
/// load/transfer, which issue DDL/DML through the same path.
pub async fn execute(
    state: &AppState,
    sql: String,
    caller: Option<&crate::auth::Principal>,
) -> Result<QueryResult, String> {
    execute_from(state, sql, caller, SOURCE_NOVA).await
}

/// [`execute`] with an explicit `source` (the SQL editor passes `nova-sql-editor`).
pub async fn execute_from(
    state: &AppState,
    sql: String,
    caller: Option<&crate::auth::Principal>,
    source: &str,
) -> Result<QueryResult, String> {
    let guard_user = caller
        .map(|c| c.identity.display_name.as_str())
        .unwrap_or(&state.dev_user)
        .to_string();
    let guard_auth = crate::auth::upstream_authorization(state.forward_identity, caller);
    let mut guard = CancelOnDrop {
        http: state.http.clone(),
        next: None,
        user: guard_user,
        auth: guard_auth,
    };

    let request = statement_post(state, caller, source);
    let initial = request.body(sql).send().await.map_err(|e| {
        tracing::warn!(error = %e, "trino request failed");
        "the query engine is not reachable".to_string()
    })?;

    let mut current: TrinoResponse = initial
        .json()
        .await
        .map_err(|e| format!("malformed Trino response: {e}"))?;

    let mut all_rows: Vec<Vec<Value>> = Vec::new();
    let mut columns: Vec<Column> = Vec::new();
    let query_id = current.id.clone();
    let mut last_stats: Option<Value> = None;
    let mut truncated = false;
    let mut poll_count = 0usize;

    let timeout = query_timeout();
    let secs = timeout.as_secs();
    let polled = tokio::time::timeout(timeout, async {
        loop {
            if let Some(err) = current.error.take() {
                guard.disarm();
                return Err(client_error(&err));
            }

            if columns.is_empty()
                && let Some(cols) = current.columns.take()
            {
                columns = cols
                    .into_iter()
                    .map(|c| Column {
                        name: c.name,
                        r#type: c.ty,
                    })
                    .collect();
            }

            if let Some(d) = current.data.take() {
                all_rows.extend(d);
                if apply_row_cap(&mut all_rows) {
                    truncated = true;
                    // Trino cancels a query on DELETE nextUri: abandon the rest
                    // server-side instead of leaving it RUNNING.
                    if let Some(next) = current.next_uri.take() {
                        let _ = trino_request(state, caller, state.http.delete(&next))
                            .send()
                            .await;
                    }
                    guard.disarm();
                    break;
                }
            }

            if let Some(stats) = current.stats.take() {
                last_stats = Some(stats);
            }

            let Some(next) = current.next_uri.take() else {
                guard.disarm();
                break;
            };

            guard.arm(next.clone());

            let delay_ms = (50u64 + (poll_count as u64) * 25).min(500);
            sleep(Duration::from_millis(delay_ms)).await;
            poll_count += 1;

            // A poll without the caller's identity fails closed: Trino answers
            // 401 and the query surfaces as an error, never as another user's.
            let response = trino_request(state, caller, state.http.get(&next))
                .send()
                .await
                .map_err(|e| {
                    tracing::warn!(error = %e, "trino paginated request failed");
                    "the query engine is not reachable".to_string()
                })?;
            current = response
                .json()
                .await
                .map_err(|e| format!("malformed Trino paginated response: {e}"))?;
        }
        Ok::<(), String>(())
    })
    .await;

    match polled {
        Err(_) => Err(format!(
            "query exceeded the {secs}s limit and was cancelled"
        )),
        Ok(Err(e)) => Err(e),
        Ok(Ok(())) => Ok(QueryResult {
            row_count: all_rows.len(),
            columns,
            rows: all_rows,
            query_id,
            stats: last_stats,
            truncated,
        }),
    }
}

/// One line for the UI and the activity feed: Trino's `errorName` and its `message`.
///
/// Trino's error document also carries `failureInfo` — the full Java stack, the
/// coordinator and worker URLs, the build — internals no signed-in user needs and which
/// the Query page used to print verbatim. The document is logged once at debug level;
/// the response says what went wrong.
fn client_error(err: &Value) -> String {
    tracing::debug!(error = %err, "trino error");
    let name = err
        .get("errorName")
        .and_then(Value::as_str)
        .unwrap_or("Trino query failed");
    match err.get("message").and_then(Value::as_str) {
        // Trino's message already starts with `line L:C:`, so no location is added.
        Some(message) if !message.trim().is_empty() => {
            scrub_internal(&format!("{name}: {message}"))
        }
        _ => name.to_string(),
    }
}

/// Replace every URL-shaped token with `<internal>`: Trino's messages can name the
/// worker a task ran on (`http://10.244.0.59:8080/v1/task/...`). No regex crate for
/// this; splitting on whitespace is enough and keeps the dependency set as it is.
fn scrub_internal(message: &str) -> String {
    message
        .split_whitespace()
        .map(|token| {
            if token.starts_with("http://") || token.starts_with("https://") {
                "<internal>"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn run_query(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(req): ApiJson<QueryRequest>,
) -> Response {
    let title = truncate(&req.sql, 90);
    let (user, owner) = caller
        .as_ref()
        .map(|c| (c.0.identity.display_name.clone(), c.0.identity.sub.clone()))
        .unwrap_or_else(|| (s.dev_user.clone(), s.dev_user.clone()));

    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return match demo.run_query(&req.sql) {
            Ok(r) => {
                jobs::record(
                    &s.jobs,
                    "query",
                    title,
                    "succeeded",
                    r.query_id.clone(),
                    Some(r.row_count),
                    &user,
                    &owner,
                );
                Json(r).into_response()
            }
            Err(e) => {
                jobs::record(
                    &s.jobs,
                    "query",
                    title,
                    "failed",
                    Some(e.clone()),
                    None,
                    &user,
                    &owner,
                );
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e })),
                )
                    .into_response()
            }
        };
    }

    match execute_from(
        &s,
        req.sql,
        caller.as_ref().map(|c| &c.0),
        "nova-sql-editor",
    )
    .await
    {
        Ok(r) => {
            jobs::record(
                &s.jobs,
                "query",
                title,
                "succeeded",
                r.query_id.clone(),
                Some(r.row_count),
                &user,
                &owner,
            );
            Json(r).into_response()
        }
        Err(e) => {
            jobs::record(
                &s.jobs,
                "query",
                title,
                "failed",
                Some(e.clone()),
                None,
                &user,
                &owner,
            );
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response()
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > n {
        format!("{}…", one_line.chars().take(n).collect::<String>())
    } else {
        one_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a real Trino `TABLE_NOT_FOUND` answer, `failureInfo.stack` included.
    fn table_not_found() -> Value {
        serde_json::json!({
            "errorCode": 46,
            "errorLocation": { "lineNumber": 1, "columnNumber": 15 },
            "errorName": "TABLE_NOT_FOUND",
            "errorType": "USER_ERROR",
            "message": "line 1:15: Table 'warehouse.demo.nonexistent_tbl' does not exist",
            "failureInfo": {
                "type": "io.trino.sql.analyzer.SemanticExceptions$SemanticException",
                "message": "line 1:15: Table 'warehouse.demo.nonexistent_tbl' does not exist",
                "stack": [
                    "io.trino.sql.analyzer.SemanticExceptions.semanticException(SemanticExceptions.java:58)",
                    "io.trino.sql.analyzer.Analyzer.analyze(Analyzer.java:123)",
                    "io.trino.execution.QueryStateMachine$State.execute(QueryStateMachine.java:456)"
                ],
                "suppressed": [],
                "errorLocation": { "lineNumber": 1, "columnNumber": 15 }
            }
        })
    }

    #[test]
    fn client_error_keeps_message_and_drops_stack() {
        let out = client_error(&table_not_found());
        assert_eq!(
            out,
            "TABLE_NOT_FOUND: line 1:15: Table 'warehouse.demo.nonexistent_tbl' does not exist"
        );
        assert!(!out.contains("io.trino."), "stack leaked: {out}");
        assert!(!out.contains("failureInfo"), "raw document leaked: {out}");
    }

    #[test]
    fn client_error_falls_back_when_trino_is_terse() {
        assert_eq!(
            client_error(&serde_json::json!({ "errorName": "SYNTAX_ERROR" })),
            "SYNTAX_ERROR"
        );
        assert_eq!(
            client_error(&serde_json::json!({})),
            "Trino query failed",
            "an error document Nova cannot read still yields one line"
        );
    }

    #[test]
    fn scrub_internal_hides_urls() {
        assert_eq!(
            scrub_internal("Error fetching http://10.244.0.59:8080/v1/task/x: boom"),
            "Error fetching <internal> boom"
        );
        assert_eq!(
            scrub_internal("see https://trino.example/v1/info now"),
            "see <internal> now"
        );
        assert_eq!(scrub_internal("plain message"), "plain message");
    }

    #[test]
    fn client_error_scrubs_urls_inside_the_message() {
        let err = serde_json::json!({
            "errorName": "WORKER_FAILURE",
            "message": "Error fetching http://10.244.0.59:8080/v1/task/x: 503",
        });
        let out = client_error(&err);
        assert_eq!(out, "WORKER_FAILURE: Error fetching <internal> 503");
        assert!(!out.contains("10.244.0.59"), "pod IP leaked: {out}");
    }

    use crate::auth::{Identity, Principal, Secret};

    fn alice(token: Option<&str>) -> Principal {
        Principal {
            identity: Identity {
                sub: "s".into(),
                display_name: "alice".into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: false,
            },
            via: "session",
            access_token: token.map(Secret::new),
        }
    }

    fn test_state(forward: bool, dev_user: &str) -> AppState {
        AppState {
            dev_user: dev_user.into(),
            trino_url: "http://trino:8080".into(),
            lakekeeper_url: "http://lakekeeper:8181".into(),
            superset_url: "http://superset:8088".into(),
            jupyterhub_url: "http://proxy-public:80".into(),
            warehouse_name: "warehouse".into(),
            warehouse_id: tokio::sync::OnceCell::new(),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: forward,
            jobs: std::sync::Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(crate::demo::DemoState::default()),
            kube: None,
            workloads: crate::cluster::WorkloadCache::default(),
            admin_console_url: None,
            db: None,
        }
    }

    #[test]
    fn headers_without_caller_use_dev_user() {
        let h = upstream_headers("dev", true, None);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].0.as_str(), "x-trino-user");
        assert_eq!(h[0].1, "dev");
    }

    #[test]
    fn headers_forward_the_token_only_when_asked() {
        let p = alice(Some("t"));
        let on = upstream_headers("dev", true, Some(&p));
        assert_eq!(on.len(), 2);
        assert_eq!(on[0].1, "alice");
        assert_eq!(on[1].0, reqwest::header::AUTHORIZATION);
        assert_eq!(on[1].1, "Bearer t");

        let off = upstream_headers("dev", false, Some(&p));
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].1, "alice");
    }

    #[test]
    fn the_statement_post_names_its_source_and_the_nova_tag() {
        let state = test_state(true, "dev");
        let p = alice(Some("t"));
        let req = statement_post(&state, Some(&p), "nova-sql-editor")
            .build()
            .unwrap();
        assert_eq!(req.method(), reqwest::Method::POST);
        assert!(req.url().path().ends_with("/v1/statement"));
        assert_eq!(
            req.headers().get("x-trino-source").unwrap(),
            "nova-sql-editor"
        );
        assert_eq!(req.headers().get("x-trino-client-tags").unwrap(), "nova");
        assert_eq!(req.headers().get("x-trino-user").unwrap(), "alice");
        assert_eq!(req.headers().get("content-type").unwrap(), "text/plain");

        // Polls and cancels carry the identity but not the source (the engine takes it from the POST).
        let poll = trino_request(&state, Some(&p), state.http.get("http://example/"))
            .build()
            .unwrap();
        assert!(poll.headers().get("x-trino-source").is_none());

        // Everything Nova runs for itself reports the generic source.
        let own = statement_post(&state, None, SOURCE_NOVA).build().unwrap();
        assert_eq!(own.headers().get("x-trino-source").unwrap(), "nova");
    }

    #[test]
    fn trino_request_adds_authorization_when_forwarding() {
        let state = test_state(true, "dev");
        let p = alice(Some("t"));
        let req = trino_request(&state, Some(&p), state.http.get("http://example/"))
            .build()
            .unwrap();
        assert_eq!(
            req.headers().get("x-trino-user").unwrap(),
            "alice",
            "polls carry the caller, not the dev user"
        );
        assert_eq!(
            req.headers().get(reqwest::header::AUTHORIZATION).unwrap(),
            "Bearer t"
        );

        // Forwarding off: same user, no bearer.
        let off_state = test_state(false, "dev");
        let req = trino_request(&off_state, Some(&p), off_state.http.get("http://example/"))
            .build()
            .unwrap();
        assert_eq!(req.headers().get("x-trino-user").unwrap(), "alice");
        assert!(req.headers().get(reqwest::header::AUTHORIZATION).is_none());

        // No caller: dev user, no bearer even when forwarding is on.
        let req = trino_request(&state, None, state.http.get("http://example/"))
            .build()
            .unwrap();
        assert_eq!(req.headers().get("x-trino-user").unwrap(), "dev");
        assert!(req.headers().get(reqwest::header::AUTHORIZATION).is_none());
    }

    #[test]
    fn query_timeout_defaults_to_300s() {
        assert_eq!(query_timeout_secs_from(|_| None), 300);
        assert_eq!(
            query_timeout_secs_from(|k| (k == "NOVA_QUERY_TIMEOUT_SECS").then(|| "45".to_string())),
            45
        );
        // Garbage and zero fail closed to the default / stay positive.
        assert_eq!(
            query_timeout_secs_from(|_| Some("not-a-number".into())),
            300
        );
        assert_eq!(query_timeout_secs_from(|_| Some("0".into())), 300);
    }

    #[test]
    fn row_cap_cuts_to_exactly_max_rows() {
        let mut big: Vec<Vec<Value>> = (0..55_000).map(|i| vec![serde_json::json!(i)]).collect();
        assert!(apply_row_cap(&mut big));
        assert_eq!(big.len(), MAX_ROWS);

        let mut small: Vec<Vec<Value>> = (0..10).map(|i| vec![serde_json::json!(i)]).collect();
        assert!(!apply_row_cap(&mut small));
        assert_eq!(small.len(), 10);
    }
}
