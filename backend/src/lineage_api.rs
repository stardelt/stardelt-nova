//! Lineage graph API (D40, WP-34a).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::Principal;
use crate::catalog;

#[derive(Debug, Deserialize)]
pub struct GraphQuery {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    #[serde(default = "default_depth")]
    pub depth: i32,
    /// Time window in days, 1-365, default 30.
    pub days: Option<i32>,
}

fn default_depth() -> i32 {
    2
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LineageNode {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub visible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 0 for the focus table, negative upstream, positive downstream (visible nodes only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i32>,
    /// The newest event time of an edge that wrote this node.
    #[serde(rename = "lastWrite", skip_serializing_if = "Option::is_none")]
    pub last_write: Option<String>,
    /// Read-only queries against this node in the window (visible nodes only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readers: Option<LineageReaders>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineageReaders {
    pub queries: i64,
    pub people: i64,
    pub last_read: String,
}

/// One job that wrote an edge of the answer, with its runs in the window.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineageJob {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub runs: i64,
    pub last_run: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
    #[serde(rename = "jobNamespace")]
    pub job_namespace: String,
    #[serde(rename = "jobName")]
    pub job_name: String,
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "eventTime")]
    pub event_time: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LineageGraph {
    pub nodes: Vec<LineageNode>,
    pub edges: Vec<LineageEdge>,
    pub jobs: Vec<LineageJob>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, ToSchema)]
