//! Platform app launcher: which services are running, and where to reach them.
//!
//! A small, honest inventory. Each app gets an external URL (for deep-linking
//! from Nova) and an internal health probe so the UI can show what is actually
//! up rather than what is merely configured.
//!
//! `GET /api/apps/:id` adds the detail view: the workloads the app runs, facts
//! read from the service's own API, and deep links into the app's UI. Every fact
//! names its source, and a fact that cannot be read is omitted — the detail page
//! is never asked to render a guess.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::cluster::{self, Workload};

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

/// One label/value pair about an app, as Nova can honestly observe it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Fact {
    pub label: String,
    pub value: String,
    /// Where the value comes from, e.g. `Trino /v1/info`. Facts without a source
    /// are those Nova knows from its own configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `ok` | `bad` when the value is a health judgement; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<&'static str>,
}

impl Fact {
    fn new(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            source: None,
            tone: None,
        }
    }

    fn sourced(label: &str, value: impl Into<String>, source: &str) -> Self {
        Self {
            source: Some(source.to_string()),
            ..Self::new(label, value)
        }
    }

    fn with_tone(mut self, tone: &'static str) -> Self {
        self.tone = Some(tone);
        self
    }
}

/// A deep link into an app's own UI.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// `GET /api/apps/:id` — everything Nova can say about one service.
#[derive(Serialize)]
pub struct AppDetail {
    pub app: PlatformApp,
    /// In-cluster URL Nova probes for liveness, shown verbatim: it is the reason
    /// the status is what it is.
    pub probe: String,
    /// Ready/desired replicas across the app's workloads — a scale-to-zero
    /// workload contributes `0/0`, which is a fact, not a failure.
    pub replicas: Replicas,
    /// The app's workloads in Nova's namespace, with their images and replicas.
    pub workloads: Vec<Workload>,
    pub facts: Vec<Fact>,
    /// Deep links into the app's UI; empty when it has none or Nova has no base
    /// domain to build them from.
    pub links: Vec<Link>,
}

#[derive(Serialize)]
pub struct Replicas {
    pub ready: i32,
    pub desired: i32,
}

/// Where Nova probes an app. Trino and Lakekeeper are probed where Nova actually
/// talks to them (`NOVA_TRINO_URL` / `NOVA_LAKEKEEPER_URL`), so the status can
/// never disagree with the connection Nova uses; the rest have a fixed in-cluster
/// address in their spec.
enum Endpoint {
    Fixed(&'static str),
    Trino,
    Lakekeeper,
}

impl Endpoint {
    fn url(&self, trino_url: &str, lakekeeper_url: &str) -> String {
        match self {
            Endpoint::Fixed(url) => (*url).to_string(),
            Endpoint::Trino => format!("{}/v1/info", trino_url.trim_end_matches('/')),
            Endpoint::Lakekeeper => format!("{}/health", lakekeeper_url.trim_end_matches('/')),
        }
    }
}

struct Spec {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    category: &'static str,
    /// Subdomain under the base domain, e.g. `superset`.
    sub: &'static str,
    /// URL probed for liveness.
    probe: Endpoint,
    /// Workload name prefixes that make up the app — how the app's images are found.
    workload_prefixes: &'static [&'static str],
    /// UI paths appended to the external URL, as deep links.
    paths: &'static [(&'static str, &'static str)],
}

