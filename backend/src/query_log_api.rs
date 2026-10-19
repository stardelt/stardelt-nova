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

use crate::AppState;
use crate::auth::Principal;
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListItem {
    id: String,
    state: String,
    user: String,
    source: Option<String>,
    warehouse: String,
    query_type: Option<String>,
    sql: String,
    created_at: i64,
    wall_ms: Option<i64>,
    output_rows: Option<i64>,
    error_code: Option<String>,
}

impl From<ListRow> for ListItem {
    fn from(r: ListRow) -> Self {
        Self {
            id: r.query_id,
            state: r.state,
            user: r.principal,
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

/// `GET /api/query-log?scope=mine|team|all&state=&warehouse=&q=&from=&to=&limit=&cursor=`
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
        q.push(" AND principal = ").push_bind(who.username.clone());
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
    let items: Vec<ListItem> = rows.into_iter().map(ListItem::from).collect();
    Json(serde_json::json!({ "items": items, "nextCursor": next })).into_response()
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

/// `GET /api/query-log/{id}` — everything stored about one query the caller may see.
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
    if r.principal != who.username {
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
    Json(serde_json::json!({
        "id": r.query_id,
        "state": r.state,
        "user": r.principal,
        "source": r.source,
        "clientTags": r.client_tags,
        "warehouse": r.warehouse,
        "catalog": r.catalog,
        "schema": r.schema_name,
        "queryType": r.query_type,
        "sql": r.sql_text,
        "sqlHash": r.sql_hash,
        "createdAt": r.created_ms,
        "startedAt": r.started_ms,
        "endedAt": r.ended_ms,
        "timing": {
            "queuedMs": r.queued_ms, "planningMs": r.planning_ms, "executionMs": r.execution_ms,
            "wallMs": r.wall_ms, "cpuMs": r.cpu_ms
        },
        "data": {
            "inputRows": r.input_rows, "inputBytes": r.input_bytes, "outputRows": r.output_rows,
            "outputBytes": r.output_bytes, "writtenRows": r.written_rows,
            "writtenBytes": r.written_bytes, "peakMemoryBytes": r.peak_memory_bytes
        },
        "tablesRead": r.tables_read,
        "tablesWritten": r.tables_written,
        "error": r.error_code.map(|code| serde_json::json!({ "code": code, "message": r.error_message })),
    }))
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
