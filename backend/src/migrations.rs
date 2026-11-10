//! Bundle migrations (stardelt-nova#617 decision 3): what a bundle's
//! `sql/migrations/*.sql` would do to one target, what already ran there, and the
//! apply route that runs the new files and records them.
//!
//! The ledger is `stardelt_meta.migrations` in the target catalog:
//! `file`, `sha256`, `target`, `applied_at`, `applied_by`. The plan reads it; the
//! apply route creates it on first use and appends one row per file it ran.

use std::sync::Arc;

use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::AppState;
use crate::api_json::ApiJson;
use crate::pipelines::Caller;

/// One file, as the bundle has it. The client hashes the bytes it deploys.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationFile {
    /// `NNN_<words>.sql`: three digits, an underscore, lower-case words, `.sql`.
    pub name: String,
    /// The file's sha256, 64 lower-case hex digits.
    pub sha256: String,
    pub sql: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    /// The catalog the sink tables live in; the ledger is `stardelt_meta` in it.
    pub catalog: String,
    /// The bundle target whose ledger rows count (`dev`, `prod`, …).
    pub target: String,
    /// Every migration file of the bundle, in any order; the answer sorts them by name.
    pub files: Vec<MigrationFile>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFile {
    pub name: String,
    pub sha256: String,
    /// `additive` when every statement only adds (see `classify`), else `breaking`.
    pub class: String,
    /// `new` (not in the ledger), `applied` (same sha) or `changed` (applied with another sha).
    pub status: String,
    /// How many statements the file holds, as the splitter counts them.
    pub statements: usize,
    /// The first statement that is not additive, cut to 120 characters. Absent for `additive`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanAnswer {
    /// False when `errors` is not empty: the bundle must not be applied as it is.
    pub ok: bool,
    /// Whether `stardelt_meta.migrations` exists in the catalog yet (false on a fresh catalog).
    pub ledger: bool,
    /// The identity of this plan: what `bundle approve` takes and what apply needs.
    pub plan_id: String,
    /// Whether this Nova can record an approval (a state store is configured).
    pub approvable: bool,
    pub files: Vec<PlannedFile>,
    /// One sentence per problem: a changed file, a file added before an applied one.
    pub errors: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRequest {
    /// The catalog the migration tables live in; the ledger is `stardelt_meta` in it.
    pub catalog: String,
    /// The bundle target whose ledger rows count (`dev`, `prod`, …).
    pub target: String,
    /// Every migration file of the bundle; the answer runs the `new` ones in name order.
    pub files: Vec<MigrationFile>,
    /// Run statements that are not additive. Default false.
    #[serde(default)]
    pub allow_breaking: bool,
    /// The bundle target's `mode` (`targets.<target>.mode` in bundle.yaml):
    /// `development` or `production`. Client-declared, like `allowBreaking`.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// The `planId` a person other than the caller approved, for a breaking file in a
    /// production target.
    #[serde(default)]
    pub approval_id: Option<String>,
}

fn default_mode() -> String {
    "development".into()
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyAnswer {
    /// False when `errors` is not empty: nothing after the failing file was applied.
    pub ok: bool,
    /// Files this call ran and recorded, in name order.
    pub applied: Vec<String>,
    /// Files the ledger already had with the same sha256.
    pub skipped: Vec<String>,
    /// One sentence per problem.
    pub errors: Vec<String>,
}

/// The file name the design accepts (`nova#617` decision 3).
fn is_file_name(value: &str) -> bool {
    let Some(rest) = value.strip_suffix(".sql") else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() >= 5
        && bytes[..3].iter().all(|b| b.is_ascii_digit())
        && bytes[3] == b'_'
        && bytes[4..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// A target: the same shape as a bundle target in `Provenance`.
fn is_target(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lower-case hex, no dependency on a hex crate.
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A plan's identity: exactly the work a second person approves. The same `catalog`,
/// `target` and set of files that would run always give the same id, on any replica.
fn plan_id(catalog: &str, target: &str, planned: &[PlannedFile]) -> String {
    let mut lines = vec![format!("catalog={catalog}"), format!("target={target}")];
    lines.extend(
        planned
            .iter()
            .filter(|f| f.status == "new")
            .map(|f| format!("{}\t{}\t{}", f.name, f.sha256, f.class)),
    );
    hex_lower(&Sha256::digest(lines.join("\n").as_bytes()))
}

/// Where the scanner is: plain SQL, inside a `'…'` string, a `--` line comment or a
/// `/* … */` block comment.
#[derive(PartialEq)]
enum Scan {
    Plain,
    Single,
    Line,
    Block,
}

/// Split a file into statements on `;`, ignoring semicolons inside strings and comments.
/// Comments are dropped, each piece is trimmed, empty pieces are skipped.
fn statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut scan = Scan::Plain;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match scan {
            Scan::Plain => match c {
                '\'' => {
                    current.push(c);
                    scan = Scan::Single;
                }
                '-' if chars.peek() == Some(&'-') => {
                    chars.next();
                    scan = Scan::Line;
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    scan = Scan::Block;
                }
                ';' => {
                    let piece = current.trim();
                    if !piece.is_empty() {
                        out.push(piece.to_string());
                    }
                    current.clear();
                }
                _ => current.push(c),
            },
            Scan::Single => {
                current.push(c);
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        // `''` is an escaped quote inside the string.
                        if let Some(quote) = chars.next() {
                            current.push(quote);
                        }
                    } else {
                        scan = Scan::Plain;
                    }
                }
            }
            Scan::Line => {
                if c == '\n' {
                    current.push(' ');
                    scan = Scan::Plain;
                }
            }
            Scan::Block => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    current.push(' ');
                    scan = Scan::Plain;
                }
            }
        }
    }
    let piece = current.trim();
    if !piece.is_empty() {
        out.push(piece.to_string());
    }
    out
}

