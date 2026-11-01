//! In-memory job registry. Every query, load and transfer is recorded so Nova can
//! show a Databricks-style activity feed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, extract::State};
use serde::Serialize;
use std::sync::Arc;

use crate::AppState;

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Clone, utoipa::ToSchema)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: String,
    pub started_at: u64,
    pub detail: Option<String>,
    pub rows: Option<usize>,
    #[serde(default)]
    pub user: String,
    #[serde(skip)]
    pub owner: String,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        now_ms(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Record a completed job and return it. Keeps the newest 200.
#[allow(clippy::too_many_arguments)]
pub fn record(
    jobs: &Mutex<Vec<Job>>,
    kind: &str,
    title: impl Into<String>,
    status: &str,
    detail: Option<String>,
    rows: Option<usize>,
    user: &str,
    owner: &str,
) -> Job {
    let job = Job {
        id: new_id(kind),
        kind: kind.to_string(),
        title: title.into(),
        status: status.to_string(),
        started_at: now_ms(),
        detail,
        rows,
        user: user.to_string(),
        owner: owner.to_string(),
    };
    if let Ok(mut guard) = jobs.lock() {
        guard.insert(0, job.clone());
        guard.truncate(200);
    }
    job
}

fn visible<'a>(jobs: &'a [Job], who: Option<&crate::auth::Principal>) -> Vec<&'a Job> {
    match who {
        Some(p) if !p.identity.is_admin => {
            jobs.iter().filter(|j| j.owner == p.identity.sub).collect()
        }
        _ => jobs.iter().collect(),
    }
}

#[utoipa::path(
    get,
    operation_id = "jobs_list",
    path = "/api/jobs",
    tag = "internal",
    description = "The loads (`ingest_upload`) and copies (`tables_transfer`) this Nova process has run, newest first, at most the newest 200. They are kept in memory only, so a restart of Nova empties the list. A person sees their own jobs; an administrator sees all. `kind` is `load` or `transfer`, `status` is `succeeded` or `failed`, `started_at` is milliseconds since the Unix epoch, `detail` is a sentence (the row count, or why it failed) and `rows` the rows loaded. Field names are snake_case. These are not the scheduled pipelines: those are under `/api/pipelines`.",
    responses(
        (status = 200, description = "The caller's jobs, newest first", body = [Job]),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
) -> Json<Vec<Job>> {
    let guard = s.jobs.lock().expect("job registry poisoned");
    let who = caller.as_ref().map(|c| &c.0);
    Json(
        visible(&guard, who)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_within_one_millisecond() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let id = new_id("query");
            assert!(seen.insert(id), "duplicate job id");
        }
        assert_eq!(seen.len(), 1000);
    }

    fn principal(sub: &str, admin: bool) -> crate::auth::Principal {
        crate::auth::Principal {
            identity: crate::auth::Identity {
                sub: sub.into(),
                display_name: sub.into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: admin,
            },
            via: "session",
            access_token: None,
        }
    }

    fn job(owner: &str) -> Job {
        Job {
            id: format!("query-1-{owner}"),
            kind: "query".into(),
            title: "select 1".into(),
            status: "succeeded".into(),
            started_at: 0,
            detail: None,
            rows: Some(1),
            user: owner.into(),
            owner: owner.into(),
        }
    }

    #[test]
    fn non_admin_sees_only_own_jobs() {
        let jobs = vec![job("alice"), job("bob")];
        let alice = principal("alice", false);
        let seen: Vec<_> = visible(&jobs, Some(&alice))
            .into_iter()
            .map(|j| &j.owner)
            .collect();
        assert_eq!(seen, vec!["alice"]);
        let admin = principal("root", true);
        assert_eq!(visible(&jobs, Some(&admin)).len(), 2);
        assert_eq!(visible(&jobs, None).len(), 2);
    }
}
