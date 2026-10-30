//! Reading the query log (decision D25, stardelt-nova#116): `GET /api/query-log` and
//! `GET /api/query-log/{id}`.
//!
//! A person sees their own queries; an administrator can see everyone's (`scope=all`) and is
//! audited when opening somebody else's statement; the `team` scope (queries of service
//! principals the person's group owns) stays empty until ownership data exists. The rule itself
//! lives in `query_log.rs`; the SQL here only narrows rows, and every row is checked against
//! that rule again before it leaves. Somebody else's id answers `404`, exactly like a missing one.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sqlx::{FromRow, Postgres, QueryBuilder};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::Principal;
use crate::people;
use crate::query_log::{RowOwner, Scope, Viewer, in_scope, visible};
use crate::saved_queries::{error, not_found, store_failed, unavailable};

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const MAX_SEARCH_CHARS: usize = 200;
/// The statement preview in the list; the detail has the whole text.
const PREVIEW_CHARS: i32 = 300;

/// The person asking, with owned data (a [`Viewer`] borrows it).
struct Caller {
    username: String,
    sub: String,
    groups: Vec<String>,
    is_admin: bool,
}

impl Caller {
    /// In `oidc` mode the engine user is the token's `preferred_username`, which the identity
    /// keeps as `display_name`. In `dev` mode there is one user and he is the operator.
    fn of(principal: Option<&Principal>, dev_user: &str) -> Self {
        match principal {
            Some(p) => Self {
                username: p.identity.display_name.clone(),
                sub: p.identity.sub.clone(),
                groups: p.identity.groups.clone(),
                is_admin: p.identity.is_admin,
            },
            None => Self {
                username: dev_user.to_string(),
                sub: dev_user.to_string(),
                groups: vec![],
                is_admin: true,
            },
        }
    }

    fn viewer(&self) -> Viewer<'_> {
        Viewer {
            username: &self.username,
            sub: &self.sub,
            groups: &self.groups,
            is_admin: self.is_admin,
        }
    }
}

/// The checked query string of the list route.
#[derive(Debug, PartialEq)]
struct Params {
    scope: Scope,
    state: Option<String>,
    warehouse: Option<String>,
    search: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    limit: i64,
    cursor: Option<(i64, String)>,
}

fn parse_params(raw: &HashMap<String, String>) -> Result<Params, String> {
    let scope = match raw.get("scope").map(String::as_str) {
        None | Some("mine") => Scope::Mine,
        Some("team") => Scope::Team,
        Some("all") => Scope::All,
        Some(_) => return Err("scope must be mine, team or all".into()),
    };
    let state = match raw.get("state").map(|s| s.to_ascii_uppercase()) {
        None => None,
        Some(s) if ["QUEUED", "FINISHED", "FAILED", "CANCELED"].contains(&s.as_str()) => Some(s),
        Some(_) => return Err("state must be QUEUED, FINISHED, FAILED or CANCELED".into()),
    };
    let warehouse = raw.get("warehouse").filter(|w| !w.is_empty()).cloned();
    if warehouse.as_ref().is_some_and(|w| w.len() > 63) {
        return Err("warehouse is too long".into());
    }
    let search = raw
        .get("q")
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty());
    if search
        .as_ref()
        .is_some_and(|q| q.chars().count() > MAX_SEARCH_CHARS)
    {
        return Err(format!("q must be at most {MAX_SEARCH_CHARS} characters"));
    }
    let number = |name: &str| -> Result<Option<i64>, String> {
        raw.get(name)
            .map(|v| {
                v.parse::<i64>()
                    .map_err(|_| format!("{name} must be a number (epoch milliseconds)"))
            })
            .transpose()
    };
    let limit = match raw.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => match v.parse::<i64>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(format!("limit must be between 1 and {MAX_LIMIT}")),
        },
    };
    let cursor = raw
        .get("cursor")
        .map(|c| decode_cursor(c).ok_or_else(|| "invalid cursor".to_string()))
        .transpose()?;
    Ok(Params {
        scope,
        state,
        warehouse,
        search,
        from_ms: number("from")?,
        to_ms: number("to")?,
        limit,
        cursor,
    })
}

/// `created_at` in microseconds plus the id: the keyset position after the last item of a page.
fn encode_cursor(created_us: i64, id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{created_us}:{id}"))
}

fn decode_cursor(cursor: &str) -> Option<(i64, String)> {
    let text = String::from_utf8(URL_SAFE_NO_PAD.decode(cursor).ok()?).ok()?;
    let (us, id) = text.split_once(':')?;
    (!id.is_empty()).then(|| Some((us.parse().ok()?, id.to_string())))?
}