/// Whether one statement only adds: creates a table or a view, or adds columns.
/// Anything unrecognised is breaking (fail safe).
fn classify(statement: &str) -> bool {
    let upper = statement.to_uppercase();
    let collapsed = upper.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = format!(" {collapsed} ");
    text.starts_with(" CREATE TABLE ")
        || text.starts_with(" CREATE TABLE AS ")
        || text.starts_with(" CREATE VIEW ")
        || text.starts_with(" CREATE OR REPLACE VIEW ")
        || (text.starts_with(" ALTER TABLE ")
            && (text.contains(" ADD COLUMN ") || text.contains(" ADD COLUMNS ")))
}

/// The plan for a set of files against the ledger rows of one target
/// (`(file, sha256)`, any order): the answer's files in name order and the problems.
fn plan(files: &[MigrationFile], applied: &[(String, String)]) -> (Vec<PlannedFile>, Vec<String>) {
    let mut ordered: Vec<&MigrationFile> = files.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));

    let mut errors = Vec::new();
    let mut planned = Vec::with_capacity(ordered.len());
    let mut fresh = Vec::new();

    for file in &ordered {
        let statements = statements(&file.sql);
        let breaking = statements.iter().find(|s| !classify(s));
        let class = if breaking.is_none() {
            "additive"
        } else {
            "breaking"
        };
        let reason = breaking.map(|s| s.chars().take(120).collect::<String>());

        let status = match applied.iter().find(|(name, _)| name == &file.name) {
            Some((_, sha)) if sha == &file.sha256 => "applied",
            Some((_, sha)) => {
                let first: String = sha.chars().take(12).collect();
                errors.push(format!(
                    "{} was applied as {first}… and its content changed: a migration is never re-run — add a new file",
                    file.name
                ));
                "changed"
            }
            None => {
                fresh.push(file.name.clone());
                "new"
            }
        };

        planned.push(PlannedFile {
            name: file.name.clone(),
            sha256: file.sha256.clone(),
            class: class.to_string(),
            status: status.to_string(),
            statements: statements.len(),
            reason,
        });
    }

    if let Some(last_applied) = applied.iter().map(|(name, _)| name).max() {
        for name in &fresh {
            if name < last_applied {
                errors.push(format!(
                    "{name} is new but {last_applied} was already applied: migrations run in name order"
                ));
            }
        }
    }

    (planned, errors)
}

/// One sentence per refusal of a request, in a fixed order.
fn validate(request: &PlanRequest) -> Result<(), String> {
    if !crate::streams::is_catalog(&request.catalog) {
        return Err("catalog is 1 to 63 lower-case letters, digits and dashes".to_string());
    }
    if !is_target(&request.target) {
        return Err("target is 1 to 32 lower-case letters, digits and dashes".to_string());
    }
    if request.files.is_empty() || request.files.len() > 200 {
        return Err("send 1 to 200 migration files".to_string());
    }
    let mut total = 0usize;
    for file in &request.files {
        if !is_file_name(&file.name) {
            return Err(format!(
                "{} is not a migration file name: NNN_<words>.sql, three digits, an underscore, lower-case words",
                file.name
            ));
        }
        if !is_sha256(&file.sha256) {
            return Err(format!(
                "{} needs a sha256 of 64 lower-case hex digits",
                file.name
            ));
        }
        if file.sql.is_empty() {
            return Err(format!("{} is empty", file.name));
        }
        if file.sql.len() > 512 * 1024 {
            return Err(format!("{} is larger than 512 KiB", file.name));
        }
        total += file.sql.len();
    }
    if total > 4 * 1024 * 1024 {
        return Err("the migration files are larger than 4 MiB".to_string());
    }
    let mut names: Vec<&str> = request.files.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(format!("two migration files are named {}", pair[0]));
    }
    Ok(())
}

