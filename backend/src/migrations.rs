//! Bundle migrations, plan half (stardelt-nova#617 decision 3): what a bundle's
//! `sql/migrations/*.sql` would do to one target, and what already ran there.
//!
//! The ledger is `stardelt_meta.migrations` in the target catalog:
//! `file`, `sha256`, `target`, `applied_at`, `applied_by`. This module only reads it;
//! the apply endpoint (the next slice) creates it and appends rows.

use std::sync::Arc;

use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
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
    pub files: Vec<PlannedFile>,
    /// One sentence per problem: a changed file, a file added before an applied one.
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
    description = "What a bundle's migration files would do to one target, and what already ran there (stardelt-nova#617 decision 3). Writes nothing. The body is `catalog`, `target` and `files` (`name` `NNN_<words>.sql`, `sha256`, `sql`); `class` is `additive` when every statement only creates or adds columns or views, else `breaking` with the first non-additive statement in `reason`; `status` is `new`, `applied` (same sha256 in `stardelt_meta.migrations`) or `changed` (applied with another sha256). `ok` is false, with one sentence per problem in `errors`, when a file changed after it was applied or a new file sorts before an applied one. `ledger` is false on a catalog whose `stardelt_meta.migrations` does not exist yet. The ledger is read as the caller, so the caller's own catalog privileges decide what can be read. Answers `Cache-Control: no-store`.",
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
    Json(PlanAnswer {
        ok: errors.is_empty(),
        ledger,
        files,
        errors,
    })
    .into_response()
}

pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
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
    }
}
