//! The internal collector of the query log (decision D25, stardelt-nova#116): a second
//! listener, reachable inside the cluster only, that receives the engine's event-listener
//! posts and hands the parsed records to a consumer.
//!
//! It is best effort by design: a lost event costs one log row and never delays a query.
//! Authentication is one shared bearer token (Secret `nova-events`), compared in constant
//! time. The consumer is a task that stores each record when a state store is configured.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::lineage_event;
use crate::query_log_event::{self, QueryLogRecord};

/// Largest accepted body. A completed event carries the query plan and is routinely
/// larger than axum's 2 MiB default (a trivial query measured 290 KB).
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_LINEAGE_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Records waiting for the consumer. A full queue answers `503`, which the engine retries.
pub const QUEUE_LEN: usize = 10_000;
pub const LINEAGE_QUEUE_LEN: usize = 10_000;

/// A parsed record and the warehouse whose engine sent it.
#[derive(Debug)]
pub struct Accepted {
    pub record: QueryLogRecord,
    pub warehouse: String,
}

#[derive(Clone)]
struct Collector {
    token_digest: [u8; 32],
    tx: mpsc::Sender<Accepted>,
    lineage_tx: mpsc::Sender<lineage_event::LineageEvent>,
}

fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// Constant-time comparison of two digests.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `Authorization: Bearer <token>` → the token.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// A warehouse name is a DNS-label-like word: lower case letters, digits and dashes.
fn valid_warehouse(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "missing or wrong token" })),
    )
        .into_response()
}

fn error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

async fn ingest(State(c): State<Arc<Collector>>, headers: HeaderMap, body: Bytes) -> Response {
    match bearer(&headers) {
        Some(token) if same(&digest(token), &c.token_digest) => {}
        _ => return unauthorized(),
    }
    let warehouse = headers
        .get("x-stardelt-warehouse")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("default")
        .to_string();
    if !valid_warehouse(&warehouse) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid X-Stardelt-Warehouse".into(),
        );
    }
    let record = match query_log_event::parse(&body) {
        Ok(record) => record,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    match c.tx.try_send(Accepted { record, warehouse }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "collector queue is full".into(),
        ),
    }
}

async fn ingest_lineage(
    State(c): State<Arc<Collector>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match bearer(&headers) {
        Some(token) if same(&digest(token), &c.token_digest) => {}
        _ => return unauthorized(),
    }
    let event = match lineage_event::parse(&body) {
        Ok(event) => event,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.to_string()),
    };
    match c.lineage_tx.try_send(event) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "collector queue is full".into(),
        ),
    }
}

