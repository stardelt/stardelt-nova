//! Lineage graph API (D40, WP-34a).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
    /// True when every used flow that touches this node was a hand-made (ad-hoc) job.
    #[serde(rename = "handMade")]
    pub hand_made: bool,
    /// Read-only queries against this node in the window (visible nodes only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readers: Option<LineageReaders>,
    /// The jobs that wrote this node, newest first; only on the focus node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writers: Option<Vec<LineageWriter>>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineageReaders {
    pub queries: i64,
    /// Distinct people: the SQL editor and notebooks, never a BI tool.
    pub people: i64,
    pub last_read: String,
    /// The tools that read this node, most queries first, at most 5.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
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
    #[serde(rename = "handMade")]
    pub hand_made: bool,
}

/// One job that wrote the focus table, with its runs in the window.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineageWriter {
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
    /// The runs of this job that wrote this edge in the window.
    pub runs: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LineageGraph {
    pub nodes: Vec<LineageNode>,
    pub edges: Vec<LineageEdge>,
    pub jobs: Vec<LineageJob>,
    pub impact: LineageImpact,
}

/// The whole closure, at any number of steps (nova#646 L8a). The drawn graph keeps `depth`;
/// these numbers answer "what breaks if I change this table".
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineageImpact {
    /// Distinct visible datasets reachable downstream at any number of steps; the focus is not one.
    pub downstream: i64,
    /// The same upstream.
    pub upstream: i64,
    /// Distinct jobs (`(job_ns, job)`) on the downstream flows.
    pub downstream_jobs: i64,
}

/// One table a person read most, with its newest write.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadTable {
    /// `<catalog>.<schema>` (demo data may carry the schema alone).
    pub namespace: String,
    pub name: String,
    /// Read-only queries against it in the window.
    pub queries: i64,
    pub last_read: String,
    /// The newest write in the window; absent when nothing wrote it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_write: Option<String>,
}

/// `GET /api/lineage/read-tables`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReadTables {
    pub tables: Vec<ReadTable>,
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
const EDGES_SQL: &str = "SELECT run_id::text, job_namespace, job_name, dataset_namespace, dataset_name, direction, \
       to_char(event_time, 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_edges \
     WHERE event_time > now() - make_interval(days => $1::int)";

/// Readers of the visible datasets in the window (lineage L2, stardelt-nova#647).
const READERS_SQL: &str = "SELECT dataset_namespace, dataset_name, job_namespace, job_name, sum(queries), \
       to_char(max(last_read), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_reads \
     WHERE day > (now() - make_interval(days => $1::int))::date \
       AND job_namespace IN ('sql-editor','query','notebook') \
       AND (dataset_namespace, dataset_name) IN (SELECT * FROM unnest($2::text[], $3::text[])) \
     GROUP BY 1, 2, 3, 4";

/// The datasets one person read most in the window, with the newest read (nova#510 slice 4a).
/// `$1` is the engine's name for the caller, as `query_log_api` reads it: the person's sub once
/// the platform forwards identity, and the sign-in name for older rows and in dev mode.
const TOP_READS_SQL: &str = "SELECT dataset_namespace, dataset_name, sum(queries)::bigint, \
       to_char(max(last_read), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_reads \
     WHERE job_namespace IN ('sql-editor','notebook') \
       AND job_name = ANY($1::text[]) \
       AND day > (now() - make_interval(days => $2::int))::date \
     GROUP BY 1, 2 \
     ORDER BY 3 DESC, 1, 2 \
     LIMIT $3";

/// The newest write of the given datasets in the window. `$1`/`$2` are the namespace/name arrays.
const LAST_WRITES_SQL: &str = "SELECT dataset_namespace, dataset_name, \
       to_char(max(event_time), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
     FROM lineage_edges \
     WHERE direction = 'output' \
       AND (dataset_namespace, dataset_name) IN (SELECT * FROM unnest($1::text[], $2::text[])) \
     GROUP BY 1, 2";

/// One `lineage_edges` row: a dataset of one run, in or out.
#[derive(Debug, Clone)]
pub(crate) struct EdgeRow {
    pub run: String,
    pub job_ns: String,
    pub job: String,
    pub ns: String,
    pub name: String,
    pub output: bool,
    pub time: String,
}

