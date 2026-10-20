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
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::cluster::{self, Workload};
use crate::embed::{self, Embed};

#[derive(Serialize, Clone)]
pub struct PlatformApp {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    /// Reachable-from-the-browser URL. Empty when no base domain is configured,
    /// and empty for an app with no browsable surface (nothing serves its host —
    /// see `public_url`).
    pub external_url: String,
    /// Whether a browser has somewhere to go for this app. False for an app with
    /// no browsable surface of its own (an API-only catalog, say), which is
    /// independent of whether the app is installed or reachable.
    pub browsable: bool,
    /// up | unreachable | unknown | not-installed
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
    /// Whether a browser will let Nova frame the app's UI. Absent for apps Nova
    /// does not embed anywhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed: Option<Embed>,
}

#[derive(Serialize)]
pub struct Replicas {
    pub ready: i32,
    pub desired: i32,
}

/// The service addresses Nova talks to, from `NOVA_*_URL` (with in-cluster
/// defaults). Probes, fact reads and the embed check all use these, so nothing
/// in the UI can disagree with the connection Nova actually makes.
pub struct Targets<'a> {
    pub trino: &'a str,
    pub lakekeeper: &'a str,
    pub superset: &'a str,
    pub jupyterhub: &'a str,
}

/// Where Nova probes an app. Trino, Lakekeeper, Superset and JupyterHub are
/// probed where Nova actually talks to them; the rest have a fixed in-cluster
/// address in their spec.
enum Endpoint {
    Fixed(&'static str),
    Trino,
    Lakekeeper,
    Superset,
    JupyterHub,
}

impl Endpoint {
    fn base<'a>(&self, targets: &'a Targets<'_>) -> &'a str {
        match self {
            Endpoint::Fixed(url) => url,
            Endpoint::Trino => targets.trino,
            Endpoint::Lakekeeper => targets.lakekeeper,
            Endpoint::Superset => targets.superset,
            Endpoint::JupyterHub => targets.jupyterhub,
        }
    }

    /// The URL Nova requests: the app's base address plus the path Nova needs.
    fn url(&self, targets: &Targets<'_>, path: &str) -> String {
        format!("{}{path}", self.base(targets).trim_end_matches('/'))
    }
}

struct Spec {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    category: &'static str,
    /// Subdomain under the base domain, e.g. `superset`.
    sub: &'static str,
    /// Where the app runs, plus the path probed for liveness.
    probe: Endpoint,
    probe_path: &'static str,
    /// Workload name prefixes that make up the app — how the app's images are found.
    workload_prefixes: &'static [&'static str],
    /// UI paths appended to the external URL, as deep links.
    paths: &'static [(&'static str, &'static str)],
    /// Set when Nova frames this app's UI somewhere in its own UI.
    embed: Option<EmbedSpec>,
}

