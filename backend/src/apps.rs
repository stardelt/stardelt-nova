//! Platform app launcher: which services are running, and where to reach them.
//!
//! A small, honest inventory. Each app gets an external URL (for deep-linking
//! from Nova) and an internal health probe so the UI can show what is actually
//! up rather than what is merely configured.

use std::sync::Arc;
use std::time::Duration;

use axum::{Json, extract::State};
use serde::Serialize;

use crate::AppState;
use crate::cluster;

#[derive(Serialize, Clone)]
pub struct PlatformApp {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    /// Reachable-from-the-browser URL (empty when no base domain is configured).
    pub external_url: String,
    /// up | unreachable | unknown
    pub status: String,
    /// Images the app's workloads actually run, read from the cluster. Empty when
    /// there is no cluster to read (demo mode, local run) or the app is not installed.
    pub images: Vec<String>,
}

struct Spec {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    category: &'static str,
    /// Subdomain under the base domain, e.g. `superset`.
    sub: &'static str,
    /// In-cluster URL probed for liveness.
    internal: &'static str,
    /// Workload name prefixes that make up the app — how the app's images are found.
    workload_prefixes: &'static [&'static str],
}

const SPECS: &[Spec] = &[
    Spec {
        id: "nova",
        name: "Nova",
        description: "Control plane: catalog, SQL, load & transfer",
        category: "Experience",
        sub: "",
        internal: "http://nova:8080/api/health",
        workload_prefixes: &["nova"],
    },
    Spec {
        id: "trino",
        name: "Trino",
        description: "Distributed SQL engine (Iceberg)",
        category: "Query",
        sub: "trino",
        internal: "http://trino:8080/v1/info",
        workload_prefixes: &["trino-"],
    },
    Spec {
        id: "lakekeeper",
        name: "Lakekeeper",
        description: "Iceberg REST catalog",
        category: "Catalog",
        sub: "lakekeeper",
        internal: "http://lakekeeper:8181/health",
        workload_prefixes: &["lakekeeper"],
    },
    Spec {
        id: "superset",
        name: "Superset",
        description: "BI & dashboards",
        category: "Analytics",
        sub: "superset",
        internal: "http://superset:8088/health",
        workload_prefixes: &["superset"],
    },
    Spec {
        id: "airflow",
        name: "Airflow",
        description: "Workflow orchestration",
        category: "Pipelines",
        sub: "airflow",
        internal: "http://airflow-api-server:8080/health",
        workload_prefixes: &["airflow-"],
    },
    Spec {
        id: "jupyterhub",
        name: "JupyterHub",
        description: "Notebook workspaces",
        category: "Engineering",
        sub: "jupyter",
        internal: "http://proxy-public:80",
        workload_prefixes: &["hub", "proxy"],
    },
];

fn external_url(base: &str, sub: &str) -> String {
    if base.is_empty() {
        return String::new();
    }
    if sub.is_empty() {
        format!("https://{base}")
    } else {
        format!("https://{sub}.{base}")
    }
}

pub async fn list(State(s): State<Arc<AppState>>) -> Json<Vec<PlatformApp>> {
    let base = std::env::var("NOVA_BASE_DOMAIN").unwrap_or_default();
    let snapshot = s.workloads.snapshot(s.kube.as_ref(), s.demo).await;
    let mut apps = Vec::with_capacity(SPECS.len());

    for spec in SPECS {
        let status = probe(&s, spec.internal).await;
        apps.push(PlatformApp {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            category: spec.category.to_string(),
            external_url: external_url(&base, spec.sub),
            status,
            images: cluster::images_for(&snapshot, spec.workload_prefixes),
        });
    }

    Json(apps)
}

async fn probe(state: &AppState, url: &str) -> String {
    if state.demo {
        return "up".into();
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .unwrap_or_else(|_| state.http.clone());
    match client.get(url).send().await {
        Ok(r) => {
            if r.status().as_u16() < 500 {
                "up".into()
            } else {
                "unreachable".into()
            }
        }
        Err(_) => "unreachable".into(),
    }
}
