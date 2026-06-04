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

use crate::AppState;

#[derive(Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Serialize)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub query_id: Option<String>,
    pub stats: Option<Value>,
}

#[derive(Serialize)]
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

pub async fn run_query(State(s): State<Arc<AppState>>, Json(req): Json<QueryRequest>) -> Response {
    let initial = match s
        .http
        .post(format!("{}/v1/statement", s.trino_url))
        .header("X-Trino-User", &s.dev_user)
        .header("Content-Type", "text/plain")
        .body(req.sql)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let mut current: TrinoResponse = match initial.json().await {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("malformed Trino response: {e}")})),
            )
                .into_response();
        }
    };

    let mut all_rows: Vec<Vec<Value>> = Vec::new();
    let mut columns: Vec<Column> = Vec::new();
    let query_id = current.id.clone();
    let mut last_stats: Option<Value>;
    let mut poll_count = 0usize;

    loop {
        if let Some(err) = current.error {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": err,
                    "query_id": query_id,
                })),
            )
                .into_response();
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

        last_stats = current.stats.take();

        let Some(next) = current.next_uri.take() else {
            break;
        };

        // 50k row cap protects the backend
        if all_rows.len() > 50_000 {
            break;
        }

        // small backoff that grows slowly — Trino docs recommend ~50ms minimum
        let delay_ms = (50u64 + (poll_count as u64) * 25).min(500);
        sleep(Duration::from_millis(delay_ms)).await;
        poll_count += 1;

        current = match s
            .http
            .get(&next)
            .header("X-Trino-User", &s.dev_user)
            .send()
            .await
        {
            Ok(r) => match r.json().await {
                Ok(j) => j,
                Err(e) => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({
                            "error": format!("malformed Trino paginated response: {e}"),
                            "query_id": query_id,
                            "rows_so_far": all_rows.len(),
                        })),
                    )
                        .into_response();
                }
            },
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({
                        "error": e.to_string(),
                        "query_id": query_id,
                    })),
                )
                    .into_response();
            }
        };
    }

    Json(QueryResult {
        row_count: all_rows.len(),
        columns,
        rows: all_rows,
        query_id,
        stats: last_stats,
    })
    .into_response()
}
