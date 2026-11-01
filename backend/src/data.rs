//! Load & transfer: CSV/JSON ingestion into an Iceberg table, and table-to-table
//! copies. In demo mode the operations mutate the in-memory lakehouse; otherwise
//! they issue DDL/DML through Trino.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AppState, api_json::ApiJson, demo, jobs, trino};

#[derive(Clone, Deserialize, utoipa::ToSchema)]
pub struct UploadRequest {
    pub namespace: String,
    pub table: String,
    #[serde(default = "default_format")]
    pub format: String,
    pub content: String,
}

fn default_format() -> String {
    "csv".into()
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct UploadResult {
    pub namespace: String,
    pub table: String,
    pub rows_loaded: usize,
    pub job_id: String,
}

#[derive(Clone, Deserialize, utoipa::ToSchema)]
pub struct TransferRequest {
    pub source: String,
    pub target: String,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "overwrite".into()
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TransferResult {
    pub source: String,
    pub target: String,
    pub rows: usize,
    pub job_id: String,
}

#[utoipa::path(
    post,
    operation_id = "ingest_upload",
    path = "/api/ingest/upload",
    tag = "internal",
    description = "Load a CSV or JSON text into the table `namespace.table` of the `warehouse` catalog: the whole file is the string `content` of the request. `format` is `csv` (the default; any value except `json` is read as CSV) or `json` (an array of objects). Column types are inferred from the values. The rows are built in a staging table first and then replace the target, so an existing table of that name is replaced, not appended to. The statements run with the caller's own identity, so the engine's permissions apply. Every call is recorded as a job (`job_id`, listed by `GET /api/jobs`). A file that cannot be read or a statement the engine refuses is a 400 with a sentence, and the job is recorded as failed. In demo mode the rows go into an in-memory lakehouse.",
    request_body = UploadRequest,
    responses(
        (status = 200, description = "The table was loaded", body = UploadResult),
        (status = 400, description = "The file could not be read or the engine refused a statement", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn upload(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(req): ApiJson<UploadRequest>,
) -> Response {
    let fmt = req.format.to_lowercase();
    let title = format!("{}.{}", req.namespace, req.table);
    let (user, owner) = caller
        .as_ref()
        .map(|c| (c.0.identity.display_name.clone(), c.0.identity.sub.clone()))
        .unwrap_or_else(|| (s.dev_user.clone(), s.dev_user.clone()));

    if s.demo {
        let mut demo = s.demo_state.write().expect("demo state poisoned");
        return match demo.upload(&req.namespace, &req.table, &fmt, &req.content) {
            Ok(n) => {
                let job = jobs::record(
                    &s.jobs,
                    "load",
                    title,
                    "succeeded",
                    Some(format!("{} via {fmt}", rows(n))),
                    Some(n),
                    &user,
                    &owner,
                );
                Json(UploadResult {
                    namespace: req.namespace,
                    table: req.table,
                    rows_loaded: n,
                    job_id: job.id,
                })
                .into_response()
            }
            Err(e) => {
                jobs::record(
                    &s.jobs,
                    "load",
                    title,
                    "failed",
                    Some(e.clone()),
                    None,
                    &user,
                    &owner,
                );
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e })),
                )
                    .into_response()
            }
        };
    }

    let (s2, req2, fmt2) = (s.clone(), req.clone(), fmt.clone());
    let p = caller.map(|c| c.0);
    let outcome = tokio::spawn(async move { real_upload(&s2, &req2, &fmt2, p.as_ref()).await })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
    match outcome {
        Ok(n) => {
            let job = jobs::record(
                &s.jobs,
                "load",
                title,
                "succeeded",
                Some(format!("{} via {fmt}", rows(n))),
                Some(n),
                &user,
                &owner,
            );
            Json(UploadResult {
                namespace: req.namespace,
                table: req.table,
                rows_loaded: n,
                job_id: job.id,
            })
            .into_response()
        }
        Err(e) => {
            jobs::record(
                &s.jobs,
                "load",
                title,
                "failed",
                Some(e.clone()),
                None,
                &user,
                &owner,
            );
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response()
        }
    }
}

