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

use crate::{AppState, demo, jobs, trino};

#[derive(Deserialize)]
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

#[derive(Serialize)]
pub struct UploadResult {
    pub namespace: String,
    pub table: String,
    pub rows_loaded: usize,
    pub job_id: String,
}

#[derive(Deserialize)]
pub struct TransferRequest {
    pub source: String,
    pub target: String,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "overwrite".into()
}

#[derive(Serialize)]
pub struct TransferResult {
    pub source: String,
    pub target: String,
    pub rows: usize,
    pub job_id: String,
}

pub async fn upload(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    Json(req): Json<UploadRequest>,
) -> Response {
    let fmt = req.format.to_lowercase();
    let title = format!("{}.{}", req.namespace, req.table);

    if s.demo {
        let mut demo = s.demo_state.write().expect("demo state poisoned");
        return match demo.upload(&req.namespace, &req.table, &fmt, &req.content) {
            Ok(n) => {
                let job = jobs::record(
                    &s.jobs,
                    "load",
                    title,
                    "succeeded",
                    Some(format!("{n} rows via {fmt}")),
                    Some(n),
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
                jobs::record(&s.jobs, "load", title, "failed", Some(e.clone()), None);
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e })),
                )
                    .into_response()
            }
        };
    }

    match real_upload(&s, &req, &fmt, caller.as_ref().map(|c| &c.0)).await {
        Ok(n) => {
            let job = jobs::record(
                &s.jobs,
                "load",
                title,
                "succeeded",
                Some(format!("{n} rows via {fmt}")),
                Some(n),
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
            jobs::record(&s.jobs, "load", title, "failed", Some(e.clone()), None);
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response()
        }
    }
}

pub async fn transfer(
    State(s): State<Arc<AppState>>,
    caller: Option<axum::Extension<crate::auth::Principal>>,
    // The body extractor has to be last: axum consumes the request for it.
    Json(req): Json<TransferRequest>,
) -> Response {
    let title = format!("{} → {}", req.source, req.target);

    if s.demo {
        let mut demo = s.demo_state.write().expect("demo state poisoned");
        return match demo.transfer(&req.source, &req.target, &req.mode) {
            Ok(n) => {
                let job = jobs::record(
                    &s.jobs,
                    "transfer",
                    title,
                    "succeeded",
                    Some(format!("{} {n} rows", req.mode)),
                    Some(n),
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
                jobs::record(&s.jobs, "transfer", title, "failed", Some(e.clone()), None);
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e })),
                )
                    .into_response()
            }
        };
    }

    match real_transfer(&s, &req, caller.as_ref().map(|c| &c.0)).await {
        Ok(n) => {
            let job = jobs::record(
                &s.jobs,
                "transfer",
                title,
                "succeeded",
                Some(format!("{} {n} rows", req.mode)),
                Some(n),
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
            jobs::record(&s.jobs, "transfer", title, "failed", Some(e.clone()), None);
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response()
        }
    }
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

    trino::execute(state, format!("DROP TABLE IF EXISTS {target}"), caller).await?;
    trino::execute(state, format!("CREATE TABLE {target} ({ddl_cols})"), caller).await?;

    let mut loaded = 0usize;
    for chunk in rows.chunks(100) {
        let values = chunk
            .iter()
            .map(|row| {
                format!(
                    "({})",
                    row.iter().map(sql_literal).collect::<Vec<_>>().join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        trino::execute(
            state,
            format!("INSERT INTO {target} VALUES {values}"),
            caller,
        )
        .await?;
        loaded += chunk.len();
    }
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
        trino::execute(state, format!("DROP TABLE IF EXISTS {target}"), caller).await?;
        trino::execute(
            state,
            format!("CREATE TABLE {target} AS SELECT * FROM {source}"),
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

/// An overwrite drops the target before copying into it, so it must never be the
/// source: the copy would read from a table that no longer exists, and the data
/// would be gone. Trino folds unquoted identifiers to lower case, hence the
/// case-insensitive comparison.
fn check_transfer(mode: &str, source: (&str, &str), target: (&str, &str)) -> Result<(), String> {
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
    for row in rows.iter().take(50) {
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

pub fn quote_ident(raw: &str) -> String {
    format!("\"{}\"", raw.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::check_transfer;

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