const SPECS: &[Spec] = &[
    Spec {
        id: "nova",
        name: "Nova",
        description: "Control plane: catalog, SQL, load & transfer",
        category: "Experience",
        sub: "",
        probe: Endpoint::Fixed("http://nova:8080/api/health"),
        workload_prefixes: &["nova"],
        paths: &[("Overview", "/")],
    },
    Spec {
        id: "trino",
        name: "Trino",
        description: "Distributed SQL engine (Iceberg)",
        category: "Query",
        sub: "trino",
        probe: Endpoint::Trino,
        workload_prefixes: &["trino-"],
        paths: &[("Trino UI", "/ui"), ("Query list", "/ui/admin/query")],
    },
    Spec {
        id: "lakekeeper",
        name: "Lakekeeper",
        description: "Iceberg REST catalog",
        category: "Catalog",
        sub: "lakekeeper",
        probe: Endpoint::Lakekeeper,
        workload_prefixes: &["lakekeeper"],
        // No browsable UI in v0.12 — the warehouse is read through Nova's
        // Catalog page, which speaks the same REST API.
        paths: &[],
    },
    Spec {
        id: "superset",
        name: "Superset",
        description: "BI & dashboards",
        category: "Analytics",
        sub: "superset",
        probe: Endpoint::Fixed("http://superset:8088/health"),
        workload_prefixes: &["superset"],
        paths: &[
            ("Welcome", "/superset/welcome/"),
            ("SQL Lab", "/sqllab/"),
            ("Dashboards", "/dashboard/list/"),
            ("Datasets", "/tablemodelview/list/"),
            ("Databases", "/databaseview/list/"),
        ],
    },
    Spec {
        id: "airflow",
        name: "Airflow",
        description: "Workflow orchestration",
        category: "Pipelines",
        sub: "airflow",
        probe: Endpoint::Fixed("http://airflow-api-server:8080/health"),
        workload_prefixes: &["airflow-"],
        paths: &[
            ("Home", "/home"),
            ("DAGs", "/dags"),
            ("Variables", "/variables"),
        ],
    },
    Spec {
        id: "jupyterhub",
        name: "JupyterHub",
        description: "Notebook workspaces",
        category: "Engineering",
        sub: "jupyter",
        probe: Endpoint::Fixed("http://proxy-public:80"),
        // The scheduler and the placeholder StatefulSet are part of JupyterHub's
        // shape: the placeholder at 0/0 is how a healthy idle hub looks.
        workload_prefixes: &["hub", "proxy", "user-"],
        paths: &[
            ("Hub home", "/hub/home"),
            ("JupyterLab", "/lab"),
            ("Spawn server", "/hub/spawn"),
            ("API tokens", "/hub/token"),
        ],
    },
];

fn spec(id: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.id == id)
}

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

/// Build the list-page view of an app: status + images, both observed, not configured.
async fn describe(s: &AppState, spec: &Spec, base: &str, snapshot: &[Workload]) -> PlatformApp {
    PlatformApp {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        description: spec.description.to_string(),
        category: spec.category.to_string(),
        external_url: external_url(base, spec.sub),
        status: probe(s, &spec.probe.url(&s.trino_url, &s.lakekeeper_url)).await,
        images: cluster::images_for(snapshot, spec.workload_prefixes),
    }
}

pub async fn list(State(s): State<Arc<AppState>>) -> Json<Vec<PlatformApp>> {
    let base = std::env::var("NOVA_BASE_DOMAIN").unwrap_or_default();
    let snapshot = s.workloads.snapshot(s.kube.as_ref(), s.demo).await;
    let mut apps = Vec::with_capacity(SPECS.len());

    for spec in SPECS {
        apps.push(describe(&s, spec, &base, &snapshot).await);
    }

    Json(apps)
}

pub async fn detail(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(spec) = spec(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("unknown app '{id}'"),
                "known": SPECS.iter().map(|s| s.id).collect::<Vec<_>>(),
            })),
        )
            .into_response();
    };

    let base = std::env::var("NOVA_BASE_DOMAIN").unwrap_or_default();
    let snapshot = s.workloads.snapshot(s.kube.as_ref(), s.demo).await;
    let workloads = cluster::workloads_for(&snapshot, spec.workload_prefixes);
    let (ready, desired) = cluster::replicas(&workloads);

    Json(AppDetail {
        app: describe(&s, spec, &base, &snapshot).await,
        probe: spec.probe.url(&s.trino_url, &s.lakekeeper_url),
        replicas: Replicas { ready, desired },
        workloads,
        facts: facts(&s, spec).await,
        links: deep_links(spec, &base),
    })
    .into_response()
}