#[utoipa::path(
    post,
    operation_id = "tables_transfer",
    path = "/api/transfer",
    tag = "internal",
    description = "Copy one table of the `warehouse` catalog into another. `source` and `target` are `namespace.table`. `mode` is `overwrite` (the default: the target is dropped and rebuilt from the source) or `append` (rows are added to the target). Overwriting a table with itself is refused, because the copy would read from a table that no longer exists; appending to itself is allowed. The statements run with the caller's own identity, so the engine's permissions apply. The answer counts the rows now in the target. Every call is recorded as a job (`job_id`, listed by `GET /api/jobs`). A refusal or an engine error is a 400 with a sentence, and the job is recorded as failed.",
    request_body = TransferRequest,
    responses(
        (status = 200, description = "The table was copied", body = TransferResult),
        (status = 400, description = "The mode, a table name or the copy was refused", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn transfer(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    ApiJson(req): ApiJson<TransferRequest>,
) -> Response {
    let title = format!("{} → {}", req.source, req.target);
    let (user, owner) = caller
        .as_ref()
        .map(|c| (c.0.identity.display_name.clone(), c.0.identity.sub.clone()))
        .unwrap_or_else(|| (s.dev_user.clone(), s.dev_user.clone()));

    if s.demo {
        let mut demo = s.demo_state.write().expect("demo state poisoned");
        return match demo.transfer(&req.source, &req.target, &req.mode) {
            Ok(n) => {
                let job = jobs::record(
                    &s.jobs,
                    "transfer",
                    title,
                    "succeeded",
                    Some(format!("{} {}", req.mode, rows(n))),
                    Some(n),
                    &user,
                    &owner,
                );
                Json(TransferResult {
                    source: req.source,
                    target: req.target,
                    rows: n,
                    job_id: job.id,
                })
                .into_response()
            }
            Err(e) => {
                jobs::record(
                    &s.jobs,
                    "transfer",
                    title,
                    "failed",
                    Some(e.clone()),
                    None,
                    &user,
                    &owner,
                );
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e })),
                )
                    .into_response()
            }
        };
    }

    let (s2, req2) = (s.clone(), req.clone());
    let p = caller.map(|c| c.0);
    let outcome = tokio::spawn(async move { real_transfer(&s2, &req2, p.as_ref()).await })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
    match outcome {
        Ok(n) => {
            let job = jobs::record(
                &s.jobs,
                "transfer",
                title,
                "succeeded",
                Some(format!("{} {}", req.mode, rows(n))),
                Some(n),
                &user,
                &owner,
            );
            Json(TransferResult {
                source: req.source,
                target: req.target,
                rows: n,
                job_id: job.id,
            })
            .into_response()
        }
        Err(e) => {
            jobs::record(
                &s.jobs,
                "transfer",
                title,
                "failed",
                Some(e.clone()),
                None,
                &user,
                &owner,
            );
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response()
        }
    }
}

/// Name of the staging table a load is built in before it replaces the target.
/// The `__nova_load_` infix makes an orphan (left by a killed pod) recognisable.
fn staging_name(table: &str, stamp: u64) -> String {
    format!("{table}__nova_load_{stamp}")
}

