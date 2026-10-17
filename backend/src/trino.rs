//! Submit SQL to Trino, follow the nextUri pagination, return all rows JSON.
//!
//! Trino's HTTP statement protocol returns an initial response with a
//! `nextUri`; the client polls that until the response has no `nextUri`,
//! collecting `data` arrays along the way. We aggregate everything into one
//! response — fine for MVP, fine for queries that return up to a few-thousand
//! rows. Real streaming/cursor support is a future enhancement.

use std::{sync::Arc, time::Duration};

use axum::{Json, extract::State, http::StatusCode, response::{IntoResponse, Response}};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::sleep;

use crate::{AppState, jobs};

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
pub async fn execute(state: &AppState, sql: String) -> Result<QueryResult, String> {
    let initial = state
        .http
        .post(format!("{}/v1/statement", state.trino_url))
        .header("X-Trino-User", &state.dev_user)
        .header("Content-Type", "text/plain")
        .body(sql)
        .send()
        .await
        .map_err(|e| e.to_string())?;

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
            return Err(serde_json::to_string(&err).unwrap_or_else(|_| "Trino query failed".into()));
        }

        if columns.is_empty() {
            if let Some(cols) = current.columns.take() {
                columns = cols
                    .into_iter()
                    .map(|c| Column { name: c.name, r#type: c.ty })
                    .collect();
            }
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
            .map_err(|e| e.to_string())?
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

pub async fn run_query(
    State(s): State<Arc<AppState>>,
    Json(req): Json<QueryRequest>,
) -> Response {
    let title = truncate(&req.sql, 90);

    if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        return match demo.run_query(&req.sql) {
            Ok(r) => {
                jobs::record(&s.jobs, "query", title, "succeeded", r.query_id.clone(), Some(r.row_count));
                Json(r).into_response()
            }
            Err(e) => {
                jobs::record(&s.jobs, "query", title, "failed", Some(e.clone()), None);
                (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": e }))).into_response()
            }
        };
    }

    match execute(&s, req.sql).await {
        Ok(r) => {
            jobs::record(&s.jobs, "query", title, "succeeded", r.query_id.clone(), Some(r.row_count));
            Json(r).into_response()
        }
        Err(e) => {
            jobs::record(&s.jobs, "query", title, "failed", Some(e.clone()), None);
            (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": e }))).into_response()
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