/// The page Nova frames, for the apps Nova embeds in its own UI (Dashboards,
/// Notebooks). Nova reads that page's headers to report whether the embed can
/// work — the app decides, not Nova.
struct EmbedSpec {
    /// Path of the framed page, appended to the app's base URL.
    path: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        id: "nova",
        name: "Nova",
        description: "Control plane: catalog, SQL, load & transfer",
        category: "Experience",
        sub: "",
        probe: Endpoint::Fixed("http://nova:8080"),
        probe_path: "/api/health",
        workload_prefixes: &["nova"],
        paths: &[("Overview", "/")],
        embed: None,
    },
    Spec {
        id: "trino",
        name: "Trino",
        description: "Distributed SQL engine (Iceberg)",
        category: "Query",
        sub: "trino",
        probe: Endpoint::Trino,
        probe_path: "/v1/info",
        workload_prefixes: &["trino-"],
        // No browsable surface until the administrator gate exists (stardelt-platform#194): the public host serves only /v1/statement and /oauth2; administrators port-forward the UI.
        paths: &[],
        embed: None,
    },
    Spec {
        id: "lakekeeper",
        name: "Lakekeeper",
        description: "Iceberg REST catalog",
        category: "Catalog",
        sub: "lakekeeper",
        probe: Endpoint::Lakekeeper,
        probe_path: "/health",
        workload_prefixes: &["lakekeeper"],
        // No browsable UI in v0.12 — the warehouse is read through Nova's
        // Catalog page, which speaks the same REST API.
        paths: &[],
        embed: None,
    },
    Spec {
        id: "superset",
        name: "Superset",
        description: "BI & dashboards",
        category: "Analytics",
        sub: "superset",
        probe: Endpoint::Superset,
        probe_path: "/health",
        workload_prefixes: &["superset"],
        paths: &[
            ("Welcome", "/superset/welcome/"),
            ("SQL Lab", "/sqllab/"),
            ("Dashboards", "/dashboard/list/"),
            ("Datasets", "/tablemodelview/list/"),
            ("Databases", "/databaseview/list/"),
        ],
        // Nova frames Superset on its Dashboards page.
        embed: Some(EmbedSpec {
            path: "/superset/welcome/",
        }),
    },
    Spec {
        id: "airflow",
        name: "Airflow",
        description: "Workflow orchestration",
        category: "Pipelines",
        sub: "airflow",
        probe: Endpoint::Fixed("http://airflow-api-server:8080"),
        probe_path: "/api/v2/monitor/health",
        workload_prefixes: &["airflow-"],
        paths: &[
            ("Home", "/home"),
            ("DAGs", "/dags"),
            ("Variables", "/variables"),
        ],
        embed: None,
    },
    Spec {
        id: "jupyterhub",
        name: "JupyterHub",
        description: "Notebook workspaces",
        category: "Engineering",
        sub: "jupyter",
        probe: Endpoint::JupyterHub,
        probe_path: "",
        // The scheduler and the placeholder StatefulSet are part of JupyterHub's
        // shape: the placeholder at 0/0 is how a healthy idle hub looks.
        workload_prefixes: &["hub", "proxy", "user-"],
        paths: &[
            // `/hub/` is the hub's *root*: for a signed-in person it redirects
            // to their own server (`user.url`, i.e. JupyterLab), and to the
            // spawn page when no server runs — `/hub/home` is the control panel
            // and never redirects (jupyterhub/handlers/pages.py, RootHandler vs
            // HomeHandler).
            ("My server", "/hub/"),
            ("Hub home", "/hub/home"),
            ("Start server", "/hub/spawn"),
            ("API tokens", "/hub/token"),
        ],
        // Nova frames JupyterHub on its Notebooks page: the hub root, so a
        // signed-in person lands in their own JupyterLab (or on the spawn page
        // when they have no server yet). The server may in turn be framed only
        // by Nova — that is the image's jupyter_server_config.py.
        embed: Some(EmbedSpec { path: "/hub/" }),
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

/// The URL worth handing to a browser, or "" when there is nowhere to go.
///
/// An app with no browsable paths still has a subdomain that wildcard DNS resolves,
/// but nothing serves it: the ingress controller answers with its own fake
/// certificate. Worse, the apex sends HSTS with `includeSubDomains`, so a browser
/// will not even let the reader click past that certificate warning. Advertising
/// the host is therefore worse than advertising nothing — report no URL and let
/// the app be reached through the surfaces that do exist (Nova's Catalog page, or
/// the engines that speak its protocol).
fn public_url(base: &str, spec: &Spec) -> String {
    if browsable(spec) {
        external_url(base, spec.sub)
    } else {
        String::new()
    }
}

/// An app is browsable when it has paths a browser can open. That is the same fact
/// that decides whether an Ingress exists for its host.
fn browsable(spec: &Spec) -> bool {
    !spec.paths.is_empty()
}

fn targets(state: &AppState) -> Targets<'_> {
    Targets {
        trino: &state.trino_url,
        lakekeeper: &state.lakekeeper_url,
        superset: &state.superset_url,
        jupyterhub: &state.jupyterhub_url,
    }
}

/// Build the list-page view of an app: status + images, both observed, not configured.
async fn describe(
    s: &AppState,
    spec: &Spec,
    base: &str,
    snapshot: &cluster::Snapshot,
) -> PlatformApp {
    let probed = probe(s, &spec.probe.url(&targets(s), spec.probe_path)).await;
    let has_workloads =
        !cluster::workloads_for(&snapshot.workloads, spec.workload_prefixes).is_empty();
    let status = resolve_status(probed, snapshot.state != "unreachable", has_workloads);
    PlatformApp {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        description: spec.description.to_string(),
        category: spec.category.to_string(),
        external_url: public_url(base, spec),
        browsable: browsable(spec),
        status,
        images: cluster::images_for(&snapshot.workloads, spec.workload_prefixes),
    }
}