/// Facts read from the service's own API — plus, for Nova itself, the one fact
/// only Nova knows. An unreachable service contributes no facts: the status
/// badge already says "unreachable", and a guess about a version helps nobody.
async fn facts(state: &AppState, spec: &Spec) -> Vec<Fact> {
    match spec.id {
        "nova" => vec![Fact::new("Nova build", env!("CARGO_PKG_VERSION"))],
        "trino" => {
            if state.demo {
                return trino_facts(&demo_trino_info(), "demo stub");
            }
            match fetch_json(state, &format!("{}/v1/info", state.trino_url)).await {
                Some(info) => trino_facts(&info, "Trino /v1/info"),
                None => Vec::new(),
            }
        }
        "lakekeeper" => {
            if state.demo {
                return vec![
                    Fact::sourced("Warehouse", &state.warehouse_name, "demo"),
                    Fact::sourced("Warehouse id", "demo", "demo"),
                ];
            }
            let mut facts = Vec::new();
            match crate::catalog::resolve_warehouse_id(state).await {
                Ok(id) => {
                    facts.push(Fact::sourced(
                        "Warehouse",
                        &state.warehouse_name,
                        "Lakekeeper /management/v1/warehouse",
                    ));
                    facts.push(Fact::sourced(
                        "Warehouse id",
                        id,
                        "Lakekeeper /management/v1/warehouse",
                    ));
                }
                Err(e) => facts.push(Fact::new("Warehouse", e).with_tone("bad")),
            }
            if let Some(info) = fetch_json(
                state,
                &format!("{}/management/v1/info", state.lakekeeper_url),
            )
            .await
            {
                facts.extend(lakekeeper_info_facts(&info));
            }
            let health_url = Endpoint::Lakekeeper.url(&state.trino_url, &state.lakekeeper_url);
            if let Some(health) = fetch_json(state, &health_url).await {
                facts.extend(lakekeeper_health_facts(&health));
            }
            facts
        }
        // Superset and Airflow answer `/health` with a component report; the
        // probe URL is that endpoint, so one fetch serves both purposes.
        "superset" | "airflow" => {
            if state.demo {
                return Vec::new();
            }
            let url = spec.probe.url(&state.trino_url, &state.lakekeeper_url);
            match fetch_json(state, &url).await {
                Some(health) if spec.id == "superset" => superset_facts(&health),
                Some(health) => airflow_facts(&health),
                None => Vec::new(),
            }
        }
        // The hub API needs a token, so Nova reports what it can see without
        // one: the hub/proxy workloads and the probe on the public proxy.
        _ => Vec::new(),
    }
}

/// Trino's `/v1/info`. Trino 480 dropped `/v1/cluster`, so per-node detail is
/// gone — but the coordinator reports the version, state and uptime, and the
/// workers are visible as workloads on the detail page.
fn trino_facts(info: &Value, source: &str) -> Vec<Fact> {
    let mut facts = Vec::new();
    if let Some(version) = info.pointer("/nodeVersion/version").and_then(Value::as_str) {
        facts.push(Fact::sourced("Engine version", version, source));
    }
    if let Some(state) = info.get("state").and_then(Value::as_str) {
        let tone = if state == "ACTIVE" { "ok" } else { "bad" };
        facts.push(Fact::sourced("State", state, source).with_tone(tone));
    }
    if let Some(coordinator) = info.get("coordinator").and_then(Value::as_bool) {
        facts.push(Fact::sourced("Coordinator", yes_no(coordinator), source));
    }
    if let Some(uptime) = info.get("uptime").and_then(Value::as_str) {
        facts.push(Fact::sourced("Uptime", uptime, source));
    }
    facts
}

/// The stub `/api/trino/cluster` answers with in demo mode.
fn demo_trino_info() -> Value {
    json!({ "nodeVersion": { "version": "demo" }, "state": "ACTIVE", "demo": true })
}

/// Lakekeeper's `/management/v1/info`: the catalog's own version, whether the
/// instance has been bootstrapped (terms accepted, first warehouse created), and
/// how it is configured — an `allow-all` authorization backend is a fact worth
/// flagging in a governance platform.
fn lakekeeper_info_facts(info: &Value) -> Vec<Fact> {
    let source = "Lakekeeper /management/v1/info";
    let mut facts = Vec::new();
    if let Some(version) = info.get("version").and_then(Value::as_str) {
        facts.push(Fact::sourced("Catalog version", version, source));
    }
    if let Some(bootstrapped) = info.get("bootstrapped").and_then(Value::as_bool) {
        let tone = if bootstrapped { "ok" } else { "bad" };
        facts.push(Fact::sourced("Bootstrapped", yes_no(bootstrapped), source).with_tone(tone));
    }
    if let Some(authz) = info.get("authz-backend").and_then(Value::as_str) {
        let tone = if authz.eq_ignore_ascii_case("allow-all") {
            "bad"
        } else {
            "ok"
        };
        facts.push(Fact::sourced("Authorization", authz, source).with_tone(tone));
    }
    if let Some(license) = info
        .pointer("/license-status/license-type")
        .and_then(Value::as_str)
    {
        facts.push(Fact::sourced("License", license, source));
    }
    facts
}

