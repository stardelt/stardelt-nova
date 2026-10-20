//! Audit trail: who changed what, in one line per mutating request.
//!
//! Deliberately a middleware rather than calls inside handlers: a route added later
//! cannot forget it, and there is exactly one place to read when asking "what does Nova
//! record?". The record is the *action*, not its content — method, path, status and the
//! authenticated principal, never a query string or a body. Statement-level auditing
//! belongs in Trino's query events, not in an HTTP log that would start capturing
//! literals (and, on the sign-in path, `code=` parameters).
//!
//! Reads are not audited: they are the overwhelming majority of traffic, and a log that
//! records everything is a log nobody reads.
//!
//! This layer sits *inside* the auth middlewares, because it needs the principal — which
//! means a refused request never reaches it. That is not a hole: `require_auth` and
//! `require_admin` record their own refusals on the same `audit` target, with the actor
//! when they know it. Between them: the audit target carries what was done and what was
//! refused; the application log carries everything else.
//!
//! The line goes to the `audit` tracing target at INFO, so it can be routed separately
//! from the application log (`RUST_LOG=audit=info,info`).

use axum::{extract::Request, http::Method, middleware::Next, response::Response};
use serde::Serialize;

/// The public path a caller used, including the `/api` prefix that nesting strips.
///
/// Inside `.nest("/api", ...)` the router rewrites `req.uri()` to the inner path
/// (`/query`), while `OriginalUri` keeps what the caller sent (`/api/query`). Audit
/// lines must carry the public path so a grep for `/api/` finds them. Falls back to
/// `uri().path()` where no outer router set it (dev mode, tests).
pub fn original_path(req: &Request) -> String {
    req.extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string())
}

/// Fields an audit line carries, extracted before the request is consumed.
struct Actor {
    name: String,
    sub: String,
    via: &'static str,
}

/// One line per mutating request. GET/HEAD/OPTIONS pass through untouched.
pub async fn record_mutations(req: Request, next: Next) -> Response {
    let audited = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !audited {
        return next.run(req).await;
    }

    let method = req.method().clone();
    let path = original_path(&req);
    // `require_auth` inserts a Principal; in dev mode there is none and the single user
    // is the operator, which is what the line should say rather than pretending.
    let actor = req
        .extensions()
        .get::<crate::auth::Principal>()
        .map(|p| Actor {
            name: p.identity.display_name.clone(),
            sub: p.identity.sub.clone(),
            via: p.via,
        });

    let response = next.run(req).await;
    let status = response.status().as_u16();

    match actor {
        Some(a) => tracing::info!(
            target: "audit",
            actor = %a.name,
            sub = %a.sub,
            via = a.via,
            method = %method,
            path = %path,
            status,
            "mutating request"
        ),
        None => tracing::info!(
            target: "audit",
            actor = "dev",
            method = %method,
            path = %path,
            status,
            "mutating request"
        ),
    }

    response
}

/// One connection event: what was done, to which scope, group and name, by whom. Never a value,
/// never a field's value — the field *names* are fine. The line goes to the `audit` target, so a
/// deployment that routes `audit` separately gets these beside the mutating-request lines.
///
/// `via` names how the right was held when that is not the obvious way: an administrator who
/// deletes a group connection they are not a member of is recorded as `via = administrator`, so an
/// incident action reads differently from a member's own.
pub fn connection_event(
    action: &str,
    who: &crate::auth::Identity,
    scope: &str,
    group: Option<&str>,
    name: &str,
    via: Option<&str>,
) {
    match (group, via) {
        (Some(group), Some(via)) => tracing::info!(
            target: "audit",
            action,
            scope,
            group = %group,
            name = %name,
            actor = %who.display_name,
            sub = %who.sub,
            via = %via,
            "connection"
        ),
        (Some(group), None) => tracing::info!(
            target: "audit",
            action,
            scope,
            group = %group,
            name = %name,
            actor = %who.display_name,
            sub = %who.sub,
            "connection"
        ),
        (None, Some(via)) => tracing::info!(
            target: "audit",
            action,
            scope,
            name = %name,
            actor = %who.display_name,
            sub = %who.sub,
            via = %via,
            "connection"
        ),
        (None, None) => tracing::info!(
            target: "audit",
            action,
            scope,
            name = %name,
            actor = %who.display_name,
            sub = %who.sub,
            "connection"
        ),
    }
}

