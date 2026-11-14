//! The query log in the state store (decision D25, stardelt-nova#116): writing rows from the
//! collector and removing old ones. Reading comes with the API.

use std::time::Duration;

use sqlx::PgPool;
use tracing::{info, warn};

use crate::query_log_collector::Accepted;

/// How long rows are kept when `NOVA_QUERY_LOG_RETENTION_DAYS` is not set.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// How long profiles are kept when `NOVA_QUERY_PROFILE_RETENTION_DAYS` is not set.
pub const DEFAULT_PROFILE_RETENTION_DAYS: i64 = 14;

/// How often old rows are removed.
const PURGE_EVERY: Duration = Duration::from_secs(3600);

/// Insert the row, or update it when the query is already known. A late *created* event must
/// never overwrite a finished row (events can arrive out of order after a retry), so an existing
/// row is only replaced while it is still `QUEUED` or by a terminal state.
pub async fn upsert(pool: &PgPool, accepted: &Accepted) -> sqlx::Result<()> {
    let r = &accepted.record;
    let s = r.stats.as_ref();
    sqlx::query(
        "INSERT INTO query_log (query_id, warehouse, state, principal, source, client_tags, catalog, \
            schema_name, query_type, sql_text, sql_hash, created_at, started_at, ended_at, \
            queued_ms, planning_ms, execution_ms, wall_ms, cpu_ms, peak_memory_bytes, input_rows, \
            input_bytes, output_rows, output_bytes, written_rows, written_bytes, error_code, \
            error_message, tables_read, tables_written) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12::timestamptz, $13::timestamptz, \
            $14::timestamptz, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, \
            $29, $30) \
         ON CONFLICT (query_id) DO UPDATE SET \
            warehouse = EXCLUDED.warehouse, state = EXCLUDED.state, principal = EXCLUDED.principal, \
            source = EXCLUDED.source, client_tags = EXCLUDED.client_tags, catalog = EXCLUDED.catalog, \
            schema_name = EXCLUDED.schema_name, query_type = EXCLUDED.query_type, \
            sql_text = EXCLUDED.sql_text, sql_hash = EXCLUDED.sql_hash, created_at = EXCLUDED.created_at, \
            started_at = EXCLUDED.started_at, ended_at = EXCLUDED.ended_at, \
            queued_ms = EXCLUDED.queued_ms, planning_ms = EXCLUDED.planning_ms, \
            execution_ms = EXCLUDED.execution_ms, wall_ms = EXCLUDED.wall_ms, cpu_ms = EXCLUDED.cpu_ms, \
            peak_memory_bytes = EXCLUDED.peak_memory_bytes, input_rows = EXCLUDED.input_rows, \
            input_bytes = EXCLUDED.input_bytes, output_rows = EXCLUDED.output_rows, \
            output_bytes = EXCLUDED.output_bytes, written_rows = EXCLUDED.written_rows, \
            written_bytes = EXCLUDED.written_bytes, error_code = EXCLUDED.error_code, \
            error_message = EXCLUDED.error_message, tables_read = EXCLUDED.tables_read, \
            tables_written = EXCLUDED.tables_written, updated_at = now() \
         WHERE query_log.state = 'QUEUED' OR EXCLUDED.state <> 'QUEUED'",
    )
    .bind(&r.query_id)
    .bind(&accepted.warehouse)
    .bind(r.state.as_str())
    .bind(&r.principal)
    .bind(&r.source)
    .bind(&r.client_tags)
    .bind(&r.catalog)
    .bind(&r.schema)
    .bind(&r.query_type)
    .bind(&r.sql_text)
    .bind(&r.sql_hash)
    .bind(&r.created_at)
    .bind(&r.started_at)
    .bind(&r.ended_at)
    .bind(s.map(|s| s.queued_ms))
    .bind(s.map(|s| s.planning_ms))
    .bind(s.map(|s| s.execution_ms))
    .bind(s.map(|s| s.wall_ms))
    .bind(s.map(|s| s.cpu_ms))
    .bind(s.map(|s| s.peak_memory_bytes))
    .bind(s.map(|s| s.input_rows))
    .bind(s.map(|s| s.input_bytes))
    .bind(s.map(|s| s.output_rows))
    .bind(s.map(|s| s.output_bytes))
    .bind(s.map(|s| s.written_rows))
    .bind(s.map(|s| s.written_bytes))
    .bind(&r.error_code)
    .bind(&r.error_message)
    .bind(&r.tables_read)
    .bind(&r.tables_written)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete rows created more than `days` days ago; returns how many.
pub async fn purge(pool: &PgPool, days: i64) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM query_log WHERE created_at < now() - make_interval(days => $1::int)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Keep one query's profile; a second event for the same query replaces it.
pub async fn upsert_profile(
    pool: &PgPool,
    query_id: &str,
    profile: &serde_json::Value,
) -> sqlx::Result<()> {
    let bytes = serde_json::to_vec(profile).map(|b| b.len()).unwrap_or(0) as i32;
    sqlx::query(
        "INSERT INTO query_profile (query_id, profile, bytes) VALUES ($1, $2, $3) \
         ON CONFLICT (query_id) DO UPDATE SET profile = EXCLUDED.profile, \
            bytes = EXCLUDED.bytes, created_at = now()",
    )
    .bind(query_id)
    .bind(profile)
    .bind(bytes)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete profiles created more than `days` days ago; returns how many.
pub async fn purge_profiles(pool: &PgPool, days: i64) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM query_profile WHERE created_at < now() - make_interval(days => $1::int)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Retention from the environment: unset or unparsable gives the default, `0` switches the
/// purge off (`None`), anything else is the number of days.
pub fn retention_days_from(get: impl Fn(&str) -> Option<String>) -> Option<i64> {
    days_from(get, "NOVA_QUERY_LOG_RETENTION_DAYS", DEFAULT_RETENTION_DAYS)
}

/// The same rule for `NOVA_QUERY_PROFILE_RETENTION_DAYS`.
pub fn profile_retention_days_from(get: impl Fn(&str) -> Option<String>) -> Option<i64> {
    days_from(
        get,
        "NOVA_QUERY_PROFILE_RETENTION_DAYS",
        DEFAULT_PROFILE_RETENTION_DAYS,
    )
}

fn days_from(get: impl Fn(&str) -> Option<String>, name: &str, default: i64) -> Option<i64> {
    match get(name).map(|v| v.trim().to_string()) {
        None => Some(default),
        Some(v) if v.is_empty() => Some(default),
        Some(v) => match v.parse::<i64>() {
            Ok(0) => None,
            Ok(days) if days > 0 => Some(days),
            _ => {
                warn!(value = %v, "the {name} value is not a positive number; using the default");
                Some(default)
            }
        },
    }
}

/// Remove old rows once an hour for as long as Nova runs.
pub fn spawn_retention(pool: PgPool) {
    let log_days = retention_days_from(|name| std::env::var(name).ok());
    let profile_days = profile_retention_days_from(|name| std::env::var(name).ok());
    if log_days.is_none() && profile_days.is_none() {
        info!(
            "query-log retention is off (NOVA_QUERY_LOG_RETENTION_DAYS=0 and NOVA_QUERY_PROFILE_RETENTION_DAYS=0)"
        );
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PURGE_EVERY);
        loop {
            tick.tick().await;
            if let Some(days) = log_days {
                match purge(&pool, days).await {
                    Ok(0) => {}
                    Ok(n) => info!(rows = n, days, "removed old query-log rows"),
                    Err(e) => warn!(error = %e, "query-log purge failed"),
                }
            }
            if let Some(days) = profile_days {
                match purge_profiles(&pool, days).await {
                    Ok(0) => {}
                    Ok(n) => info!(rows = n, days, "removed old query profiles"),
                    Err(e) => warn!(error = %e, "query-profile purge failed"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::migrated_pool;
    use crate::query_log_event::parse;

    const CREATED: &str = include_str!("testdata/query_log/created.json");
    const SELECT: &str = include_str!("testdata/query_log/completed_select.json");

    fn accepted(body: &str, warehouse: &str) -> Accepted {
        Accepted {
            record: parse(body.as_bytes()).expect("a fixture"),
            warehouse: warehouse.into(),
            profile: None,
        }
    }

    async fn state_of(pool: &PgPool, id: &str) -> (String, Option<i64>, String) {
        sqlx::query_as::<_, (String, Option<i64>, String)>(
            "SELECT state, wall_ms, warehouse FROM query_log WHERE query_id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("the row exists")
    }

    #[test]
    fn retention_defaults_to_ninety_days_and_zero_switches_it_off() {
        let env = |value: Option<&'static str>| move |_: &str| value.map(String::from);
        assert_eq!(retention_days_from(env(None)), Some(90));
        assert_eq!(retention_days_from(env(Some(""))), Some(90));
        assert_eq!(retention_days_from(env(Some("30"))), Some(30));
        assert_eq!(retention_days_from(env(Some("0"))), None);
        assert_eq!(retention_days_from(env(Some("-5"))), Some(90));
        assert_eq!(retention_days_from(env(Some("many"))), Some(90));
    }

    #[test]
    fn profile_retention_defaults_to_fourteen_days_and_zero_switches_it_off() {
        let env = |value: Option<&'static str>| move |_: &str| value.map(String::from);
        assert_eq!(profile_retention_days_from(env(None)), Some(14));
        assert_eq!(profile_retention_days_from(env(Some(""))), Some(14));
        assert_eq!(profile_retention_days_from(env(Some("7"))), Some(7));
        assert_eq!(profile_retention_days_from(env(Some("0"))), None);
        assert_eq!(profile_retention_days_from(env(Some("-5"))), Some(14));
        assert_eq!(profile_retention_days_from(env(Some("many"))), Some(14));
    }

    fn profile_fixture() -> serde_json::Value {
        crate::query_log_event::parse_profile(
            include_str!("testdata/query_log/completed_profile.json").as_bytes(),
        )
        .expect("a profile")
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn a_profile_is_stored_once_and_purged_after_its_retention() {
        let pool = migrated_pool().await;
        upsert(&pool, &accepted(SELECT, "default")).await.unwrap();
        let id = "20261019_113323_00000_tnvu4";
        let profile = profile_fixture();
        upsert_profile(&pool, id, &profile).await.unwrap();
        upsert_profile(&pool, id, &profile).await.unwrap();
        let (stored, bytes): (serde_json::Value, i32) =
            sqlx::query_as("SELECT profile, bytes FROM query_profile WHERE query_id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, profile);
        assert_eq!(bytes as usize, serde_json::to_vec(&profile).unwrap().len());
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM query_profile")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(purge_profiles(&pool, 0).await.unwrap(), 1);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM query_profile")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn deleting_a_query_removes_its_profile() {
        let pool = migrated_pool().await;
        upsert(&pool, &accepted(SELECT, "default")).await.unwrap();
        let id = "20261019_113323_00000_tnvu4";
        upsert_profile(&pool, id, &profile_fixture()).await.unwrap();
        sqlx::query("DELETE FROM query_log WHERE query_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM query_profile")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn a_created_event_makes_a_queued_row_and_the_completed_event_finishes_it() {
        let pool = migrated_pool().await;
        let id = "20261019_113323_00000_tnvu4";
        upsert(&pool, &accepted(CREATED, "default")).await.unwrap();
        assert_eq!(
            state_of(&pool, id).await,
            ("QUEUED".into(), None, "default".into())
        );
        upsert(&pool, &accepted(SELECT, "default")).await.unwrap();
        assert_eq!(
            state_of(&pool, id).await,
            ("FINISHED".into(), Some(2324), "default".into())
        );
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM query_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn a_late_created_event_does_not_overwrite_a_finished_row() {
        let pool = migrated_pool().await;
        let id = "20261019_113323_00000_tnvu4";
        upsert(&pool, &accepted(SELECT, "analytics")).await.unwrap();
        upsert(&pool, &accepted(CREATED, "default")).await.unwrap();
        assert_eq!(
            state_of(&pool, id).await,
            ("FINISHED".into(), Some(2324), "analytics".into())
        );
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn tables_and_tags_are_stored_as_arrays() {
        let pool = migrated_pool().await;
        upsert(&pool, &accepted(SELECT, "default")).await.unwrap();
        let (read, tags): (Vec<String>, Vec<String>) =
            sqlx::query_as("SELECT tables_read, client_tags FROM query_log")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(read, ["tpch.tiny.nation"]);
        assert_eq!(tags, ["nova", "warehouse-default"]);
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn purge_removes_only_rows_older_than_the_retention() {
        let pool = migrated_pool().await;
        upsert(&pool, &accepted(SELECT, "default")).await.unwrap();
        sqlx::query("UPDATE query_log SET created_at = now() - interval '100 days'")
            .execute(&pool)
            .await
            .unwrap();
        upsert(
            &pool,
            &accepted(
                CREATED
                    .replace("20261019_113323_00000_tnvu4", "new_one")
                    .as_str(),
                "default",
            ),
        )
        .await
        .unwrap();
        assert_eq!(purge(&pool, 90).await.unwrap(), 1);
        let left: Vec<String> = sqlx::query_scalar("SELECT query_id FROM query_log")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, ["new_one"]);
    }
}
