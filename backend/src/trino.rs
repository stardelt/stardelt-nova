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

/// Run SQL and return the aggregated result. Used by the query handler and by
/// load/transfer, which issue DDL/DML through the same path.
pub async fn execute(
    state: &AppState,
    sql: String,
    caller: Option<&crate::auth::Principal>,
) -> Result<QueryResult, String> {
    // `X-Trino-User` is what Trino records in its query events, so it should name the person
    // who asked — the token identifies them for authentication, this identifies them to the
    // audit trail. Falls back to the dev user when nobody is signed in.
    let user = caller
        .map(|c| c.identity.display_name.as_str())
        .unwrap_or(&state.dev_user);
    let mut request = state
        .http
        .post(format!("{}/v1/statement", state.trino_url))
        .header("X-Trino-User", user)
        .header("Content-Type", "text/plain");
    if let Some(header) = crate::auth::upstream_authorization(state.forward_identity, caller) {
        request = request.header(reqwest::header::AUTHORIZATION, header);
    }
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
    let mut poll_count = 0usize;

    loop {
        if let Some(err) = current.error.take() {
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
        }

        if let Some(stats) = current.stats.take() {
            last_stats = Some(stats);
        }

        let Some(next) = current.next_uri.take() else {
            break;
        };

        // 50k row cap protects the backend
        if all_rows.len() > 50_000 {
            break;
        }

        let delay_ms = (50u64 + (poll_count as u64) * 25).min(500);
        sleep(Duration::from_millis(delay_ms)).await;
        poll_count += 1;

        current = state
            .http
            .get(&next)
            .header("X-Trino-User", &state.dev_user)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "trino paginated request failed");
                "the query engine is not reachable".to_string()
            })?
            .json()
            .await
            .map_err(|e| format!("malformed Trino paginated response: {e}"))?;
    }

    Ok(QueryResult {
        row_count: all_rows.len(),
        columns,
        rows: all_rows,
        query_id,
        stats: last_stats,
    })
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

    match execute(&s, req.sql, caller.as_ref().map(|c| &c.0)).await {
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
}
