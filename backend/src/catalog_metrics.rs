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
//! catalog, not the person — so they live in one cache keyed by catalog, filled by a
//! background refresher ([`spawn_refresher`], stardelt-nova#263) that reads every catalog
//! every five minutes with Nova's own service token. A request never scans: it answers
//! from the last numbers at once, and a catalog with no numbers yet is answered as
//! `pending` while one background scan reads it (single-flight). A catalog whose scan
//! failed keeps its last good numbers and says why; one that never answered is listed
//! with zeros and the sentence: one unreachable catalog must not blank the page. Only a
//! failure of the *visibility* question (an unreachable engine) is a 502, as everywhere
//! else in the catalog routes.

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
use tracing::{info, warn};

use crate::{
    AppState,
    auth::Principal,
    catalog::{
        HIDDEN_CATALOGS, NO_CATALOG_SENTENCE, WarehouseError, allowed_catalogs,
        lakekeeper_authorization, path_segment, warehouse_id_for, warehouse_list,
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

/// How many catalogs one refresher round scans at once. Two, not four: the round runs for
/// minutes in the background and must never look like load on the catalog.
const REFRESH_CONCURRENCY: usize = 2;

/// How long the refresher waits after start-up before its first round: long enough for the
/// listener, the catalog client and the first page load to settle.
const REFRESH_FIRST_DELAY: Duration = Duration::from_secs(10);

/// The environment variable that overrides [`DEFAULT_REFRESH`].
pub const REFRESH_ENV: &str = "NOVA_METRICS_REFRESH_SECS";

/// How often the background refresher reads every catalog.
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(300);

/// The bounds of the override: a round every few seconds would keep the catalog busy for
/// no fresher a page, and anything above an hour is a different product decision.
pub const MIN_REFRESH: Duration = Duration::from_secs(30);
pub const MAX_REFRESH: Duration = Duration::from_secs(3600);

/// At most one administrator-triggered round per minute: the button must not become a way
/// to scan the warehouse on demand.
pub const REFRESH_LIMIT: Duration = Duration::from_secs(60);

/// The sentence a refused refresh answers with.
pub const REFRESH_TOO_SOON: &str = "the numbers were refreshed less than a minute ago";

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
    /// `bronze`, `silver` or `gold` when the catalog is a medallion layer (see
    /// `crate::catalog::catalog_layers`); absent for every other catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
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

// ---------------------------------------------------------------------------
// The cache: one entry per catalog, filled in the background (stardelt-nova#263)
// ---------------------------------------------------------------------------

/// One catalog's numbers in the cache, and when they were read.
///
/// `updated` is the monotonic clock (ages, staleness); `updated_ms` is the same moment as
/// Unix milliseconds, which is what the answer carries as `updatedAt` — a person reads a
/// time, not an uptime. `error` survives a failed round on purpose: the numbers of the
/// last good scan stay, and the row says why they are old.
#[derive(Debug, Clone)]
pub struct MetricsEntry {
    /// The catalog's numbers, from the last successful scan.
    pub stats: CatalogStats,
    /// When those numbers were read, on the monotonic clock.
    pub updated: Instant,
    /// The same moment in Unix milliseconds: what `updatedAt` answers.
    pub updated_ms: i64,
    /// Why the last scan failed; `None` after a good one. A sentence, never a URL or token.
    pub error: Option<String>,
}

/// The refresh period from [`REFRESH_ENV`], clamped to [`MIN_REFRESH`]–[`MAX_REFRESH`].
/// A value that is not a number is the default: a typo must not stop the refresher.
pub fn refresh_interval_from_env() -> Duration {
    interval_from(std::env::var(REFRESH_ENV).ok().as_deref())
}

/// [`refresh_interval_from_env`] for an explicit value: the tests use this, production
/// always reads the environment.
fn interval_from(raw: Option<&str>) -> Duration {
    let secs = raw
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_REFRESH);
    secs.clamp(MIN_REFRESH, MAX_REFRESH)
}

/// Whether this state gets a background refresher at all: never in demo mode (the numbers
/// are the seeded tables) and never without a catalog to ask.
pub fn refresher_wanted(state: &AppState) -> bool {
    !state.demo && !state.lakekeeper_url.trim().is_empty()
}

/// Start the background refresher: after [`REFRESH_FIRST_DELAY`], one round per
/// [`refresh_interval_from_env`] for as long as the process runs.
///
/// Answers whether a task was started: demo mode and an unconfigured catalog have none, and
/// a request then starts one scan per unknown catalog itself, at the first sight of it.
/// Called from `main` after the state exists, once.
pub fn spawn_refresher(state: Arc<AppState>) -> bool {
    if !refresher_wanted(&state) {
        info!(
            demo = state.demo,
            "catalog metrics refresher off — the endpoint answers from what a request scans"
        );
        return false;
    }
    let interval = refresh_interval_from_env();
    info!(
        interval_secs = interval.as_secs(),
        first_delay_secs = REFRESH_FIRST_DELAY.as_secs(),
        "catalog metrics refresher started"
    );
    tokio::spawn(async move {
        refresher_loop(state, REFRESH_FIRST_DELAY, interval).await;
    });
    true
}

/// Wait, round, wait: the loop itself, with both waits injected so a test can drive it on
/// a paused clock instead of sleeping.
async fn refresher_loop(state: Arc<AppState>, first_delay: Duration, interval: Duration) {
    tokio::time::sleep(first_delay).await;
    loop {
        run_round(&state).await;
        tokio::time::sleep(interval).await;
    }
}

/// One full round: every catalog the warehouse list names (minus the engine's own), two
/// scans at a time, one entry each. Answers `false` when another round holds the lock —
/// the loop and an administrator's Refresh never scan the warehouse at the same time.
async fn run_round(state: &Arc<AppState>) -> bool {
    if state.demo {
        return false;
    }
    // One round at a time. `try_lock` rather than `lock`: a refresh that is already
    // running covers this one, and waiting would only queue another full scan.
    let Ok(_round) = state.metrics_round.try_lock() else {
        info!("catalog metrics: a refresh is already running — this round was skipped");
        return false;
    };
    let started = Instant::now();

    let warehouses = match warehouse_list(state).await {
        Ok(warehouses) => warehouses,
        Err(e) => {
            // The list is the shape of the warehouse, not one catalog's numbers: keep every
            // entry as it is and try again next round.
            warn!(error = %e, "catalog metrics: the warehouse list failed — keeping the last numbers");
            return false;
        }
    };
    // The round just learned every id: the scans need not ask again.
    {
        let mut ids = state.catalog_ids.write().expect("catalog ids poisoned");
        for warehouse in &warehouses {
            ids.insert(warehouse.name.clone(), warehouse.id.clone());
        }
    }
    let names: Vec<String> = warehouses
        .into_iter()
        .map(|warehouse| warehouse.name)
        .filter(|name| !HIDDEN_CATALOGS.contains(&name.as_str()))
        .collect();

    let results: Vec<(String, Option<Result<CatalogStats, WarehouseError>>)> = stream::iter(names)
        .map(|name| {
            let state = Arc::clone(state);
            async move {
                let result = scan_and_store(&state, &name).await;
                (name, result)
            }
        })
        .buffer_unordered(REFRESH_CONCURRENCY)
        .collect()
        .await;

    let mut catalogs = 0usize;
    let mut failed = 0usize;
    let mut tables = 0u64;
    for (name, result) in results {
        match result {
            // `None`: another scan held the catalog, and that one stores the answer.
            None => continue,
            Some(Ok(stats)) => {
                catalogs += 1;
                tables = tables.saturating_add(u64::from(stats.tables));
            }
            Some(Err(e)) => {
                failed += 1;
                warn!(catalog = %name, error = %e, "catalog metrics: refresh failed, the last numbers stay");
            }
        }
    }
    // One line per round, counts only: never a table name, never a token.
    info!(
        catalogs,
        failed,
        tables,
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "catalog metrics refreshed"
    );
    true
}

/// Scan one catalog unless another scan already holds it, and store the entry.
/// `None` means "somebody else is reading it" — not an error.
async fn scan_and_store(
    state: &Arc<AppState>,
    name: &str,
) -> Option<Result<CatalogStats, WarehouseError>> {
    if !claim(state, name) {
        return None;
    }
    let result = scan_catalog(state, name).await;
    store(state, name, &result);
    release(state, name);
    Some(result)
}

/// Start one background scan for `name` unless one is running. The caller never waits:
/// the row answers as `pending` and the numbers appear with the next request.
fn start_background_scan(state: &Arc<AppState>, name: &str) {
    if !claim(state, name) {
        return;
    }
    let state = Arc::clone(state);
    let name = name.to_string();
    tokio::spawn(async move {
        let result = scan_catalog(&state, &name).await;
        store(&state, &name, &result);
        release(&state, &name);
    });
}

/// Take the one slot a catalog has for a scan; `false` when it is taken already.
fn claim(state: &AppState, name: &str) -> bool {
    state
        .metrics_in_flight
        .lock()
        .expect("catalog metrics in flight poisoned")
        .insert(name.to_string())
}

/// Give the slot back, whatever the scan did.
fn release(state: &AppState, name: &str) {
    state
        .metrics_in_flight
        .lock()
        .expect("catalog metrics in flight poisoned")
        .remove(name);
}

/// Put one scan's result in the cache.
///
/// A good scan replaces the numbers and clears the error. A failed one keeps the numbers
/// and their timestamp — they are the last good ones, and the page says how old — and
/// only adds the sentence; a catalog that never answered gets zeros with the sentence, so
/// it is listed (a page that drops it says "you have no such catalog").
fn store(state: &AppState, name: &str, result: &Result<CatalogStats, WarehouseError>) {
    let mut cache = state
        .catalog_metrics
        .lock()
        .expect("catalog metrics poisoned");
    let now = i64::try_from(crate::jobs::now_ms()).unwrap_or(i64::MAX);
    match result {
        Ok(stats) => {
            cache.insert(
                name.to_string(),
                MetricsEntry {
                    stats: stats.clone(),
                    updated: Instant::now(),
                    updated_ms: now,
                    error: None,
                },
            );
        }
        Err(e) => match cache.get_mut(name) {
            Some(entry) => entry.error = Some(e.to_string()),
            None => {
                cache.insert(
                    name.to_string(),
                    MetricsEntry {
                        stats: CatalogStats {
                            name: name.to_string(),
                            ..CatalogStats::default()
                        },
                        updated: Instant::now(),
                        updated_ms: now,
                        error: Some(e.to_string()),
                    },
                );
            }
        },
    }
}

// ---------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------

/// One catalog in the answer: the numbers, plus the sentence when the last scan failed. A
/// failed catalog is zeros only when it never answered — a catalog that answered once
/// keeps those numbers — never omitted: a page that silently drops a catalog says "you
/// have none", which is not what happened.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMetricsView {
    #[serde(flatten)]
    pub stats: CatalogStats,
    /// Why this catalog could not be read — absent for a catalog that was. A sentence,
    /// never URL, token or stack trace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When these numbers were read (Unix ms), so the page can say "Updated 4 minutes
    /// ago". `null` while `pending`: there are no numbers yet.
    pub updated_at: Option<i64>,
    /// No numbers yet — the catalog appeared after the last round (or before the first),
    /// and one scan is reading it now. Its numbers are zeros and the totals leave it out.
    pub pending: bool,
}