/// A dataset pair one job wrote, with the runs that wrote it in the window.
struct Flow {
    from: (String, String),
    to: (String, String),
    job_ns: String,
    job: String,
    runs: HashSet<String>,
    last: String,
    last_run: String,
}

type Dataset = (String, String);

fn dataset(ns: &str, name: &str) -> Dataset {
    (ns.to_string(), name.to_string())
}

/// A job namespace as the answer names it: a query id's namespace is a query, not a job kind.
fn job_kind(job_ns: &str) -> String {
    if job_ns == "trino" {
        "query".to_string()
    } else {
        job_ns.to_string()
    }
}

/// Every dataset reachable from `focus` along the flows, at any number of steps. `forward`
/// follows `from → to` (downstream), otherwise `to → from`. The focus is not in the result
/// unless a cycle leads back to it.
fn reachable(flows: &[Flow], focus: &Dataset, forward: bool) -> HashSet<Dataset> {
    let mut seen: HashSet<Dataset> = HashSet::new();
    let mut queue: Vec<Dataset> = vec![focus.clone()];
    while let Some(node) = queue.pop() {
        for flow in flows {
            let next = if forward {
                if flow.from != node {
                    continue;
                }
                &flow.to
            } else {
                if flow.to != node {
                    continue;
                }
                &flow.from
            };
            if seen.insert(next.clone()) {
                queue.push(next.clone());
            }
        }
    }
    seen
}