/// The only two target modes an apply may declare.
fn validate_mode(mode: &str) -> Result<(), String> {
    if mode == "development" || mode == "production" {
        Ok(())
    } else {
        Err("mode must be 'development' or 'production'".to_string())
    }
}

/// The ledger's DDL, in this order: the schema, then the table. Runs as the caller.
fn ledger_ddl(catalog: &str) -> [String; 2] {
    [
        format!("CREATE SCHEMA IF NOT EXISTS \"{catalog}\".stardelt_meta"),
        format!(
            "CREATE TABLE IF NOT EXISTS \"{catalog}\".stardelt_meta.migrations (\
             file varchar, sha256 varchar, target varchar, \
             applied_at timestamp(6) with time zone, applied_by varchar)"
        ),
    ]
}

/// The row for one applied file. `NOT EXISTS` keeps a retried call from writing it twice.
fn ledger_row(catalog: &str, file: &str, sha256: &str, target: &str, actor: &str) -> String {
    let actor = actor.replace('\'', "''");
    format!(
        "INSERT INTO \"{catalog}\".stardelt_meta.migrations (file, sha256, target, applied_at, applied_by) \
         SELECT '{file}', '{sha256}', '{target}', current_timestamp, '{actor}' \
         WHERE NOT EXISTS (SELECT 1 FROM \"{catalog}\".stardelt_meta.migrations \
         WHERE file = '{file}' AND target = '{target}')"
    )
}

/// The sentence that refuses this call, or `None` when it may run.
fn refusal(files: &[PlannedFile], allow_breaking: bool) -> Option<String> {
    if allow_breaking {
        return None;
    }
    files
        .iter()
        .find(|f| f.status == "new" && f.class == "breaking")
        .map(|file| {
            format!(
                "{} has statements that are not additive: rerun with allowBreaking (the CLI's --allow-breaking)",
                file.name
            )
        })
}

/// A breaking change in a production target needs a second person (nova#617 decision 3).
/// `approver` is the sub of someone who approved `plan`, if anybody did.
fn approval_refusal(
    planned: &[PlannedFile],
    mode: &str,
    approval_id: Option<&str>,
    plan: &str,
    approver: Option<&str>,
    caller_sub: &str,
) -> Option<String> {
    if mode != "production"
        || !planned
            .iter()
            .any(|f| f.status == "new" && f.class == "breaking")
    {
        return None;
    }
    let ask = format!("stardelt bundle approve {plan}");
    match approval_id {
        None => {
            return Some(format!(
                "a breaking change in a production target needs a second person: {ask}"
            ));
        }
        Some(id) if id != plan => {
            return Some(format!(
                "the approval is for another plan, or the files changed since it was approved: {ask}"
            ));
        }
        Some(_) => {}
    }
    match approver {
        Some(who) if who != caller_sub => None,
        Some(_) => Some(format!(
            "the plan's author cannot approve their own plan: ask a second person to run `{ask}`"
        )),
        None => Some(format!("no one has approved this plan yet: {ask}")),
    }
}

/// Whether `stardelt_meta.migrations` exists in the catalog. Runs as the caller.
async fn ledger_exists(
    state: &AppState,
    caller: Option<&crate::auth::Principal>,
    catalog: &str,
) -> Result<bool, String> {
    let sql = format!(
        "SELECT table_name FROM \"{catalog}\".information_schema.tables \
         WHERE table_schema = 'stardelt_meta' AND table_name = 'migrations'"
    );
    let answer = crate::trino::execute(state, sql, caller).await?;
    Ok(!answer.rows.is_empty())
}

/// The ledger rows for one target: `(file, sha256)`, sorted by file.
async fn applied(
    state: &AppState,
    caller: Option<&crate::auth::Principal>,
    catalog: &str,
    target: &str,
) -> Result<Vec<(String, String)>, String> {
    let sql = format!(
        "SELECT file, sha256 FROM \"{catalog}\".stardelt_meta.migrations \
         WHERE target = '{target}' ORDER BY file"
    );
    let answer = crate::trino::execute(state, sql, caller).await?;
    Ok(answer
        .rows
        .iter()
        .filter_map(|row| {
            Some((
                row.first()?.as_str()?.to_string(),
                row.get(1)?.as_str()?.to_string(),
            ))
        })
        .collect())
}

