//! Personal settings: one JSON value per (person, key), kept in the state store
//! (ticket stardelt-nova#103, step 3). The UI decides what the keys mean; this module only
//! stores them, per owner (see `saved_queries::owner_sub`).
//!
//! Without a state store all three routes answer
//! `503 {"error":"the state store is not configured"}`.

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::auth::Principal;
use crate::saved_queries::{error, not_found, owner_sub, store_failed, unavailable};

/// Largest value Nova stores for one key, measured on the JSON text.
const MAX_VALUE_BYTES: usize = 16 * 1024;

/// `a-z`, `0-9`, `.`, `_`, `-`; 1 to 64 characters; starts with a letter or digit.
/// The same rule as the CHECK constraint in `0003_user_setting.sql`.
fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && key.len() <= 64
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

const BAD_KEY: &str =
    "key must be 1 to 64 characters: a-z, 0-9, '.', '_' or '-', starting with a letter or digit";

/// What `PUT /api/settings/{key}` answers: the key and the value as stored.
#[derive(Serialize, ToSchema)]
pub struct StoredSetting {
    key: String,
    /// Any JSON value.
    #[schema(value_type = Value)]
    value: serde_json::Value,
}

/// `GET /api/settings` — the caller's settings as one JSON object (`{}` when none).
#[utoipa::path(
    get,
    operation_id = "settings_list",
    path = "/api/settings",
    tag = "internal",
    description = "The caller's personal settings as one JSON object, key to value (`{}` when there are none). The UI decides what a key means; Nova only stores them.",
    responses(
        (status = 200, description = "Key to value", body = Object),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let owner = owner_sub(caller.as_ref().map(|c| &c.0), &s.dev_user);
    match sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT key, value FROM user_setting WHERE owner_sub = $1 ORDER BY key",
    )
    .bind(owner)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => Json(serde_json::Map::from_iter(rows)).into_response(),
        Err(e) => store_failed(e),
    }
}

/// `PUT /api/settings/{key}` — the body is the value (any JSON); creates or replaces.
/// Answers `200 {"key":…,"value":…}`.
#[utoipa::path(
    put,
    operation_id = "settings_put",
    path = "/api/settings/{key}",
    tag = "internal",
    description = "Create or replace one of the caller's settings. The body is the value itself, any JSON, at most 16 KiB of JSON text. A key is 1 to 64 characters: a-z, 0-9, '.', '_' or '-', starting with a letter or digit.",
    params(("key" = String, Path, description = "The setting's key")),
    request_body(content = Value, description = "The value: any JSON"),
    responses(
        (status = 200, description = "The key and the value as stored", body = StoredSetting),
        (status = 400, description = "The key or the value is not valid", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn put(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Path(key): Path<String>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(value): ApiJson<serde_json::Value>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    if !valid_key(&key) {
        return error(StatusCode::BAD_REQUEST, BAD_KEY);
    }
    if value.to_string().len() > MAX_VALUE_BYTES {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("value must be at most {MAX_VALUE_BYTES} bytes of JSON"),
        );
    }
    let owner = owner_sub(caller.as_ref().map(|c| &c.0), &s.dev_user);
    match sqlx::query(
        "INSERT INTO user_setting (owner_sub, key, value) VALUES ($1, $2, $3) \
         ON CONFLICT (owner_sub, key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(owner)
    .bind(&key)
    .bind(&value)
    .execute(pool)
    .await
    {
        Ok(_) => Json(serde_json::json!({ "key": key, "value": value })).into_response(),
        Err(e) => store_failed(e),
    }
}

/// `DELETE /api/settings/{key}` — `204`, or `404` when the caller has no such key.
#[utoipa::path(
    delete,
    operation_id = "settings_remove",
    path = "/api/settings/{key}",
    tag = "internal",
    description = "Remove one of the caller's settings.",
    params(("key" = String, Path, description = "The setting's key")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "The caller has no such setting", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn remove(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Path(key): Path<String>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let owner = owner_sub(caller.as_ref().map(|c| &c.0), &s.dev_user);
    match sqlx::query("DELETE FROM user_setting WHERE owner_sub = $1 AND key = $2")
        .bind(owner)
        .bind(&key)
        .execute(pool)
        .await
    {
        Ok(done) if done.rows_affected() == 1 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => not_found(),
        Err(e) => store_failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_lowercase_words_up_to_64_characters() {
        for ok in ["theme", "query.page_size", "a", "0-day", &"k".repeat(64)] {
            assert!(valid_key(ok), "{ok} should be valid");
        }
        for bad in ["", "Theme", "-x", ".x", "a b", "a/b", "ä", &"k".repeat(65)] {
            assert!(!valid_key(bad), "{bad:?} should be invalid");
        }
    }
}
