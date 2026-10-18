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
    let path = req.uri().path().to_string();
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
}
