//! Lineage graph API (D40, WP-34a).

use std::collections::{HashMap, HashSet};
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

#[utoipa::path(
    get,
    operation_id = "lineage_graph",
    path = "/api/lineage",
    tag = "public",
    params(
        ("catalog" = Option<String>, Query, description = "Catalog"),
        ("schema" = Option<String>, Query, description = "Schema"),
        ("table" = Option<String>, Query, description = "Table"),
        ("depth" = Option<i32>, Query, description = "Depth 1-5, default 2")
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

    // Load all edges
    let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT run_id::text, job_namespace, job_name, dataset_namespace, dataset_name, direction FROM lineage_edges"
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // Get event times
    let times: Vec<(String, String)> = sqlx::query_as(
        "SELECT run_id::text, to_char(MAX(event_time), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM lineage_edges GROUP BY run_id"
    )
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
        },
    );

    let mut to_visit = vec![(start_ns.clone(), start_name.clone(), 0i32, start_id.clone())];
    let mut visited = HashSet::new();
    visited.insert((start_ns.clone(), start_name.clone()));

    while let Some((cur_ns, cur_name, d, cur_id)) = to_visit.pop() {
        if d >= depth {
            continue;
        }
        for (run_id, (job_ns, job_name, datasets)) in &runs {
            let has_cur = datasets
                .iter()
                .any(|(_, dns, dname)| dns == &cur_ns && dname == &cur_name);
            if !has_cur {
                continue;
            }
            for (_dir, dns, dname) in datasets {
                if dns == &cur_ns && dname == &cur_name {
                    continue;
                }
                let real_id = dataset_id(dns, dname);
                let ds_catalog = dns.split('.').next().unwrap_or("");
                let is_visible = allowed.is_empty()
                    || allowed.contains(ds_catalog)
                    || (catalog_name.is_some()
                        && allowed
                            .iter()
                            .any(|a| dns.starts_with(&format!("{a}.")) || dns == a));
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
                    if !is_visible {
                        result_nodes.insert(
                            nds_id.clone(),
                            LineageNode {
                                id: nds_id.clone(),
                                namespace: String::new(),
                                name: String::new(),
                                visible: false,
                                label: Some(hidden_label()),
                            },
                        );
                    } else {
                        result_nodes.insert(
                            nds_id.clone(),
                            LineageNode {
                                id: nds_id.clone(),
                                namespace: dns.clone(),
                                name: dname.clone(),
                                visible: true,
                                label: None,
                            },
                        );
                    }
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

    let graph = LineageGraph {
        nodes: result_nodes.into_values().collect(),
        edges: result_edges,
    };
    (StatusCode::OK, Json(graph)).into_response()
}

fn demo_graph_response(_start_ns: &str, _start_name: &str) -> Response {
    let graph = LineageGraph {
        nodes: vec![
            LineageNode {
                id: "nyc_taxi|yellow_trips".to_string(),
                namespace: "nyc_taxi".to_string(),
                name: "yellow_trips".to_string(),
                visible: true,
                label: None,
            },
            LineageNode {
                id: "analytics|trips_by_borough".to_string(),
                namespace: "analytics".to_string(),
                name: "trips_by_borough".to_string(),
                visible: true,
                label: None,
            },
            LineageNode {
                id: "analytics|daily_revenue".to_string(),
                namespace: "analytics".to_string(),
                name: "daily_revenue".to_string(),
                visible: true,
                label: None,
            },
        ],
        edges: vec![
            LineageEdge {
                from: "nyc_taxi|yellow_trips".to_string(),
                to: "analytics|trips_by_borough".to_string(),
                job_namespace: "default".to_string(),
                job_name: "trips_by_borough_job".to_string(),
                run_id: "00000000-0000-0000-0000-000000000001".to_string(),
                event_time: "2026-01-01T00:00:00Z".to_string(),
            },
            LineageEdge {
                from: "nyc_taxi|yellow_trips".to_string(),
                to: "analytics|daily_revenue".to_string(),
                job_namespace: "default".to_string(),
                job_name: "daily_revenue_job".to_string(),
                run_id: "00000000-0000-0000-0000-000000000002".to_string(),
                event_time: "2026-01-01T00:00:00Z".to_string(),
            },
        ],
    };
    (StatusCode::OK, Json(graph)).into_response()
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