/// One entry of the header's app switcher: enough to open the app, and nothing else.
#[derive(Serialize)]
pub struct AppLink {
    pub id: String,
    pub name: String,
    pub category: String,
    pub external_url: String,
}

/// The apps a signed-in user may open, in the order the app specs declare them.
///
/// Nova itself is not a destination (you are already in it), and an app with no
/// browsable surface has no URL worth offering. When no base domain is configured
/// (demo mode, a local run) every URL is empty, so the honest answer is no links
/// at all rather than anchors that go nowhere.
fn links_for(base: &str, admin: bool) -> Vec<AppLink> {
    SPECS
        .iter()
        .filter(|spec| spec.id != "nova" && browsable(spec))
        // The engine's own UI lists cluster-wide activity: administrators only.
        .filter(|spec| admin || spec.id != "trino")
        .filter_map(|spec| {
            let external_url = public_url(base, spec);
            if external_url.is_empty() {
                return None;
            }
            Some(AppLink {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                category: spec.category.to_string(),
                external_url,
            })
        })
        .collect()
}

/// `GET /api/apps/links` — the header's app switcher, for every signed-in user.
///
/// Deliberately no status: liveness comes from cluster probes, which is operator
/// information (see `apps::list`), and this endpoint is called by people who are
/// not operators.
pub async fn links(principal: Option<Extension<crate::auth::Principal>>) -> Json<Vec<AppLink>> {
    let base = std::env::var("NOVA_BASE_DOMAIN").unwrap_or_default();
    // No principal means the static-dev mode, where everybody acts as an administrator.
    let admin = match &principal {
        None => true,
        Some(Extension(p)) => p.identity.is_admin,
    };
    Json(links_for(&base, admin))
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

/// What the embedded pages (Dashboards, Notebooks) need, readable by every signed-in user:
/// the apps Nova frames, their address, liveness and framing state. No workloads, images or
/// replica counts; those stay behind the admin-only `/api/apps`.
///
/// Deliberately not an admin route and deliberately not a filtered `/api/apps`: an ordinary
/// user needs to open the notebook and the dashboard, and nothing else on that payload.
#[derive(Serialize)]
pub struct EmbedApp {
    pub id: String,
    pub name: String,
    pub external_url: String,
    pub status: String,
    pub embed: Embed,
}

pub async fn embeds(State(s): State<Arc<AppState>>) -> Json<Vec<EmbedApp>> {
    let base = std::env::var("NOVA_BASE_DOMAIN").unwrap_or_default();
    let snapshot = s.workloads.snapshot(s.kube.as_ref(), s.demo).await;
    let mut out = Vec::new();
    for spec in SPECS.iter().filter(|spec| spec.embed.is_some()) {
        let probed = probe(&s, &spec.probe.url(&targets(&s), spec.probe_path)).await;
        let has_workloads =
            !cluster::workloads_for(&snapshot.workloads, spec.workload_prefixes).is_empty();
        out.push(EmbedApp {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            external_url: public_url(&base, spec),
            status: resolve_status(probed, snapshot.state != "unreachable", has_workloads),
            embed: embed_report(&s, spec, &base).await,
        });
    }
    Json(out)
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
    let workloads = cluster::workloads_for(&snapshot.workloads, spec.workload_prefixes);
    let (ready, desired) = cluster::replicas(&workloads);

    Json(AppDetail {
        app: describe(&s, spec, &base, &snapshot).await,
        probe: spec.probe.url(&targets(&s), spec.probe_path),
        replicas: Replicas { ready, desired },
        workloads,
        facts: facts(&s, spec).await,
        links: deep_links(spec, &base),
        embed: match &spec.embed {
            Some(_) => Some(embed_report(&s, spec, &base).await),
            None => None,
        },
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
            let mut facts = vec![Fact::new(
                "Open the UI",
                "kubectl -n stardelt port-forward svc/trino 8443:8443, then open https://localhost:8443/ui/",
            )];
            if state.demo {
                facts.extend(trino_facts(&demo_trino_info(), "demo stub"));
            } else if let Some(info) =
                fetch_json(state, &format!("{}/v1/info", state.trino_url)).await
            {
                facts.extend(trino_facts(&info, "Trino /v1/info"));
            }
            facts
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
                Err(e) => facts.push(Fact::new("Warehouse", e.to_string()).with_tone("bad")),
            }
            // The management API takes Nova's service token like every other Lakekeeper
            // call (stardelt-nova#222); the public `/health` below needs none.
            let authorization = crate::catalog::lakekeeper_authorization(state)
                .await
                .ok()
                .flatten();
            if let Some(info) = fetch_json_authorized(
                state,
                &format!("{}/management/v1/info", state.lakekeeper_url),
                authorization.as_deref(),
            )
            .await
            {
                facts.extend(lakekeeper_info_facts(&info));
            }
            let health_url = Endpoint::Lakekeeper.url(&targets(state), "/health");
            if let Some(health) = fetch_json(state, &health_url).await {
                facts.extend(lakekeeper_health_facts(&health));
            }
            facts
        }
        // Superset and Airflow answer their health endpoint with a component report; the
        // probe URL is that endpoint, so one fetch serves both purposes.
        "superset" | "airflow" => {
            if state.demo {
                return Vec::new();
            }
            let url = spec.probe.url(&targets(state), spec.probe_path);
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

/// Airflow 3's `/api/v2/monitor/health` reports one status per component — the fastest way to
/// tell "the API server answers" from "the scheduler is actually running".
fn airflow_facts(health: &Value) -> Vec<Fact> {
    let source = "Airflow /api/v2/monitor/health";
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
    fetch_json_authorized(state, url, None).await
}

/// [`fetch_json`] with one `Authorization` header — Lakekeeper's management API wants
/// Nova's service token, the other probes want nothing.
async fn fetch_json_authorized(
    state: &AppState,
    url: &str,
    authorization: Option<&str>,
) -> Option<Value> {
    let mut request = http_client(state).get(url);
    if let Some(header) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, header);
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

/// Short deadlines: the UI polls, and a detail page must not hang on a service
/// that is down. The probe and the fact fetches are separate requests, but the
/// worst case is bounded by the same budget.
fn http_client(state: &AppState) -> reqwest::Client {
    crate::add_platform_ca(reqwest::Client::builder())
        .and_then(|b| Ok(b.timeout(Duration::from_millis(1500)).build()?))
        .unwrap_or_else(|_| state.http.clone())
}

/// Read the framed page's headers so the UI can explain a blank frame instead of
/// showing one. The page Nova frames is the external URL, but only the in-cluster
/// address is reachable from here — the headers are the app's own either way.
///
/// Redirects are followed only while they stay on the app's hosts. A signed-out
/// visitor's journey normally ends at the app's own login redirect (which carries
/// the same CSP as its pages); following it one hop further lands on Keycloak,
/// whose login page must refuse framing — judging *its* headers would report
/// "blocked" for an app that is frameable (stardelt-nova#91). The host that hop
/// goes to is reported as `sign_in_host` so the UI can offer the top-level sign-in
/// instead of a blank frame.
async fn embed_report(state: &AppState, spec: &Spec, base: &str) -> Embed {
    let Some(frame) = &spec.embed else {
        return Embed::unknown(String::new(), None);
    };
    let root = external_url(base, spec.sub);
    let url = if root.is_empty() {
        String::new()
    } else {
        format!("{root}{}", frame.path)
    };
    let checked = spec.probe.url(&targets(state), frame.path);

    if state.demo || url.is_empty() {
        return Embed::unknown(url, Some(checked));
    }

    let own_hosts = app_hosts(&checked, &url);
    match framing_client(state, own_hosts.clone())
        .get(&checked)
        .send()
        .await
    {
        Ok(response) => {
            let sign_in_host = response
                .status()
                .is_redirection()
                .then(|| {
                    response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|location| embed::sign_in_host(location, &checked, &own_hosts))
                })
                .flatten();
            match embed::frame_policy(response.headers(), base) {
                embed::FramePolicy::Allowed => Embed {
                    url,
                    state: "allowed".into(),
                    blocked_by: None,
                    detail: None,
                    checked: Some(checked),
                    sign_in_host,
                },
                embed::FramePolicy::Blocked { header, value } => Embed {
                    url,
                    state: "blocked".into(),
                    blocked_by: Some(header.to_string()),
                    detail: Some(value),
                    checked: Some(checked),
                    sign_in_host,
                },
            }
        }
        Err(_) => Embed::unknown(url, Some(checked)),
    }
}

/// The hosts the app answers from: the in-cluster address Nova reads headers
/// from, plus the public URL a browser uses. A redirect between them is still
/// the app; a redirect anywhere else is not.
fn app_hosts(checked: &str, public_url: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for url in [checked, public_url] {
        if let Some(host) = url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    }
    hosts
}

/// The client the framing check uses: same short deadline as every other probe,
/// but redirects stop at the app's own hosts (a few hops to allow the app's own
/// login loop, then a stop — the journey off the app is what `sign_in_host`
/// reports, not something to fetch).
fn framing_client(state: &AppState, own_hosts: Vec<String>) -> reqwest::Client {
    crate::add_platform_ca(reqwest::Client::builder())
        .and_then(|b| {
            Ok(b.timeout(Duration::from_millis(1500))
                .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                    if attempt.previous().len() >= 5 {
                        return attempt.stop();
                    }
                    match attempt.url().host_str() {
                        Some(host) if own_hosts.iter().any(|own| own == host) => attempt.follow(),
                        _ => attempt.stop(),
                    }
                }))
                .build()?)
        })
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