pub struct RunDetail {
    pub run_id: String,
    pub job_namespace: String,
    pub job_name: String,
    pub event_type: Option<String>,
    pub event_time: Option<String>,
    pub inputs: Vec<DatasetRef>,
    pub outputs: Vec<DatasetRef>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DatasetRef {
    pub namespace: String,
    pub name: String,
}

fn error_response(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn hidden_label() -> String {
    "a dataset you cannot see".to_string()
}

fn dataset_id(ns: &str, name: &str) -> String {
    format!("{}|{}", ns, name)
}

/// Edges inside the window; `$1` is the window in days.
const EDGES_SQL: &str = "SELECT run_id::text, job_namespace, job_name, dataset_namespace, dataset_name, direction \
     FROM lineage_edges \
     WHERE event_time > now() - make_interval(days => $1::int)";

/// Each run's newest event time inside the window.
const EDGE_TIMES_SQL: &str = "SELECT run_id::text, to_char(MAX(event_time), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_edges \
     WHERE event_time > now() - make_interval(days => $1::int) \
     GROUP BY run_id";

/// Readers of the visible datasets in the window (lineage L2, stardelt-nova#647).
const READERS_SQL: &str = "SELECT dataset_namespace, dataset_name, sum(queries), count(DISTINCT job_name), \
       to_char(max(last_read), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_reads \
     WHERE day > (now() - make_interval(days => $1::int))::date \
       AND job_namespace IN ('sql-editor','query','notebook') \
       AND (dataset_namespace, dataset_name) IN (SELECT * FROM unnest($2::text[], $3::text[])) \
     GROUP BY 1, 2";

/// The level of a dataset one run away from `level`: a dataset the run reads sits one
/// level further upstream (negative), one it writes one level further downstream (positive).
fn adjacent_level(level: i32, direction: &str) -> i32 {
    if direction == "input" {
        level - 1
    } else {
        level + 1
    }
}

/// The datasets a run links `current` to, each with its level. The run's own datasets are
/// `(direction, namespace, name)` rows.
fn run_neighbours(
    datasets: &[(String, String, String)],
    current_ns: &str,
    current_name: &str,
    level: i32,
) -> Vec<(String, String, i32)> {
    datasets
        .iter()
        .filter(|(_, ns, name)| ns != current_ns || name != current_name)
        .map(|(direction, ns, name)| (ns.clone(), name.clone(), adjacent_level(level, direction)))
        .collect()
}

#[utoipa::path(
    get,
    operation_id = "lineage_graph",
    path = "/api/lineage",
    tag = "public",
    params(
        ("catalog" = Option<String>, Query, description = "Catalog"),
        ("schema" = Option<String>, Query, description = "Schema"),
        ("table" = Option<String>, Query, description = "Table"),
        ("depth" = Option<i32>, Query, description = "Depth 1-5, default 2"),
        ("days" = Option<i32>, Query, description = "Time window in days, 1–365, default 30")
    ),
    responses(
        (status = 200, description = "Lineage graph", body = LineageGraph),
        (status = 403, description = "Not visible")
    )
)]
pub async fn graph(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(q): Query<GraphQuery>,
) -> Response {
    let principal = principal.map(|Extension(p)| p);
    let depth = q.depth.clamp(1i32, 5i32);
    let days = q.days.unwrap_or(30).clamp(1, 365);

    // Determine the starting dataset
    let (start_ns, start_name, catalog_name) = match (&q.catalog, &q.schema, &q.table) {
        (Some(cat), Some(schema), Some(table)) => (
            format!("{}.{}", cat, schema),
            table.clone(),
            Some(cat.clone()),
        ),
        (Some(schema), None, Some(table)) => (schema.clone(), table.clone(), None),
        _ => {
            return error_response(StatusCode::BAD_REQUEST, "catalog/schema/table required");
        }
    };

    if state.demo {
        return demo_graph_response(&start_ns, &start_name);
    }

    let allowed = match catalog::allowed_catalogs(&state, principal.as_ref()).await {
        Ok(a) => a,
        Err(_) => return error_response(StatusCode::BAD_GATEWAY, "could not resolve catalogs"),
    };

    if let Some(cat) = &catalog_name
        && !allowed.contains(cat)
    {
        return error_response(StatusCode::FORBIDDEN, "forbidden");
    }

    let pool = match &state.db {
        Some(p) => p,
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "state store unavailable"),
    };

    let start_id = dataset_id(&start_ns, &start_name);

    // Load the edges and their times inside the window.
    let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(EDGES_SQL)
        .bind(days)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    let times: Vec<(String, String)> = sqlx::query_as(EDGE_TIMES_SQL)
        .bind(days)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let time_map: HashMap<String, String> = times.into_iter().collect();

    // Group by run
    #[allow(clippy::type_complexity)]
    let mut runs: HashMap<String, (String, String, Vec<(String, String, String)>)> = HashMap::new();
    for row in &rows {
        runs.entry(row.0.clone())
            .or_insert_with(|| (row.1.clone(), row.2.clone(), vec![]))
            .2
            .push((row.5.clone(), row.3.clone(), row.4.clone()));
    }

    let mut result_nodes: HashMap<String, LineageNode> = HashMap::new();
    let mut result_edges: Vec<LineageEdge> = Vec::new();
    let mut hidden_ids: HashMap<String, String> = HashMap::new(); // real id -> opaque id
    let mut hidden_seq: usize = 0;

    // Add start node (must be visible, checked above)
    result_nodes.insert(
        start_id.clone(),
        LineageNode {
            id: start_id.clone(),
            namespace: start_ns.clone(),
            name: start_name.clone(),
            visible: true,
            label: None,
            level: Some(0),
            last_write: None,
            readers: None,
        },
    );

    let mut to_visit = vec![(start_ns.clone(), start_name.clone(), 0i32, start_id.clone())];
    let mut visited = HashSet::new();
    visited.insert((start_ns.clone(), start_name.clone()));

    while let Some((cur_ns, cur_name, d, cur_id)) = to_visit.pop() {
        if d >= depth {
            continue;
        }
        let cur_level = result_nodes
            .get(&cur_id)
            .and_then(|n| n.level)
            .unwrap_or_default();
        for (run_id, (job_ns, job_name, datasets)) in &runs {
            let has_cur = datasets
                .iter()
                .any(|(_, dns, dname)| dns == &cur_ns && dname == &cur_name);
            if !has_cur {
                continue;
            }
            for (dns, dname, node_level) in run_neighbours(datasets, &cur_ns, &cur_name, cur_level)
            {
                let real_id = dataset_id(&dns, &dname);
                let ds_catalog = dns.split('.').next().unwrap_or("");
                let is_visible = allowed.is_empty()
                    || allowed.contains(ds_catalog)
                    || (catalog_name.is_some()
                        && allowed
                            .iter()
                            .any(|a| dns.starts_with(&format!("{a}.")) || dns == *a));
                let nds_id = if is_visible {
                    real_id.clone()
                } else if let Some(id) = hidden_ids.get(&real_id) {
                    id.clone()
                } else {
                    hidden_seq += 1;
                    let id = format!("hidden-{hidden_seq}");
                    hidden_ids.insert(real_id.clone(), id.clone());
                    id
                };
                if !visited.contains(&(dns.clone(), dname.clone())) {
                    // The level of the first path that reaches a node is the one it keeps.
                    let level = if is_visible {
                        Some(
                            result_nodes
                                .get(&nds_id)
                                .and_then(|n| n.level)
                                .unwrap_or(node_level),
                        )
                    } else {
                        None
                    };
                    let (namespace, name, label) = if is_visible {
                        (dns.clone(), dname.clone(), None)
                    } else {
                        (String::new(), String::new(), Some(hidden_label()))
                    };
                    result_nodes.insert(
                        nds_id.clone(),
                        LineageNode {
                            id: nds_id.clone(),
                            namespace,
                            name,
                            visible: is_visible,
                            label,
                            level,
                            last_write: None,
                            readers: None,
                        },
                    );
                    if d + 1 < depth {
                        to_visit.push((dns.clone(), dname.clone(), d + 1, nds_id.clone()));
                        visited.insert((dns.clone(), dname.clone()));
                    }
                }
                result_edges.push(LineageEdge {
                    from: cur_id.clone(),
                    to: nds_id.clone(),
                    job_namespace: job_ns.clone(),
                    job_name: job_name.clone(),
                    run_id: run_id.clone(),
                    event_time: time_map
                        .get(run_id)
                        .cloned()
                        .unwrap_or_else(|| "2026-01-01T00:00:00Z".to_string()),
                });
            }
        }
    }

    // The newest write that reached each visible node.
    for edge in &result_edges {
        if let Some(node) = result_nodes.get_mut(&edge.to)
            && node.visible
            && node
                .last_write
                .as_ref()
                .is_none_or(|seen| edge.event_time > *seen)
        {
            node.last_write = Some(edge.event_time.clone());
        }
    }

    // Readers of the visible nodes, in one query.
    let mut read_ns: Vec<String> = Vec::new();
    let mut read_names: Vec<String> = Vec::new();
    for node in result_nodes.values().filter(|n| n.visible) {
        read_ns.push(node.namespace.clone());
        read_names.push(node.name.clone());
    }
    if !read_ns.is_empty() {
        let reads: Vec<(String, String, i64, i64, String)> = sqlx::query_as(READERS_SQL)
            .bind(days)
            .bind(&read_ns)
            .bind(&read_names)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
        for (ns, name, queries, people, last_read) in reads {
            if let Some(node) = result_nodes.get_mut(&dataset_id(&ns, &name)) {
                node.readers = Some(LineageReaders {
                    queries,
                    people,
                    last_read,
                });
            }
        }
    }

    // One entry per job that wrote an edge of the answer.
    let mut jobs: Vec<LineageJob> = Vec::new();
    let mut job_index: HashMap<(String, String), usize> = HashMap::new();
    let mut job_runs: Vec<HashSet<String>> = Vec::new();
    for edge in &result_edges {
        let key = (edge.job_namespace.clone(), edge.job_name.clone());
        let at = match job_index.get(&key) {
            Some(&at) => at,
            None => {
                job_index.insert(key.clone(), jobs.len());
                job_runs.push(HashSet::new());
                jobs.push(LineageJob {
                    id: format!("{}|{}", key.0, key.1),
                    kind: key.0.clone(),
                    name: key.1.clone(),
                    runs: 0,
                    last_run: edge.event_time.clone(),
                });
                jobs.len() - 1
            }
        };
        job_runs[at].insert(edge.run_id.clone());
        if edge.event_time > jobs[at].last_run {
            jobs[at].last_run = edge.event_time.clone();
        }
    }
    for (job, runs) in jobs.iter_mut().zip(job_runs) {
        job.runs = runs.len() as i64;
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));

    let graph = LineageGraph {
        nodes: result_nodes.into_values().collect(),
        edges: result_edges,
        jobs,
    };
    (StatusCode::OK, Json(graph)).into_response()
}

