//! Notification destinations: named email and webhook endpoints owned by one group
//! (design ticket stardelt-nova#273, slice a).
//!
//! A destination is an *address* other parts of Nova point at — an alert rule, a
//! pipeline failure notice, an access request. Slice a is the resource and its CRUD
//! only: nothing here sends mail or calls a webhook.
//!
//! The owner group decides who sees a destination, the same rule the rest of Nova uses
//! for group-facing resources: a member of the group, or an administrator; another
//! group's id answers `404`, exactly like an id that does not exist. In `dev` mode
//! there is no principal and one person, so every destination is theirs.
//!
//! No secret is stored here. A webhook's header secrets wait for the service-identity
//! decision (slice c).
//!
//! Without a state store (`NOVA_DATABASE_URL` unset) all three routes answer
//! `503 {"error":"the state store is not configured"}`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::auth::Principal;
use crate::saved_queries::{error, not_found, store_failed, unavailable, validate_group};

/// The longest webhook address Nova stores, in characters.
const MAX_URL_CHARS: usize = 2048;
/// A destination's name and its owner group, like the table's CHECK constraints.
const MAX_NAME_CHARS: usize = 64;
/// An email destination carries 1 to 20 recipients.
const MAX_RECIPIENTS: usize = 20;

/// A named email or webhook endpoint owned by one group (`GET /api/destinations`).
#[derive(Serialize, FromRow, Debug, PartialEq, utoipa::ToSchema)]
pub struct Destination {
    pub id: i64,
    pub owner_group: String,
    pub name: String,
    /// `email` or `webhook`.
    pub kind: String,
    /// The kind's address: `{"recipients": [...]}` or `{"url": "https://…"}`.
    #[schema(value_type = Value)]
    pub config: serde_json::Value,
    /// Epoch milliseconds, like `Job.started_at`.
    pub created_at: i64,
    /// Epoch milliseconds, like `Job.started_at`.
    pub updated_at: i64,
}

/// `POST /api/destinations` body.
#[derive(Deserialize, Debug, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewDestination {
    pub owner_group: String,
    pub name: String,
    pub kind: String,
    /// The kind's address: `{"recipients": [...]}` or `{"url": "https://…"}`.
    #[schema(value_type = Value)]
    pub config: serde_json::Value,
}

/// `{"recipients": [1 to 20 non-empty addresses]}` and nothing else needed.
fn email_config(config: &serde_json::Value) -> bool {
    config
        .get("recipients")
        .and_then(|v| v.as_array())
        .is_some_and(|list| {
            (1..=MAX_RECIPIENTS).contains(&list.len())
                && list
                    .iter()
                    .all(|r| r.as_str().is_some_and(|s| !s.trim().is_empty()))
        })
}

/// `{"url": "http(s)://…"}`, at most [`MAX_URL_CHARS`] characters and parseable.
fn webhook_config(config: &serde_json::Value) -> bool {
    let Some(raw) = config.get("url").and_then(|v| v.as_str()) else {
        return false;
    };
    raw.chars().count() <= MAX_URL_CHARS
        && matches!(url::Url::parse(raw), Ok(u) if matches!(u.scheme(), "http" | "https"))
}

/// Trim the text fields and check the address, so a caller gets a sentence instead of
/// a database error. The sentences are answered `400` by [`create`].
fn validate(req: NewDestination) -> Result<NewDestination, String> {
    if req.kind != "email" && req.kind != "webhook" {
        return Err("kind must be email or webhook".into());
    }
    if req.kind == "email" && !email_config(&req.config) {
        return Err(format!(
            "an email destination needs config.recipients: 1 to {MAX_RECIPIENTS} addresses"
        ));
    }
    if req.kind == "webhook" && !webhook_config(&req.config) {
        return Err("a webhook destination needs config.url with an http or https address".into());
    }
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("name must be 1 to {MAX_NAME_CHARS} characters"));
    }
    let owner_group = validate_group(&req.owner_group)?;
    Ok(NewDestination {
        owner_group,
        name,
        kind: req.kind,
        config: req.config,
    })
}