/// One pipeline log read: who read which task's log. **No field can hold log content**, and the
/// run and task come from the checked pipeline, never from an unchecked browser value.
pub fn pipeline_log_event(
    who: &crate::auth::Identity,
    namespace: &str,
    pipeline: &str,
    run: &str,
    task: &str,
) {
    tracing::info!(
        target: "audit",
        action = "pipeline-log",
        namespace = %namespace,
        pipeline = %pipeline,
        run = %run,
        task = %task,
        actor = %who.display_name,
        sub = %who.sub,
        "pipeline log read"
    );
}

/// What a pipeline write records beside the action: the statement's **length** and the cron
/// text (stardelt-nova#210). There is deliberately no field for the statement itself — a line on
/// the `audit` target must never start carrying SQL literals.
#[derive(Debug, Clone, Copy)]
pub struct PipelineWrite<'a> {
    pub statement_chars: usize,
    pub schedule: Option<&'a str>,
}

/// One pipeline action: run, pause, resume, stop, create, update or delete. Who, which pipeline
/// and run, and how it ended (`ok`, `refused:<why>`, `error`). **No field can hold a note, log
/// text or a statement**: a write adds the statement's length and the cron, never its text.
pub fn pipeline_event(
    action: &str,
    who: &crate::auth::Identity,
    namespace: &str,
    pipeline: &str,
    run: Option<&str>,
    outcome: &str,
    write: Option<PipelineWrite<'_>>,
) {
    let statement_chars = write.map(|write| write.statement_chars);
    let schedule = write.and_then(|write| write.schedule);
    tracing::info!(
        target: "audit",
        action = %action,
        namespace = %namespace,
        pipeline = %pipeline,
        run = run.unwrap_or(""),
        outcome = %outcome,
        statement_chars = statement_chars.unwrap_or(0),
        schedule = schedule.unwrap_or(""),
        actor = %who.display_name,
        sub = %who.sub,
        "pipeline action"
    );
    #[cfg(test)]
    test_sink::push(test_sink::Record {
        action: action.to_string(),
        actor: who.display_name.clone(),
        sub: who.sub.clone(),
        namespace: namespace.to_string(),
        pipeline: pipeline.to_string(),
        run: run.map(str::to_string),
        outcome: outcome.to_string(),
        statement_chars,
        schedule: schedule.map(str::to_string),
    });
}

/// Test-only capture of [`pipeline_event`]'s records.
///
/// Deliberately not a tracing subscriber: callsite interest in `tracing` is process-global and
/// decided at the first hit, so a parallel test that emits without a subscriber switches the
/// callsite off for a scoped one (this made the audit test fail only when the whole suite ran).
/// The sink records exactly what the line carries, and `every_action_is_audited_including_refusals`
/// filters by its own `sub` so other tests' records cannot interfere.
#[cfg(test)]
pub(crate) mod test_sink {
    use std::sync::{Mutex, OnceLock};

    /// The fields a pipeline audit line carries. There is no field for a note, log text or a
    /// statement: a write adds the statement's length and the cron, nothing more.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Record {
        pub action: String,
        pub actor: String,
        pub sub: String,
        pub namespace: String,
        pub pipeline: String,
        pub run: Option<String>,
        pub outcome: String,
        pub statement_chars: Option<usize>,
        pub schedule: Option<String>,
    }

    static RECORDS: OnceLock<Mutex<Vec<Record>>> = OnceLock::new();

    fn records() -> &'static Mutex<Vec<Record>> {
        RECORDS.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub fn push(record: Record) {
        records()
            .lock()
            .expect("audit test sink poisoned")
            .push(record);
    }

    /// Drain one actor's records, leaving everybody else's for their own test. The sink is
    /// process-wide and tests run in parallel, so a test must never drain another's records.
    pub fn take_for(sub: &str) -> Vec<Record> {
        let mut records = records().lock().expect("audit test sink poisoned");
        let mut mine = Vec::new();
        let mut theirs = Vec::new();
        for record in records.drain(..) {
            if record.sub == sub {
                mine.push(record);
            } else {
                theirs.push(record);
            }
        }
        *records = theirs;
        mine
    }
}

/// One published notebook: group, name, immutable version and stored size — and nothing
/// that could carry content. A notebook's cells, outputs, parameters and sha256 are
/// deliberately absent: the audit line records the action, not the bytes
/// (stardelt-nova#225).
pub fn notebook_publish_event(
    who: &crate::auth::Identity,
    group: &str,
    name: &str,
    version: &str,
    size: u64,
) {
    tracing::info!(
        target: "audit",
        action = "publish",
        group = %group,
        name = %name,
        version = %version,
        size,
        actor = %who.display_name,
        sub = %who.sub,
        "notebook published"
    );
    #[cfg(test)]
    notebook_test_sink::push(notebook_test_sink::Record {
        group: group.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        size,
        actor: who.display_name.clone(),
        sub: who.sub.clone(),
    });
}