fn demo_graph_response(_start_ns: &str, _start_name: &str) -> Response {
    let last_write = Some("2026-01-01T00:00:00Z".to_string());
    let graph = LineageGraph {
        nodes: vec![
            LineageNode {
                id: "nyc_taxi|yellow_trips".to_string(),
                namespace: "nyc_taxi".to_string(),
                name: "yellow_trips".to_string(),
                visible: true,
                label: None,
                level: Some(-1),
                last_write: None,
                readers: None,
            },
            LineageNode {
                id: "analytics|trips_by_borough".to_string(),
                namespace: "analytics".to_string(),
                name: "trips_by_borough".to_string(),
                visible: true,
                label: None,
                level: Some(0),
                last_write: last_write.clone(),
                readers: Some(LineageReaders {
                    queries: 12,
                    people: 3,
                    last_read: "2026-01-01T00:00:00Z".to_string(),
                }),
            },
            LineageNode {
                id: "analytics|daily_revenue".to_string(),
                namespace: "analytics".to_string(),
                name: "daily_revenue".to_string(),
                visible: true,
                label: None,
                level: Some(1),
                last_write: last_write.clone(),
                readers: Some(LineageReaders {
                    queries: 5,
                    people: 2,
                    last_read: "2026-01-01T00:00:00Z".to_string(),
                }),
            },
        ],
        edges: vec![
            LineageEdge {
                from: "nyc_taxi|yellow_trips".to_string(),
                to: "analytics|trips_by_borough".to_string(),
                job_namespace: "pipeline".to_string(),
                job_name: "trips_by_borough_job".to_string(),
                run_id: "00000000-0000-0000-0000-000000000001".to_string(),
                event_time: "2026-01-01T00:00:00Z".to_string(),
            },
            LineageEdge {
                from: "nyc_taxi|yellow_trips".to_string(),
                to: "analytics|daily_revenue".to_string(),
                job_namespace: "pipeline".to_string(),
                job_name: "daily_revenue_job".to_string(),
                run_id: "00000000-0000-0000-0000-000000000002".to_string(),
                event_time: "2026-01-01T00:00:00Z".to_string(),
            },
        ],
        jobs: vec![
            LineageJob {
                id: "pipeline|daily_revenue_job".to_string(),
                kind: "pipeline".to_string(),
                name: "daily_revenue_job".to_string(),
                runs: 1,
                last_run: "2026-01-01T00:00:00Z".to_string(),
            },
            LineageJob {
                id: "pipeline|trips_by_borough_job".to_string(),
                kind: "pipeline".to_string(),
                name: "trips_by_borough_job".to_string(),
                runs: 1,
                last_run: "2026-01-01T00:00:00Z".to_string(),
            },
        ],
    };
    (StatusCode::OK, Json(graph)).into_response()
}