#[utoipa::path(
    post,
    operation_id = "migrations_plan",
    path = "/api/migrations/plan",
    tag = "public",
    description = "What a bundle's migration files would do to one target, and what already ran there (stardelt-nova#617 decision 3). Writes nothing. The body is `catalog`, `target` and `files` (`name` `NNN_<words>.sql`, `sha256`, `sql`); `class` is `additive` when every statement only creates or adds columns or views, else `breaking` with the first non-additive statement in `reason`; `status` is `new`, `applied` (same sha256 in `stardelt_meta.migrations`) or `changed` (applied with another sha256). `ok` is false, with one sentence per problem in `errors`, when a file changed after it was applied or a new file sorts before an applied one. `ledger` is false on a catalog whose `stardelt_meta.migrations` does not exist yet. `planId` names exactly the files that would run, and is what `approve` takes; `approvable` is false when no state store is configured. The ledger is read as the caller, so the caller's own catalog privileges decide what can be read. Answers `Cache-Control: no-store`.",
    request_body = PlanRequest,
    responses(
        (status = 200, description = "The plan: per file, class and status, plus the problems", body = PlanAnswer),
        (status = 400, description = "A malformed request, one sentence", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The query engine is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The demo has no catalog to plan migrations against", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn plan_handler(
    caller: Caller,
    ApiJson(request): ApiJson<PlanRequest>,
) -> Response {
    let state = caller.state();
    if state.demo {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"error": "the demo has no catalog to plan migrations against"}),
            ),
        )
            .into_response();
    }
    if let Err(message) = validate(&request) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": message})),
        )
            .into_response();
    }
    let who = caller.principal();
    let ledger = match ledger_exists(state, Some(who), &request.catalog).await {
        Ok(v) => v,
        Err(e) => {
            return (
                crate::trino::error_status(&e),
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };
    let rows = if ledger {
        match applied(state, Some(who), &request.catalog, &request.target).await {
            Ok(v) => v,
            Err(e) => {
                return (
                    crate::trino::error_status(&e),
                    Json(serde_json::json!({"error": e})),
                )
                    .into_response();
            }
        }
    } else {
        Vec::new()
    };
    let (files, errors) = plan(&request.files, &rows);
    let id = plan_id(&request.catalog, &request.target, &files);
    let mut approvable = state.db.is_some();
    if let Some(pool) = state.db.as_ref() {
        let who = crate::saved_queries::owner_sub(Some(who), &state.dev_user);
        if let Err(e) = sqlx::query(
            "INSERT INTO migration_plan (plan_id, catalog, target, created_by) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (plan_id) DO NOTHING",
        )
        .bind(&id)
        .bind(&request.catalog)
        .bind(&request.target)
        .bind(&who)
        .execute(pool)
        .await
        {
            tracing::warn!(error = %e, "recording a migration plan failed");
            approvable = false;
        }
    }
    Json(PlanAnswer {
        ok: errors.is_empty(),
        ledger,
        plan_id: id,
        approvable,
        files,
        errors,
    })
    .into_response()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApproveRequest {
    /// The `planId` a plan answered with.
    pub plan_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApproveAnswer {
    pub plan_id: String,
    /// The signed-in name of the person who approved the plan.
    pub approver: String,
    /// Epoch milliseconds, as everywhere else.
    pub approved_at: i64,
}

/// `POST /api/migrations/approve` — a second person says yes to a plan.
#[utoipa::path(
    post,
    operation_id = "migrations_approve",
    path = "/api/migrations/approve",
    tag = "public",
    description = "Record that the caller has looked at a migration plan, so a breaking change may run in a production target (stardelt-nova#617 decision 3). The body is the `planId` a plan answered with. The plan's own author cannot approve it: a second person has to. Repeating a call is a no-op that answers the same row with the first approval time. Plans and approvals live in the state store.",
    request_body = ApproveRequest,
    responses(
        (status = 200, description = "The recorded approval: who approved and when", body = ApproveAnswer),
        (status = 400, description = "A malformed request, one sentence", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such plan: it was never planned on this Nova, or the state store was reset", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The caller is the plan's author: a second person has to approve it", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The state store is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn approve_handler(
    caller: Caller,
    ApiJson(request): ApiJson<ApproveRequest>,
) -> Response {
    let state = caller.state();
    let Some(pool) = state.db.as_ref() else {
        return crate::saved_queries::unavailable();
    };
    let who = caller.principal();
    let caller_sub = crate::saved_queries::owner_sub(Some(who), &state.dev_user);
    let created_by: Option<String> =
        match sqlx::query_scalar("SELECT created_by FROM migration_plan WHERE plan_id = $1")
            .bind(&request.plan_id)
            .fetch_optional(pool)
            .await
        {
            Ok(v) => v,
            Err(e) => return crate::saved_queries::store_failed(e),
        };
    let Some(created_by) = created_by else {
        return crate::saved_queries::error(
            StatusCode::NOT_FOUND,
            "unknown plan: it was never planned on this Nova, or the state store was reset",
        );
    };
    if created_by == caller_sub {
        return crate::saved_queries::error(
            StatusCode::CONFLICT,
            &format!(
                "the plan's author cannot approve their own plan: ask a second person to run `stardelt bundle approve {}`",
                request.plan_id
            ),
        );
    }
    let stored = sqlx::query_as::<_, (String, i64)>(
        "INSERT INTO migration_approval (plan_id, approver_sub, approver_name) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (plan_id, approver_sub) DO UPDATE SET approved_at = now() \
         RETURNING approver_name, (extract(epoch FROM approved_at) * 1000)::bigint AS approved_at",
    )
    .bind(&request.plan_id)
    .bind(&caller_sub)
    .bind(&who.identity.display_name)
    .fetch_one(pool)
    .await;
    match stored {
        Ok((approver, approved_at)) => Json(ApproveAnswer {
            plan_id: request.plan_id,
            approver,
            approved_at,
        })
        .into_response(),
        Err(e) => crate::saved_queries::store_failed(e),
    }
}

#[utoipa::path(
    post,
    operation_id = "migrations_apply",
    path = "/api/migrations/apply",
    tag = "public",
    description = "Run a bundle's new migration files against one target, once each, and record them in stardelt_meta.migrations in that catalog (stardelt-nova#617 decision 3). Files already applied with the same sha256 are skipped; a file whose content changed after it was applied, or a new file that sorts before an applied one, is refused (409). Statements that are not additive need allowBreaking. The statements and the ledger writes run as the caller, so the caller's catalog privileges apply.",
    request_body = ApplyRequest,
    responses(
        (status = 200, description = "What ran and what the ledger already had", body = ApplyAnswer),
        (status = 400, description = "A malformed request, or a statement the engine refused: one sentence", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 409, description = "Refused before anything ran: a changed or reordered file (ApplyAnswer with the errors), or a breaking file without allowBreaking (one sentence)", body = ApplyAnswer),
        (status = 502, description = "The query engine is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "The demo has no catalog to run migrations against", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn apply_handler(
    caller: Caller,
    ApiJson(request): ApiJson<ApplyRequest>,
) -> Response {
    let state = caller.state();
    if state.demo {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"error": "the demo has no catalog to plan migrations against"}),
            ),
        )
            .into_response();
    }
    let allow_breaking = request.allow_breaking;
    let mode = request.mode.clone();
    let approval_id = request.approval_id.clone();
    let check = PlanRequest {
        catalog: request.catalog,
        target: request.target,
        files: request.files,
    };
    if let Err(message) = validate(&check) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": message})),
        )
            .into_response();
    }
    if let Err(message) = validate_mode(&mode) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": message})),
        )
            .into_response();
    }
    let PlanRequest {
        catalog,
        target,
        files,
    } = check;
    let who = caller.principal();
    let ledger = match ledger_exists(state, Some(who), &catalog).await {
        Ok(v) => v,
        Err(e) => {
            return (
                crate::trino::error_status(&e),
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };
    let rows = if ledger {
        match applied(state, Some(who), &catalog, &target).await {
            Ok(v) => v,
            Err(e) => {
                return (
                    crate::trino::error_status(&e),
                    Json(serde_json::json!({"error": e})),
                )
                    .into_response();
            }
        }
    } else {
        Vec::new()
    };
    let (planned, errors) = plan(&files, &rows);
    if !errors.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(ApplyAnswer {
                ok: false,
                applied: vec![],
                skipped: vec![],
                errors,
            }),
        )
            .into_response();
    }
    if let Some(sentence) = refusal(&planned, allow_breaking) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": sentence})),
        )
            .into_response();
    }
    let id = plan_id(&catalog, &target, &planned);
    let caller_sub = crate::saved_queries::owner_sub(Some(who), &state.dev_user);
    let needed = mode == "production"
        && planned
            .iter()
            .any(|f| f.status == "new" && f.class == "breaking");
    let approver: Option<String> = if needed {
        let Some(pool) = state.db.as_ref() else {
            return crate::saved_queries::unavailable();
        };
        match sqlx::query_scalar(
            "SELECT approver_sub FROM migration_approval \
             WHERE plan_id = $1 AND approver_sub <> $2 LIMIT 1",
        )
        .bind(&id)
        .bind(&caller_sub)
        .fetch_optional(pool)
        .await
        {
            Ok(v) => v,
            Err(e) => return crate::saved_queries::store_failed(e),
        }
    } else {
        None
    };
    if let Some(sentence) = approval_refusal(
        &planned,
        &mode,
        approval_id.as_deref(),
        &id,
        approver.as_deref(),
        &caller_sub,
    ) {
        return (
            StatusCode::CONFLICT,
            Json(ApplyAnswer {
                ok: false,
                applied: vec![],
                skipped: vec![],
                errors: vec![sentence],
            }),
        )
            .into_response();
    }
    for sql in ledger_ddl(&catalog) {
        if let Err(e) = crate::trino::execute(state, sql, Some(who)).await {
            return (
                crate::trino::error_status(&e),
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    }
    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    for planned_file in &planned {
        if planned_file.status == "applied" {
            skipped.push(planned_file.name.clone());
            continue;
        }
        let Some(file) = files.iter().find(|f| f.name == planned_file.name) else {
            continue;
        };
        for (index, statement) in statements(&file.sql).iter().enumerate() {
            if let Err(e) = crate::trino::execute(state, statement.clone(), Some(who)).await {
                return (
                    crate::trino::error_status(&e),
                    Json(serde_json::json!({"error": format!(
                        "{}: statement {} failed: {e}",
                        planned_file.name,
                        index + 1
                    )})),
                )
                    .into_response();
            }
        }
        let row = ledger_row(
            &catalog,
            &planned_file.name,
            &planned_file.sha256,
            &target,
            &who.identity.display_name,
        );
        if let Err(e) = crate::trino::execute(state, row, Some(who)).await {
            return (
                crate::trino::error_status(&e),
                Json(serde_json::json!({"error": format!(
                    "{} ran but was not recorded: {e}",
                    planned_file.name
                )})),
            )
                .into_response();
        }
        ran.push(planned_file.name.clone());
    }
    Json(ApplyAnswer {
        ok: true,
        applied: ran,
        skipped,
        errors: vec![],
    })
    .into_response()
}

pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/migrations/apply", post(apply_handler))
        .route("/migrations/approve", post(approve_handler))
        .route("/migrations/plan", post(plan_handler))
        .route_layer(axum::middleware::from_fn(crate::pipelines::no_store))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, sha: &str, sql: &str) -> MigrationFile {
        MigrationFile {
            name: name.to_string(),
            sha256: sha.to_string(),
            sql: sql.to_string(),
        }
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(64)
    }

    #[test]
    fn statements_skip_strings_and_comments() {
        let split = statements(
            "select ';';\n-- a comment\ncreate table \"x\".t (a int); /* block; */ insert into t values (1);",
        );
        assert_eq!(split.len(), 3, "{split:?}");
        assert_eq!(split[0], "select ';'");
        assert_eq!(split[1], "create table \"x\".t (a int)");
        assert_eq!(split[2], "insert into t values (1)");
        for piece in &split {
            assert!(!piece.contains("a comment"), "{piece}");
            assert!(!piece.contains("block"), "{piece}");
        }
    }

    #[test]
    fn statements_keep_escaped_quotes_inside_a_string() {
        let split = statements("insert into t values ('it''s; fine');");
        assert_eq!(split.len(), 1, "{split:?}");
        assert_eq!(split[0], "insert into t values ('it''s; fine')");
    }

    #[test]
    fn classify_is_fail_safe() {
        for additive in [
            "CREATE TABLE t (a int)",
            "ALTER TABLE t ADD COLUMN b int",
            "CREATE OR REPLACE VIEW v AS SELECT 1",
        ] {
            assert!(classify(additive), "{additive}");
        }
        for breaking in [
            "DROP TABLE t",
            "ALTER TABLE t DROP COLUMN b",
            "DELETE FROM t",
            "garbage",
        ] {
            assert!(!classify(breaking), "{breaking}");
        }
    }

    #[test]
    fn plan_reports_applied_and_changed() {
        let applied = vec![("001_init.sql".to_string(), sha('a'))];
        let (planned, errors) = plan(
            &[file("001_init.sql", &sha('a'), "create table t (a int);")],
            &applied,
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].status, "applied");
        assert_eq!(planned[0].class, "additive");
        assert_eq!(planned[0].statements, 1);
        assert!(planned[0].reason.is_none());
        assert!(errors.is_empty(), "{errors:?}");

        let (planned, errors) = plan(
            &[file("001_init.sql", &sha('b'), "create table t (a int);")],
            &applied,
        );
        assert_eq!(planned[0].status, "changed");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("001_init.sql was applied as aaaaaaaaaaaa…"),
            "{errors:?}"
        );
    }

    #[test]
    fn plan_reports_a_new_file_before_an_applied_one() {
        let applied = vec![("002_more.sql".to_string(), sha('a'))];
        let (planned, errors) = plan(
            &[
                file("001_init.sql", &sha('b'), "drop table t;"),
                file("003_last.sql", &sha('c'), "create view v as select 1;"),
            ],
            &applied,
        );
        assert_eq!(planned[0].name, "001_init.sql");
        assert_eq!(planned[0].status, "new");
        assert_eq!(planned[0].class, "breaking");
        assert_eq!(planned[0].reason.as_deref(), Some("drop table t"));
        assert_eq!(planned[1].status, "new");
        assert_eq!(planned[1].class, "additive");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            errors[0],
            "001_init.sql is new but 002_more.sql was already applied: migrations run in name order"
        );
    }

    #[test]
    fn plan_cuts_the_reason_at_120_characters() {
        let long = format!("delete from t where {}", "x".repeat(200));
        let (planned, _) = plan(&[file("001_long.sql", &sha('a'), &format!("{long};"))], &[]);
        assert_eq!(
            planned[0].reason.as_ref().map(|r| r.chars().count()),
            Some(120)
        );
    }

    #[test]
    fn validate_refuses_malformed_requests() {
        let good = || file("001_init.sql", &sha('a'), "create table t (a int);");
        let request = |catalog: &str, target: &str, files: Vec<MigrationFile>| PlanRequest {
            catalog: catalog.to_string(),
            target: target.to_string(),
            files,
        };

        assert_eq!(
            validate(&request("Bad Catalog", "prod", vec![good()])).unwrap_err(),
            "catalog is 1 to 63 lower-case letters, digits and dashes"
        );
        assert_eq!(
            validate(&request("mobilitylab", "-prod", vec![good()])).unwrap_err(),
            "target is 1 to 32 lower-case letters, digits and dashes"
        );
        assert_eq!(
            validate(&request("mobilitylab", "prod", Vec::new())).unwrap_err(),
            "send 1 to 200 migration files"
        );
        assert_eq!(
            validate(&request(
                "mobilitylab",
                "prod",
                vec![
                    good(),
                    file("001_init.sql", &sha('b'), "create view v as select 1;")
                ]
            ))
            .unwrap_err(),
            "two migration files are named 001_init.sql"
        );
        let short = file(
            "001_init.sql",
            "a".repeat(63).as_str(),
            "create table t (a int);",
        );
        assert!(
            validate(&request("mobilitylab", "prod", vec![short]))
                .unwrap_err()
                .contains("sha256")
        );
        assert_eq!(
            validate(&request(
                "mobilitylab",
                "prod",
                vec![file("init.sql", &sha('a'), "select 1;")]
            ))
            .unwrap_err(),
            "init.sql is not a migration file name: NNN_<words>.sql, three digits, an underscore, lower-case words"
        );
        assert_eq!(
            validate(&request(
                "mobilitylab",
                "prod",
                vec![file("001_a.sql", &sha('a'), "")]
            ))
            .unwrap_err(),
            "001_a.sql is empty"
        );
        assert!(validate(&request("mobilitylab", "prod", vec![good()])).is_ok());
        assert_eq!(
            validate_mode("prod").unwrap_err(),
            "mode must be 'development' or 'production'"
        );
        assert!(validate_mode("production").is_ok());
        assert!(validate_mode("development").is_ok());
    }

    #[test]
    fn ledger_ddl_names_the_catalog() {
        let ddl = ledger_ddl("mobilitylab");
        assert_eq!(
            ddl[0],
            "CREATE SCHEMA IF NOT EXISTS \"mobilitylab\".stardelt_meta"
        );
        assert!(
            ddl[1].starts_with(
                "CREATE TABLE IF NOT EXISTS \"mobilitylab\".stardelt_meta.migrations ("
            )
        );
        assert!(ddl[1].contains("applied_at timestamp(6) with time zone"));
    }

    #[test]
    fn ledger_row_is_idempotent_and_escapes_the_actor() {
        let row = ledger_row("c", "001_init.sql", &sha('a'), "prod", "O'Brien");
        assert!(row.contains("SELECT '001_init.sql', '"));
        assert!(row.contains(", 'prod', current_timestamp, 'O''Brien'"));
        assert!(row.contains("WHERE NOT EXISTS (SELECT 1 FROM \"c\".stardelt_meta.migrations"));
        assert!(row.ends_with("WHERE file = '001_init.sql' AND target = 'prod')"));
    }

    #[test]
    fn refusal_allows_additive_and_needs_the_flag_for_breaking() {
        let planned = |class: &str, status: &str| PlannedFile {
            name: "001_a.sql".to_string(),
            sha256: sha('a'),
            class: class.to_string(),
            status: status.to_string(),
            statements: 1,
            reason: None,
        };
        let additive = planned("additive", "new");
        let breaking = planned("breaking", "new");
        let already_applied_breaking = planned("breaking", "applied");

        assert!(refusal(std::slice::from_ref(&additive), false).is_none());
        assert!(refusal(std::slice::from_ref(&additive), true).is_none());
        let sentence = refusal(std::slice::from_ref(&breaking), false).expect("a sentence");
        assert!(sentence.starts_with("001_a.sql has statements that are not additive"));
        assert!(refusal(std::slice::from_ref(&breaking), true).is_none());
        assert!(
            refusal(std::slice::from_ref(&already_applied_breaking), false).is_none(),
            "an applied file never runs again"
        );
    }

    #[test]
    fn plan_id_is_stable_and_covers_the_work() {
        fn planned(name: &str, sha: &str, class: &str, status: &str) -> PlannedFile {
            PlannedFile {
                name: name.to_string(),
                sha256: sha.to_string(),
                class: class.to_string(),
                status: status.to_string(),
                statements: 1,
                reason: None,
            }
        }
        let first = planned("001_a.sql", &sha('a'), "additive", "new");
        let second = planned("002_b.sql", &sha('b'), "breaking", "new");
        let files = vec![first, second];

        let id = plan_id("cat", "prod", &files);
        assert_eq!(id.len(), 64, "{id}");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{id}"
        );
        assert_eq!(
            id,
            plan_id("cat", "prod", &files),
            "the same work gives the same id"
        );
        assert_ne!(id, plan_id("cat", "dev", &files), "the target counts");
        assert_ne!(id, plan_id("other", "prod", &files), "the catalog counts");
        assert_ne!(
            id,
            plan_id(
                "cat",
                "prod",
                &[
                    planned("001_a.sql", &sha('a'), "additive", "new"),
                    planned("002_b.sql", &sha('c'), "breaking", "new")
                ]
            ),
            "a changed file counts"
        );
        assert_ne!(
            id,
            plan_id(
                "cat",
                "prod",
                &[
                    planned("001_a.sql", &sha('a'), "additive", "new"),
                    planned("002_b.sql", &sha('b'), "additive", "new")
                ]
            ),
            "a changed class counts"
        );
        assert_eq!(
            id,
            plan_id(
                "cat",
                "prod",
                &[
                    planned("001_a.sql", &sha('a'), "additive", "new"),
                    planned("002_b.sql", &sha('b'), "breaking", "new"),
                    planned("003_c.sql", &sha('c'), "breaking", "applied")
                ]
            ),
            "an applied file never runs, so it changes nothing"
        );

        // The request's order does not count either: `plan()` sorts by name.
        let ordered = plan(
            &[
                file("001_a.sql", &sha('a'), "create table t (a int);"),
                file("002_b.sql", &sha('b'), "drop table t;"),
            ],
            &[],
        );
        let reordered = plan(
            &[
                file("002_b.sql", &sha('b'), "drop table t;"),
                file("001_a.sql", &sha('a'), "create table t (a int);"),
            ],
            &[],
        );
        assert_eq!(
            plan_id("cat", "prod", &ordered.0),
            plan_id("cat", "prod", &reordered.0)
        );
        assert_eq!(plan_id("cat", "prod", &ordered.0), id);
    }

    #[test]
    fn a_breaking_change_in_production_needs_another_person() {
        let planned = |class: &str, status: &str| PlannedFile {
            name: "001_a.sql".to_string(),
            sha256: sha('a'),
            class: class.to_string(),
            status: status.to_string(),
            statements: 1,
            reason: None,
        };
        let additive = planned("additive", "new");
        let breaking = vec![planned("breaking", "new")];

        assert!(
            approval_refusal(&[], "production", Some("x"), "p", Some("bob"), "alice").is_none(),
            "nothing runs, nothing to approve"
        );
        assert!(
            approval_refusal(
                std::slice::from_ref(&additive),
                "production",
                None,
                "p",
                None,
                "alice"
            )
            .is_none(),
            "an additive file needs nobody"
        );
        let sentence = approval_refusal(&breaking, "production", None, "p", None, "alice")
            .expect("a sentence");
        assert!(sentence.contains("stardelt bundle approve"), "{sentence}");
        assert!(
            approval_refusal(&breaking, "development", None, "p", None, "alice").is_none(),
            "a development target needs nobody"
        );
        let other = approval_refusal(
            &breaking,
            "production",
            Some("other"),
            "p",
            Some("bob"),
            "alice",
        )
        .expect("a sentence");
        assert!(other.contains("another plan"), "{other}");
        let own = approval_refusal(
            &breaking,
            "production",
            Some("p"),
            "p",
            Some("alice"),
            "alice",
        )
        .expect("a sentence");
        assert!(own.contains("cannot approve their own plan"), "{own}");
        assert!(
            approval_refusal(
                &breaking,
                "production",
                Some("p"),
                "p",
                Some("bob"),
                "alice"
            )
            .is_none(),
            "a second person approved this exact plan"
        );
    }
}