/// Lakekeeper's `/health`: the same shape as Airflow's — one status per
/// dependency, grouped by what it is (catalog database pool, secrets store).
fn lakekeeper_health_facts(health: &Value) -> Vec<Fact> {
    let source = "Lakekeeper /health";
    let mut facts = Vec::new();
    if let Some(status) = health.get("health").and_then(Value::as_str) {
        facts.push(Fact::sourced("Health", status, source).with_tone(health_tone(status)));
    }
    let Some(services) = health.get("services").and_then(Value::as_object) else {
        return facts;
    };
    let mut groups: Vec<(&String, &Value)> = services.iter().collect();
    groups.sort_by_key(|(group, _)| group.as_str());
    for (group, entries) in groups {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            let (Some(name), Some(status)) = (
                entry.get("name").and_then(Value::as_str),
                entry.get("status").and_then(Value::as_str),
            ) else {
                continue;
            };
            let label = format!("{} {name}", component_label(group));
            facts.push(Fact::sourced(&label, status, source).with_tone(health_tone(status)));
        }
    }
    facts
}

/// Superset's `/health`: `{"status": "OK", "version": "4.1.1"}`. Older releases
/// report the status alone, which is why the version fact is conditional.
fn superset_facts(health: &Value) -> Vec<Fact> {
    let source = "Superset /health";
    let mut facts = Vec::new();
    if let Some(status) = health.get("status").and_then(Value::as_str) {
        facts.push(Fact::sourced("Health", status, source).with_tone(health_tone(status)));
    }
    if let Some(version) = health.get("version").and_then(Value::as_str) {
        facts.push(Fact::sourced("Version", version, source));
    }
    facts
}

/// Airflow's `/health` reports one status per component — the fastest way to
/// tell "the API server answers" from "the scheduler is actually running".
fn airflow_facts(health: &Value) -> Vec<Fact> {
    let source = "Airflow /health";
    let mut facts = Vec::new();
    if let Some(status) = health.get("status").and_then(Value::as_str) {
        facts.push(Fact::sourced("Overall", status, source).with_tone(health_tone(status)));
    }
    let Some(map) = health.as_object() else {
        return facts;
    };
    let mut components: Vec<(&String, &Value)> = map
        .iter()
        .filter(|(key, value)| key.as_str() != "status" && value.get("status").is_some())
        .collect();
    components.sort_by_key(|(key, _)| key.as_str());
    for (key, value) in components {
        if let Some(status) = value.get("status").and_then(Value::as_str) {
            facts.push(
                Fact::sourced(&component_label(key), status, source).with_tone(health_tone(status)),
            );
        }
        if let Some(heartbeat) = value
            .get("latest_scheduler_heartbeat")
            .and_then(Value::as_str)
        {
            facts.push(Fact::sourced("Scheduler heartbeat", heartbeat, source));
        }
    }
    facts
}

/// Read the API payloads the same way the UI reads status: `healthy`/`ok` is
/// good, anything else is worth colouring.
fn health_tone(status: &str) -> &'static str {
    let status = status.trim().to_ascii_lowercase();
    if status == "healthy" || status == "ok" || status == "up" {
        "ok"
    } else {
        "bad"
    }
}