/// Whether `principal` may work with destinations owned by `group`: a member of the
/// group, an administrator, or — in `dev` mode, where there is one person — anybody.
fn member_of(group: &str, principal: Option<&Principal>) -> bool {
    principal.is_none_or(|p| p.identity.is_admin || p.identity.groups.iter().any(|g| g == group))
}

/// Whether `principal` may see this destination: the dev mode rule above, its owner
/// group, or an administrator.
///
/// `_dev_user` keeps the shape the other group-facing resources use; in `dev` mode the
/// static user is the only person, so it belongs to every group.
// The handlers filter in SQL (one round trip); the alert slice (b) asks row by row.
#[allow(dead_code)]
pub fn may_see(destination: &Destination, principal: Option<&Principal>, _dev_user: &str) -> bool {
    member_of(&destination.owner_group, principal)
}

/// `GET /api/destinations` — the caller's groups' destinations, newest first.
///
/// `?group=<group>` narrows to one group and answers `403` for a group the caller is
/// not in; the group rule here is the same one [`create`] applies.
#[utoipa::path(
    get,
    operation_id = "destinations_list",
    path = "/api/destinations",
    tag = "internal",
    description = "The notification destinations of the caller's groups, newest first. `?group=<group>` narrows to one group the caller belongs to. Times are epoch milliseconds.",
    params(
        ("group" = Option<String>, Query, description = "Only this group's destinations"),
    ),
    responses(
        (status = 200, description = "The caller's groups' destinations", body = Vec<Destination>),
        (status = 403, description = "The caller is not a member of the requested group", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let principal = caller.as_ref().map(|c| &c.0);
    let narrow = params
        .get("group")
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty());
    if let Some(group) = &narrow
        && !member_of(group, principal)
    {
        return error(
            StatusCode::FORBIDDEN,
            &format!("you are not a member of the group {group}"),
        );
    }
    let groups: Vec<String> = principal
        .map(|p| p.identity.groups.clone())
        .unwrap_or_default();
    // `$1` carries the dev-mode and administrator case: every group is theirs.
    let see_all = principal.is_none_or(|p| p.identity.is_admin);
    let sql = "SELECT id, owner_group, name, kind, config, created_at, updated_at \
               FROM destination \
               WHERE ($1::boolean OR owner_group = ANY($2)) \
                 AND ($3::text IS NULL OR owner_group = $3) \
               ORDER BY created_at DESC, id DESC";
    match sqlx::query_as::<_, Destination>(sql)
        .bind(see_all)
        .bind(groups)
        .bind(narrow)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => store_failed(e),
    }
}