/// `%`, `_` and `\` in a search text are literals, not wildcards.
fn like_pattern(text: &str) -> String {
    let mut out = String::from("%");
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[derive(FromRow, Debug)]
struct ListRow {
    query_id: String,
    state: String,
    principal: String,
    source: Option<String>,
    warehouse: String,
    query_type: Option<String>,
    sql_preview: String,
    created_us: i64,
    wall_ms: Option<i64>,
    output_rows: Option<i64>,
    error_code: Option<String>,
}

/// One row of the query log list; the statement is cut to a preview.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryLogItem {
    id: String,
    state: String,
    /// The raw engine user (the `sub` once authz 8/8 is live).
    user: String,
    /// The person's display name when Nova knows the engine user; `None` falls back to
    /// `user` in the UI.
    user_name: Option<String>,
    source: Option<String>,
    warehouse: String,
    query_type: Option<String>,
    sql: String,
    created_at: i64,
    wall_ms: Option<i64>,
    output_rows: Option<i64>,
    error_code: Option<String>,
}

impl From<ListRow> for QueryLogItem {
    fn from(r: ListRow) -> Self {
        Self {
            id: r.query_id,
            state: r.state,
            user: r.principal,
            user_name: None,
            source: r.source,
            warehouse: r.warehouse,
            query_type: r.query_type,
            sql: r.sql_preview,
            created_at: r.created_us / 1000,
            wall_ms: r.wall_ms,
            output_rows: r.output_rows,
            error_code: r.error_code,
        }
    }
}

/// One page of the query log; `nextCursor` is null on the last page.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryLogPage {
    items: Vec<QueryLogItem>,
    next_cursor: Option<String>,
}

