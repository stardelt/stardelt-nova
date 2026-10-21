//! What is in the lakehouse, per catalog: schemas, tables, rows, bytes, last change
//! (`GET /api/catalog/metrics`, stardelt-nova#249).
//!
//! Every number comes from the catalog's own Iceberg metadata, never from a statement: a
//! `LoadTable` answer carries the current snapshot's `summary` (`total-records`,
//! `total-files-size`) and `timestamp-ms`, so the Overview page costs one namespace list,
//! one table list per schema and one load per table — no `SELECT count(*)` over anybody's
//! data.
//!
//! Which catalogs appear is the caller's own answer from the engine
//! ([`crate::catalog::allowed_catalogs`]), so one group's catalog never leaks into another
//! group's Overview. The numbers themselves are caller independent — they describe the
//! catalog, not the person — so they are cached per catalog for [`METRICS_TTL`] and
//! filtered per caller afterwards. A catalog whose scan fails is still listed, with zeros
//! and the sentence: one unreachable catalog must not blank the page. Only a failure of the
//! *visibility* question (an unreachable engine) is a 502, as everywhere else in the
//! catalog routes.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Extension, Json,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use futures::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use crate::{
    AppState,
    auth::Principal,
    catalog::{
        NO_CATALOG_SENTENCE, WarehouseError, allowed_catalogs, lakekeeper_authorization,
        path_segment, warehouse_id_for,
    },
    trino::error_status,
};

/// How many tables one catalog scan loads before it stops. A catalog larger than this is
/// counted completely but its sizes and rows are a sample — `truncated` says so, and the
/// page shows "at least". Two hundred loads at [`CONCURRENCY`] are a few seconds against a
/// local catalog; more would turn the Overview into a scan of the warehouse.
pub const MAX_TABLES_PER_CATALOG: usize = 200;

/// How many table metadata reads one scan keeps in flight.
pub const CONCURRENCY: usize = 8;

/// How many catalogs one request scans at once (the rest wait for a slot).
const SCAN_CONCURRENCY: usize = 4;

/// How long one catalog's numbers are trusted. Caller independent on purpose: the numbers
/// are a property of the catalog, the *list* of catalogs is the person's.
const METRICS_TTL: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Pure part: one table's numbers, and how they add up per catalog
// ---------------------------------------------------------------------------

/// One table's numbers, read from its Iceberg metadata document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableStats {
    /// `total-records` of the current snapshot: rows in the table, not rows in a file.
    pub records: u64,
    /// `total-files-size` of the current snapshot: bytes on the object store.
    pub size_bytes: u64,
    /// When the snapshot was committed (Unix ms), else the table's `last-updated-ms`.
    pub last_commit_ms: Option<i64>,
}