/// Build the answer from the window's edge rows. Data flows `from` → `to`; `level` is 0 at
/// the focus, negative upstream, positive downstream.
pub(crate) fn build_graph(
    rows: &[EdgeRow],
    focus: (&str, &str),
    depth: i32,
    visible: &dyn Fn(&str) -> bool,
) -> LineageGraph {
    // One flow per (input, output, job): 30 daily runs of one job are one flow.
    let mut by_run: BTreeMap<&str, (Vec<&EdgeRow>, Vec<&EdgeRow>)> = BTreeMap::new();
    for row in rows {
        let entry = by_run.entry(row.run.as_str()).or_default();
        if row.output {
            entry.1.push(row);
        } else {
            entry.0.push(row);
        }
    }

    let mut flows: Vec<Flow> = Vec::new();
    let mut flow_index: HashMap<(Dataset, Dataset, String, String), usize> = HashMap::new();
    for (run, (inputs, outputs)) in &by_run {
        for i in inputs {
            for o in outputs {
                if i.ns == o.ns && i.name == o.name {
                    continue;
                }
                let key = (
                    dataset(&i.ns, &i.name),
                    dataset(&o.ns, &o.name),
                    i.job_ns.clone(),
                    i.job.clone(),
                );
                let at = match flow_index.get(&key) {
                    Some(&at) => at,
                    None => {
                        flow_index.insert(key.clone(), flows.len());
                        flows.push(Flow {
                            from: key.0,
                            to: key.1,
                            job_ns: key.2,
                            job: key.3,
                            runs: HashSet::new(),
                            last: String::new(),
                            last_run: String::new(),
                        });
                        flows.len() - 1
                    }
                };
                let flow = &mut flows[at];
                flow.runs.insert((*run).to_string());
                let time = if i.time >= o.time { &i.time } else { &o.time };
                if flow.last.is_empty() || *time > flow.last {
                    flow.last = time.clone();
                    flow.last_run = (*run).to_string();
                }
            }
        }
    }

    // The walks. A node keeps the level of the first path that reaches it; both walks share
    // the levels, so neither turns around at a node the other one found.
    let focus_ds = dataset(focus.0, focus.1);
    let mut levels: HashMap<Dataset, i32> = HashMap::new();
    levels.insert(focus_ds.clone(), 0);
    let mut order: Vec<Dataset> = vec![focus_ds.clone()];
    let mut used: HashSet<usize> = HashSet::new();

    let mut frontier = vec![focus_ds.clone()];
    while !frontier.is_empty() {
        let mut next: Vec<Dataset> = Vec::new();
        for node in &frontier {
            let level = levels[node];
            if level - 1 < -depth {
                continue;
            }
            for (at, flow) in flows.iter().enumerate() {
                if flow.to != *node {
                    continue;
                }
                if levels.contains_key(&flow.from) {
                    continue;
                }
                used.insert(at);
                levels.insert(flow.from.clone(), level - 1);
                order.push(flow.from.clone());
                next.push(flow.from.clone());
            }
        }
        frontier = next;
    }

    let mut frontier = vec![focus_ds.clone()];
    while !frontier.is_empty() {
        let mut next: Vec<Dataset> = Vec::new();
        for node in &frontier {
            let level = levels[node];
            if level + 1 > depth {
                continue;
            }
            for (at, flow) in flows.iter().enumerate() {
                if flow.from != *node {
                    continue;
                }
                if levels.contains_key(&flow.to) {
                    continue;
                }
                used.insert(at);
                levels.insert(flow.to.clone(), level + 1);
                order.push(flow.to.clone());
                next.push(flow.to.clone());
            }
        }
        frontier = next;
    }

    // The whole closure, at any number of steps. The drawn graph keeps `depth`; these numbers
    // answer "what breaks if I change this table" (nova#646 L8a). Hidden datasets are not
    // counted: the number must not reveal tables the caller cannot see.
    let downstream_seen = reachable(&flows, &focus_ds, true);
    let upstream_seen = reachable(&flows, &focus_ds, false);
    let count = |seen: &HashSet<Dataset>| -> i64 {
        seen.iter()
            .filter(|ds| **ds != focus_ds && visible(&ds.0))
            .count() as i64
    };
    let mut downstream_jobs: HashSet<(&str, &str)> = HashSet::new();
    for flow in &flows {
        if flow.from == focus_ds || downstream_seen.contains(&flow.from) {
            downstream_jobs.insert((flow.job_ns.as_str(), flow.job.as_str()));
        }
    }
    let impact = LineageImpact {
        downstream: count(&downstream_seen),
        upstream: count(&upstream_seen),
        downstream_jobs: downstream_jobs.len() as i64,
    };

    // The newest write that reached each dataset, from every row of the window.
    let mut last_writes: HashMap<Dataset, String> = HashMap::new();
    for row in rows.iter().filter(|r| r.output) {
        let entry = last_writes.entry(dataset(&row.ns, &row.name)).or_default();
        if row.time > *entry {
            *entry = row.time.clone();
        }
    }

    // Nodes, focus first, in the order the walks found them.
    let mut nodes: Vec<LineageNode> = Vec::new();
    let mut hidden_ids: HashMap<Dataset, String> = HashMap::new();
    let mut hidden_seq: usize = 0;
    for ds in &order {
        let is_visible = ds == &focus_ds || visible(&ds.0);
        let id = if is_visible {
            dataset_id(&ds.0, &ds.1)
        } else {
            hidden_ids.entry(ds.clone()).or_insert_with(|| {
                hidden_seq += 1;
                format!("hidden-{hidden_seq}")
            });
            hidden_ids[ds].clone()
        };
        let hand_made = if ds == &focus_ds {
            false
        } else {
            used.iter().all(|&at| {
                let flow = &flows[at];
                (flow.from != *ds && flow.to != *ds) || flow.job_ns != "pipeline"
            })
        };
        let (namespace, name, label) = if is_visible {
            (ds.0.clone(), ds.1.clone(), None)
        } else {
            (String::new(), String::new(), Some(hidden_label()))
        };
        nodes.push(LineageNode {
            id,
            namespace,
            name,
            visible: is_visible,
            label,
            level: if is_visible { Some(levels[ds]) } else { None },
            last_write: if is_visible {
                last_writes.get(ds).cloned()
            } else {
                None
            },
            hand_made,
            readers: None,
            writers: None,
        });
    }

    // One edge per used flow.
    let id_of = |ds: &Dataset| -> String {
        match hidden_ids.get(ds) {
            Some(id) => id.clone(),
            None => dataset_id(&ds.0, &ds.1),
        }
    };
    let mut edges: Vec<LineageEdge> = Vec::new();
    for (at, flow) in flows.iter().enumerate() {
        if !used.contains(&at) {
            continue;
        }
        edges.push(LineageEdge {
            from: id_of(&flow.from),
            to: id_of(&flow.to),
            job_namespace: flow.job_ns.clone(),
            job_name: flow.job.clone(),
            run_id: flow.last_run.clone(),
            event_time: flow.last.clone(),
            runs: flow.runs.len() as i64,
        });
    }

    // One entry per job of the used flows.
    let mut jobs: Vec<LineageJob> = Vec::new();
    let mut job_index: HashMap<(String, String), usize> = HashMap::new();
    let mut job_runs: Vec<HashSet<String>> = Vec::new();
    for (at, flow) in flows.iter().enumerate() {
        if !used.contains(&at) {
            continue;
        }
        let key = (job_kind(&flow.job_ns), flow.job.clone());
        let at_idx = match job_index.get(&key) {
            Some(&idx) => idx,
            None => {
                job_index.insert(key.clone(), jobs.len());
                job_runs.push(HashSet::new());
                jobs.push(LineageJob {
                    id: format!("{}|{}", key.0, key.1),
                    kind: key.0.clone(),
                    name: key.1.clone(),
                    runs: 0,
                    last_run: flow.last.clone(),
                    hand_made: key.0 != "pipeline",
                });
                jobs.len() - 1
            }
        };
        for run in &flow.runs {
            job_runs[at_idx].insert(run.clone());
        }
        if flow.last > jobs[at_idx].last_run {
            jobs[at_idx].last_run = flow.last.clone();
        }
    }
    for (job, runs) in jobs.iter_mut().zip(job_runs) {
        job.runs = runs.len() as i64;
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));

    // The jobs that wrote the focus, pipeline first.
    let mut writers: Vec<LineageWriter> = Vec::new();
    let mut writer_index: HashMap<(String, String), usize> = HashMap::new();
    let mut writer_runs: Vec<HashSet<String>> = Vec::new();
    for row in rows
        .iter()
        .filter(|r| r.output && r.ns == focus.0 && r.name == focus.1)
    {
        let key = (job_kind(&row.job_ns), row.job.clone());
        let at = match writer_index.get(&key) {
            Some(&at) => at,
            None => {
                writer_index.insert(key.clone(), writers.len());
                writer_runs.push(HashSet::new());
                writers.push(LineageWriter {
                    kind: key.0.clone(),
                    name: key.1.clone(),
                    runs: 0,
                    last_run: row.time.clone(),
                });
                writers.len() - 1
            }
        };
        writer_runs[at].insert(row.run.clone());
        if row.time > writers[at].last_run {
            writers[at].last_run = row.time.clone();
        }
    }
    for (writer, runs) in writers.iter_mut().zip(writer_runs) {
        writer.runs = runs.len() as i64;
    }
    writers.sort_by(|a, b| {
        (a.kind != "pipeline")
            .cmp(&(b.kind != "pipeline"))
            .then(b.runs.cmp(&a.runs))
            .then(b.last_run.cmp(&a.last_run))
    });
    if let Some(focus_node) = nodes.first_mut() {
        focus_node.writers = Some(writers);
    }

    LineageGraph {
        nodes,
        edges,
        jobs,
        impact,
    }
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
        None => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Nova's database is not answering; try again in a minute",
            );
        }
    };

    let rows: Vec<(String, String, String, String, String, String, String)> =
        sqlx::query_as(EDGES_SQL)
            .bind(days)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let edge_rows: Vec<EdgeRow> = rows
        .into_iter()
        .map(|(run, job_ns, job, ns, name, direction, time)| EdgeRow {
            run,
            job_ns,
            job,
            ns,
            name,
            output: direction == "output",
            time,
        })
        .collect();

    let is_visible = |ns: &str| -> bool {
        let ds_catalog = ns.split('.').next().unwrap_or("");
        allowed.is_empty()
            || allowed.contains(ds_catalog)
            || (catalog_name.is_some()
                && allowed
                    .iter()
                    .any(|a| ns.starts_with(&format!("{a}.")) || ns == a))
    };
    let mut graph = build_graph(&edge_rows, (&start_ns, &start_name), depth, &is_visible);

    // Readers of the visible nodes, in one query.
    let mut read_ns: Vec<String> = Vec::new();
    let mut read_names: Vec<String> = Vec::new();
    for node in graph.nodes.iter().filter(|n| n.visible) {
        read_ns.push(node.namespace.clone());
        read_names.push(node.name.clone());
    }
    if !read_ns.is_empty() {
        struct ReadAcc {
            queries: i64,
            people: HashSet<String>,
            tools: HashMap<String, i64>,
            last_read: String,
        }
        let reads: Vec<(String, String, String, String, i64, String)> = sqlx::query_as(READERS_SQL)
            .bind(days)
            .bind(&read_ns)
            .bind(&read_names)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
        let mut accs: HashMap<String, ReadAcc> = HashMap::new();
        for (ns, name, job_ns, job, queries, last_read) in reads {
            let acc = accs
                .entry(dataset_id(&ns, &name))
                .or_insert_with(|| ReadAcc {
                    queries: 0,
                    people: HashSet::new(),
                    tools: HashMap::new(),
                    last_read: String::new(),
                });
            acc.queries += queries;
            match job_ns.as_str() {
                "sql-editor" | "notebook" => {
                    acc.people.insert(job);
                }
                "query" => {
                    *acc.tools.entry(job).or_default() += queries;
                }
                _ => {}
            }
            if last_read > acc.last_read {
                acc.last_read = last_read;
            }
        }
        for (id, acc) in accs {
            let mut tools: Vec<(String, i64)> = acc.tools.into_iter().collect();
            tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let tools: Vec<String> = tools.into_iter().take(5).map(|(name, _)| name).collect();
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == id) {
                node.readers = Some(LineageReaders {
                    queries: acc.queries,
                    people: acc.people.len() as i64,
                    last_read: acc.last_read,
                    tools,
                });
            }
        }
    }

    (StatusCode::OK, Json(graph)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ReadTablesQuery {
    /// Time window in days, 1–365, default 30.
    pub days: Option<i32>,
    /// How many tables to answer, 1–20, default 5.
    pub limit: Option<usize>,
}

#[utoipa::path(
    get,
    operation_id = "lineage_read_tables",
    path = "/api/lineage/read-tables",
    tag = "public",
    params(
        ("days" = Option<i32>, Query, description = "Time window in days, 1–365, default 30"),
        ("limit" = Option<usize>, Query, description = "Tables to answer, 1–20, default 5")
    ),
    responses(
        (status = 200, description = "The tables the caller read most, newest read first", body = ReadTables),
        (status = 503, description = "No state store")
    )
)]
pub async fn read_tables(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(q): Query<ReadTablesQuery>,
) -> Response {
    let principal = principal.map(|Extension(p)| p);
    let days = q.days.unwrap_or(30).clamp(1, 365);
    let limit = q.limit.unwrap_or(5).clamp(1, 20);

    if state.demo {
        return demo_read_tables();
    }

    let allowed = match catalog::allowed_catalogs(&state, principal.as_ref()).await {
        Ok(a) => a,
        Err(_) => return error_response(StatusCode::BAD_GATEWAY, "could not resolve catalogs"),
    };
    let keys: Vec<String> = match principal.as_ref() {
        Some(p) => vec![p.identity.display_name.clone(), p.identity.sub.clone()],
        None => vec![state.dev_user.clone()],
    };

    let pool = match &state.db {
        Some(p) => p,
        None => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Nova's database is not answering; try again in a minute",
            );
        }
    };

    let rows: Vec<(String, String, i64, String)> = sqlx::query_as(TOP_READS_SQL)
        .bind(&keys)
        .bind(days)
        .bind(i64::try_from(limit).unwrap_or(5))
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    // The caller's own visibility rule, the same one the graph uses: a namespace's first
    // segment is the catalog name.
    let visible = |ns: &str| -> bool {
        allowed.is_empty() || allowed.contains(ns.split('.').next().unwrap_or(""))
    };
    let mut tables: Vec<ReadTable> = rows
        .into_iter()
        .filter(|(ns, _, _, _)| visible(ns))
        .map(|(namespace, name, queries, last_read)| ReadTable {
            namespace,
            name,
            queries,
            last_read,
            last_write: None,
        })
        .collect();

    let namespaces: Vec<String> = tables.iter().map(|t| t.namespace.clone()).collect();
    let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
    if !namespaces.is_empty() {
        let writes: Vec<(String, String, String)> = sqlx::query_as(LAST_WRITES_SQL)
            .bind(&namespaces)
            .bind(&names)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
        for (namespace, name, last_write) in writes {
            if let Some(table) = tables
                .iter_mut()
                .find(|t| t.namespace == namespace && t.name == name)
            {
                table.last_write = Some(last_write);
            }
        }
    }

    (StatusCode::OK, Json(ReadTables { tables })).into_response()
}