/// `POST /api/destinations` — `201` with the stored row.
#[utoipa::path(
    post,
    operation_id = "destinations_create",
    path = "/api/destinations",
    tag = "internal",
    description = "Create a notification destination for one of the caller's groups. An `email` destination carries `config.recipients` (1 to 20 addresses); a `webhook` destination carries `config.url` (http or https). Nothing here sends anything.",
    request_body = NewDestination,
    responses(
        (status = 201, description = "The stored row", body = Destination),
        (status = 400, description = "The kind, the address, the name or the group is not valid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller is not a member of the owner group", body = crate::openapi::ErrorResponse),
        (status = 409, description = "A destination with this name already exists in the group", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn create(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(body): ApiJson<NewDestination>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let new = match validate(body) {
        Ok(new) => new,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    if !member_of(&new.owner_group, caller.as_ref().map(|c| &c.0)) {
        return error(
            StatusCode::FORBIDDEN,
            &format!("you are not a member of the group {}", new.owner_group),
        );
    }
    // Epoch milliseconds, the unit `Job.started_at` uses. `chrono` is built without its
    // `clock` feature (workspace `Cargo.toml`), so `Utc::now()` is not available.
    let now = crate::jobs::now_ms() as i64;
    let duplicate = format!(
        "a destination named {} already exists in {}",
        new.name, new.owner_group
    );
    let sql = "INSERT INTO destination (owner_group, name, kind, config, created_at, updated_at) \
               VALUES ($1, $2, $3, $4, $5, $5) \
               RETURNING id, owner_group, name, kind, config, created_at, updated_at";
    match sqlx::query_as::<_, Destination>(sql)
        .bind(&new.owner_group)
        .bind(&new.name)
        .bind(&new.kind)
        .bind(&new.config)
        .bind(now)
        .fetch_one(pool)
        .await
    {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        // The UNIQUE (owner_group, name) the table declares, for the race the check
        // before the insert cannot close.
        Err(e) if is_unique_violation(&e) => error(StatusCode::CONFLICT, &duplicate),
        Err(e) => store_failed(e),
    }
}

/// `DELETE /api/destinations/{id}` — `204`; `404` for another group's id or none.
///
/// No reference check yet: alerts do not exist, so nothing can point at a destination.
#[utoipa::path(
    delete,
    operation_id = "destinations_remove",
    path = "/api/destinations/{id}",
    tag = "internal",
    description = "Delete one of the caller's groups' notification destinations.",
    params(("id" = i64, Path, description = "The destination's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such destination, or it is not the caller's", body = crate::openapi::ErrorResponse),
        (status = 400, description = "The id is not a number", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn remove(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let principal = caller.as_ref().map(|c| &c.0);
    let groups: Vec<String> = principal
        .map(|p| p.identity.groups.clone())
        .unwrap_or_default();
    let see_all = principal.is_none_or(|p| p.identity.is_admin);
    let done = sqlx::query(
        "DELETE FROM destination WHERE id = $1 AND ($2::boolean OR owner_group = ANY($3))",
    )
    .bind(id)
    .bind(see_all)
    .bind(groups)
    .execute(pool)
    .await;
    match done {
        Ok(done) if done.rows_affected() == 1 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => not_found(),
        Err(e) => store_failed(e),
    }
}

/// PostgreSQL's `unique_violation`.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|code| code == "23505")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Identity;

    fn principal(sub: &str, groups: &[&str], is_admin: bool) -> Principal {
        Principal {
            identity: Identity {
                sub: sub.into(),
                display_name: sub.into(),
                email: None,
                name: None,
                groups: groups.iter().map(|g| g.to_string()).collect(),
                roles: vec![],
                groups_overage: false,
                is_admin,
            },
            via: "session",
            access_token: None,
        }
    }

    fn email(owner_group: &str, name: &str, recipients: serde_json::Value) -> NewDestination {
        NewDestination {
            owner_group: owner_group.into(),
            name: name.into(),
            kind: "email".into(),
            config: serde_json::json!({ "recipients": recipients }),
        }
    }

    fn webhook(owner_group: &str, name: &str, url: &str) -> NewDestination {
        NewDestination {
            owner_group: owner_group.into(),
            name: name.into(),
            kind: "webhook".into(),
            config: serde_json::json!({ "url": url }),
        }
    }

    fn destination(owner_group: &str) -> Destination {
        Destination {
            id: 1,
            owner_group: owner_group.into(),
            name: "on call".into(),
            kind: "webhook".into(),
            config: serde_json::json!({ "url": "https://example.com/hook" }),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn validate_trims_and_accepts_an_email_and_a_webhook() {
        let ok = validate(email(
            "  analytics  ",
            "  on call  ",
            serde_json::json!(["a@b.c"]),
        ))
        .unwrap();
        assert_eq!(ok.owner_group, "analytics");
        assert_eq!(ok.name, "on call");
        assert_eq!(ok.kind, "email");
        assert_eq!(ok.config["recipients"][0], "a@b.c");

        let ok = validate(webhook("analytics", "pager", "https://example.com/hook")).unwrap();
        assert_eq!(ok.kind, "webhook");
        assert_eq!(ok.config["url"], "https://example.com/hook");

        // Besides "a@b.c" and "https" the limits themselves are accepted at the edge.
        let twenty: Vec<String> = (0..20).map(|i| format!("p{i}@b.c")).collect();
        assert!(validate(email("g", "n", serde_json::json!(twenty))).is_ok());
        let long = format!(
            "https://{}.example.com/{}",
            "a".repeat(100),
            "b".repeat(1900)
        );
        assert!(long.chars().count() <= MAX_URL_CHARS);
        assert!(validate(webhook("g", "n", &long)).is_ok());
        assert!(validate(webhook("g", "n", "http://example.com/hook")).is_ok());
    }

    #[test]
    fn validate_rejects_an_unknown_kind() {
        let mut req = webhook("g", "n", "https://example.com/hook");
        req.kind = "smtp".into();
        assert_eq!(validate(req).unwrap_err(), "kind must be email or webhook");
    }

    #[test]
    fn validate_rejects_a_bad_email_config() {
        let message = "an email destination needs config.recipients: 1 to 20 addresses";
        let err = |r| validate(r).unwrap_err();
        assert_eq!(err(email("g", "n", serde_json::json!([]))), message);
        assert_eq!(
            err(email("g", "n", serde_json::json!(["a@b.c", "  "]))),
            message
        );
        assert_eq!(
            err(email("g", "n", serde_json::json!(["a@b.c", 7]))),
            message
        );
        let many: Vec<String> = (0..21).map(|i| format!("p{i}@b.c")).collect();
        assert_eq!(err(email("g", "n", serde_json::json!(many))), message);
        // Not the shape at all: no `recipients`, or a body that is not an object.
        assert_eq!(err(email("g", "n", serde_json::Value::Null)), message);
        let mut req = webhook("g", "n", "https://example.com/hook");
        req.kind = "email".into();
        assert_eq!(err(req), message);
    }

    #[test]
    fn validate_rejects_a_bad_webhook_config() {
        let message = "a webhook destination needs config.url with an http or https address";
        let err = |r| validate(r).unwrap_err();
        assert_eq!(err(webhook("g", "n", "")), message);
        assert_eq!(err(webhook("g", "n", "example.com/hook")), message);
        assert_eq!(err(webhook("g", "n", "ftp://example.com/hook")), message);
        assert_eq!(err(webhook("g", "n", "javascript:alert(1)")), message);
        let too_long = format!("https://example.com/{}", "b".repeat(MAX_URL_CHARS));
        assert_eq!(err(webhook("g", "n", &too_long)), message);
        let mut req = email("g", "n", serde_json::json!(["a@b.c"]));
        req.kind = "webhook".into();
        assert_eq!(err(req), message);
    }

    #[test]
    fn validate_rejects_a_bad_name_and_reuses_the_group_sentence() {
        let err = |r| validate(r).unwrap_err();
        assert_eq!(
            err(email("g", "   ", serde_json::json!(["a@b.c"]))),
            "name must be 1 to 64 characters"
        );
        assert_eq!(
            err(email("g", &"n".repeat(65), serde_json::json!(["a@b.c"]))),
            "name must be 1 to 64 characters"
        );
        assert!(validate(email("g", &"n".repeat(64), serde_json::json!(["a@b.c"]))).is_ok());
        let bad_group = "group must be 1 to 64 characters, letters, digits, dot, underscore or \
                         dash, starting and ending with a letter or digit";
        assert_eq!(
            err(email("-g", "n", serde_json::json!(["a@b.c"]))),
            bad_group
        );
        assert_eq!(err(email("", "n", serde_json::json!(["a@b.c"]))), bad_group);
        assert_eq!(
            err(email(&"g".repeat(65), "n", serde_json::json!(["a@b.c"]))),
            bad_group
        );
    }

    #[test]
    fn may_see_is_the_owner_group_or_an_administrator_and_everything_in_dev_mode() {
        let owner = destination("analytics");
        let member = principal("alice", &["analytics"], false);
        let outsider = principal("bob", &["sales"], false);
        let admin = principal("root", &["sales"], true);
        assert!(may_see(&owner, Some(&member), "stardelt-dev"));
        assert!(!may_see(&owner, Some(&outsider), "stardelt-dev"));
        assert!(may_see(&owner, Some(&admin), "stardelt-dev"));
        assert!(may_see(&owner, None, "stardelt-dev"));
    }
}