async fn load_rows(
    state: &AppState,
    table: &str,
    ddl_cols: &str,
    rows: &[Vec<Value>],
    types: &[String],
    caller: Option<&crate::auth::Principal>,
) -> Result<usize, String> {
    trino::execute(state, format!("CREATE TABLE {table} ({ddl_cols})"), caller).await?;
    let mut loaded = 0usize;
    for chunk in rows.chunks(1000) {
        let values = chunk
            .iter()
            .map(|row| {
                format!(
                    "({})",
                    row.iter()
                        .zip(types.iter())
                        .map(|(v, ty)| sql_literal_for(v, ty))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        trino::execute(
            state,
            format!("INSERT INTO {table} VALUES {values}"),
            caller,
        )
        .await?;
        loaded += chunk.len();
    }
    Ok(loaded)
}

async fn real_upload(
    state: &AppState,
    req: &UploadRequest,
    fmt: &str,
    caller: Option<&crate::auth::Principal>,
) -> Result<usize, String> {
    let (columns, rows) = if fmt == "json" {
        demo::parse_json_table(&req.content)?
    } else {
        demo::parse_csv(&req.content)?
    };
    if columns.is_empty() {
        return Err("no columns found".into());
    }

    let types: Vec<String> = (0..columns.len()).map(|i| infer_type(&rows, i)).collect();
    trino::execute(
        state,
        format!(
            "CREATE SCHEMA IF NOT EXISTS warehouse.{}",
            quote_ident(&req.namespace)
        ),
        caller,
    )
    .await?;
    let target = format!(
        "warehouse.{}.{}",
        quote_ident(&req.namespace),
        quote_ident(&req.table)
    );
    let ddl_cols = columns
        .iter()
        .zip(types.iter())
        .map(|(c, t)| format!("{} {}", quote_ident(c), t))
        .collect::<Vec<_>>()
        .join(", ");

    let staging = format!(
        "warehouse.{}.{}",
        quote_ident(&req.namespace),
        quote_ident(&staging_name(&req.table, crate::jobs::now_ms()))
    );

    let loaded = match load_rows(state, &staging, &ddl_cols, &rows, &types, caller).await {
        Ok(n) => n,
        Err(e) => {
            let _ = trino::execute(state, format!("DROP TABLE IF EXISTS {staging}"), caller).await;
            return Err(e);
        }
    };
    let swap = trino::execute(
        state,
        format!("CREATE OR REPLACE TABLE {target} AS SELECT * FROM {staging}"),
        caller,
    )
    .await;
    let _ = trino::execute(state, format!("DROP TABLE IF EXISTS {staging}"), caller).await;
    swap?;
    Ok(loaded)
}

async fn real_transfer(
    state: &AppState,
    req: &TransferRequest,
    caller: Option<&crate::auth::Principal>,
) -> Result<usize, String> {
    let (sns, stbl) = demo::parse_ref(&req.source)?;
    let (tns, ttbl) = demo::parse_ref(&req.target)?;
    let source = format!("warehouse.{}.{}", quote_ident(&sns), quote_ident(&stbl));
    let target = format!("warehouse.{}.{}", quote_ident(&tns), quote_ident(&ttbl));
    check_transfer(&req.mode, (&sns, &stbl), (&tns, &ttbl))?;

    if req.mode.eq_ignore_ascii_case("append") {
        trino::execute(
            state,
            format!("INSERT INTO {target} SELECT * FROM {source}"),
            caller,
        )
        .await?;
    } else {
        trino::execute(
            state,
            format!(
                "CREATE SCHEMA IF NOT EXISTS warehouse.{}",
                quote_ident(&tns)
            ),
            caller,
        )
        .await?;
        trino::execute(
            state,
            format!("CREATE OR REPLACE TABLE {target} AS SELECT * FROM {source}"),
            caller,
        )
        .await?;
    }

    let count = trino::execute(state, format!("SELECT count(*) FROM {target}"), caller).await?;
    let n = count
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    Ok(n)
}

pub(crate) fn check_transfer_mode(mode: &str) -> Result<(), String> {
    let m = mode.to_ascii_lowercase();
    if m == "overwrite" || m == "append" {
        Ok(())
    } else {
        Err("mode must be 'overwrite' or 'append'".into())
    }
}

/// An overwrite drops the target before copying into it, so it must never be the
/// source: the copy would read from a table that no longer exists, and the data
/// would be gone. Trino folds unquoted identifiers to lower case, hence the
/// case-insensitive comparison.
fn check_transfer(mode: &str, source: (&str, &str), target: (&str, &str)) -> Result<(), String> {
    check_transfer_mode(mode)?;
    let same = source.0.eq_ignore_ascii_case(target.0) && source.1.eq_ignore_ascii_case(target.1);
    if same && !mode.eq_ignore_ascii_case("append") {
        return Err(format!(
            "source and target are the same table ({}.{}); overwrite would delete it",
            source.0, source.1
        ));
    }
    Ok(())
}

fn infer_type(rows: &[Vec<Value>], col: usize) -> String {
    let mut ints = true;
    let mut numbers = true;
    let mut bools = true;
    let mut seen = false;
    for row in rows.iter() {
        match row.get(col) {
            None | Some(Value::Null) => {}
            Some(Value::Bool(_)) => {
                ints = false;
                numbers = false;
            }
            Some(Value::Number(n)) => {
                bools = false;
                seen = true;
                if !n.is_i64() && !n.is_u64() {
                    ints = false;
                }
            }
            Some(_) => {
                ints = false;
                numbers = false;
                bools = false;
            }
        }
    }
    if !seen && bools {
        return "boolean".into();
    }
    if seen && ints {
        "bigint".into()
    } else if seen && numbers {
        "double".into()
    } else if bools
        && rows
            .iter()
            .any(|r| matches!(r.get(col), Some(Value::Bool(_))))
    {
        "boolean".into()
    } else {
        "varchar".into()
    }
}

fn sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => format!("'{}'", other.to_string().replace('\'', "''")),
    }
}

fn sql_literal_for(v: &Value, ty: &str) -> String {
    if ty == "varchar" {
        match v {
            Value::Null => "NULL".into(),
            Value::Bool(b) => format!("'{b}'"),
            Value::Number(n) => format!("'{n}'"),
            Value::String(s) => format!("'{}'", s.replace('\'', "''")),
            other => format!("'{}'", other.to_string().replace('\'', "''")),
        }
    } else {
        sql_literal(v)
    }
}

pub fn quote_ident(raw: &str) -> String {
    format!("\"{}\"", raw.replace('"', "\"\""))
}

/// `1 row`, `3 rows`.
fn rows(n: usize) -> String {
    if n == 1 {
        "1 row".into()
    } else {
        format!("{n} rows")
    }
}

#[cfg(test)]
mod tests {
    use super::{check_transfer, staging_name};

    #[test]
    fn unknown_transfer_mode_is_refused() {
        assert!(check_transfer("bogus", ("a", "b"), ("c", "d")).is_err());
        assert!(check_transfer("REPLACE", ("a", "b"), ("c", "d")).is_err());
        assert!(check_transfer("APPEND", ("a", "b"), ("c", "d")).is_ok());
    }

    #[test]
    fn staging_name_is_distinct_from_target() {
        let s = staging_name("Trips", 42).to_lowercase();
        assert_eq!(s, "trips__nova_load_42");
        assert_ne!(s, "trips");
    }

    #[test]
    fn mixed_column_is_varchar_and_quotes_numbers() {
        use serde_json::json;
        let rows = vec![vec![json!(1), json!(2)], vec![json!("abc"), json!(3)]];
        assert_eq!(super::infer_type(&rows, 0), "varchar");
        assert_eq!(super::sql_literal_for(&json!(7), "varchar"), "'7'");
        assert_eq!(super::sql_literal_for(&json!(true), "varchar"), "'true'");
        assert_eq!(super::sql_literal_for(&json!(7), "bigint"), "7");
    }

    #[test]
    fn rows_are_pluralised() {
        assert_eq!(super::rows(0), "0 rows");
        assert_eq!(super::rows(1), "1 row");
        assert_eq!(super::rows(2), "2 rows");
    }

    #[test]
    fn overwriting_a_table_with_itself_is_refused() {
        assert!(check_transfer("overwrite", ("raw", "trips"), ("raw", "trips")).is_err());
        assert!(check_transfer("overwrite", ("raw", "Trips"), ("RAW", "trips")).is_err());
        assert!(check_transfer("replace", ("raw", "trips"), ("raw", "trips")).is_err());
    }

    #[test]
    fn distinct_tables_and_appends_are_allowed() {
        assert!(check_transfer("overwrite", ("raw", "trips"), ("curated", "trips")).is_ok());
        assert!(check_transfer("append", ("raw", "trips"), ("raw", "trips")).is_ok());
    }
}
