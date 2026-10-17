//! In-memory job registry. Every query, load and transfer is recorded so Nova can
//! show a Databricks-style activity feed.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, extract::State};
use serde::Serialize;
use std::sync::Arc;

use crate::AppState;

#[derive(Serialize, Clone)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: String,
    pub started_at: u64,
    pub detail: Option<String>,
    pub rows: Option<usize>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", now_ms())
}

/// Record a completed job and return it. Keeps the newest 200.
pub fn record(
    jobs: &Mutex<Vec<Job>>,
    kind: &str,
    title: impl Into<String>,
    status: &str,
    detail: Option<String>,
    rows: Option<usize>,
) -> Job {
    let job = Job {
        id: new_id(kind),
        kind: kind.to_string(),
        title: title.into(),
        status: status.to_string(),
        started_at: now_ms(),
        detail,
        rows,
    };
    if let Ok(mut guard) = jobs.lock() {
        guard.insert(0, job.clone());
        guard.truncate(200);
    }
    job
}

pub async fn list(State(s): State<Arc<AppState>>) -> Json<Vec<Job>> {
    let guard = s.jobs.lock().expect("job registry poisoned");
    Json(guard.clone())
}