/// A snapshot id as a number: from a JSON integer or a string of digits. `None` for a
/// missing, negative or unparsable id — which is also how a table with no data reads
/// (`current-snapshot-id: -1` or absent).
fn snapshot_id(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// The current snapshot of a metadata document: the one `current-snapshot-id` names.
///
/// A table that was never written has no `current-snapshot-id`, or `-1`; both mean "no
/// snapshot", never "snapshot -1". An id that names no snapshot in `snapshots` is the same
/// story told badly: no snapshot to report numbers from.
fn current_snapshot(metadata: &Value) -> Option<&Value> {
    let current = snapshot_id(metadata.get("current-snapshot-id"))?;
    metadata
        .get("snapshots")?
        .as_array()?
        .iter()
        .find(|snapshot| snapshot_id(snapshot.get("snapshot-id")) == Some(current))
}

/// One `summary` counter. Iceberg writes them as strings (`{"total-records": "12"}`), but
/// nothing guarantees it: a number counts, a blank, negative or unparsable value counts
/// nothing — a wrong row count is worse than an honest zero.
fn counter(summary: Option<&Value>) -> u64 {
    match summary {
        Some(Value::String(text)) => text.trim().parse::<u64>().unwrap_or(0),
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// A millisecond timestamp as a JSON number, or as a string of digits; anything else is
/// unknown. Iceberg writes numbers, but Nova has seen string-shaped metadata from tools.
fn millis(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// One table's numbers: the current snapshot's `summary`, and when it was committed.
pub fn table_stats(metadata: &Value) -> TableStats {
    let snapshot = current_snapshot(metadata);
    let (records, size_bytes) = match snapshot.and_then(|snapshot| snapshot.get("summary")) {
        Some(summary) => (
            counter(summary.get("total-records")),
            counter(summary.get("total-files-size")),
        ),
        None => (0, 0),
    };
    let last_commit_ms = snapshot
        .and_then(|snapshot| millis(snapshot.get("timestamp-ms")))
        .or_else(|| millis(metadata.get("last-updated-ms")));
    TableStats {
        records,
        size_bytes,
        last_commit_ms,
    }
}

/// One catalog's numbers, as the endpoint answers them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStats {
    /// Catalog name, as the engine and the catalog spell it.
    pub name: String,
    /// Schemas (namespaces) in the catalog.
    pub schemas: u32,
    /// Tables in the catalog — every one the catalog lists, loaded or not.
    pub tables: u32,
    /// Rows in the loaded tables' current snapshots.
    pub records: u64,
    /// Bytes the loaded tables' current snapshots occupy on the object store.
    pub size_bytes: u64,
    /// The newest commit among the loaded tables (Unix ms); `null` when none has one.
    pub last_commit_ms: Option<i64>,
    /// More tables exist than the endpoint loads (200 per catalog): `tables` is exact,
    /// `records` and `sizeBytes` cover the first 200 only.
    pub truncated: bool,
}

/// Add one table's numbers to a catalog's. Saturating: a catalog whose rows overflow a
/// `u64` is a broken catalog, not a panic in a UI request.
pub fn add(stats: &mut CatalogStats, t: &TableStats) {
    stats.records = stats.records.saturating_add(t.records);
    stats.size_bytes = stats.size_bytes.saturating_add(t.size_bytes);
    stats.last_commit_ms = match (stats.last_commit_ms, t.last_commit_ms) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (left, right) => left.or(right),
    };
}

// ---------------------------------------------------------------------------
// The scan: catalog metadata over the Lakekeeper proxy's own routes
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct Namespaces {
    namespaces: Vec<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct TableIdentifier {
    namespace: Vec<String>,
    name: String,
}

#[derive(serde::Deserialize)]
struct Tables {
    identifiers: Vec<TableIdentifier>,
}

/// The part of a `LoadTable` answer this module reads. `metadata-location` and `config` are
/// not needed — and the metadata document is the catalog's own shape, never reshaped.
#[derive(serde::Deserialize)]
struct LoadTable {
    metadata: Value,
}

/// GET one JSON answer from the catalog. Nova's own service token is the only credential:
/// the catalog knows one identity, Nova's (`catalog_token.rs`).
async fn get_json<T: serde::de::DeserializeOwned>(
    state: &AppState,
    authorization: &Option<String>,
    url: &str,
) -> Result<T, WarehouseError> {
    let mut request = state.http.get(url);
    if let Some(header) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, header);
    }
    let response = request.send().await.map_err(|e| {
        warn!("lakekeeper unreachable while reading catalog metrics: {e}");
        WarehouseError::Unreachable
    })?;
    let status = response.status();
    if !status.is_success() {
        // A table deleted between the listing and the load is the ordinary race; a
        // warehouse whose id no longer exists is the same answer. Neither is a body.
        warn!(url = %url, %status, "lakekeeper answered catalog metrics with a non-2xx status");
        return Err(WarehouseError::Malformed);
    }
    response.json().await.map_err(|e| {
        warn!(url = %url, "lakekeeper catalog metrics answer malformed: {e}");
        WarehouseError::Malformed
    })
}

/// The Iceberg REST namespace path of one (possibly multi-level) namespace: the levels
/// joined by the unit separator, which `path_segment` then percent-encodes.
fn namespace_path(levels: &[String]) -> Option<String> {
    path_segment(&levels.join("\u{1f}"))
}

/// Read one catalog's numbers: namespaces, tables, then up to
/// [`MAX_TABLES_PER_CATALOG`] table metadata documents at [`CONCURRENCY`] in flight.
///
/// A table whose metadata cannot be read is counted in `tables` and adds nothing — one
/// unreadable table must not hide the rest of the catalog. A namespace list or a table
/// list that fails fails the scan: those are the shape of the catalog, not a detail.
pub async fn scan_catalog(state: &AppState, name: &str) -> Result<CatalogStats, WarehouseError> {
    let id = warehouse_id_for(state, name).await?;
    let id = path_segment(&id).ok_or(WarehouseError::Malformed)?;
    let authorization = lakekeeper_authorization(state)
        .await
        .map_err(WarehouseError::Token)?;
    let base = format!("{}/catalog/v1/{id}", state.lakekeeper_url);

    let namespaces: Namespaces =
        get_json(state, &authorization, &format!("{base}/namespaces")).await?;
    let mut stats = CatalogStats {
        name: name.to_string(),
        schemas: u32::try_from(namespaces.namespaces.len()).unwrap_or(u32::MAX),
        ..CatalogStats::default()
    };

    // The table list is what `tables` counts: exact even when the metadata loads stop at
    // the cap.
    let mut tables: Vec<(Vec<String>, String)> = Vec::new();
    for levels in &namespaces.namespaces {
        let Some(ns) = namespace_path(levels) else {
            warn!(catalog = %name, "catalog metrics: unusable namespace name");
            return Err(WarehouseError::Malformed);
        };
        let list: Tables = get_json(
            state,
            &authorization,
            &format!("{base}/namespaces/{ns}/tables"),
        )
        .await?;
        for identifier in list.identifiers {
            // The catalog's own answer names the namespace; fall back to the one listed.
            let levels = if identifier.namespace.is_empty() {
                levels.clone()
            } else {
                identifier.namespace
            };
            tables.push((levels, identifier.name));
        }
    }
    stats.tables = u32::try_from(tables.len()).unwrap_or(u32::MAX);
    stats.truncated = tables.len() > MAX_TABLES_PER_CATALOG;
    tables.truncate(MAX_TABLES_PER_CATALOG);

    let mut loads = stream::iter(tables)
        .map(|(levels, table)| {
            let label = format!("{}.{table}", levels.join("."));
            // References are `Copy`, so every future can carry them; the token string
            // itself is fetched once per scan, not once per table.
            let authorization = &authorization;
            let base = &base;
            async move {
                let (Some(ns), Some(tbl)) = (namespace_path(&levels), path_segment(&table)) else {
                    warn!(catalog = %name, table = %label, "catalog metrics: unusable table name");
                    return None;
                };
                let url = format!("{base}/namespaces/{ns}/tables/{tbl}");
                match get_json::<LoadTable>(state, authorization, &url).await {
                    Ok(load) => Some(table_stats(&load.metadata)),
                    Err(e) => {
                        warn!(catalog = %name, table = %label, error = %e, "catalog metrics: table metadata not readable");
                        None
                    }
                }
            }
        })
        .buffer_unordered(CONCURRENCY);
    while let Some(loaded) = loads.next().await {
        if let Some(t) = loaded {
            add(&mut stats, &t);
        }
    }
    Ok(stats)
}

/// One catalog's numbers, from the cache when they are younger than [`METRICS_TTL`],
/// otherwise scanned (and cached). A failed scan is never cached: the next request tries
/// again instead of serving zeros for a minute.
pub async fn cached_or_scan(state: &AppState, name: &str) -> Result<CatalogStats, WarehouseError> {
    cached_or_scan_within(state, name, METRICS_TTL).await
}

/// [`cached_or_scan`] with an explicit TTL. The tests use it instead of waiting a minute;
/// production always passes [`METRICS_TTL`].
async fn cached_or_scan_within(
    state: &AppState,
    name: &str,
    ttl: Duration,
) -> Result<CatalogStats, WarehouseError> {
    let cached = {
        let cache = state
            .catalog_metrics
            .lock()
            .expect("catalog metrics poisoned");
        cache
            .get(name)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, stats)| stats.clone())
    };
    if let Some(stats) = cached {
        return Ok(stats);
    }
    let stats = scan_catalog(state, name).await?;
    state
        .catalog_metrics
        .lock()
        .expect("catalog metrics poisoned")
        .insert(name.to_string(), (Instant::now(), stats.clone()));
    Ok(stats)
}