impl CatalogMetricsView {
    /// A catalog whose numbers were read: nothing pending, nothing failed.
    fn read(stats: CatalogStats, updated_ms: i64, error: Option<String>) -> Self {
        Self {
            stats,
            error,
            updated_at: Some(updated_ms),
            pending: false,
        }
    }

    /// A catalog with no numbers yet: listed, zeros, and the scan is on its way.
    fn pending(name: String) -> Self {
        Self {
            stats: CatalogStats {
                name,
                ..CatalogStats::default()
            },
            error: None,
            updated_at: None,
            pending: true,
        }
    }
}

impl From<CatalogStats> for CatalogMetricsView {
    fn from(stats: CatalogStats) -> Self {
        Self {
            stats,
            error: None,
            updated_at: None,
            pending: false,
        }
    }
}

/// The sums over every catalog in the answer. Pending catalogs contribute nothing: their
/// zeros are not numbers, and a tile must not read "0 tables" while the scan runs.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricsTotals {
    /// How many catalogs with numbers are listed.
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
    /// One entry per catalog the caller may browse: bronze, silver, gold first, then the
    /// rest by name.
    pub catalogs: Vec<CatalogMetricsView>,
    /// The sums over `catalogs`, leaving out the `pending` ones.
    pub totals: MetricsTotals,
    /// When this answer was built (Unix ms). The numbers themselves are from the last
    /// background round, which `updatedAt` on each row names.
    pub generated_at: u64,
}