/// Test-only capture of [`notebook_publish_event`]'s records, with the same reasoning as
/// [`test_sink`]: a scoped tracing subscriber is process-global and unreliable under the
/// parallel test runner. A record has no field that could hold notebook content.
#[cfg(test)]
pub(crate) mod notebook_test_sink {
    use std::sync::{Mutex, OnceLock};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Record {
        pub group: String,
        pub name: String,
        pub version: String,
        pub size: u64,
        pub actor: String,
        pub sub: String,
    }

    static RECORDS: OnceLock<Mutex<Vec<Record>>> = OnceLock::new();

    fn records() -> &'static Mutex<Vec<Record>> {
        RECORDS.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub fn push(record: Record) {
        records()
            .lock()
            .expect("notebook audit sink poisoned")
            .push(record);
    }

    /// Drain one actor's records, leaving everybody else's for their own test.
    pub fn take_for(sub: &str) -> Vec<Record> {
        let mut records = records().lock().expect("notebook audit sink poisoned");
        let mut mine = Vec::new();
        let mut theirs = Vec::new();
        for record in records.drain(..) {
            if record.sub == sub {
                mine.push(record);
            } else {
                theirs.push(record);
            }
        }
        *records = theirs;
        mine
    }
}

/// One break-glass read or refusal: administrator, connection, owner group, reason, outcome and
/// the request that carried it. **No field can hold a value**, so neither the tracing line nor
/// the state-store row can leak the thing that was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BreakGlassRecord {
    pub admin: String,
    pub admin_sub: String,
    pub owner_group: String,
    pub connection: String,
    pub scope: String,
    pub reason: String,
    pub outcome: String,
    pub request_id: Option<String>,
}

/// The break-glass audit line on the `audit` target: what happened, by whom, to which group and
/// connection, why, and how it ended. Never a value. The state-store row is written by the route.
pub fn break_glass_event(rec: &BreakGlassRecord) {
    tracing::info!(
        target: "audit",
        action = "break-glass",
        admin = %rec.admin,
        sub = %rec.admin_sub,
        owner_group = %rec.owner_group,
        connection = %rec.connection,
        scope = %rec.scope,
        reason = %rec.reason,
        outcome = %rec.outcome,
        request_id = rec.request_id.as_deref().unwrap_or(""),
        "break-glass read"
    );
}

/// What one table-documentation change records: the fields that changed, **never their text**.
/// `changed` holds `description`, `tags`, `owner`, `domain` or `column:<name>`.
// The fields are read only through `Debug`, which dead-code analysis does not count.
#[allow(dead_code)]
#[derive(Debug)]
pub struct CatalogDocRecord<'a> {
    pub actor: &'a str,
    pub sub: &'a str,
    pub table: &'a str,
    pub changed: &'a [&'a str],
}

/// The table-documentation audit line on the `audit` target. The description and the tags
/// themselves are content, not action, so only the names of the changed fields appear.
pub fn catalog_doc_event(who: &crate::auth::Identity, table: &str, changed: &[&str]) {
    let record = CatalogDocRecord {
        actor: &who.display_name,
        sub: &who.sub,
        table,
        changed,
    };
    tracing::info!(
        target: "audit",
        action = "catalog-documentation",
        record = ?record,
        "table documentation"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/thing", post(|| async { StatusCode::CREATED }))
            .route("/thing", get(|| async { StatusCode::OK }))
            .layer(axum::middleware::from_fn(record_mutations))
    }

    #[tokio::test]
    async fn mutations_pass_through_with_their_status() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/thing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn reads_are_untouched() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/thing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn original_path_prefers_the_public_uri() {
        let mut req = Request::builder()
            .method("POST")
            .uri("/query")
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::OriginalUri("/api/query".parse().unwrap()));
        assert_eq!(original_path(&req), "/api/query");
        let req = Request::builder()
            .method("POST")
            .uri("/query")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(original_path(&req), "/query");
    }

    #[tokio::test]
    async fn nested_router_keeps_the_public_path() {
        let inner = Router::new()
            .route("/thing", post(|| async { StatusCode::CREATED }))
            .layer(axum::middleware::from_fn(record_mutations));
        let app = Router::new().nest("/api", inner);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/thing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }
}