// ---------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------

/// One catalog in the answer: the numbers, plus the sentence when the scan failed. A
/// failed catalog is zeros, never omitted — a page that silently drops a catalog says
/// "you have none", which is not what happened.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMetricsView {
    #[serde(flatten)]
    pub stats: CatalogStats,
    /// Why this catalog could not be read — absent for a catalog that was. A sentence,
    /// never URL, token or stack trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<CatalogStats> for CatalogMetricsView {
    fn from(stats: CatalogStats) -> Self {
        Self { stats, error: None }
    }
}

impl CatalogMetricsView {
    fn failed(name: String, error: String) -> Self {
        Self {
            stats: CatalogStats {
                name,
                ..CatalogStats::default()
            },
            error: Some(error),
        }
    }
}

/// The sums over every catalog in the answer. Failed catalogs contribute their zeros.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricsTotals {
    /// How many catalogs are listed.
    pub catalogs: u32,
    /// Schemas across them.
    pub schemas: u64,
    /// Tables across them.
    pub tables: u64,
    /// Rows in the loaded tables' current snapshots.
    pub records: u64,
    /// Bytes the loaded tables' snapshots occupy.
    pub size_bytes: u64,
}

/// `GET /api/catalog/metrics`: what is in each catalog the caller may browse.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricsResponse {
    /// One entry per catalog the caller may browse, sorted by name.
    pub catalogs: Vec<CatalogMetricsView>,
    /// The sums over `catalogs`.
    pub totals: MetricsTotals,
    /// When this answer was built (Unix ms). Per-catalog numbers may be up to a minute
    /// older (they are cached).
    pub generated_at: u64,
}

/// Demo mode has no catalog behind it: the numbers are the seeded in-memory tables. There
/// are no Parquet files, so the size is the JSON size of the rows — a plausible number
/// derived from the same data the query runner answers with, rather than a constant. The
/// last change is the demo lakehouse's own last write (its seed, or the last upload or
/// transfer), so it does not move with the clock.
fn demo_numbers(state: &AppState) -> CatalogStats {
    let demo = state.demo_state.read().expect("demo state poisoned");
    let mut stats = CatalogStats {
        name: state.warehouse_name.clone(),
        ..CatalogStats::default()
    };
    let mut namespaces = BTreeSet::new();
    for table in &demo.tables {
        namespaces.insert(table.namespace.clone());
        stats.tables = stats.tables.saturating_add(1);
        stats.records = stats.records.saturating_add(table.rows.len() as u64);
        let bytes = serde_json::to_string(&table.rows)
            .map(|rows| rows.len() as u64)
            .unwrap_or(0);
        stats.size_bytes = stats.size_bytes.saturating_add(bytes);
    }
    stats.schemas = u32::try_from(namespaces.len()).unwrap_or(u32::MAX);
    stats.last_commit_ms = demo.last_write_ms;
    stats
}