/// The checked query string of `/api/lineage/columns` (stardelt-nova#646 slice L5).
#[derive(Debug, Deserialize)]
pub struct ColumnsQuery {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    #[serde(default = "default_days")]
    pub days: i64,
}

fn default_days() -> i64 {
    90
}

/// One source column of an output column. A source outside the caller's catalogs is
/// answered as `{ "hidden": true }`, without names.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ColumnLineageEntry {
    pub column: String,
    pub sources: Vec<ColumnSource>,
    pub last_seen: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ColumnLineage {
    pub columns: Vec<ColumnLineageEntry>,
}

/// Merge stored `columns` arrays (newest event first) into one entry per output column:
/// its source columns, deduplicated, and the newest time the column was written.
fn merge_columns(
    rows: &[(serde_json::Value, String)],
    allowed: &BTreeSet<String>,
) -> Vec<ColumnLineageEntry> {
    let mut entries: Vec<ColumnLineageEntry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut seen: Vec<HashSet<String>> = Vec::new();
    for (columns, event_time) in rows {
        let Some(list) = columns.as_array() else {
            continue;
        };
        for col in list {
            let Some(name) = col.get("columnName").and_then(|v| v.as_str()) else {
                continue;
            };
            let at = match index.get(name) {
                Some(&at) => at,
                None => {
                    index.insert(name.to_string(), entries.len());
                    seen.push(HashSet::new());
                    entries.push(ColumnLineageEntry {
                        column: name.to_string(),
                        sources: vec![],
                        last_seen: event_time.clone(),
                    });
                    entries.len() - 1
                }
            };
            if event_time > &entries[at].last_seen {
                entries[at].last_seen = event_time.clone();
            }
            let Some(sources) = col.get("sourceColumns").and_then(|v| v.as_array()) else {
                continue;
            };
            for src in sources {
                let Some((key, source)) = source_of(src, allowed) else {
                    continue;
                };
                if seen[at].insert(key) {
                    entries[at].sources.push(source);
                }
            }
        }
    }
    entries
}

