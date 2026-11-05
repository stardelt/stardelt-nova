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