#[utoipa::path(
    get,
    path = "/api/catalog/metrics",
    tag = "public",
    description = "What is in each catalog the caller may browse: schemas, tables, rows, bytes on the object store and the newest commit, plus the sums. The catalog list is the caller's own `SHOW CATALOGS` (`GET /api/catalog/catalogs`); a catalog the caller may not browse is never read and never appears. The numbers come from the catalog's Iceberg metadata (the current snapshot's `total-records` and `total-files-size`), so the endpoint runs no query. They are cached per catalog for 60 s — they describe the catalog, not the person — and at most 200 tables per catalog are loaded: `truncated` says the rows and bytes are a sample while `tables` stays exact. A catalog that cannot be read is listed with zeros and an `error` sentence; only an unreachable engine or catalog fails the request with 502. 403 when no catalog is open to the caller. Never cached by the client.",
    responses(
        (status = 200, description = "One entry per browsable catalog, sorted by name, with the totals", body = MetricsResponse),
        (status = 403, description = "No catalog is open to the caller", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The query engine is not reachable, so the catalogs cannot be named", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn get(State(s): State<Arc<AppState>>, caller: Option<Extension<Principal>>) -> Response {
    let caller = caller.as_ref().map(|c| &c.0);
    let allowed = match allowed_catalogs(&s, caller).await {
        Ok(names) => names,
        Err(e) => {
            return (error_status(&e), Json(serde_json::json!({ "error": e }))).into_response();
        }
    };
    if allowed.is_empty() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": NO_CATALOG_SENTENCE })),
        )
            .into_response();
    }

    let mut catalogs: Vec<CatalogMetricsView> = Vec::with_capacity(allowed.len());
    if s.demo {
        catalogs.push(demo_numbers(&s).into());
    } else {
        let scans: Vec<(String, Result<CatalogStats, WarehouseError>)> = stream::iter(allowed)
            .map(|name| {
                let state = Arc::clone(&s);
                async move {
                    let stats = cached_or_scan(&state, &name).await;
                    (name, stats)
                }
            })
            .buffer_unordered(SCAN_CONCURRENCY)
            .collect()
            .await;
        for (name, stats) in scans {
            match stats {
                Ok(stats) => catalogs.push(stats.into()),
                Err(e) => {
                    warn!(catalog = %name, error = %e, "catalog metrics: scan failed");
                    catalogs.push(CatalogMetricsView::failed(name, e.to_string()));
                }
            }
        }
        // `allowed` is sorted already, but the scans finish in their own order.
        catalogs.sort_by(|a, b| a.stats.name.cmp(&b.stats.name));
    }

    let mut totals = MetricsTotals {
        catalogs: u32::try_from(catalogs.len()).unwrap_or(u32::MAX),
        ..MetricsTotals::default()
    };
    for catalog in &catalogs {
        totals.schemas = totals.schemas.saturating_add(catalog.stats.schemas as u64);
        totals.tables = totals.tables.saturating_add(catalog.stats.tables as u64);
        totals.records = totals.records.saturating_add(catalog.stats.records);
        totals.size_bytes = totals.size_bytes.saturating_add(catalog.stats.size_bytes);
    }

    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(MetricsResponse {
            catalogs,
            totals,
            generated_at: crate::jobs::now_ms(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::{
        Router,
        extract::{Path, State},
        http::HeaderMap,
        routing::{get as get_route, post},
    };
    use serde_json::json;

    use super::*;
    use crate::auth::Identity;

    // -----------------------------------------------------------------------
    // Fixtures: a fake Lakekeeper and a fake Trino, as `catalog.rs` builds them
    // -----------------------------------------------------------------------

    fn snapshot(id: i64, records: &str, size: &str, at: i64) -> Value {
        json!({
            "snapshot-id": id,
            "timestamp-ms": at,
            "summary": {
                "operation": "append",
                "total-records": records,
                "total-files-size": size,
                "total-data-files": "1",
            }
        })
    }

    fn metadata_with(snapshots: Value, current: Value, updated: Option<i64>) -> Value {
        let mut metadata = json!({
            "format-version": 2,
            "current-snapshot-id": current,
            "snapshots": snapshots,
        });
        if let Some(updated) = updated {
            metadata["last-updated-ms"] = json!(updated);
        }
        metadata
    }

    /// The body one `LoadTable` answers with: the given metadata document.
    fn load_body(metadata: Value) -> Value {
        json!({
            "metadata-location": "s3://lakehouse/x/metadata/00001.metadata.json",
            "metadata": metadata,
            "config": {},
        })
    }

    // -----------------------------------------------------------------------
    // The pure functions
    // -----------------------------------------------------------------------

    #[test]
    fn table_stats_reads_the_current_snapshot() {
        // Two snapshots: only the current one counts, whatever the older one said.
        let metadata = metadata_with(
            json!([
                snapshot(1, "999", "999999", 1_700_000_000_000),
                snapshot(2, "12", "4096", 1_700_000_100_000),
            ]),
            json!(2),
            Some(1_700_000_050_000),
        );
        assert_eq!(
            table_stats(&metadata),
            TableStats {
                records: 12,
                size_bytes: 4096,
                last_commit_ms: Some(1_700_000_100_000),
            }
        );
        // Numeric counters count as well as strings: Iceberg writes strings, tools differ.
        let metadata = metadata_with(
            json!([{
                "snapshot-id": 7,
                "timestamp-ms": 5,
                "summary": { "total-records": 3, "total-files-size": 8 },
            }]),
            json!(7),
            None,
        );
        assert_eq!(table_stats(&metadata).records, 3);
        assert_eq!(table_stats(&metadata).size_bytes, 8);
        // A string-shaped id (metadata from a tool, not from Iceberg) still finds its
        // snapshot instead of reporting a table with data as empty.
        let metadata = metadata_with(json!([snapshot(7, "4", "16", 5)]), json!("7"), None);
        assert_eq!(table_stats(&metadata).records, 4);
    }

    #[test]
    fn table_stats_without_snapshot_is_zero() {
        // `-1` is how Iceberg says "never written", an absent key says the same, and a
        // current id that names no snapshot is the same story told badly.
        for metadata in [
            metadata_with(json!([]), json!(-1), Some(1_700_000_000_000)),
            json!({ "format-version": 2, "last-updated-ms": 1_700_000_000_000_i64 }),
            metadata_with(json!([snapshot(1, "12", "4096", 1)]), Value::Null, None),
            metadata_with(json!([snapshot(1, "12", "4096", 1)]), json!(2), None),
        ] {
            let stats = table_stats(&metadata);
            assert_eq!(stats.records, 0, "no snapshot, no rows: {metadata}");
            assert_eq!(stats.size_bytes, 0, "no snapshot, no bytes: {metadata}");
        }
        // The table's own timestamp survives: a created-but-empty table has a history.
        let stats = table_stats(&metadata_with(
            json!([]),
            json!(-1),
            Some(1_700_000_000_000),
        ));
        assert_eq!(stats.last_commit_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn table_stats_survives_missing_and_garbage_summary() {
        let cases = [
            json!({ "snapshot-id": 1, "timestamp-ms": 9 }),
            json!({ "snapshot-id": 1, "timestamp-ms": 9, "summary": {} }),
            json!({
                "snapshot-id": 1,
                "timestamp-ms": 9,
                "summary": { "total-records": "not a number", "total-files-size": "-5" },
            }),
            json!({
                "snapshot-id": 1,
                "timestamp-ms": 9,
                "summary": { "total-records": "", "total-files-size": "  " },
            }),
            json!({
                "snapshot-id": 1,
                "timestamp-ms": 9,
                "summary": { "total-records": null, "total-files-size": -8 },
            }),
        ];
        for snapshot in cases {
            let metadata = metadata_with(json!([snapshot.clone()]), json!(1), None);
            let stats = table_stats(&metadata);
            assert_eq!(stats.records, 0, "garbage is zero: {snapshot}");
            assert_eq!(stats.size_bytes, 0, "garbage is zero: {snapshot}");
            // The commit time is still read: it is not part of the summary.
            assert_eq!(stats.last_commit_ms, Some(9), "commit time: {snapshot}");
        }
    }

    #[test]
    fn last_commit_prefers_snapshot_time_then_last_updated() {
        let metadata = metadata_with(
            json!([snapshot(1, "1", "1", 1_700_000_100_000)]),
            json!(1),
            Some(1_700_000_000_000),
        );
        assert_eq!(
            table_stats(&metadata).last_commit_ms,
            Some(1_700_000_100_000),
            "the snapshot's own time wins"
        );

        let metadata = metadata_with(json!([]), json!(-1), Some(1_700_000_000_000));
        assert_eq!(
            table_stats(&metadata).last_commit_ms,
            Some(1_700_000_000_000),
            "without a snapshot the table's last update is the change"
        );

        let metadata = json!({ "format-version": 2 });
        assert_eq!(
            table_stats(&metadata).last_commit_ms,
            None,
            "neither: unknown, never 1970"
        );
    }

    #[test]
    fn add_sums() {
        let mut stats = CatalogStats {
            name: "warehouse".into(),
            ..CatalogStats::default()
        };
        add(
            &mut stats,
            &TableStats {
                records: 10,
                size_bytes: 100,
                last_commit_ms: Some(5),
            },
        );
        add(
            &mut stats,
            &TableStats {
                records: 7,
                size_bytes: 50,
                last_commit_ms: Some(9),
            },
        );
        add(
            &mut stats,
            &TableStats {
                records: 3,
                size_bytes: 25,
                last_commit_ms: None,
            },
        );
        assert_eq!(stats.records, 20);
        assert_eq!(stats.size_bytes, 175);
        assert_eq!(stats.last_commit_ms, Some(9), "the newest commit wins");

        // An unknown time on the left is filled by the right; both unknown stays unknown.
        let mut stats = CatalogStats::default();
        add(
            &mut stats,
            &TableStats {
                records: 1,
                size_bytes: 2,
                last_commit_ms: None,
            },
        );
        assert_eq!(stats.last_commit_ms, None);
        add(
            &mut stats,
            &TableStats {
                records: 0,
                size_bytes: 0,
                last_commit_ms: Some(4),
            },
        );
        assert_eq!(stats.last_commit_ms, Some(4));
        assert_eq!(stats.tables, 0, "add() never counts tables or schemas");
    }

    // -----------------------------------------------------------------------
    // The scan and the cache
    // -----------------------------------------------------------------------

    #[derive(Default)]
    struct Lakekeeper {
        /// `(name, id)`, as the management API lists them.
        warehouses: Vec<(String, String)>,
        /// Namespaces per warehouse id.
        namespaces: HashMap<String, Vec<String>>,
        /// Table names per `(warehouse id, namespace)`.
        tables: HashMap<(String, String), Vec<String>>,
        /// Load answers per `(warehouse id, namespace, table)`; unset answers the default.
        loads: HashMap<(String, String, String), Value>,
        /// Warehouse ids whose namespace list answers 500.
        broken: Vec<String>,
        namespace_requests: usize,
        load_requests: usize,
    }

    #[derive(Clone, Default)]
    struct FakeLakekeeper(Arc<Mutex<Lakekeeper>>);

    impl FakeLakekeeper {
        fn new() -> Self {
            Self::default()
        }

        fn warehouse(&self, name: &str, id: &str) {
            self.0
                .lock()
                .unwrap()
                .warehouses
                .push((name.into(), id.into()));
        }

        fn namespaces(&self, id: &str, namespaces: &[&str]) {
            self.0.lock().unwrap().namespaces.insert(
                id.into(),
                namespaces.iter().map(|n| n.to_string()).collect(),
            );
        }

        fn tables(&self, id: &str, ns: &str, names: &[String]) {
            self.0
                .lock()
                .unwrap()
                .tables
                .insert((id.into(), ns.into()), names.to_vec());
        }

        fn load(&self, id: &str, ns: &str, table: &str, metadata: Value) {
            self.0
                .lock()
                .unwrap()
                .loads
                .insert((id.into(), ns.into(), table.into()), load_body(metadata));
        }

        fn break_namespaces(&self, id: &str) {
            self.0.lock().unwrap().broken.push(id.into());
        }

        fn load_requests(&self) -> usize {
            self.0.lock().unwrap().load_requests
        }

        async fn serve(&self) -> String {
            let app = Router::new()
                .route("/management/v1/warehouse", get_route(warehouses))
                .route("/catalog/v1/{wh}/namespaces", get_route(namespaces_route))
                .route(
                    "/catalog/v1/{wh}/namespaces/{ns}/tables",
                    get_route(tables_route),
                )
                .route(
                    "/catalog/v1/{wh}/namespaces/{ns}/tables/{tbl}",
                    get_route(load_route),
                )
                .with_state(self.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{addr}")
        }
    }

    async fn warehouses(State(fake): State<FakeLakekeeper>) -> Json<Value> {
        let lakekeeper = fake.0.lock().unwrap();
        let list: Vec<Value> = lakekeeper
            .warehouses
            .iter()
            .map(|(name, id)| json!({ "id": id, "name": name }))
            .collect();
        Json(json!({ "warehouses": list }))
    }

    async fn namespaces_route(
        State(fake): State<FakeLakekeeper>,
        Path(wh): Path<String>,
    ) -> Response {
        let mut lakekeeper = fake.0.lock().unwrap();
        lakekeeper.namespace_requests += 1;
        if lakekeeper.broken.contains(&wh) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": { "message": "boom" } })),
            )
                .into_response();
        }
        let namespaces = lakekeeper.namespaces.get(&wh).cloned().unwrap_or_default();
        Json(json!({
            "namespaces": namespaces.into_iter().map(|ns| vec![ns]).collect::<Vec<_>>()
        }))
        .into_response()
    }

    async fn tables_route(
        State(fake): State<FakeLakekeeper>,
        Path((wh, ns)): Path<(String, String)>,
    ) -> Response {
        let lakekeeper = fake.0.lock().unwrap();
        let names = lakekeeper
            .tables
            .get(&(wh.clone(), ns.clone()))
            .cloned()
            .unwrap_or_default();
        Json(json!({
            "identifiers": names
                .into_iter()
                .map(|name| json!({ "namespace": [ns.clone()], "name": name }))
                .collect::<Vec<_>>()
        }))
        .into_response()
    }

    /// The default load answer: ten rows in 100 bytes, committed at a fixed time. The
    /// 200-table test relies on it, so it is deliberately small and constant.
    async fn load_route(
        State(fake): State<FakeLakekeeper>,
        Path((wh, ns, tbl)): Path<(String, String, String)>,
    ) -> Response {
        let mut lakekeeper = fake.0.lock().unwrap();
        lakekeeper.load_requests += 1;
        match lakekeeper.loads.get(&(wh, ns, tbl)) {
            Some(body) => Json(body.clone()).into_response(),
            None => Json(load_body(metadata_with(
                json!([snapshot(1, "10", "100", 1_700_000_000_000)]),
                json!(1),
                None,
            )))
            .into_response(),
        }
    }

    fn names(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("t{i:03}")).collect()
    }

    #[tokio::test]
    async fn scan_truncates_at_200_but_counts_all() {
        let fake = FakeLakekeeper::new();
        fake.warehouse("warehouse", "wid-warehouse");
        fake.namespaces("wid-warehouse", &["s1"]);
        fake.tables("wid-warehouse", "s1", &names(250));
        let url = fake.serve().await;
        let state = crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", &url);

        let stats = scan_catalog(&state, "warehouse").await.expect("the scan");
        assert_eq!(stats.schemas, 1);
        assert_eq!(stats.tables, 250, "every table is counted");
        assert!(stats.truncated, "250 tables are more than the cap");
        assert_eq!(
            fake.load_requests(),
            MAX_TABLES_PER_CATALOG,
            "the metadata loads stop at the cap"
        );
        assert_eq!(
            stats.records,
            MAX_TABLES_PER_CATALOG as u64 * 10,
            "records cover the loaded tables only"
        );
        assert_eq!(stats.last_commit_ms, Some(1_700_000_000_000));
        assert_eq!(stats.name, "warehouse");
    }

    #[tokio::test]
    async fn cache_serves_within_60s_and_refreshes_after() {
        let fake = FakeLakekeeper::new();
        fake.warehouse("warehouse", "wid-warehouse");
        fake.namespaces("wid-warehouse", &["demo"]);
        fake.tables("wid-warehouse", "demo", &["taxi".to_string()]);
        fake.load(
            "wid-warehouse",
            "demo",
            "taxi",
            metadata_with(
                json!([snapshot(3, "42", "4096", 1_700_000_000_000)]),
                json!(3),
                None,
            ),
        );
        let url = fake.serve().await;
        let state = crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", &url);

        let first = cached_or_scan_within(&state, "warehouse", METRICS_TTL)
            .await
            .expect("the scan");
        assert_eq!(first.records, 42);
        assert_eq!(fake.load_requests(), 1);

        // Within the minute the cached numbers are served; nobody is asked again.
        let second = cached_or_scan_within(&state, "warehouse", METRICS_TTL)
            .await
            .expect("the cached answer");
        assert_eq!(second, first);
        assert_eq!(
            fake.load_requests(),
            1,
            "a second call within 60 s must not read the catalog again"
        );

        // After it — a zero TTL is "already expired", so the test does not wait a minute —
        // the catalog is read again and the cache refreshed.
        let third = cached_or_scan_within(&state, "warehouse", Duration::ZERO)
            .await
            .expect("the refresh");
        assert_eq!(third, first);
        assert_eq!(
            fake.load_requests(),
            2,
            "an entry older than the TTL is read again"
        );
    }

    // -----------------------------------------------------------------------
    // The handler
    // -----------------------------------------------------------------------

    /// A fake Trino answering `SHOW CATALOGS` per person, as the permission bridge does:
    /// `X-Trino-User` is the display name (`trino::upstream_headers`).
    #[derive(Clone, Default)]
    struct FakeTrino {
        by_user: HashMap<String, Vec<String>>,
    }

    async fn show_catalogs(State(fake): State<FakeTrino>, headers: HeaderMap) -> Json<Value> {
        let user = headers
            .get("x-trino-user")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let rows: Vec<Value> = fake
            .by_user
            .get(user)
            .map(|names| names.iter().map(|name| json!([name])).collect())
            .unwrap_or_default();
        Json(json!({
            "id": "query_1",
            "columns": [{ "name": "Catalog", "type": "varchar" }],
            "data": rows,
            "stats": { "state": "FINISHED" }
        }))
    }

    async fn fake_trino(by_user: &[(&str, &[&str])]) -> String {
        let fake = FakeTrino {
            by_user: by_user
                .iter()
                .map(|(user, names)| {
                    (
                        user.to_string(),
                        names.iter().map(|name| name.to_string()).collect(),
                    )
                })
                .collect(),
        };
        let app = Router::new()
            .route("/v1/statement", post(show_catalogs))
            .with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn person(name: &str) -> Principal {
        Principal {
            identity: Identity {
                sub: name.into(),
                display_name: name.into(),
                email: None,
                name: None,
                groups: Vec::new(),
                roles: Vec::new(),
                groups_overage: false,
                is_admin: false,
            },
            via: "session",
            access_token: None,
        }
    }

    async fn body_of(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn handler_lists_only_allowed_catalogs() {
        let trino = fake_trino(&[("ada", &["warehouse", "finance", "system", "tpch"])]).await;
        let fake = FakeLakekeeper::new();
        fake.warehouse("warehouse", "wid-warehouse");
        fake.warehouse("finance", "wid-finance");
        fake.warehouse("product", "wid-product");
        fake.namespaces("wid-warehouse", &["demo"]);
        fake.tables("wid-warehouse", "demo", &["taxi".to_string()]);
        fake.load(
            "wid-warehouse",
            "demo",
            "taxi",
            metadata_with(
                json!([snapshot(1, "12", "4096", 1_700_000_100_000)]),
                json!(1),
                None,
            ),
        );
        fake.namespaces("wid-finance", &["seed"]);
        fake.tables("wid-finance", "seed", &["nation".to_string()]);
        fake.load(
            "wid-finance",
            "seed",
            "nation",
            metadata_with(
                json!([snapshot(2, "25", "1024", 1_700_000_200_000)]),
                json!(2),
                None,
            ),
        );
        let lakekeeper = fake.serve().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = body_of(response).await;
        assert_eq!(
            body["catalogs"],
            json!([
                {
                    "name": "finance",
                    "schemas": 1,
                    "tables": 1,
                    "records": 25,
                    "sizeBytes": 1024,
                    "lastCommitMs": 1_700_000_200_000_i64,
                    "truncated": false,
                },
                {
                    "name": "warehouse",
                    "schemas": 1,
                    "tables": 1,
                    "records": 12,
                    "sizeBytes": 4096,
                    "lastCommitMs": 1_700_000_100_000_i64,
                    "truncated": false,
                },
            ]),
            "product is not hers, the engine's own catalogs are never listed"
        );
        assert_eq!(
            body["totals"],
            json!({
                "catalogs": 2,
                "schemas": 2,
                "tables": 2,
                "records": 37,
                "sizeBytes": 5120,
            })
        );
        assert!(body["generatedAt"].as_u64().is_some_and(|ms| ms > 0));
    }

    #[tokio::test]
    async fn handler_403_when_nothing_allowed() {
        // Only the engine's own catalogs: nothing for Nova to offer.
        let trino = fake_trino(&[("nobody", &["system", "tpch"])]).await;
        let lakekeeper = FakeLakekeeper::new().serve().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        let response = get(State(Arc::clone(&state)), Some(Extension(person("nobody")))).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = body_of(response).await;
        assert_eq!(body["error"], NO_CATALOG_SENTENCE);
    }

    #[tokio::test]
    async fn failed_scan_is_listed_with_error_not_500() {
        let trino = fake_trino(&[("ada", &["warehouse", "finance"])]).await;
        let fake = FakeLakekeeper::new();
        fake.warehouse("warehouse", "wid-warehouse");
        fake.warehouse("finance", "wid-finance");
        fake.namespaces("wid-warehouse", &["demo"]);
        fake.tables("wid-warehouse", "demo", &["taxi".to_string()]);
        fake.load(
            "wid-warehouse",
            "demo",
            "taxi",
            metadata_with(
                json!([snapshot(1, "5", "50", 1_700_000_000_000)]),
                json!(1),
                None,
            ),
        );
        // The finance warehouse answers 500 on its namespace list.
        fake.break_namespaces("wid-finance");
        let lakekeeper = fake.serve().await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "one unreadable catalog must not blank the page"
        );
        let body = body_of(response).await;
        assert_eq!(
            body["catalogs"][0]["name"], "finance",
            "listed first by name, never omitted"
        );
        assert_eq!(body["catalogs"][0]["tables"], 0);
        assert_eq!(body["catalogs"][0]["records"], 0);
        assert_eq!(
            body["catalogs"][0]["error"], "the catalog returned an unexpected response",
            "a sentence, never a stack trace"
        );
        assert_eq!(body["catalogs"][1]["name"], "warehouse");
        assert_eq!(body["catalogs"][1]["tables"], 1);
        assert_eq!(body["catalogs"][1]["records"], 5);
        assert_eq!(
            body["catalogs"][1].get("error"),
            None,
            "a catalog that was read carries no error"
        );
    }

    #[tokio::test]
    async fn demo_mode_numbers() {
        let state = crate::router_tests::test_state();
        *state.demo_state.write().expect("demo state") = crate::demo::DemoState::seed();

        let response = get(State(Arc::clone(&state)), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_of(response).await;
        assert_eq!(
            body["catalogs"][0]["name"], "warehouse",
            "demo mode has the one configured catalog"
        );
        assert_eq!(body["catalogs"][0]["schemas"], 2);
        assert_eq!(body["catalogs"][0]["tables"], 3);
        assert_eq!(
            body["catalogs"][0]["records"], 20,
            "the seeded rows: 12 + 5 + 3"
        );
        assert!(
            body["catalogs"][0]["sizeBytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0),
            "demo mode derives a size from the seeded rows"
        );
        assert_eq!(body["catalogs"][0]["truncated"], false);
        assert!(
            body["catalogs"][0]["lastCommitMs"].as_i64().is_some(),
            "the demo seed is a change"
        );
        assert_eq!(body["totals"]["catalogs"], 1);
        assert_eq!(body["totals"]["schemas"], 2);
        assert_eq!(body["totals"]["tables"], 3);
        assert_eq!(body["totals"]["records"], 20);
        assert_eq!(
            body["totals"]["sizeBytes"], body["catalogs"][0]["sizeBytes"],
            "one catalog: the totals are its numbers"
        );
    }
}