/// One stored source column, ready for the answer: named when the caller may read its
/// catalog, `hidden` otherwise. `None` when the row has no catalog to judge.
fn source_of(
    src: &serde_json::Value,
    allowed: &BTreeSet<String>,
) -> Option<(String, ColumnSource)> {
    let catalog = src.get("catalog").and_then(|v| v.as_str())?;
    if !allowed.contains(catalog) {
        return Some((
            "hidden".to_string(),
            ColumnSource {
                catalog: None,
                schema: None,
                table: None,
                column: None,
                hidden: true,
            },
        ));
    }
    let schema = src.get("schema").and_then(|v| v.as_str()).unwrap_or("");
    let table = src.get("table").and_then(|v| v.as_str()).unwrap_or("");
    let column = src.get("columnName").and_then(|v| v.as_str()).unwrap_or("");
    let key = format!("{catalog}|{schema}|{table}|{column}");
    Some((
        key,
        ColumnSource {
            catalog: Some(catalog.to_string()),
            schema: Some(schema.to_string()),
            table: Some(table.to_string()),
            column: Some(column.to_string()),
            hidden: false,
        },
    ))
}

const COLUMNS_SQL: &str = "SELECT payload->'ioMetadata'->'output'->'columns', to_char(event_time,'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_events \
     WHERE payload->'ioMetadata'->'output'->>'catalogName' = $1 \
       AND payload->'ioMetadata'->'output'->>'schema' = $2 \
       AND payload->'ioMetadata'->'output'->>'table' = $3 \
       AND event_time > now() - make_interval(days => $4::int) \
     ORDER BY event_time DESC \
     LIMIT 200";