/// `up` stays `up`. Anything else is `not-installed` when the cluster was read
/// and holds no workload for the app (not a failure); otherwise the probe's
/// verdict stands.
fn resolve_status(probed: String, cluster_read: bool, has_workloads: bool) -> String {
    if probed != "up" && cluster_read && !has_workloads {
        "not-installed".into()
    } else {
        probed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_targets<'a>() -> Targets<'a> {
        Targets {
            trino: "http://trino.stardelt.svc:8080",
            lakekeeper: "http://catalog.stardelt.svc:8181",
            superset: "http://superset.stardelt.svc:8088",
            jupyterhub: "http://proxy-public.stardelt.svc:80",
        }
    }

    #[test]
    fn probe_targets_follow_the_configured_urls() {
        let targets = test_targets();

        let trino = spec("trino").unwrap();
        assert_eq!(
            trino.probe.url(&targets, trino.probe_path),
            "http://trino.stardelt.svc:8080/v1/info"
        );

        let lakekeeper = spec("lakekeeper").unwrap();
        assert_eq!(
            lakekeeper.probe.url(&targets, lakekeeper.probe_path),
            "http://catalog.stardelt.svc:8181/health"
        );

        let superset = spec("superset").unwrap();
        assert_eq!(
            superset.probe.url(&targets, superset.probe_path),
            "http://superset.stardelt.svc:8088/health"
        );

        // JupyterHub's public proxy answers on its root path.
        let jupyterhub = spec("jupyterhub").unwrap();
        assert_eq!(
            jupyterhub.probe.url(&targets, jupyterhub.probe_path),
            "http://proxy-public.stardelt.svc:80"
        );

        // Airflow has no NOVA_*_URL of its own: the spec's fixed address stands.
        let airflow = spec("airflow").unwrap();
        assert_eq!(
            airflow.probe.url(&targets, airflow.probe_path),
            "http://airflow-api-server:8080/api/v2/monitor/health"
        );
    }

    #[test]
    fn a_trailing_slash_in_configuration_does_not_double_up() {
        let targets = Targets {
            trino: "http://trino:8080/",
            ..test_targets()
        };
        let trino = spec("trino").unwrap();
        assert_eq!(
            trino.probe.url(&targets, trino.probe_path),
            "http://trino:8080/v1/info"
        );
    }

    #[test]
    fn only_embedded_apps_carry_an_embed_spec() {
        let embedded: Vec<&str> = SPECS
            .iter()
            .filter(|s| s.embed.is_some())
            .map(|s| s.id)
            .collect();
        assert_eq!(embedded, vec!["superset", "jupyterhub"]);
        assert_eq!(
            spec("jupyterhub").unwrap().embed.as_ref().unwrap().path,
            "/hub/"
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
    fn app_links_offer_the_browsable_apps_except_nova() {
        let links = links_for("alpha.dev.stardelt.io", true);
        let ids: Vec<&str> = links.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["superset", "airflow", "jupyterhub"]);
        assert!(
            links.iter().all(|l| l.external_url.starts_with("https://")),
            "every link must be an absolute https URL"
        );
        // Lakekeeper has no UI and Nova is where the user already is.
        assert!(links.iter().all(|l| l.id != "nova" && l.id != "lakekeeper"));

        // No base domain (demo mode, local run): no links, rather than empty hrefs.
        assert!(links_for("", true).is_empty());

        // Both calls return the same list: the engine has no browsable surface
        // (stardelt-platform#194), so there is nothing left to restrict from
        // non-admins (platform#181).
        let user_links = links_for("alpha.dev.stardelt.io", false);
        let ids: Vec<&str> = user_links.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["superset", "airflow", "jupyterhub"]);
    }

    #[test]
    fn an_app_with_no_browsable_surface_advertises_no_host() {
        // An app with `paths: &[]` still has a subdomain that wildcard DNS
        // resolves, which is exactly the trap: DNS says yes, the ingress
        // controller's fake certificate says no, and HSTS `includeSubDomains`
        // removes the click-through. It has no Ingress, so it gets no URL.
        let lakekeeper = spec("lakekeeper").unwrap();
        assert_eq!(public_url("alpha.dev.stardelt.io", lakekeeper), "");
        assert!(
            !browsable(lakekeeper),
            "Lakekeeper has no UI, so it is not browsable"
        );

        // The engine joins it until the administrator gate exists
        // (stardelt-platform#194): the public host serves only /v1/statement and
        // /oauth2, so the subdomain has nothing for a browser to open.
        let trino = spec("trino").unwrap();
        assert_eq!(public_url("alpha.dev.stardelt.io", trino), "");
        assert!(
            !browsable(trino),
            "the engine UI is port-forwarded, not published, so it is not browsable"
        );

        // Everything with somewhere to go keeps its URL.
        for spec in SPECS.iter().filter(|s| !s.paths.is_empty()) {
            assert_eq!(
                public_url("alpha.dev.stardelt.io", spec),
                external_url("alpha.dev.stardelt.io", spec.sub),
                "{} lost its URL",
                spec.id
            );
        }
    }

    #[test]
    fn deep_links_need_a_base_domain() {
        // The engine has no browsable surface (stardelt-platform#194): no link
        // to build, base domain or not.
        let trino = spec("trino").unwrap();
        assert!(deep_links(trino, "").is_empty());
        assert!(deep_links(trino, "alpha.dev.stardelt.io").is_empty());

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
                Fact::sourced("DAG processor", "healthy", "Airflow /api/v2/monitor/health")
                    .with_tone("ok"),
                Fact::sourced(
                    "Metadata database",
                    "healthy",
                    "Airflow /api/v2/monitor/health"
                )
                .with_tone("ok"),
                Fact::sourced("Scheduler", "healthy", "Airflow /api/v2/monitor/health")
                    .with_tone("ok"),
                Fact::sourced(
                    "Scheduler heartbeat",
                    "2026-10-18T08:00:00+00:00",
                    "Airflow /api/v2/monitor/health"
                ),
                Fact::sourced("Triggerer", "unhealthy", "Airflow /api/v2/monitor/health")
                    .with_tone("bad"),
            ]
        );
    }

    #[test]
    fn unknown_component_keys_are_still_readable() {
        assert_eq!(component_label("dag_processor"), "DAG processor");
        assert_eq!(component_label("celery_worker"), "Celery worker");
        assert_eq!(component_label("scheduler"), "Scheduler");
    }

    #[test]
    fn embed_checks_stop_at_the_app_s_own_hosts() {
        // The in-cluster address Nova probes and the public URL a browser uses:
        // both count as the app, nothing else does.
        assert_eq!(
            app_hosts(
                "http://hub:8081/hub/home",
                "https://jupyter.alpha.dev.stardelt.io/hub/home"
            ),
            vec![
                "hub".to_string(),
                "jupyter.alpha.dev.stardelt.io".to_string()
            ]
        );
        // An unparseable checked URL contributes nothing, not a stray host.
        assert_eq!(
            app_hosts("", "https://jupyter.alpha.dev.stardelt.io/hub/home"),
            vec!["jupyter.alpha.dev.stardelt.io".to_string()]
        );
    }

    #[test]
    fn a_missing_workload_is_not_installed_not_unreachable() {
        assert_eq!(
            resolve_status("unreachable".into(), true, false),
            "not-installed"
        );
        assert_eq!(
            resolve_status("unreachable".into(), true, true),
            "unreachable"
        );
        assert_eq!(
            resolve_status("unreachable".into(), false, false),
            "unreachable"
        );
        assert_eq!(resolve_status("up".into(), true, false), "up");
    }
}