/// `GET /api/query-log?scope=mine|team|all&state=&warehouse=&q=&from=&to=&limit=&cursor=`
#[utoipa::path(
    get,
    operation_id = "query_log_list",
    path = "/api/query-log",
    tag = "public",
    description = "The statements run on the engine, newest first, a page at a time. A person sees their own; an administrator may ask for everyone's with scope=all. Times are epoch milliseconds.",
    params(
        ("scope" = Option<String>, Query, description = "mine (default), team or all. all is for administrators; team stays empty until ownership is recorded"),
        ("state" = Option<String>, Query, description = "QUEUED, FINISHED, FAILED or CANCELED, in any case"),
        ("warehouse" = Option<String>, Query, description = "Only queries that ran on this warehouse"),
        ("q" = Option<String>, Query, description = "Text inside the statement, or an exact query id; at most 200 characters"),
        ("from" = Option<i64>, Query, description = "Created at or after this time"),
        ("to" = Option<i64>, Query, description = "Created at or before this time"),
        ("limit" = Option<i64>, Query, description = "Page size from 1 to 200; 50 by default"),
        ("cursor" = Option<String>, Query, description = "The nextCursor of the previous page"),
    ),
    responses(
        (status = 200, description = "One page of the log", body = QueryLogPage),
        (status = 400, description = "A parameter is not valid; the message names it", body = crate::openapi::ErrorResponse),
        (status = 403, description = "scope=all by somebody who is not an administrator", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn list(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Query(raw): Query<HashMap<String, String>>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let who = Caller::of(caller.as_ref().map(|c| &c.0), &s.dev_user);
    let params = match parse_params(&raw) {
        Ok(p) => p,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    if params.scope == Scope::All && !who.is_admin {
        return error(StatusCode::FORBIDDEN, "the all scope is for administrators");
    }
    if params.scope == Scope::Team {
        // Service-principal ownership is not recorded yet, so no row can belong to a team.
        return Json(serde_json::json!({ "items": [], "nextCursor": null })).into_response();
    }

    let mut q: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT query_id, state, principal, source, warehouse, query_type, \
         left(sql_text, {PREVIEW_CHARS}) AS sql_preview, \
         (extract(epoch FROM created_at) * 1000000)::bigint AS created_us, \
         wall_ms, output_rows, error_code FROM query_log WHERE true"
    ));
    if params.scope == Scope::Mine {
        // The engine user is the sub once authz 8/8 is live; rows written before it (or in a
        // deployment that has not switched) still carry the sign-in name.
        q.push(" AND (principal = ")
            .push_bind(who.username.clone())
            .push(" OR principal = ")
            .push_bind(who.sub.clone())
            .push(")");
    }
    if let Some(state) = &params.state {
        q.push(" AND state = ").push_bind(state.clone());
    }
    if let Some(w) = &params.warehouse {
        q.push(" AND warehouse = ").push_bind(w.clone());
    }
    if let Some(ms) = params.from_ms {
        q.push(" AND created_at >= 'epoch'::timestamptz + (")
            .push_bind(ms)
            .push("::bigint * interval '1 millisecond')");
    }
    if let Some(ms) = params.to_ms {
        q.push(" AND created_at <= 'epoch'::timestamptz + (")
            .push_bind(ms)
            .push("::bigint * interval '1 millisecond')");
    }
    if let Some(text) = &params.search {
        q.push(" AND (sql_text ILIKE ")
            .push_bind(like_pattern(text))
            .push(" ESCAPE '\\' OR query_id = ")
            .push_bind(text.clone())
            .push(")");
    }
    if let Some((us, id)) = &params.cursor {
        q.push(" AND (created_at, query_id) < ('epoch'::timestamptz + (")
            .push_bind(*us)
            .push("::bigint * interval '1 microsecond'), ")
            .push_bind(id.clone())
            .push(")");
    }
    q.push(" ORDER BY created_at DESC, query_id DESC LIMIT ")
        .push_bind(params.limit + 1);

    let mut rows: Vec<ListRow> = match q.build_query_as().fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => return store_failed(e),
    };
    let more = rows.len() as i64 > params.limit;
    rows.truncate(params.limit as usize);
    let viewer = who.viewer();
    let before = rows.len();
    rows.retain(|r| {
        let owner = RowOwner {
            principal: &r.principal,
            owner_group: None,
        };
        in_scope(params.scope, &owner, &viewer)
    });
    if rows.len() != before {
        tracing::warn!(
            dropped = before - rows.len(),
            "query-log rows outside the caller's scope were dropped"
        );
    }
    let next = if more {
        rows.last()
            .map(|r| encode_cursor(r.created_us, &r.query_id))
    } else {
        None
    };
    let mut items: Vec<QueryLogItem> = rows.into_iter().map(QueryLogItem::from).collect();
    // An engine user that is a UUID is a person's subject; the directory turns it into a
    // name. Deduplicated, because a page is often the same person's queries.
    let mut subs: Vec<String> = items
        .iter()
        .filter(|item| people::is_uuid(&item.user))
        .map(|item| item.user.clone())
        .collect();
    subs.sort();
    subs.dedup();
    if !subs.is_empty() {
        let display = people::names(pool, &subs).await;
        for item in &mut items {
            item.user_name = display.get(&item.user).cloned();
        }
    }
    Json(QueryLogPage {
        items,
        next_cursor: next,
    })
    .into_response()
}

#[derive(FromRow, Debug)]
struct DetailRow {
    query_id: String,
    warehouse: String,
    state: String,
    principal: String,
    source: Option<String>,
    client_tags: Vec<String>,
    catalog: Option<String>,
    schema_name: Option<String>,
    query_type: Option<String>,
    sql_text: String,
    sql_hash: String,
    created_ms: i64,
    started_ms: Option<i64>,
    ended_ms: Option<i64>,
    queued_ms: Option<i64>,
    planning_ms: Option<i64>,
    execution_ms: Option<i64>,
    wall_ms: Option<i64>,
    cpu_ms: Option<i64>,
    peak_memory_bytes: Option<i64>,
    input_rows: Option<i64>,
    input_bytes: Option<i64>,
    output_rows: Option<i64>,
    output_bytes: Option<i64>,
    written_rows: Option<i64>,
    written_bytes: Option<i64>,
    error_code: Option<String>,
    error_message: Option<String>,
    tables_read: Vec<String>,
    tables_written: Vec<String>,
}

/// How long the query waited, planned and ran, in milliseconds.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryTiming {
    queued_ms: Option<i64>,
    planning_ms: Option<i64>,
    execution_ms: Option<i64>,
    wall_ms: Option<i64>,
    cpu_ms: Option<i64>,
}

/// How much data the query read, wrote and held in memory.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryData {
    input_rows: Option<i64>,
    input_bytes: Option<i64>,
    output_rows: Option<i64>,
    output_bytes: Option<i64>,
    written_rows: Option<i64>,
    written_bytes: Option<i64>,
    peak_memory_bytes: Option<i64>,
}

/// Why a query failed.
#[derive(Serialize, ToSchema)]
pub struct QueryFailure {
    code: String,
    message: Option<String>,
}