#[utoipa::path(
    get,
    operation_id = "lineage_columns",
    path = "/api/lineage/columns",
    tag = "public",
    params(
        ("catalog" = String, Query, description = "Catalog"),
        ("schema" = String, Query, description = "Schema"),
        ("table" = String, Query, description = "Table"),
        ("days" = Option<i64>, Query, description = "Days to look back, 1-365, default 90")
    ),
    responses(
        (status = 200, description = "Where each column comes from", body = ColumnLineage),
        (status = 400, description = "catalog/schema/table required"),
        (status = 403, description = "Not visible")
    )
)]
pub async fn columns(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(q): Query<ColumnsQuery>,
) -> Response {
    let principal = principal.map(|Extension(p)| p);
    let (Some(catalog_name), Some(schema), Some(table)) = (q.catalog, q.schema, q.table) else {
        return error_response(StatusCode::BAD_REQUEST, "catalog/schema/table required");
    };
    let days = q.days.clamp(1, 365);

    if state.demo {
        return (StatusCode::OK, Json(demo_columns_response())).into_response();
    }

    let (catalog_name, _id) =
        match catalog::catalog_scope(&state, principal.as_ref(), Some(&catalog_name)).await {
            Ok(scope) => scope,
            Err(refusal) => return refusal,
        };
    let allowed = match catalog::allowed_catalogs(&state, principal.as_ref()).await {
        Ok(a) => a,
        Err(_) => return error_response(StatusCode::BAD_GATEWAY, "could not resolve catalogs"),
    };

    let pool = match &state.db {
        Some(p) => p,
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "state store unavailable"),
    };

    let rows: Vec<(serde_json::Value, String)> = sqlx::query_as(COLUMNS_SQL)
        .bind(&catalog_name)
        .bind(&schema)
        .bind(&table)
        .bind(days)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    (
        StatusCode::OK,
        Json(ColumnLineage {
            columns: merge_columns(&rows, &allowed),
        }),
    )
        .into_response()
}

fn demo_columns_response() -> ColumnLineage {
    let source = |table: &str, column: &str| ColumnSource {
        catalog: Some("mobility".to_string()),
        schema: Some("gold".to_string()),
        table: Some(table.to_string()),
        column: Some(column.to_string()),
        hidden: false,
    };
    ColumnLineage {
        columns: vec![
            ColumnLineageEntry {
                column: "total_trips".to_string(),
                sources: vec![source("kpis", "total_trips")],
                last_seen: "2026-01-01T00:00:00Z".to_string(),
            },
            ColumnLineageEntry {
                column: "active_days".to_string(),
                sources: vec![source("kpis", "active_days")],
                last_seen: "2026-01-01T00:00:00Z".to_string(),
            },
            ColumnLineageEntry {
                column: "total_revenue".to_string(),
                sources: vec![source("kpis", "total_revenue")],
                last_seen: "2026-01-01T00:00:00Z".to_string(),
            },
        ],
    }
}

