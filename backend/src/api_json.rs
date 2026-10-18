//! `ApiJson<T>`: axum's `Json` request extractor with a JSON rejection body.
//!
//! Every handler error in Nova answers `{"error": "..."}`. The rejections axum's own
//! `Json` extractor raises — malformed JSON, a missing field, a wrong `content-type`, a
//! body over the 2 MiB limit — answer plain text instead, so a client that trusts one
//! rule ("errors are JSON") had to special-case four shapes. This extractor routes those
//! rejections through the same envelope. The response side keeps using `axum::Json`.

use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Same as `axum::Json<T>`, but a rejection is `{"error": "..."}` with the status axum
/// chose (400 malformed, 415 wrong content-type, 413 too large, 422 wrong shape).
#[derive(axum::extract::FromRequest)]
#[from_request(via(Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

/// The rejection body. `body_text()` is axum's own description of what was wrong with
/// the request ("Failed to parse the request body as JSON: key must be a string at line
/// 1 column 2"); it never contains server state or upstream URLs.
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        Self {
            status: r.status(),
            message: r.body_text(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request, routing::post};
    use tower::ServiceExt;

    async fn echo(ApiJson(value): ApiJson<serde_json::Value>) -> Json<serde_json::Value> {
        Json(value)
    }

    fn app() -> Router {
        Router::new().route("/echo", post(echo))
    }

    async fn error_body(response: Response) -> (StatusCode, String) {
        let status = response.status();
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "an API rejection is JSON"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("error body");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON error body");
        let message = value["error"]
            .as_str()
            .expect("the envelope has an `error` string")
            .to_string();
        (status, message)
    }

    #[tokio::test]
    async fn malformed_body_is_json_400() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/echo")
                    .header("content-type", "application/json")
                    .body(Body::from("{bad json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, message) = error_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            message.contains("Failed to parse the request body"),
            "got {message}"
        );
    }

    #[tokio::test]
    async fn wrong_content_type_is_json_415() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/echo")
                    .header("content-type", "text/plain")
                    .body(Body::from("x"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, message) = error_body(response).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(message.contains("Content-Type"), "got {message}");
    }

    #[tokio::test]
    async fn a_valid_body_still_passes_through() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/echo")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"select 1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