fn demo_read_tables() -> Response {
    let table = |namespace: &str, name: &str, queries: i64| ReadTable {
        namespace: namespace.to_string(),
        name: name.to_string(),
        queries,
        last_read: "2026-01-01T00:00:00Z".to_string(),
        last_write: Some("2026-01-01T00:00:00Z".to_string()),
    };
    (
        StatusCode::OK,
        Json(ReadTables {
            tables: vec![
                table("nyc_taxi", "yellow_trips", 42),
                table("analytics", "trips_by_borough", 18),
            ],
        }),
    )
        .into_response()
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
                hand_made: false,
                readers: None,
                writers: None,
            },
            LineageNode {
                id: "analytics|trips_by_borough".to_string(),
                namespace: "analytics".to_string(),
                name: "trips_by_borough".to_string(),
                visible: true,
                label: None,
                level: Some(0),
                last_write: last_write.clone(),
                hand_made: false,
                readers: Some(LineageReaders {
                    queries: 12,
                    people: 3,
                    last_read: "2026-01-01T00:00:00Z".to_string(),
                    tools: vec!["supertable".to_string()],
                }),
                writers: Some(vec![LineageWriter {
                    kind: "pipeline".to_string(),
                    name: "trips_by_borough_job".to_string(),
                    runs: 30,
                    last_run: "2026-01-01T00:00:00Z".to_string(),
                }]),
            },
            LineageNode {
                id: "analytics|daily_revenue".to_string(),
                namespace: "analytics".to_string(),
                name: "daily_revenue".to_string(),
                visible: true,
                label: None,
                level: Some(1),
                last_write: last_write.clone(),
                hand_made: false,
                readers: Some(LineageReaders {
                    queries: 5,
                    people: 2,
                    last_read: "2026-01-01T00:00:00Z".to_string(),
                    tools: vec![],
                }),
                writers: None,
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
                runs: 30,
            },
            LineageEdge {
                from: "nyc_taxi|yellow_trips".to_string(),
                to: "analytics|daily_revenue".to_string(),
                job_namespace: "pipeline".to_string(),
                job_name: "daily_revenue_job".to_string(),
                run_id: "00000000-0000-0000-0000-000000000002".to_string(),
                event_time: "2026-01-01T00:00:00Z".to_string(),
                runs: 30,
            },
        ],
        jobs: vec![
            LineageJob {
                id: "pipeline|daily_revenue_job".to_string(),
                kind: "pipeline".to_string(),
                name: "daily_revenue_job".to_string(),
                runs: 1,
                last_run: "2026-01-01T00:00:00Z".to_string(),
                hand_made: false,
            },
            LineageJob {
                id: "pipeline|trips_by_borough_job".to_string(),
                kind: "pipeline".to_string(),
                name: "trips_by_borough_job".to_string(),
                runs: 1,
                last_run: "2026-01-01T00:00:00Z".to_string(),
                hand_made: false,
            },
        ],
        // One table on each side of the focus; the one downstream row is written by one job.
        impact: LineageImpact {
            downstream: 1,
            upstream: 1,
            downstream_jobs: 1,
        },
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
        None => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Nova's database is not answering; try again in a minute",
            );
        }
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
        None => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Nova's database is not answering; try again in a minute",
            );
        }
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

    fn edge(
        run: &str,
        job_ns: &str,
        job: &str,
        ns: &str,
        name: &str,
        output: bool,
        time: &str,
    ) -> EdgeRow {
        EdgeRow {
            run: run.to_string(),
            job_ns: job_ns.to_string(),
            job: job.to_string(),
            ns: ns.to_string(),
            name: name.to_string(),
            output,
            time: time.to_string(),
        }
    }

    /// Three pipelines over one table: 30 load runs, 30 zone-stat runs, 5 ad-hoc queries
    /// that write a scratch table and 2 backfill queries.
    fn fixture() -> Vec<EdgeRow> {
        let mut rows: Vec<EdgeRow> = Vec::new();
        for day in 0..30 {
            let time = format!("2026-10-{:02}T00:00:00Z", day + 1);
            let load = format!("load-{day}");
            let zones = format!("zones-{day}");
            rows.push(edge(
                &load,
                "pipeline",
                "taxi/load-trips",
                "l.landing",
                "raw",
                false,
                &time,
            ));
            rows.push(edge(
                &load,
                "pipeline",
                "taxi/load-trips",
                "l.staging",
                "clean",
                true,
                &time,
            ));
            rows.push(edge(
                &zones,
                "pipeline",
                "taxi/zone-stats",
                "l.staging",
                "clean",
                false,
                &time,
            ));
            rows.push(edge(
                &zones,
                "pipeline",
                "taxi/zone-stats",
                "l.ref",
                "zones",
                false,
                &time,
            ));
            rows.push(edge(
                &zones,
                "pipeline",
                "taxi/zone-stats",
                "l.marts",
                "zone_stats",
                true,
                &time,
            ));
        }
        for i in 0..5 {
            let run = format!("adhoc-{i}");
            let job = format!("q{i}");
            rows.push(edge(
                &run,
                "trino",
                &job,
                "l.staging",
                "clean",
                false,
                "2026-11-01T00:00:00Z",
            ));
            rows.push(edge(
                &run,
                "trino",
                &job,
                "l.scratch",
                "tmp",
                true,
                "2026-11-01T00:00:00Z",
            ));
        }
        for i in 0..2 {
            let run = format!("adhoc2-{i}");
            let job = format!("q2{i}");
            rows.push(edge(
                &run,
                "trino",
                &job,
                "l.landing",
                "raw_2023",
                false,
                "2026-10-30T00:00:00Z",
            ));
            rows.push(edge(
                &run,
                "trino",
                &job,
                "l.staging",
                "clean",
                true,
                "2026-10-30T00:00:00Z",
            ));
        }
        rows
    }

    fn graph_of(rows: &[EdgeRow], focus: (&str, &str), depth: i32) -> LineageGraph {
        build_graph(rows, focus, depth, &|_| true)
    }

    fn node<'a>(graph: &'a LineageGraph, ns: &str, name: &str) -> &'a LineageNode {
        graph
            .nodes
            .iter()
            .find(|n| n.namespace == ns && n.name == name)
            .unwrap_or_else(|| panic!("no node {ns}.{name}"))
    }

    #[test]
    fn edges_point_the_way_data_flows() {
        let graph = graph_of(&fixture(), ("l.staging", "clean"), 2);
        let raw = dataset_id("l.landing", "raw");
        let clean = dataset_id("l.staging", "clean");
        assert!(
            graph.edges.iter().any(|e| e.from == raw && e.to == clean),
            "raw feeds clean"
        );
        assert!(
            !graph.edges.iter().any(|e| e.from == clean && e.to == raw),
            "clean is not fed by its own source"
        );
    }

    #[test]
    fn a_sibling_input_is_not_a_source() {
        let graph = graph_of(&fixture(), ("l.staging", "clean"), 3);
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| n.namespace == "l.ref" && n.name == "zones"),
            "zones is a sibling input of the run, not a source of clean"
        );
    }

    #[test]
    fn runs_of_one_job_make_one_edge() {
        let graph = graph_of(&fixture(), ("l.staging", "clean"), 2);
        let raw = dataset_id("l.landing", "raw");
        let clean = dataset_id("l.staging", "clean");
        let edges: Vec<&LineageEdge> = graph
            .edges
            .iter()
            .filter(|e| e.from == raw && e.to == clean)
            .collect();
        assert_eq!(edges.len(), 1, "30 runs of one job are one edge");
        assert_eq!(edges[0].runs, 30);
        assert!(
            graph.edges.len() <= 4,
            "the answer has at most four edges, found {}",
            graph.edges.len()
        );
    }

    #[test]
    fn the_pipeline_is_the_writer() {
        let graph = graph_of(&fixture(), ("l.staging", "clean"), 2);
        let focus = graph
            .nodes
            .iter()
            .find(|n| n.level == Some(0))
            .expect("focus node");
        let writers = focus.writers.as_ref().expect("the focus has writers");
        assert_eq!(writers[0].kind, "pipeline");
        assert_eq!(writers[0].name, "taxi/load-trips");
        assert_eq!(writers[0].runs, 30);
        assert!(
            writers.iter().skip(1).any(|w| w.kind == "query"),
            "an ad-hoc query is listed after the pipeline"
        );
    }

    #[test]
    fn ad_hoc_tables_are_marked() {
        let graph = graph_of(&fixture(), ("l.staging", "clean"), 2);
        assert!(
            node(&graph, "l.scratch", "tmp").hand_made,
            "a query-created table is hand-made"
        );
        assert!(
            !node(&graph, "l.marts", "zone_stats").hand_made,
            "a pipeline-written table is not hand-made"
        );
    }

    #[test]
    fn levels_along_a_chain_are_negative_upstream_zero_at_the_focus_and_positive_downstream() {
        // Fixture chain a → b → c, focus b: a = -1, b = 0, c = 1.
        let rows = vec![
            edge(
                "1",
                "pipeline",
                "j1",
                "mobility.silver",
                "a",
                false,
                "2026-11-01T00:00:00Z",
            ),
            edge(
                "1",
                "pipeline",
                "j1",
                "mobility.gold",
                "b",
                true,
                "2026-11-01T00:00:00Z",
            ),
            edge(
                "2",
                "pipeline",
                "j2",
                "mobility.gold",
                "b",
                false,
                "2026-11-01T00:00:00Z",
            ),
            edge(
                "2",
                "pipeline",
                "j2",
                "mobility.gold",
                "c",
                true,
                "2026-11-01T00:00:00Z",
            ),
        ];
        let graph = graph_of(&rows, ("mobility.gold", "b"), 2);
        assert_eq!(
            node(&graph, "mobility.silver", "a").level,
            Some(-1),
            "a is upstream"
        );
        assert_eq!(
            node(&graph, "mobility.gold", "b").level,
            Some(0),
            "b is the focus"
        );
        assert_eq!(
            node(&graph, "mobility.gold", "c").level,
            Some(1),
            "c is downstream"
        );
    }

    #[test]
    fn the_impact_counts_every_step_not_only_the_drawn_ones() {
        // A seven-step chain d0 → d1 → … → d6, focus d0: the drawing stops at depth 2,
        // the impact counts the six tables beyond it.
        let mut rows: Vec<EdgeRow> = Vec::new();
        for i in 0..6 {
            let run = format!("r{i}");
            let job = format!("j{i}");
            rows.push(edge(
                &run,
                "pipeline",
                &job,
                "mobility.gold",
                &format!("d{i}"),
                false,
                "2026-11-01T00:00:00Z",
            ));
            rows.push(edge(
                &run,
                "pipeline",
                &job,
                "mobility.gold",
                &format!("d{}", i + 1),
                true,
                "2026-11-01T00:00:00Z",
            ));
        }
        let graph = build_graph(&rows, ("mobility.gold", "d0"), 2, &|_| true);
        let drawn = graph
            .nodes
            .iter()
            .filter(|n| n.level.unwrap_or(0) > 0)
            .count();
        assert_eq!(drawn, 2, "the drawing stops at depth 2");
        assert_eq!(graph.impact.downstream, 6, "six tables are downstream");
        assert_eq!(
            graph.impact.upstream, 0,
            "nothing feeds the head of the chain"
        );
        assert_eq!(graph.impact.downstream_jobs, 6, "one job per step");

        let hidden = build_graph(&rows, ("mobility.gold", "d0"), 2, &|_| false);
        assert_eq!(
            hidden.impact.downstream, 0,
            "a dataset the caller cannot see is not counted"
        );
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
        let rows: Vec<(String, String, String, String, String, String, String)> =
            sqlx::query_as(EDGES_SQL)
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