#[utoipa::path(
    get,
    operation_id = "lineage_run_detail",
    path = "/api/lineage/runs/{run_id}",
    tag = "public",
    params(
        ("run_id" = String, Path, description = "Run ID")
    ),
    responses(
        (status = 200, description = "Run detail"),
        (status = 404, description = "Not found")
    )
)]
pub async fn run_detail(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(run_id): Path<String>,
) -> Response {
    let principal = principal.map(|Extension(p)| p);
    if state.demo {
        return Json(serde_json::json!({
            "run_id": run_id,
            "job_namespace": "default",
            "job_name": "demo_job",
            "event_type": "COMPLETE",
            "event_time": "2026-01-01T00:00:00Z",
            "inputs": [{"namespace": "nyc_taxi", "name": "yellow_trips"}],
            "outputs": [{"namespace": "analytics", "name": "trips_by_borough"}]
        }))
        .into_response();
    }

    let pool = match &state.db {
        Some(p) => p,
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "state store unavailable"),
    };

    let events: Vec<(String, String, String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT job_namespace, job_name, event_type, to_char(event_time, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as event_time, payload FROM lineage_events WHERE run_id::text = $1 ORDER BY event_time ASC"
    )
    .bind(&run_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if events.is_empty() {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }

    let allowed = match catalog::allowed_catalogs(&state, principal.as_ref()).await {
        Ok(a) => a,
        Err(_) => return error_response(StatusCode::BAD_GATEWAY, "could not resolve catalogs"),
    };

    // Check if run only touches hidden datasets
    let mut has_any_dataset = false;
    let mut any_visible_dataset = false;
    for (_jns, _jn, _et, _etm, payload) in &events {
        if let Some(inputs) = payload.get("inputs").and_then(|v| v.as_array()) {
            if !inputs.is_empty() {
                has_any_dataset = true;
            }
            for inp in inputs {
                if let Some(ns) = inp.get("namespace").and_then(|v| v.as_str()) {
                    let cat = ns.split('.').next().unwrap_or("");
                    if allowed.is_empty() || allowed.contains(cat) {
                        any_visible_dataset = true;
                    }
                }
            }
        }
        if let Some(outputs) = payload.get("outputs").and_then(|v| v.as_array()) {
            if !outputs.is_empty() {
                has_any_dataset = true;
            }
            for out in outputs {
                if let Some(ns) = out.get("namespace").and_then(|v| v.as_str()) {
                    let cat = ns.split('.').next().unwrap_or("");
                    if allowed.is_empty() || allowed.contains(cat) {
                        any_visible_dataset = true;
                    }
                }
            }
        }
    }
    if !allowed.is_empty() && has_any_dataset && !any_visible_dataset {
        return error_response(StatusCode::NOT_FOUND, "not found");
    }

    let first = &events[0];
    let mut inputs = vec![];
    let mut outputs = vec![];
    for (_jns, _jn, _et, _etm, payload) in &events {
        if let Some(ins) = payload.get("inputs").and_then(|v| v.as_array()) {
            for inp in ins {
                if let (Some(ns), Some(name)) = (
                    inp.get("namespace").and_then(|v| v.as_str()),
                    inp.get("name").and_then(|v| v.as_str()),
                ) {
                    inputs.push(DatasetRef {
                        namespace: ns.to_string(),
                        name: name.to_string(),
                    });
                }
            }
        }
        if let Some(outs) = payload.get("outputs").and_then(|v| v.as_array()) {
            for out in outs {
                if let (Some(ns), Some(name)) = (
                    out.get("namespace").and_then(|v| v.as_str()),
                    out.get("name").and_then(|v| v.as_str()),
                ) {
                    outputs.push(DatasetRef {
                        namespace: ns.to_string(),
                        name: name.to_string(),
                    });
                }
            }
        }
    }

    Json(serde_json::json!({
        "run_id": run_id,
        "job_namespace": first.0,
        "job_name": first.1,
        "event_type": first.2,
        "event_time": first.3,
        "inputs": inputs,
        "outputs": outputs,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn source(catalog: &str, schema: &str, table: &str, column: &str) -> serde_json::Value {
        serde_json::json!({ "catalog": catalog, "schema": schema, "table": table, "columnName": column })
    }

    fn column(name: &str, sources: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "columnName": name, "columnType": "bigint", "sourceColumns": sources })
    }

    #[test]
    fn merge_keeps_one_entry_per_column_deduplicates_sources_and_the_newest_time() {
        // Newest first, as the SQL orders them.
        let rows = vec![
            (
                serde_json::json!([column(
                    "total_trips",
                    vec![source("mobility", "silver", "yellow_trips", "trip_id")]
                )]),
                "2026-11-09T10:00:00Z".to_string(),
            ),
            (
                serde_json::json!([column(
                    "total_trips",
                    vec![
                        source("mobility", "silver", "yellow_trips", "trip_id"),
                        source("mobility", "silver", "yellow_trips", "zone_id"),
                    ]
                )]),
                "2026-11-08T09:00:00Z".to_string(),
            ),
        ];
        let merged = merge_columns(&rows, &allowed(&["mobility"]));
        assert_eq!(merged.len(), 1, "one entry per columnName");
        assert_eq!(merged[0].column, "total_trips");
        assert_eq!(merged[0].last_seen, "2026-11-09T10:00:00Z");
        assert_eq!(
            merged[0].sources.len(),
            2,
            "trip_id appears in both events once"
        );
        assert_eq!(merged[0].sources[0].column.as_deref(), Some("trip_id"));
        assert_eq!(merged[0].sources[1].column.as_deref(), Some("zone_id"));
    }

    #[test]
    fn a_source_in_a_catalog_outside_the_allowed_set_is_hidden() {
        let rows = vec![(
            serde_json::json!([column(
                "total_trips",
                vec![
                    source("mobility", "silver", "yellow_trips", "trip_id"),
                    source("secret", "silver", "hidden_trips", "trip_id"),
                ]
            )]),
            "2026-11-09T10:00:00Z".to_string(),
        )];
        let merged = merge_columns(&rows, &allowed(&["mobility"]));
        assert_eq!(merged[0].sources.len(), 2);
        let hidden: Vec<&ColumnSource> = merged[0].sources.iter().filter(|s| s.hidden).collect();
        assert_eq!(hidden.len(), 1);
        assert!(hidden[0].catalog.is_none(), "no names on a hidden source");
        assert!(hidden[0].schema.is_none());
        assert!(hidden[0].table.is_none());
        assert!(hidden[0].column.is_none());
        let json = serde_json::to_value(hidden[0]).unwrap();
        assert_eq!(json, serde_json::json!({ "hidden": true }));
    }

    fn run(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
        rows.iter()
            .map(|(direction, ns, name)| (direction.to_string(), ns.to_string(), name.to_string()))
            .collect()
    }

    /// The level of every dataset the walk reaches from `focus`, and the focus at 0.
    fn levels(focus: (&str, &str), runs: &[Vec<(String, String, String)>]) -> HashMap<String, i32> {
        let mut levels: HashMap<String, i32> = HashMap::new();
        levels.insert(format!("{}|{}", focus.0, focus.1), 0);
        for datasets in runs {
            if !datasets
                .iter()
                .any(|(_, ns, name)| ns == focus.0 && name == focus.1)
            {
                continue;
            }
            for (ns, name, level) in run_neighbours(datasets, focus.0, focus.1, 0) {
                levels.entry(format!("{ns}|{name}")).or_insert(level);
            }
        }
        levels
    }

    #[test]
    fn levels_along_a_chain_are_negative_upstream_zero_at_the_focus_and_positive_downstream() {
        // Fixture chain a → b → c, focus b: a = -1, b = 0, c = 1.
        let runs = vec![
            run(&[
                ("input", "mobility.silver", "a"),
                ("output", "mobility.gold", "b"),
            ]),
            run(&[
                ("input", "mobility.gold", "b"),
                ("output", "mobility.gold", "c"),
            ]),
        ];
        let levels = levels(("mobility.gold", "b"), &runs);
        assert_eq!(levels.get("mobility.silver|a"), Some(&-1), "a is upstream");
        assert_eq!(levels.get("mobility.gold|b"), Some(&0), "b is the focus");
        assert_eq!(levels.get("mobility.gold|c"), Some(&1), "c is downstream");
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn an_edge_older_than_the_window_is_not_returned() {
        use crate::db::test_support::migrated_pool;
        let pool = migrated_pool().await;
        let old_run = "00000000-0000-0000-0000-00000000b001";
        let new_run = "00000000-0000-0000-0000-00000000b002";
        for (run_id, age_days) in [(old_run, 90i32), (new_run, 0i32)] {
            sqlx::query("DELETE FROM lineage_edges WHERE run_id = $1::uuid")
                .bind(run_id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO lineage_edges (run_id, job_namespace, job_name, dataset_namespace, dataset_name, direction, event_time) \
                 VALUES ($1::uuid, 'pipeline', 'window-job', 'mobility.gold', 'kpis', 'output', now() - make_interval(days => $2::int))",
            )
            .bind(run_id)
            .bind(age_days)
            .execute(&pool)
            .await
            .unwrap();
        }

        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(EDGES_SQL)
            .bind(30i32)
            .fetch_all(&pool)
            .await
            .unwrap();
        let seen: Vec<&str> = rows
            .iter()
            .map(|r| r.0.as_str())
            .filter(|r| *r == old_run || *r == new_run)
            .collect();
        assert_eq!(
            seen,
            vec![new_run],
            "the 90-day-old edge is outside the 30-day window"
        );

        for run_id in [old_run, new_run] {
            sqlx::query("DELETE FROM lineage_edges WHERE run_id = $1::uuid")
                .bind(run_id)
                .execute(&pool)
                .await
                .unwrap();
        }
    }
}