/// The collector's routes. `token` is the shared bearer token.
pub fn router(
    token: &str,
    tx: mpsc::Sender<Accepted>,
    lineage_tx: mpsc::Sender<lineage_event::LineageEvent>,
) -> Router {
    let state = Arc::new(Collector {
        token_digest: digest(token),
        tx,
        lineage_tx,
    });
    Router::new()
        .route(
            "/internal/trino-events",
            post(ingest).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/internal/lineage",
            post(ingest_lineage).layer(DefaultBodyLimit::max(MAX_LINEAGE_BODY_BYTES)),
        )
        .with_state(state)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Start the collector when `NOVA_INTERNAL_ADDR` and `NOVA_EVENTS_TOKEN` are both set;
/// otherwise do nothing (the default: no second port is opened).
pub async fn start(db: Option<sqlx::PgPool>) -> anyhow::Result<()> {
    let Some(addr) = env_nonempty("NOVA_INTERNAL_ADDR") else {
        return Ok(());
    };
    let Some(token) = env_nonempty("NOVA_EVENTS_TOKEN") else {
        warn!(
            "NOVA_INTERNAL_ADDR is set but NOVA_EVENTS_TOKEN is not: the query-log collector stays off"
        );
        return Ok(());
    };
    let (tx, mut rx) = mpsc::channel::<Accepted>(QUEUE_LEN);
    let (lineage_tx, mut lineage_rx) =
        mpsc::channel::<lineage_event::LineageEvent>(LINEAGE_QUEUE_LEN);
    // The consumer: store every record when there is a state store, otherwise just log it.
    let db_clone = db.clone();
    tokio::spawn(async move {
        while let Some(a) = rx.recv().await {
            info!(
                query_id = %a.record.query_id,
                state = a.record.state.as_str(),
                principal = %a.record.principal,
                warehouse = %a.warehouse,
                "query event"
            );
            if let Some(pool) = &db_clone
                && let Err(e) = crate::query_log_store::upsert(pool, &a).await
            {
                warn!(error = %e, query_id = %a.record.query_id, "could not store a query-log row");
            }
        }
    });
    // Lineage consumer
    tokio::spawn(async move {
        while let Some(ev) = lineage_rx.recv().await {
            if let Some(pool) = &db
                && let Err(e) = crate::lineage_store::insert_event(pool, &ev).await
            {
                warn!(error = %e, "could not store a lineage event");
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "query-log collector listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router(&token, tx, lineage_tx)).await {
            warn!(error = %e, "query-log collector stopped");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    const CREATED: &str = include_str!("testdata/query_log/created.json");
    const SELECT: &str = include_str!("testdata/query_log/completed_select.json");

    fn post_event(token: Option<&str>, warehouse: Option<&str>, body: String) -> Request<Body> {
        let mut req = Request::builder()
            .method("POST")
            .uri("/internal/trino-events");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(w) = warehouse {
            req = req.header("x-stardelt-warehouse", w);
        }
        req.body(Body::from(body)).unwrap()
    }

    async fn call(
        app: Router,
        req: Request<Body>,
    ) -> (StatusCode, Option<String>, Option<serde_json::Value>) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let challenge = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, challenge, serde_json::from_slice(&bytes).ok())
    }

    #[tokio::test]
    async fn a_missing_or_wrong_token_is_401_with_a_challenge() {
        let (tx, _rx) = mpsc::channel(4);
        let (ltx, _lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (status, challenge, _) =
            call(app.clone(), post_event(None, None, CREATED.into())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(challenge.as_deref(), Some("Bearer"));
        let (status, _, _) = call(app, post_event(Some("wrong"), None, CREATED.into())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_good_event_is_204_and_reaches_the_consumer_with_its_warehouse() {
        let (tx, mut rx) = mpsc::channel(4);
        let (ltx, _lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (status, _, _) = call(
            app.clone(),
            post_event(Some("s3cret"), None, CREATED.into()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let a = rx.recv().await.unwrap();
        assert_eq!(a.warehouse, "default");
        assert_eq!(a.record.state.as_str(), "QUEUED");
        assert_eq!(a.record.principal, "alice");

        let (status, _, _) = call(
            app,
            post_event(Some("s3cret"), Some("analytics"), SELECT.into()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let a = rx.recv().await.unwrap();
        assert_eq!(a.warehouse, "analytics");
        assert_eq!(a.record.state.as_str(), "FINISHED");
    }

    #[tokio::test]
    async fn a_bad_warehouse_or_body_is_400_and_not_queued() {
        let (tx, mut rx) = mpsc::channel(4);
        let (ltx, _lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (status, _, _) = call(
            app.clone(),
            post_event(Some("s3cret"), Some("Bad Name!"), CREATED.into()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, json) =
            call(app, post_event(Some("s3cret"), None, "not json".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            json.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("unusable")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_full_queue_is_503_so_the_engine_retries() {
        let (tx, _rx) = mpsc::channel(1);
        let (ltx, _lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (first, _, _) = call(
            app.clone(),
            post_event(Some("s3cret"), None, CREATED.into()),
        )
        .await;
        assert_eq!(first, StatusCode::NO_CONTENT);
        let (second, _, _) = call(app, post_event(Some("s3cret"), None, CREATED.into())).await;
        assert_eq!(second, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_body_over_the_axum_default_of_two_mebibytes_is_accepted() {
        let (tx, mut rx) = mpsc::channel(4);
        let (ltx, _lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let mut event: serde_json::Value = serde_json::from_str(SELECT).unwrap();
        event["metadata"]["plan"] = serde_json::Value::String("x".repeat(3 * 1024 * 1024));
        let (status, _, _) = call(app, post_event(Some("s3cret"), None, event.to_string())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn get_is_not_allowed() {
        let (tx, _rx) = mpsc::channel(1);
        let (ltx, _lrx) = mpsc::channel(4);
        let req = Request::builder()
            .method("GET")
            .uri("/internal/trino-events")
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = call(router("s3cret", tx, ltx), req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn warehouse_names_are_dns_label_like() {
        assert!(valid_warehouse("default"));
        assert!(valid_warehouse("analytics-2"));
        assert!(!valid_warehouse(""));
        assert!(!valid_warehouse("-x"));
        assert!(!valid_warehouse("Upper"));
        assert!(!valid_warehouse(&"a".repeat(64)));
    }

    #[test]
    fn digests_compare_equal_only_for_equal_tokens() {
        assert!(same(&digest("a"), &digest("a")));
        assert!(!same(&digest("a"), &digest("b")));
    }

    // --- lineage receiver tests (D40, WP-34a) ---

    fn post_lineage(token: Option<&str>, body: String) -> Request<Body> {
        let mut req = Request::builder().method("POST").uri("/internal/lineage");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        req = req.header("content-type", "application/json");
        req.body(Body::from(body)).unwrap()
    }

    fn valid_lineage_event() -> String {
        serde_json::json!({
            "eventType": "COMPLETE",
            "eventTime": "2026-01-01T00:00:00Z",
            "producer": "test",
            "schemaURL": "https://openlineage.io/spec/1-0-5/OpenLineage.json",
            "job": { "namespace": "default", "name": "test_job" },
            "run": { "runId": "00000000-0000-0000-0000-000000000001" },
            "inputs": [
                { "namespace": "mobility.gold", "name": "trips" }
            ],
            "outputs": [
                { "namespace": "mobility.gold", "name": "kpis" }
            ]
        })
        .to_string()
    }

    #[tokio::test]
    async fn lineage_receiver_missing_or_wrong_token_is_401() {
        let (tx, _rx) = mpsc::channel(4);
        let (ltx, _lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (status, challenge, _) =
            call(app.clone(), post_lineage(None, valid_lineage_event())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(challenge.as_deref(), Some("Bearer"));
        let (status, _, _) = call(app, post_lineage(Some("wrong"), valid_lineage_event())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn lineage_receiver_malformed_event_is_400_with_a_sentence() {
        let (tx, _rx) = mpsc::channel(4);
        let (ltx, mut lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (status, _, json) = call(app, post_lineage(Some("s3cret"), "not json".into())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let msg = json.unwrap()["error"].as_str().unwrap().to_string();
        assert!(!msg.is_empty(), "400 must carry a sentence: {msg}");
        assert!(lrx.try_recv().is_err());
    }

    #[tokio::test]
    async fn lineage_receiver_valid_event_is_204_and_reaches_consumer() {
        let (tx, _rx) = mpsc::channel(4);
        let (ltx, mut lrx) = mpsc::channel(4);
        let app = router("s3cret", tx, ltx);
        let (status, _, _) = call(app, post_lineage(Some("s3cret"), valid_lineage_event())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let ev = lrx.recv().await.unwrap();
        assert_eq!(ev.job_name, "test_job");
        assert_eq!(ev.run_id, "00000000-0000-0000-0000-000000000001");
        assert_eq!(ev.inputs.len(), 1);
        assert_eq!(ev.outputs.len(), 1);
    }
}
