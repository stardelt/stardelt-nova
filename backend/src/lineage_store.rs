//! OpenLineage store (D40, WP-34a): persist raw events and derive edges.

use std::time::Duration;

use sqlx::PgPool;
use tracing::{info, warn};

use crate::lineage_event::LineageEvent;

pub const DEFAULT_RETENTION_DAYS: i64 = 90;
pub const DEFAULT_EDGE_RETENTION_DAYS: i64 = 365;

const PURGE_EVERY: Duration = Duration::from_secs(3600);

pub async fn insert_event(pool: &PgPool, ev: &LineageEvent) -> sqlx::Result<()> {
    // Parse event_time as timestamptz - store as string then cast
    sqlx::query(
        "INSERT INTO lineage_events (event_time, event_type, job_namespace, job_name, run_id, producer, schema_url, payload) \
         VALUES ($1::timestamptz, $2, $3, $4, $5::uuid, $6, $7, $8)",
    )
    .bind(&ev.event_time)
    .bind(&ev.event_type)
    .bind(&ev.job_namespace)
    .bind(&ev.job_name)
    .bind(&ev.run_id)
    .bind(&ev.producer)
    .bind(&ev.schema_url)
    .bind(&ev.raw)
    .execute(pool)
    .await?;

    // Upsert edges from inputs/outputs
    // For each input
    for input in &ev.inputs {
        sqlx::query(
            "INSERT INTO lineage_edges (run_id, job_namespace, job_name, dataset_namespace, dataset_name, direction, event_time) \
             VALUES ($1::uuid, $2, $3, $4, $5, 'input', $6::timestamptz) \
             ON CONFLICT (run_id, dataset_namespace, dataset_name, direction) DO UPDATE SET \
               job_namespace = EXCLUDED.job_namespace, \
               job_name = EXCLUDED.job_name, \
               event_time = GREATEST(lineage_edges.event_time, EXCLUDED.event_time)",
        )
        .bind(&ev.run_id)
        .bind(&ev.job_namespace)
        .bind(&ev.job_name)
        .bind(&input.namespace)
        .bind(&input.name)
        .bind(&ev.event_time)
        .execute(pool)
        .await?;
    }
    for output in &ev.outputs {
        sqlx::query(
            "INSERT INTO lineage_edges (run_id, job_namespace, job_name, dataset_namespace, dataset_name, direction, event_time) \
             VALUES ($1::uuid, $2, $3, $4, $5, 'output', $6::timestamptz) \
             ON CONFLICT (run_id, dataset_namespace, dataset_name, direction) DO UPDATE SET \
               job_namespace = EXCLUDED.job_namespace, \
               job_name = EXCLUDED.job_name, \
               event_time = GREATEST(lineage_edges.event_time, EXCLUDED.event_time)",
        )
        .bind(&ev.run_id)
        .bind(&ev.job_namespace)
        .bind(&ev.job_name)
        .bind(&output.namespace)
        .bind(&output.name)
        .bind(&ev.event_time)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn purge_events(pool: &PgPool, days: i64) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM lineage_events WHERE event_time < now() - make_interval(days => $1::int)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn purge_edges(pool: &PgPool, days: i64) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM lineage_edges WHERE event_time < now() - make_interval(days => $1::int)",
    )
    .bind(days as i32)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub fn retention_days_from(get: impl Fn(&str) -> Option<String>) -> Option<i64> {
    match get("NOVA_LINEAGE_RETENTION_DAYS").map(|v| v.trim().to_string()) {
        None => Some(DEFAULT_RETENTION_DAYS),
        Some(v) if v.is_empty() => Some(DEFAULT_RETENTION_DAYS),
        Some(v) => match v.parse::<i64>() {
            Ok(0) => None,
            Ok(days) if days > 0 => Some(days),
            _ => {
                warn!(value = %v, "NOVA_LINEAGE_RETENTION_DAYS is not a positive number; using the default");
                Some(DEFAULT_RETENTION_DAYS)
            }
        },
    }
}

pub fn edge_retention_days_from(get: impl Fn(&str) -> Option<String>) -> Option<i64> {
    match get("NOVA_LINEAGE_EDGE_RETENTION_DAYS").map(|v| v.trim().to_string()) {
        None => Some(DEFAULT_EDGE_RETENTION_DAYS),
        Some(v) if v.is_empty() => Some(DEFAULT_EDGE_RETENTION_DAYS),
        Some(v) => match v.parse::<i64>() {
            Ok(0) => None,
            Ok(days) if days > 0 => Some(days),
            _ => {
                warn!(value = %v, "NOVA_LINEAGE_EDGE_RETENTION_DAYS is not a positive number; using the default");
                Some(DEFAULT_EDGE_RETENTION_DAYS)
            }
        },
    }
}

pub fn spawn_retention(pool: PgPool) {
    let event_days = retention_days_from(|name| std::env::var(name).ok());
    let edge_days = edge_retention_days_from(|name| std::env::var(name).ok());
    if event_days.is_none() && edge_days.is_none() {
        info!("lineage retention is off");
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PURGE_EVERY);
        loop {
            tick.tick().await;
            if let Some(days) = event_days {
                match purge_events(&pool, days).await {
                    Ok(0) => {}
                    Ok(n) => info!(rows = n, days, "removed old lineage events"),
                    Err(e) => warn!(error = %e, "lineage event purge failed"),
                }
            }
            if let Some(days) = edge_days {
                match purge_edges(&pool, days).await {
                    Ok(0) => {}
                    Ok(n) => info!(rows = n, days, "removed old lineage edges"),
                    Err(e) => warn!(error = %e, "lineage edge purge failed"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::migrated_pool;
    use crate::lineage_event::{LineageDataset, LineageEvent};

    fn sample_event(run_id: &str, event_time: &str, event_type: &str) -> LineageEvent {
        LineageEvent {
            event_type: event_type.to_string(),
            event_time: event_time.to_string(),
            producer: Some("test".to_string()),
            schema_url: None,
            job_namespace: "default".to_string(),
            job_name: "job1".to_string(),
            run_id: run_id.to_string(),
            inputs: vec![LineageDataset {
                namespace: "mobility.gold".to_string(),
                name: "trips".to_string(),
                facets: None,
            }],
            outputs: vec![LineageDataset {
                namespace: "mobility.gold".to_string(),
                name: "kpis".to_string(),
                facets: None,
            }],
            raw: serde_json::json!({"eventType": event_type}),
        }
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn lineage_store_valid_event_stores_one_raw_row_and_two_edges() {
        let pool = migrated_pool().await;
        let run_id = "00000000-0000-0000-0000-000000000096";
        sqlx::query("DELETE FROM lineage_edges WHERE run_id = $1::uuid")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM lineage_events WHERE run_id = $1::uuid")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();

        insert_event(
            &pool,
            &sample_event(run_id, "2026-01-01T00:00:00Z", "COMPLETE"),
        )
        .await
        .unwrap();
        let events: (i64,) =
            sqlx::query_as("SELECT count(*) FROM lineage_events WHERE run_id = $1::uuid")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events.0, 1, "one raw row stored");
        let edges: (i64,) =
            sqlx::query_as("SELECT count(*) FROM lineage_edges WHERE run_id = $1::uuid")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(edges.0, 2, "two edges stored (input + output)");
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn lineage_store_second_event_updates_edge_not_duplicates() {
        let pool = migrated_pool().await;
        let run_id = "00000000-0000-0000-0000-000000000099";
        // Clean any leftovers for this run
        sqlx::query("DELETE FROM lineage_edges WHERE run_id = $1::uuid")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM lineage_events WHERE run_id = $1::uuid")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();

        let ev1 = sample_event(run_id, "2026-01-01T00:00:00Z", "START");
        insert_event(&pool, &ev1).await.unwrap();
        let ev2 = sample_event(run_id, "2026-01-01T01:00:00Z", "COMPLETE");
        insert_event(&pool, &ev2).await.unwrap();

        let events: (i64,) =
            sqlx::query_as("SELECT count(*) FROM lineage_events WHERE run_id = $1::uuid")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events.0, 2, "each raw event is stored");

        let edges: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM lineage_edges WHERE run_id = $1::uuid AND direction = 'output'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(edges.0, 1, "same run/dataset/direction is not duplicated");

        let et: (String,) = sqlx::query_as(
            "SELECT to_char(event_time, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM lineage_edges WHERE run_id = $1::uuid AND direction = 'output'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            et.0.contains("01:00:00"),
            "edge time updated to later event: {}",
            et.0
        );
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn lineage_store_retention_deletes_only_old_rows() {
        let pool = migrated_pool().await;
        // Insert an old event/edge (far past) and a recent one
        let old_run = "00000000-0000-0000-0000-000000000097";
        let new_run = "00000000-0000-0000-0000-000000000098";
        for rid in [old_run, new_run] {
            sqlx::query("DELETE FROM lineage_edges WHERE run_id = $1::uuid")
                .bind(rid)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM lineage_events WHERE run_id = $1::uuid")
                .bind(rid)
                .execute(&pool)
                .await
                .unwrap();
        }
        insert_event(
            &pool,
            &sample_event(old_run, "2000-01-01T00:00:00Z", "COMPLETE"),
        )
        .await
        .unwrap();
        insert_event(
            &pool,
            &sample_event(new_run, "2099-01-01T00:00:00Z", "COMPLETE"),
        )
        .await
        .unwrap();

        let purged_events = purge_events(&pool, 30).await.unwrap();
        assert!(purged_events >= 1, "old event purged");
        let purged_edges = purge_edges(&pool, 30).await.unwrap();
        assert!(purged_edges >= 1, "old edge purged");

        let old_left: (i64,) =
            sqlx::query_as("SELECT count(*) FROM lineage_events WHERE run_id = $1::uuid")
                .bind(old_run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(old_left.0, 0);
        let new_left: (i64,) =
            sqlx::query_as("SELECT count(*) FROM lineage_events WHERE run_id = $1::uuid")
                .bind(new_run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(new_left.0, 1, "recent event kept");
    }
}