/// Everything stored about one query: the whole statement, timing, data volumes, tables.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryLogDetail {
    id: String,
    state: String,
    /// The raw engine user (the `sub` once authz 8/8 is live).
    user: String,
    user_name: Option<String>,
    source: Option<String>,
    client_tags: Vec<String>,
    warehouse: String,
    catalog: Option<String>,
    schema: Option<String>,
    query_type: Option<String>,
    sql: String,
    sql_hash: String,
    created_at: i64,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    timing: QueryTiming,
    data: QueryData,
    tables_read: Vec<String>,
    tables_written: Vec<String>,
    error: Option<QueryFailure>,
}

/// `GET /api/query-log/{id}` — everything stored about one query the caller may see.
#[utoipa::path(
    get,
    operation_id = "query_log_detail",
    path = "/api/query-log/{id}",
    tag = "public",
    description = "One query with its whole statement. Somebody else's query answers 404, exactly like a missing one; an administrator who opens another person's query is audited.",
    params(("id" = String, Path, description = "The engine's query id")),
    responses(
        (status = 200, description = "The stored query", body = QueryLogDetail),
        (status = 404, description = "No such query, or it is not the caller's to see", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn detail(
    State(s): State<Arc<AppState>>,
    caller: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Response {
    let Some(pool) = s.db.as_ref() else {
        return unavailable();
    };
    let who = Caller::of(caller.as_ref().map(|c| &c.0), &s.dev_user);
    let row: Option<DetailRow> = match sqlx::query_as(
        "SELECT query_id, warehouse, state, principal, source, client_tags, catalog, schema_name, \
            query_type, sql_text, sql_hash, \
            (extract(epoch FROM created_at) * 1000)::bigint AS created_ms, \
            (extract(epoch FROM started_at) * 1000)::bigint AS started_ms, \
            (extract(epoch FROM ended_at) * 1000)::bigint AS ended_ms, \
            queued_ms, planning_ms, execution_ms, wall_ms, cpu_ms, peak_memory_bytes, input_rows, \
            input_bytes, output_rows, output_bytes, written_rows, written_bytes, error_code, \
            error_message, tables_read, tables_written \
         FROM query_log WHERE query_id = $1",
    )
    .bind(&id)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(e) => return store_failed(e),
    };
    let Some(r) = row else {
        return not_found();
    };
    let owner = RowOwner {
        principal: &r.principal,
        owner_group: None,
    };
    if !visible(&owner, &who.viewer()) {
        return not_found();
    }
    if r.principal != who.username && r.principal != who.sub {
        // An administrator opening somebody else's statement leaves a trace.
        tracing::info!(
            target: "audit",
            actor = %who.username,
            sub = %who.sub,
            action = "query_log.read_other",
            query_id = %r.query_id,
            owner = %r.principal,
            "administrator opened another user's query"
        );
    }
    let user_name = if people::is_uuid(&r.principal) {
        people::names(pool, std::slice::from_ref(&r.principal))
            .await
            .get(&r.principal)
            .cloned()
    } else {
        None
    };
    let error = r.error_code.map(|code| QueryFailure {
        code,
        message: r.error_message,
    });
    Json(QueryLogDetail {
        id: r.query_id,
        state: r.state,
        user: r.principal,
        user_name,
        source: r.source,
        client_tags: r.client_tags,
        warehouse: r.warehouse,
        catalog: r.catalog,
        schema: r.schema_name,
        query_type: r.query_type,
        sql: r.sql_text,
        sql_hash: r.sql_hash,
        created_at: r.created_ms,
        started_at: r.started_ms,
        ended_at: r.ended_ms,
        timing: QueryTiming {
            queued_ms: r.queued_ms,
            planning_ms: r.planning_ms,
            execution_ms: r.execution_ms,
            wall_ms: r.wall_ms,
            cpu_ms: r.cpu_ms,
        },
        data: QueryData {
            input_rows: r.input_rows,
            input_bytes: r.input_bytes,
            output_rows: r.output_rows,
            output_bytes: r.output_bytes,
            written_rows: r.written_rows,
            written_bytes: r.written_bytes,
            peak_memory_bytes: r.peak_memory_bytes,
        },
        tables_read: r.tables_read,
        tables_written: r.tables_written,
        error,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The UI and the SDKs read these names; typing the responses must not rename one.
    #[test]
    fn the_detail_keeps_its_wire_names() {
        let detail = QueryLogDetail {
            id: "q1".into(),
            state: "FAILED".into(),
            user: "u".into(),
            user_name: None,
            source: None,
            client_tags: vec![],
            warehouse: "analytics".into(),
            catalog: None,
            schema: None,
            query_type: None,
            sql: "select 1".into(),
            sql_hash: "h".into(),
            created_at: 1,
            started_at: None,
            ended_at: None,
            timing: QueryTiming {
                queued_ms: None,
                planning_ms: None,
                execution_ms: None,
                wall_ms: Some(5),
                cpu_ms: None,
            },
            data: QueryData {
                input_rows: None,
                input_bytes: None,
                output_rows: None,
                output_bytes: None,
                written_rows: None,
                written_bytes: None,
                peak_memory_bytes: Some(7),
            },
            tables_read: vec![],
            tables_written: vec![],
            error: Some(QueryFailure {
                code: "X".into(),
                message: None,
            }),
        };
        let v = serde_json::to_value(&detail).unwrap();
        for key in [
            "id",
            "state",
            "user",
            "userName",
            "source",
            "clientTags",
            "warehouse",
            "catalog",
            "schema",
            "queryType",
            "sql",
            "sqlHash",
            "createdAt",
            "startedAt",
            "endedAt",
            "timing",
            "data",
            "tablesRead",
            "tablesWritten",
            "error",
        ] {
            assert!(v.get(key).is_some(), "the detail lost {key}");
        }
        assert_eq!(v["timing"]["wallMs"], 5);
        assert_eq!(v["data"]["peakMemoryBytes"], 7);
        assert_eq!(v["error"]["code"], "X");
        let page = serde_json::to_value(QueryLogPage {
            items: vec![],
            next_cursor: None,
        })
        .unwrap();
        assert_eq!(page, serde_json::json!({ "items": [], "nextCursor": null }));
    }

    #[test]
    fn the_defaults_are_my_queries_fifty_at_a_time() {
        let p = parse_params(&raw(&[])).unwrap();
        assert_eq!((p.scope, p.limit), (Scope::Mine, 50));
        assert!(p.state.is_none() && p.cursor.is_none() && p.search.is_none());
    }

    #[test]
    fn every_parameter_is_checked_with_a_sentence() {
        let bad = |pairs: &[(&str, &str)], needle: &str| {
            let message = parse_params(&raw(pairs)).unwrap_err();
            assert!(
                message.contains(needle),
                "{message} should contain {needle}"
            );
        };
        bad(&[("scope", "everyone")], "scope");
        bad(&[("state", "RUNNING")], "state");
        bad(&[("limit", "0")], "limit");
        bad(&[("limit", "201")], "limit");
        bad(&[("limit", "many")], "limit");
        bad(&[("from", "yesterday")], "from");
        bad(&[("cursor", "###")], "cursor");
        bad(&[("q", &"x".repeat(201))], "q must");
    }

    #[test]
    fn state_is_case_insensitive_and_filters_parse() {
        let p = parse_params(&raw(&[
            ("scope", "all"),
            ("state", "failed"),
            ("warehouse", "analytics"),
            ("q", " nation "),
            ("from", "1000"),
            ("to", "2000"),
            ("limit", "7"),
        ]))
        .unwrap();
        assert_eq!(p.scope, Scope::All);
        assert_eq!(p.state.as_deref(), Some("FAILED"));
        assert_eq!(p.warehouse.as_deref(), Some("analytics"));
        assert_eq!(p.search.as_deref(), Some("nation"));
        assert_eq!((p.from_ms, p.to_ms, p.limit), (Some(1000), Some(2000), 7));
    }

    #[test]
    fn a_cursor_round_trips_and_garbage_does_not_decode() {
        let c = encode_cursor(1_760_873_605_058_999, "20261019_113323_00000_tnvu4");
        assert_eq!(
            decode_cursor(&c),
            Some((
                1_760_873_605_058_999,
                "20261019_113323_00000_tnvu4".to_string()
            ))
        );
        assert_eq!(decode_cursor("not base64 !"), None);
        assert_eq!(decode_cursor(&URL_SAFE_NO_PAD.encode("12:")), None);
        assert_eq!(decode_cursor(&URL_SAFE_NO_PAD.encode("x:y")), None);
    }

    #[test]
    fn wildcards_in_a_search_text_are_literals() {
        assert_eq!(like_pattern("50%_off\\"), "%50\\%\\_off\\\\%");
        assert_eq!(like_pattern("plain"), "%plain%");
    }
}
