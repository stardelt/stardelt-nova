//! Saved queries: a person's named SQL snippets, kept in the state store
//! (ticket stardelt-nova#103, step 2).
//!
//! Every row has an owner, the caller's `sub` in `oidc` mode or the static dev user in
//! `dev` mode, and the owner is the only one who can list or delete it. Somebody else's
//! id answers `404`, exactly like an id that does not exist: the API never confirms
//! that another person's query is there.
//!
//! Without a state store (`NOVA_DATABASE_URL` unset) all three routes answer
//! `503 {"error":"the state store is not configured"}`.

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::auth::Principal;

const MAX_TITLE_CHARS: usize = 200;
const MAX_SQL_CHARS: usize = 100_000;

/// A row as the API returns it. Times are epoch milliseconds, like `Job.started_at`.
#[derive(Serialize, FromRow, Debug, PartialEq, utoipa::ToSchema)]
pub struct SavedQuery {
    pub id: i64,
    pub title: String,
    pub sql: String,
    pub warehouse: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// `POST /api/saved-queries` body.
#[derive(Deserialize, Debug, utoipa::ToSchema)]
pub struct NewSavedQuery {
    pub title: String,
    pub sql: String,
    /// The warehouse the query was written for; free text, may be empty.
    #[serde(default)]
    pub warehouse: String,
}

/// Who owns what the caller saves: the token's `sub` in `oidc` mode, the static dev
/// user in `dev` mode (the rule `trino.rs` uses for the activity feed's owner).
pub fn owner_sub(principal: Option<&Principal>, dev_user: &str) -> String {
    principal
        .map(|p| p.identity.sub.clone())
        .unwrap_or_else(|| dev_user.to_string())
}

/// Trim the text fields and enforce the same limits the table's CHECK constraints
/// do, so a caller gets a sentence instead of a database error.
fn validate(req: NewSavedQuery) -> Result<NewSavedQuery, String> {
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err("title must not be empty".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "title must be at most {MAX_TITLE_CHARS} characters"
        ));
    }
    if req.sql.trim().is_empty() {
        return Err("sql must not be empty".into());
    }
    if req.sql.chars().count() > MAX_SQL_CHARS {
        return Err(format!("sql must be at most {MAX_SQL_CHARS} characters"));
    }
    Ok(NewSavedQuery {
        title,
        sql: req.sql,
        warehouse: req.warehouse.trim().to_string(),
    })
}

pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

pub(crate) fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not found")
}

/// The answer while the state store is not configured.
pub(crate) fn unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "the state store is not configured",
    )
}

/// A database error is logged in full and answered in one sentence.
pub(crate) fn store_failed(e: sqlx::Error) -> Response {
    tracing::warn!(error = %e, "state store query failed");
    error(StatusCode::INTERNAL_SERVER_ERROR, "the state store failed")
}

/// The columns `SavedQuery` reads; times as epoch milliseconds.
const COLUMNS: &str = "id, title, sql, warehouse, \
    (extract(epoch FROM created_at) * 1000)::bigint AS created_at, \
    (extract(epoch FROM updated_at) * 1000)::bigint AS updated_at";

/// `GET /api/saved-queries` — the caller's own, newest first.
#[utoipa::path(
    get,
    operation_id = "saved_queries_list",
    path = "/api/saved-queries",
    tag = "public",
    description = "The caller's own saved queries, most recently changed first. Times are epoch milliseconds.",
    responses(
        (status = 200, description = "The caller's saved queries", body = Vec<SavedQuery>),
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
    let sql = format!(
        "SELECT {COLUMNS} FROM saved_query WHERE owner_sub = $1 \
         ORDER BY saved_query.updated_at DESC, id DESC"
    );
    match sqlx::query_as::<_, SavedQuery>(sqlx::AssertSqlSafe(sql))
        .bind(owner)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => store_failed(e),
    }
}

/// `POST /api/saved-queries` — `201` with the stored row.
#[utoipa::path(
    post,
    operation_id = "saved_queries_create",
    path = "/api/saved-queries",
    tag = "public",
    description = "Save a query for the caller. The title is at most 200 characters and the SQL at most 100 000.",
    request_body = NewSavedQuery,
    responses(
        (status = 201, description = "The stored row", body = SavedQuery),
        (status = 400, description = "The title or the SQL is empty or too long", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn create(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(body): ApiJson<NewSavedQuery>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let new = match validate(body) {
        Ok(new) => new,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let owner = owner_sub(caller.as_ref().map(|c| &c.0), &s.dev_user);
    let sql = format!(
        "INSERT INTO saved_query (owner_sub, title, sql, warehouse) \
         VALUES ($1, $2, $3, $4) RETURNING {COLUMNS}"
    );
    match sqlx::query_as::<_, SavedQuery>(sqlx::AssertSqlSafe(sql))
        .bind(owner)
        .bind(new.title)
        .bind(new.sql)
        .bind(new.warehouse)
        .fetch_one(pool)
        .await
    {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => store_failed(e),
    }
}

/// `DELETE /api/saved-queries/{id}` — `204`; `404` when the id is not the caller's.
///
/// The id is taken as a string and parsed here: axum's own `Path<i64>` rejection is
/// plain text, and every error under `/api/` is JSON.
#[utoipa::path(
    delete,
    operation_id = "saved_queries_remove",
    path = "/api/saved-queries/{id}",
    tag = "public",
    description = "Delete one of the caller's saved queries.",
    params(("id" = i64, Path, description = "The saved query's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such saved query, or it is not the caller's", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn remove(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return not_found();
    };
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let owner = owner_sub(caller.as_ref().map(|c| &c.0), &s.dev_user);
    match sqlx::query("DELETE FROM saved_query WHERE id = $1 AND owner_sub = $2")
        .bind(id)
        .bind(owner)
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
    use crate::auth::Identity;

    fn principal(sub: &str) -> Principal {
        Principal {
            identity: Identity {
                sub: sub.into(),
                display_name: sub.into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: false,
            },
            via: "session",
            access_token: None,
        }
    }

    fn new(title: &str, sql: &str, warehouse: &str) -> NewSavedQuery {
        NewSavedQuery {
            title: title.into(),
            sql: sql.into(),
            warehouse: warehouse.into(),
        }
    }

    #[test]
    fn owner_is_the_sub_in_oidc_mode_and_the_dev_user_otherwise() {
        let alice = principal("alice-sub");
        assert_eq!(owner_sub(Some(&alice), "stardelt-dev"), "alice-sub");
        assert_eq!(owner_sub(None, "stardelt-dev"), "stardelt-dev");
    }

    #[test]
    fn validate_trims_and_accepts_a_normal_request() {
        let ok = validate(new("  Daily trips ", "select 1", " warehouse ")).unwrap();
        assert_eq!(ok.title, "Daily trips");
        assert_eq!(ok.sql, "select 1");
        assert_eq!(ok.warehouse, "warehouse");
    }

    #[test]
    fn validate_rejects_empty_and_oversized_text() {
        let err = |r| validate(r).unwrap_err();
        assert_eq!(err(new("  ", "select 1", "")), "title must not be empty");
        assert_eq!(err(new("t", " \n", "")), "sql must not be empty");
        assert_eq!(
            err(new(&"t".repeat(201), "select 1", "")),
            "title must be at most 200 characters"
        );
        assert_eq!(
            err(new("t", &"x".repeat(100_001), "")),
            "sql must be at most 100000 characters"
        );
        assert!(validate(new(&"t".repeat(200), &"x".repeat(100_000), "")).is_ok());
    }
}