fn component_label(key: &str) -> String {
    match key {
        "metadatabase" => "Metadata database".into(),
        "dag_processor" => "DAG processor".into(),
        other => {
            let spaced = other.replace('_', " ");
            let mut chars = spaced.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        }
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn deep_links(spec: &Spec, base: &str) -> Vec<Link> {
    let root = external_url(base, spec.sub);
    if root.is_empty() {
        return Vec::new();
    }
    spec.paths
        .iter()
        .map(|(label, path)| Link {
            label: (*label).to_string(),
            url: format!("{root}{path}"),
        })
        .collect()
}

/// GET a JSON document with a deadline. `None` covers every failure — refused,
/// timed out, non-2xx, not JSON — because the caller's answer is the same: no fact.
async fn fetch_json(state: &AppState, url: &str) -> Option<Value> {
    let response = http_client(state).get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

/// Short deadlines: the UI polls, and a detail page must not hang on a service
/// that is down. The probe and the fact fetches are separate requests, but the
/// worst case is bounded by the same budget.
fn http_client(state: &AppState) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .unwrap_or_else(|_| state.http.clone())
}

async fn probe(state: &AppState, url: &str) -> String {
    if state.demo {
        return "up".into();
    }
    match http_client(state).get(url).send().await {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_targets_follow_the_configured_urls() {
        let trino = spec("trino").unwrap();
        assert_eq!(
            trino
                .probe
                .url("http://trino.stardelt.svc:8080", "http://lk:8181"),
            "http://trino.stardelt.svc:8080/v1/info"
        );
        assert_eq!(
            trino
                .probe
                .url("http://trino.stardelt.svc:8080/", "http://lk:8181"),
            "http://trino.stardelt.svc:8080/v1/info"
        );

        let lakekeeper = spec("lakekeeper").unwrap();
        assert_eq!(
            lakekeeper
                .probe
                .url("http://trino:8080", "http://catalog:8181"),
            "http://catalog:8181/health"
        );

        let airflow = spec("airflow").unwrap();
        assert_eq!(
            airflow
                .probe
                .url("http://trino:8080", "http://catalog:8181"),
            "http://airflow-api-server:8080/health"
        );
    }

    #[test]
    fn app_lookup_is_exact() {
        assert_eq!(spec("trino").map(|s| s.name), Some("Trino"));
        assert!(spec("Trino").is_none());
        assert!(spec("nope").is_none());
    }

    #[test]
    fn external_urls_use_the_subdomain_and_skip_an_empty_base() {
        assert_eq!(
            external_url("alpha.dev.stardelt.io", "trino"),
            "https://trino.alpha.dev.stardelt.io"
        );
        assert_eq!(
            external_url("alpha.dev.stardelt.io", ""),
            "https://alpha.dev.stardelt.io"
        );
        assert_eq!(external_url("", "trino"), "");
    }

    #[test]
    fn deep_links_need_a_base_domain() {
        let trino = spec("trino").unwrap();
        assert!(deep_links(trino, "").is_empty());

        let links = deep_links(trino, "alpha.dev.stardelt.io");
        assert_eq!(links[0].label, "Trino UI");
        assert_eq!(links[0].url, "https://trino.alpha.dev.stardelt.io/ui");

        // Lakekeeper has no UI to link into, base domain or not.
        assert!(deep_links(spec("lakekeeper").unwrap(), "alpha.dev.stardelt.io").is_empty());
    }

    #[test]
    fn trino_info_becomes_facts_with_a_source() {
        let info = json!({
            "nodeVersion": { "version": "480" },
            "state": "ACTIVE",
            "coordinator": true,
            "starting": false,
            "uptime": "3.42h"
        });
        let facts = trino_facts(&info, "Trino /v1/info");
        assert_eq!(
            facts,
            vec![
                Fact::sourced("Engine version", "480", "Trino /v1/info"),
                Fact::sourced("State", "ACTIVE", "Trino /v1/info").with_tone("ok"),
                Fact::sourced("Coordinator", "yes", "Trino /v1/info"),
                Fact::sourced("Uptime", "3.42h", "Trino /v1/info"),
            ]
        );
    }

    #[test]
    fn a_non_actived_engine_is_flagged() {
        let facts = trino_facts(&json!({ "state": "SHUTTING_DOWN" }), "src");
        assert_eq!(facts[0].tone, Some("bad"));
    }

    #[test]
    fn a_payload_without_the_fields_produces_no_facts() {
        assert!(trino_facts(&json!({}), "src").is_empty());
        assert!(superset_facts(&json!({})).is_empty());
        assert!(lakekeeper_info_facts(&json!({ "unrelated": 1 })).is_empty());
        assert!(lakekeeper_health_facts(&json!({ "unrelated": 1 })).is_empty());
        assert!(airflow_facts(&json!({ "unrelated": 1 })).is_empty());
    }

    #[test]
    fn superset_health_reports_status_and_version() {
        let facts = superset_facts(&json!({ "status": "OK", "version": "4.1.1" }));
        assert_eq!(
            facts,
            vec![
                Fact::sourced("Health", "OK", "Superset /health").with_tone("ok"),
                Fact::sourced("Version", "4.1.1", "Superset /health"),
            ]
        );
    }

    #[test]
    fn lakekeeper_info_reports_version_and_bootstrap_state() {
        let facts = lakekeeper_info_facts(&json!({
            "version": "0.12.2",
            "bootstrapped": true,
            "authz-backend": "openfga",
            "license-status": { "license-type": "Apache-2.0", "valid": true }
        }));
        assert_eq!(
            facts,
            vec![
                Fact::sourced(
                    "Catalog version",
                    "0.12.2",
                    "Lakekeeper /management/v1/info"
                ),
                Fact::sourced("Bootstrapped", "yes", "Lakekeeper /management/v1/info")
                    .with_tone("ok"),
                Fact::sourced("Authorization", "openfga", "Lakekeeper /management/v1/info")
                    .with_tone("ok"),
                Fact::sourced("License", "Apache-2.0", "Lakekeeper /management/v1/info"),
            ]
        );

        let unbootstrapped = lakekeeper_info_facts(&json!({ "bootstrapped": false }));
        assert_eq!(unbootstrapped[0].tone, Some("bad"));

        // The dev cluster runs `allowall`: a governance platform should say so.
        let dev = lakekeeper_info_facts(&json!({ "authz-backend": "allow-all" }));
        assert_eq!(dev[0].tone, Some("bad"));
    }

    #[test]
    fn lakekeeper_health_lists_every_dependency() {
        let facts = lakekeeper_health_facts(&json!({
            "health": "ok",
            "services": {
                "secrets": [{ "name": "write_pool", "status": "ok", "lastCheck": 1 }],
                "catalog": [
                    { "name": "read_pool", "status": "ok", "lastCheck": 1 },
                    { "name": "write_pool", "status": "unavailable", "lastCheck": 1 }
                ]
            }
        }));
        assert_eq!(
            facts,
            vec![
                Fact::sourced("Health", "ok", "Lakekeeper /health").with_tone("ok"),
                Fact::sourced("Catalog read_pool", "ok", "Lakekeeper /health").with_tone("ok"),
                Fact::sourced("Catalog write_pool", "unavailable", "Lakekeeper /health")
                    .with_tone("bad"),
                Fact::sourced("Secrets write_pool", "ok", "Lakekeeper /health").with_tone("ok"),
            ]
        );
    }

    #[test]
    fn airflow_health_becomes_one_fact_per_component_in_order() {
        let facts = airflow_facts(&json!({
            "metadatabase": { "status": "healthy" },
            "scheduler": { "status": "healthy", "latest_scheduler_heartbeat": "2026-10-18T08:00:00+00:00" },
            "triggerer": { "status": "unhealthy" },
            "dag_processor": { "status": "healthy" }
        }));
        assert_eq!(
            facts,
            vec![
                Fact::sourced("DAG processor", "healthy", "Airflow /health").with_tone("ok"),
                Fact::sourced("Metadata database", "healthy", "Airflow /health").with_tone("ok"),
                Fact::sourced("Scheduler", "healthy", "Airflow /health").with_tone("ok"),
                Fact::sourced(
                    "Scheduler heartbeat",
                    "2026-10-18T08:00:00+00:00",
                    "Airflow /health"
                ),
                Fact::sourced("Triggerer", "unhealthy", "Airflow /health").with_tone("bad"),
            ]
        );
    }

    #[test]
    fn unknown_component_keys_are_still_readable() {
        assert_eq!(component_label("dag_processor"), "DAG processor");
        assert_eq!(component_label("celery_worker"), "Celery worker");
        assert_eq!(component_label("scheduler"), "Scheduler");
    }
}