/// The answer to an accepted `POST /api/catalog/metrics/refresh`: the round runs in the
/// background, and the page reads the new numbers with its next request.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefreshAccepted {
    /// Always `true`: a `202` is the acceptance, and the round has already started.
    pub accepted: bool,
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
    operation_id = "catalog_metrics_get",
    path = "/api/catalog/metrics",
    tag = "public",
    description = "What is in each catalog the caller may browse: schemas, tables, rows, bytes on the object store and the newest commit, plus the sums. Rows are ordered bronze, silver, gold first, then by name, and carry `layer` when the catalog is one of the three (the `stardelt.io/layer` label, else the catalog's own name). The catalog list is the caller's own `SHOW CATALOGS` (`GET /api/catalog/catalogs`); a catalog the caller may not browse is never read and never appears. The numbers come from the catalog's Iceberg metadata (the current snapshot's `total-records` and `total-files-size`), so the endpoint runs no query, and they are read in the background every five minutes (`NOVA_METRICS_REFRESH_SECS`): a request answers from the last numbers at once, whatever their age, and `updatedAt` says when they were read. A catalog with no numbers yet is answered with zeros and `pending: true` while one background scan reads it; `totals` leave pending rows out. At most 200 tables per catalog are loaded: `truncated` says the rows and bytes are a sample while `tables` stays exact. A catalog whose scan failed keeps its last numbers and carries an `error` sentence (zeros for one that never answered); only an unreachable engine or catalog fails the request with 502. 403 when no catalog is open to the caller. Never cached by the client.",
    responses(
        (status = 200, description = "One entry per browsable catalog, medallion layers first, then by name, with the totals", body = MetricsResponse),
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
        // The seeded tables are the numbers: there is nothing to scan and nothing to wait
        // for, so the row is as fresh as the answer.
        let mut view = CatalogMetricsView::from(demo_numbers(&s));
        view.updated_at = Some(i64::try_from(crate::jobs::now_ms()).unwrap_or(i64::MAX));
        catalogs.push(view);
    } else {
        // The request path never scans: whatever the cache holds is the answer, however
        // old it is (`updatedAt` says how old), and a missing entry is `pending` while one
        // background scan reads it.
        let mut missing: Vec<String> = Vec::new();
        {
            let cache = s.catalog_metrics.lock().expect("catalog metrics poisoned");
            for name in &allowed {
                match cache.get(name) {
                    Some(entry) => catalogs.push(CatalogMetricsView::read(
                        entry.stats.clone(),
                        entry.updated_ms,
                        entry.error.clone(),
                    )),
                    None => missing.push(name.clone()),
                }
            }
        }
        for name in missing {
            catalogs.push(CatalogMetricsView::pending(name.clone()));
            // Single-flight: two requests for the same new catalog start one scan.
            start_background_scan(&s, &name);
        }
    }

    // The layer is a property of the catalog, not of the numbers: one cached read for the
    // whole answer (stardelt-nova#109).
    let layers = crate::catalog::catalog_layers(&s).await;
    for catalog in &mut catalogs {
        catalog.stats.layer = crate::catalog::layer_in(&layers, &catalog.stats.name);
    }
    // `allowed` is sorted already and the scans finish in their own order.
    crate::catalog::sort_by_layer(
        &mut catalogs,
        |catalog| catalog.stats.layer.as_deref(),
        |catalog| catalog.stats.name.as_str(),
    );

    let mut totals = MetricsTotals::default();
    for catalog in catalogs.iter().filter(|catalog| !catalog.pending) {
        totals.catalogs = totals.catalogs.saturating_add(1);
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

#[utoipa::path(
    post,
    operation_id = "catalog_metrics_refresh",
    path = "/api/catalog/metrics/refresh",
    tag = "public",
    description = "Read every catalog again now, instead of waiting for the next background round (every five minutes). Administrator only: it is an operator action on the whole warehouse, and `main` puts the same `require_admin` in front of it as `/api/apps`, so a non-administrator answers 403. Answers 202 — the round runs in the background, one at a time, and the page reads the new numbers with its next `GET /api/catalog/metrics`. At most one per minute: a second call answers 429 with the sentence `the numbers were refreshed less than a minute ago`.",
    responses(
        (status = 202, description = "The round is running; the numbers follow on the next read", body = RefreshAccepted),
        (status = 403, description = "Not an administrator", body = crate::openapi::ErrorResponse),
        (status = 429, description = "The numbers were refreshed less than a minute ago", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn refresh(State(s): State<Arc<AppState>>) -> Response {
    {
        let mut last = s
            .metrics_refresh
            .lock()
            .expect("catalog metrics refresh poisoned");
        if let Some(at) = *last
            && at.elapsed() < REFRESH_LIMIT
        {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "error": REFRESH_TOO_SOON })),
            )
                .into_response();
        }
        // Refusals never reach here, and a refusal must not consume the minute.
        *last = Some(Instant::now());
    }
    let state = Arc::clone(&s);
    tokio::spawn(async move {
        // One round at a time; a round that is already running covers this request.
        run_round(&state).await;
    });
    (
        StatusCode::ACCEPTED,
        Json(RefreshAccepted { accepted: true }),
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
        /// When set, a namespace list waits here: the single-flight and "no scan on the
        /// request path" tests hold a scan mid-request instead of sleeping.
        gate: Option<Arc<Gate>>,
    }

    /// A held door: a request announces itself and waits until the test lets it through.
    /// Zero permits at the start, so every request waits.
    struct Gate {
        release: tokio::sync::Semaphore,
        arrived: tokio::sync::Notify,
    }

    impl Gate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                release: tokio::sync::Semaphore::new(0),
                arrived: tokio::sync::Notify::new(),
            })
        }
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

        /// Hold every namespace list until the test releases it; answers the door so the
        /// test can wait for a request to arrive.
        fn hold(&self) -> Arc<Gate> {
            let gate = Gate::new();
            self.0.lock().unwrap().gate = Some(Arc::clone(&gate));
            gate
        }

        fn load_requests(&self) -> usize {
            self.0.lock().unwrap().load_requests
        }

        fn namespace_requests(&self) -> usize {
            self.0.lock().unwrap().namespace_requests
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
        let gate = {
            let mut lakekeeper = fake.0.lock().unwrap();
            lakekeeper.namespace_requests += 1;
            if lakekeeper.broken.contains(&wh) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": { "message": "boom" } })),
                )
                    .into_response();
            }
            lakekeeper.gate.clone()
        };
        if let Some(gate) = gate {
            // Announce, then wait: the test decides when this scan may continue.
            gate.arrived.notify_one();
            let _permit = gate
                .release
                .acquire()
                .await
                .expect("the gate is not closed");
        }
        let namespaces = fake
            .0
            .lock()
            .unwrap()
            .namespaces
            .get(&wh)
            .cloned()
            .unwrap_or_default();
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

    // -----------------------------------------------------------------------
    // The background refresh (stardelt-nova#263)
    // -----------------------------------------------------------------------

    /// A fake catalog with one table: 42 rows, 4096 bytes, committed once.
    async fn catalog_with_one_table(fake: &FakeLakekeeper) -> String {
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
        fake.serve().await
    }

    /// The cache's entry for one catalog, cloned.
    fn entry(state: &AppState, name: &str) -> Option<MetricsEntry> {
        state.catalog_metrics.lock().unwrap().get(name).cloned()
    }

    /// Wait until the cache says what the test expects — a bounded poll on the paused
    /// clock, never a fixed sleep: the work under test is a background task whose moment
    /// the test does not choose.
    async fn wait_for_entry(
        state: &AppState,
        name: &str,
        matches: impl Fn(&MetricsEntry) -> bool,
    ) -> MetricsEntry {
        for _ in 0..2_000 {
            if let Some(entry) = entry(state, name)
                && matches(&entry)
            {
                return entry;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the cache never held what the test waited for: {name}");
    }

    #[tokio::test(start_paused = true)]
    async fn refresher_fills_the_cache_on_schedule() {
        let fake = FakeLakekeeper::new();
        let url = catalog_with_one_table(&fake).await;
        let state = crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", &url);

        let task = tokio::spawn(refresher_loop(
            Arc::clone(&state),
            Duration::from_millis(10),
            Duration::from_millis(50),
        ));

        // Not before the first delay: the loop's very first act is to wait.
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(entry(&state, "warehouse").is_none());
        assert_eq!(fake.load_requests(), 0, "no scan before the first round");

        // After it, the round fills the cache without anyone asking.
        let first = wait_for_entry(&state, "warehouse", |e| e.stats.records == 42).await;
        assert_eq!(first.stats.tables, 1);
        assert_eq!(first.error, None);
        assert!(
            first.updated_ms > 0,
            "the numbers carry when they were read"
        );
        assert_eq!(fake.load_requests(), 1);

        // The catalog changes; the next round — not a request — picks the new numbers up.
        fake.load(
            "wid-warehouse",
            "demo",
            "taxi",
            metadata_with(
                json!([snapshot(4, "99", "8192", 1_700_000_200_000)]),
                json!(4),
                None,
            ),
        );
        let second = wait_for_entry(&state, "warehouse", |e| e.stats.records == 99).await;
        assert!(second.updated >= first.updated, "the entry moved forward");
        assert_eq!(second.stats.size_bytes, 8192);
        task.abort();
    }

    #[test]
    fn interval_clamped() {
        // The default when nothing (or nonsense) is configured.
        for raw in [None, Some(""), Some("soon"), Some("-5")] {
            assert_eq!(interval_from(raw), DEFAULT_REFRESH, "{raw:?}");
        }
        // Clamped to 30 s … 1 h, and a real value passes through.
        assert_eq!(interval_from(Some("0")), MIN_REFRESH);
        assert_eq!(interval_from(Some("1")), MIN_REFRESH);
        assert_eq!(interval_from(Some("30")), MIN_REFRESH);
        assert_eq!(interval_from(Some("300")), DEFAULT_REFRESH);
        assert_eq!(interval_from(Some("3601")), MAX_REFRESH);
        assert_eq!(interval_from(Some("999999")), MAX_REFRESH);
        // Whitespace is not a typo.
        assert_eq!(interval_from(Some(" 120 ")), Duration::from_secs(120));
    }

    #[test]
    fn demo_mode_has_no_refresher() {
        // Demo mode's numbers are the seeded tables; a background scan of a catalog that
        // does not exist would only log errors.
        let demo = crate::router_tests::test_state();
        assert!(demo.demo, "the test state is the demo one");
        assert!(!refresher_wanted(&demo));
        assert!(!spawn_refresher(Arc::clone(&demo)), "nothing was spawned");
        assert!(demo.catalog_metrics.lock().unwrap().is_empty());

        // A real install with a catalog gets one; an empty URL does not.
        let real = crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", "http://x");
        assert!(refresher_wanted(&real));
        let unconfigured =
            crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", "  ");
        assert!(!refresher_wanted(&unconfigured));
    }

    // -----------------------------------------------------------------------
    // The handler
    // -----------------------------------------------------------------------

    /// A fake Trino answering `SHOW CATALOGS` per person, as the permission bridge does:
    /// `X-Trino-User` is the display name (`trino::upstream_headers`).
    #[derive(Clone, Default)]
    struct FakeTrino {
        by_user: HashMap<String, Vec<String>>,
        /// bearer token -> person key: with a forwarded token the engine sees only
        /// the Authorization header (platform#311/#259).
        tokens: HashMap<String, String>,
    }

    async fn show_catalogs(State(fake): State<FakeTrino>, headers: HeaderMap) -> Json<Value> {
        let user = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .and_then(|token| fake.tokens.get(token).cloned())
            .or_else(|| {
                headers
                    .get("x-trino-user")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let rows: Vec<Value> = fake
            .by_user
            .get(&user)
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
            tokens: by_user
                .iter()
                .map(|(user, _)| (format!("token-{user}"), user.to_string()))
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
            access_token: Some(crate::auth::Secret::new(format!("token-{name}"))),
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
        // The background refresher's work, done here explicitly: the request only reads.
        assert!(run_round(&state).await);

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
        let rows = body["catalogs"].as_array().expect("rows");
        assert_eq!(
            rows.iter()
                .map(|row| row["name"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["finance", "warehouse"],
            "product is not hers, the engine's own catalogs are never listed"
        );
        assert_eq!(
            rows[0]["schemas"],
            1,
            "finance: {}",
            serde_json::to_string(&rows[0]).unwrap()
        );
        assert_eq!(rows[0]["tables"], 1);
        assert_eq!(rows[0]["records"], 25);
        assert_eq!(rows[0]["sizeBytes"], 1024);
        assert_eq!(rows[0]["lastCommitMs"], 1_700_000_200_000_i64);
        assert_eq!(rows[0]["truncated"], false);
        assert_eq!(rows[0]["pending"], false);
        assert!(
            rows[0]["updatedAt"].as_u64().is_some_and(|ms| ms > 0),
            "a read catalog says when its numbers were read"
        );
        assert_eq!(rows[1]["tables"], 1);
        assert_eq!(rows[1]["records"], 12);
        assert_eq!(rows[1]["lastCommitMs"], 1_700_000_100_000_i64);
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
        assert!(run_round(&state).await);

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
        assert_eq!(
            body["catalogs"][0]["pending"], false,
            "the scan answered — with the sentence, not with a promise"
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
    async fn get_does_not_scan_when_an_entry_exists() {
        // The request path answers from the cache, whatever its age: a scan is the
        // refresher's job, never the page's.
        let trino = fake_trino(&[("ada", &["warehouse"])]).await;
        let fake = FakeLakekeeper::new();
        let lakekeeper = catalog_with_one_table(&fake).await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);
        {
            let mut cache = state.catalog_metrics.lock().unwrap();
            cache.insert(
                "warehouse".into(),
                MetricsEntry {
                    stats: CatalogStats {
                        name: "warehouse".into(),
                        schemas: 3,
                        tables: 7,
                        records: 1_234,
                        size_bytes: 5_120,
                        last_commit_ms: Some(1_700_000_123_000),
                        truncated: false,
                        layer: None,
                    },
                    updated: Instant::now() - Duration::from_secs(600),
                    updated_ms: 1_700_000_123_456,
                    error: None,
                },
            );
        }

        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_of(response).await;
        assert_eq!(body["catalogs"][0]["tables"], 7);
        assert_eq!(body["catalogs"][0]["records"], 1_234);
        assert_eq!(body["catalogs"][0]["pending"], false);
        assert_eq!(body["catalogs"][0]["updatedAt"], 1_700_000_123_456_i64);
        assert_eq!(
            fake.load_requests(),
            0,
            "the answer came from the cache: the catalog was not read"
        );
        assert_eq!(fake.namespace_requests(), 0);
    }

    #[tokio::test]
    async fn stale_entry_is_served_and_flagged() {
        // Ten minutes old — twice a refresh period — is still the answer: it is served at
        // once, and `updatedAt` says how old it is (the page's "Updated 10 minutes ago").
        let trino = fake_trino(&[("ada", &["warehouse"])]).await;
        let fake = FakeLakekeeper::new();
        let lakekeeper = catalog_with_one_table(&fake).await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);
        let ten_minutes_ago = crate::jobs::now_ms().saturating_sub(600_000) as i64;
        state.catalog_metrics.lock().unwrap().insert(
            "warehouse".into(),
            MetricsEntry {
                stats: CatalogStats {
                    name: "warehouse".into(),
                    schemas: 1,
                    tables: 1,
                    records: 42,
                    size_bytes: 4096,
                    last_commit_ms: Some(1_700_000_000_000),
                    truncated: false,
                    layer: None,
                },
                updated: Instant::now() - Duration::from_secs(600),
                updated_ms: ten_minutes_ago,
                error: None,
            },
        );

        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        let body = body_of(response).await;
        assert_eq!(body["catalogs"][0]["records"], 42);
        assert_eq!(body["catalogs"][0]["updatedAt"], ten_minutes_ago);
        assert_eq!(body["catalogs"][0]["pending"], false);
        assert_eq!(body["totals"]["records"], 42);
        assert_eq!(
            fake.load_requests(),
            0,
            "a stale entry is served, not rescanned on the request path"
        );
    }

    #[tokio::test]
    async fn unknown_catalog_is_pending_and_scanned_once_in_background() {
        let trino = fake_trino(&[("ada", &["warehouse"])]).await;
        let fake = FakeLakekeeper::new();
        let lakekeeper = catalog_with_one_table(&fake).await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        // No entry yet: the row is listed as pending, and the request does not wait.
        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_of(response).await;
        assert_eq!(body["catalogs"][0]["name"], "warehouse");
        assert_eq!(body["catalogs"][0]["pending"], true);
        assert_eq!(body["catalogs"][0]["tables"], 0);
        assert_eq!(body["catalogs"][0]["updatedAt"], Value::Null);
        assert_eq!(
            body["totals"]["catalogs"], 0,
            "pending rows are not counted"
        );

        // One background scan, no second: it stores the numbers.
        let entry = wait_for_entry(&state, "warehouse", |entry| entry.stats.records == 42).await;
        assert_eq!(entry.error, None);
        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        let body = body_of(response).await;
        assert_eq!(body["catalogs"][0]["pending"], false);
        assert_eq!(body["catalogs"][0]["records"], 42);
        assert_eq!(fake.namespace_requests(), 1);
        assert_eq!(fake.load_requests(), 1);
    }

    #[tokio::test]
    async fn single_flight_two_requests_one_scan() {
        let trino = fake_trino(&[("ada", &["warehouse"])]).await;
        let fake = FakeLakekeeper::new();
        let lakekeeper = catalog_with_one_table(&fake).await;
        let state = crate::router_tests::catalog_scope_test_state(&trino, &lakekeeper);

        // Hold the scan inside the catalog, then send two requests for the same new
        // catalog: both answer pending, exactly one scan is on its way.
        let gate = fake.hold();
        for _ in 0..2 {
            let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
            let body = body_of(response).await;
            assert_eq!(body["catalogs"][0]["pending"], true);
        }
        gate.arrived.notified().await;
        assert_eq!(fake.namespace_requests(), 1, "one scan, not two");

        gate.release.add_permits(1);
        wait_for_entry(&state, "warehouse", |entry| entry.stats.records == 42).await;
        assert_eq!(fake.namespace_requests(), 1);
        assert_eq!(fake.load_requests(), 1);
    }

    #[tokio::test]
    async fn failed_scan_keeps_old_numbers_and_sets_error() {
        let fake = FakeLakekeeper::new();
        let lakekeeper = catalog_with_one_table(&fake).await;
        let state =
            crate::router_tests::catalog_scope_test_state("http://127.0.0.1:1", &lakekeeper);
        assert!(run_round(&state).await);
        let good = entry(&state, "warehouse").expect("the first round stores the numbers");
        assert_eq!(good.stats.records, 42);
        assert_eq!(good.error, None);

        // The catalog breaks; the round keeps the numbers, adds the sentence, and leaves
        // the timestamp alone — the numbers really are from that moment.
        fake.break_namespaces("wid-warehouse");
        assert!(run_round(&state).await);
        let after = entry(&state, "warehouse").expect("still an entry");
        assert_eq!(after.stats.records, 42, "a failed refresh wipes nothing");
        assert_eq!(
            after.updated_ms, good.updated_ms,
            "the numbers' age is honest"
        );
        assert_eq!(
            after.error.as_deref(),
            Some("the catalog returned an unexpected response")
        );
    }

    #[tokio::test]
    async fn totals_exclude_pending() {
        let trino = fake_trino(&[("ada", &["warehouse", "finance"])]).await;
        let fake = FakeLakekeeper::new();
        fake.warehouse("warehouse", "wid-warehouse");
        fake.warehouse("finance", "wid-finance");
        let state = crate::router_tests::catalog_scope_test_state(&trino, &fake.serve().await);
        // Only one of the two has numbers.
        state.catalog_metrics.lock().unwrap().insert(
            "warehouse".into(),
            MetricsEntry {
                stats: CatalogStats {
                    name: "warehouse".into(),
                    schemas: 2,
                    tables: 3,
                    records: 40,
                    size_bytes: 1_024,
                    last_commit_ms: None,
                    truncated: false,
                    layer: None,
                },
                updated: Instant::now(),
                updated_ms: 1_700_000_000_000,
                error: None,
            },
        );

        let response = get(State(Arc::clone(&state)), Some(Extension(person("ada")))).await;
        let body = body_of(response).await;
        assert_eq!(
            body["catalogs"].as_array().unwrap().len(),
            2,
            "both are listed"
        );
        assert_eq!(
            body["totals"],
            json!({
                "catalogs": 1,
                "schemas": 2,
                "tables": 3,
                "records": 40,
                "sizeBytes": 1024,
            }),
            "a pending catalog's zeros are not numbers"
        );
    }

    #[tokio::test]
    async fn refresh_endpoint_admin_only_and_rate_limited() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        fn identity(admin: bool) -> Identity {
            Identity {
                sub: "s".into(),
                display_name: "u".into(),
                email: None,
                name: None,
                groups: Vec::new(),
                roles: Vec::new(),
                groups_overage: false,
                is_admin: admin,
            }
        }

        async fn call(state: Arc<AppState>, identity: Option<Identity>) -> (StatusCode, Value) {
            // The same order `main` builds: require_admin inner, the principal outer.
            let app = crate::admin_router()
                .route_layer(axum::middleware::from_fn(crate::auth::require_admin))
                .layer(axum::middleware::from_fn(
                    move |mut req: Request<Body>, next: axum::middleware::Next| {
                        let identity = identity.clone();
                        async move {
                            if let Some(identity) = identity {
                                req.extensions_mut().insert(Principal {
                                    identity,
                                    via: "session",
                                    access_token: None,
                                });
                            }
                            next.run(req).await
                        }
                    },
                ))
                .with_state(state);
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/catalog/metrics/refresh")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            (status, body_of(response).await)
        }

        let state = crate::router_tests::catalog_scope_test_state(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
        );

        // The same refusal `/api/apps` answers: a refresh is an operator action.
        let (status, body) = call(Arc::clone(&state), Some(identity(false))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("platform administrators"),
            "{body}"
        );

        // The refusal consumed nothing, so an administrator right after is accepted; the
        // round runs in the background and the numbers follow on the next read.
        let (status, body) = call(Arc::clone(&state), Some(identity(true))).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["accepted"], true);

        // At most one per minute: a second call is refused with the sentence.
        let (status, body) = call(Arc::clone(&state), Some(identity(true))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"], REFRESH_TOO_SOON);
        assert_eq!(
            body["error"],
            "the numbers were refreshed less than a minute ago"
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
        // Demo mode has nothing to scan, so nothing waits: the row is as fresh as the answer.
        assert_eq!(body["catalogs"][0]["pending"], false);
        assert!(
            body["catalogs"][0]["updatedAt"]
                .as_u64()
                .is_some_and(|ms| ms > 0),
            "the demo numbers are read now"
        );
    }
}
