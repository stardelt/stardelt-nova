//! The Pipelines read API: list, detail, runs, tasks and logs (stardelt-nova#188; design note
//! `design/scheduled-jobs.md` §6.1, phase J1).
//!
//! Nova is the enforcement point. Airflow's one credential reads every DAG, so every route here
//! first finds the `Pipeline` resource, checks that the caller may see it, and only then asks
//! Airflow. A pipeline the caller may not see answers **404**, exactly like one that does not
//! exist — a refusal that distinguishes "hidden" from "absent" would leak the hidden one.
//!
//! - **Visibility** ([`visible`]): administrators see everything; everybody else sees a pipeline
//!   whose `spec.ownerGroup` is one of their groups. A pipeline without an `ownerGroup` is
//!   administrators-only.
//! - **Airflow's naming**: `Pipeline <namespace>/<name>` has the DAG id `<namespace>__<name>`
//!   ([`airflow::dag_id_for`]). Every Airflow path is built from that id, never from a browser
//!   value, so a run id of another pipeline cannot resolve.
//! - **A new pipeline is loading**: the loader creates the DAG about 25 seconds after the
//!   resource appears. Until then the view says `loaded: false`, `state: "loading"`.
//! - **Logs**: the `::group::` marker carries internal object-store paths in `sources`; it is
//!   dropped, and no field of an answer can hold a `sources` value. Answers are capped at 2000
//!   lines and 512 KiB and say `truncated: true` beyond that.
//! - **Unreachable upstreams are 502, never 500** (rule U12 of `design/ui-conformance.md`):
//!   without a cluster client or an Airflow client the routes answer 503 and say which one is
//!   missing; an Airflow that refuses or fails answers 502 `the scheduler did not answer`.
//!
//! The route logic sits in plain `async fn`s that take their inputs, so the tests call them
//! without a router; each handler is the thin axum wrapper that checks sign-in and the two
//! clients. The whole surface is read-only and uncacheable (`Cache-Control: no-store`).

use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(test)]
use std::sync::Mutex;

use axum::{
    Json, Router,
    extract::{Path, Query, Request},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use futures::stream::{self, StreamExt};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;
use crate::airflow::{
    AirflowClient, AirflowError, DagRun, DagSummary, LogEvent, LogPage, TaskInstance, dag_id_for,
};
use crate::audit;
use crate::auth::Identity;
use crate::connection_routes::{Fail, SignedIn};

/// The `Pipeline` resources of the platform namespace: the cluster, or memory in tests.
pub enum PipelineResources {
    Kube(Api<DynamicObject>),
    #[cfg(test)]
    Memory(Arc<Mutex<BTreeMap<String, Value>>>),
}

#[derive(Debug)]
pub enum ResourceError {
    /// The cluster did not answer or refused.
    Unavailable(String),
}

impl PipelineResources {
    /// The pipeline resources of Nova's namespace, through the cluster client.
    pub fn cluster(client: kube::Client) -> Self {
        let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "stardelt.io",
            "v1alpha1",
            "Pipeline",
        ));
        Self::Kube(Api::namespaced_with(
            client,
            &crate::cluster::namespace(),
            &ar,
        ))
    }

    /// Every `Pipeline` of the namespace, unfiltered; the caller applies [`visible`].
    pub async fn list(&self) -> Result<Vec<Value>, ResourceError> {
        match self {
            Self::Kube(api) => {
                let objects = api
                    .list(&kube::api::ListParams::default())
                    .await
                    .map_err(|e| ResourceError::Unavailable(e.to_string()))?;
                objects
                    .items
                    .into_iter()
                    .map(|o| {
                        serde_json::to_value(o)
                            .map_err(|e| ResourceError::Unavailable(e.to_string()))
                    })
                    .collect()
            }
            #[cfg(test)]
            Self::Memory(items) => Ok(items
                .lock()
                .expect("memory pipeline resources poisoned")
                .values()
                .cloned()
                .collect()),
        }
    }

    /// One `Pipeline` by name; `None` when it does not exist.
    pub async fn get(&self, name: &str) -> Result<Option<Value>, ResourceError> {
        match self {
            Self::Kube(api) => api
                .get_opt(name)
                .await
                .map_err(|e| ResourceError::Unavailable(e.to_string()))
                .map(|object| {
                    object.map(|o| {
                        serde_json::to_value(o)
                            .map_err(|e| ResourceError::Unavailable(e.to_string()))
                    })
                })?
                .transpose(),
            #[cfg(test)]
            Self::Memory(items) => Ok(items
                .lock()
                .expect("memory pipeline resources poisoned")
                .get(name)
                .cloned()),
        }
    }
}

/// The resource's `metadata.name`, or the empty string for a malformed object.
fn pipeline_name(resource: &Value) -> &str {
    resource
        .get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The resource's `spec`; the resource itself when the field is missing (every read then sees
/// no owner group, no tasks and no schedule, which is the safe answer).
fn spec_of(resource: &Value) -> &Value {
    resource.get("spec").unwrap_or(resource)
}

// ---------------------------------------------------------------------------
// Pure rules
// ---------------------------------------------------------------------------

/// Who may see a pipeline. Administrators see all; everybody else sees a pipeline whose
/// `spec.ownerGroup` names one of their groups. No `ownerGroup` means administrators only.
pub fn visible(identity: &Identity, spec: &Value) -> bool {
    if identity.is_admin {
        return true;
    }
    let Some(group) = spec.get("ownerGroup").and_then(Value::as_str) else {
        return false;
    };
    identity.groups.iter().any(|g| g == group)
}

/// The single pipeline state the UI shows.
///
/// `loading` while Airflow has no DAG for the resource yet. `paused` wins over everything else
/// (a paused pipeline is not "failed" even if its last run failed). A last run that is
/// `running` or `queued` makes the pipeline `running`; `failed`/`upstream_failed` is `failed`;
/// `success` is `succeeded`; anything else (never ran, `skipped`, …) is `idle`.
pub fn pipeline_state(paused: bool, loaded: bool, last_run_state: Option<&str>) -> &'static str {
    if !loaded {
        return "loading";
    }
    if paused {
        return "paused";
    }
    match last_run_state {
        Some("running") | Some("queued") => "running",
        Some("failed") | Some("upstream_failed") => "failed",
        Some("success") => "succeeded",
        _ => "idle",
    }
}

/// `"0 3 * * * (Europe/Berlin)"`, or just the cron when the schedule has no time zone. A
/// schedule written as a bare string is accepted too (the `IngestionSource` shape).
fn schedule_of(spec: &Value) -> Option<String> {
    match spec.get("schedule")? {
        Value::String(cron) if !cron.trim().is_empty() => Some(cron.trim().to_string()),
        Value::Object(schedule) => {
            let cron = schedule.get("cron")?.as_str()?.trim();
            if cron.is_empty() {
                return None;
            }
            match schedule
                .get("timezone")
                .and_then(Value::as_str)
                .filter(|tz| !tz.is_empty())
            {
                Some(timezone) => Some(format!("{cron} ({timezone})")),
                None => Some(cron.to_string()),
            }
        }
        _ => None,
    }
}

/// The task names and kinds the resource declares, in declaration order.
fn task_specs(spec: &Value) -> Vec<TaskSpecView> {
    spec.get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(|task| {
                    Some(TaskSpecView {
                        name: task.get("name")?.as_str()?.to_string(),
                        kind: task
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `${params.<name>}` values as the resource carries them.
fn params_of(spec: &Value) -> BTreeMap<String, String> {
    spec.get("params")
        .and_then(Value::as_object)
        .map(|params| {
            params
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The retry policy as the resource carries it.
fn retries_of(spec: &Value) -> Option<RetriesView> {
    let retries = spec.get("retries")?;
    Some(RetriesView {
        attempts: retries.get("attempts")?.as_i64()?,
        backoff: retries
            .get("backoff")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

// ---------------------------------------------------------------------------
// Time: Airflow's RFC 3339 strings and a run's duration (no date dependency)
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let month_index = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * month_index + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp, fraction included. Answers `None`
/// for anything Airflow would not send (a missing date, an unknown zone, trailing text).
fn parse_rfc3339(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let year = number(0..4)?;
    if bytes.get(4) != Some(&b'-') {
        return None;
    }
    let month = number(5..7)? as u32;
    if bytes.get(7) != Some(&b'-') {
        return None;
    }
    let day = number(8..10)? as u32;
    if !matches!(bytes.get(10), Some(b'T') | Some(b't') | Some(b' ')) {
        return None;
    }
    let hour = number(11..13)?;
    if bytes.get(13) != Some(&b':') {
        return None;
    }
    let minute = number(14..16)?;
    if bytes.get(16) != Some(&b':') {
        return None;
    }
    let second = number(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let mut at = 19;
    let mut fraction = 0.0;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
        let digits: String = text[start..at].chars().take(9).collect();
        fraction = format!("0.{digits}").parse::<f64>().ok()?;
    }

    let offset = match bytes.get(at) {
        Some(b'Z') | Some(b'z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'+' { 1 } else { -1 };
            let hours = number(at + 1..at + 3)?;
            if bytes.get(at + 3) != Some(&b':') {
                return None;
            }
            let minutes = number(at + 4..at + 6)?;
            at += 6;
            sign * (hours * 3_600 + minutes * 60)
        }
        _ => return None,
    };
    if at != bytes.len() {
        return None;
    }

    let seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    Some(seconds as f64 + fraction)
}

/// The seconds between two Airflow timestamps, rounded to microseconds; `None` unless both
/// parse and the end is not before the start. Airflow's ISO strings carry microseconds, and a
/// float difference would otherwise leak binary noise into the JSON.
fn duration_seconds(started: Option<&str>, ended: Option<&str>) -> Option<f64> {
    let ended = parse_rfc3339(ended?)?;
    let started = parse_rfc3339(started?)?;
    let duration = ended - started;
    (duration >= 0.0).then(|| (duration * 1_000_000.0).round() / 1_000_000.0)
}

// ---------------------------------------------------------------------------
// Views (serde camelCase; no raw Airflow field names and no `sources`)
// ---------------------------------------------------------------------------

/// One task as the resource declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct TaskSpecView {
    pub name: String,
    pub kind: String,
}

/// One pipeline in the list: the resource plus what Airflow says about it.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PipelineView {
    pub name: String,
    pub namespace: String,
    /// `spec.ownerGroup`; `None` means administrators only.
    pub owner_group: Option<String>,
    /// The cron schedule, with the time zone in parentheses when there is one.
    pub schedule: Option<String>,
    pub paused: bool,
    /// `false` while Airflow has no DAG for the resource yet; the loader needs about 25 seconds.
    pub loaded: bool,
    pub next_run: Option<String>,
    /// `loading`, `paused`, `running`, `failed`, `succeeded` or `idle`.
    pub state: String,
    pub last_run: Option<RunView>,
    pub tasks: Vec<TaskSpecView>,
    pub run_as: Option<String>,
    /// `spec.concurrency` as the resource carries it (`Allow`, `Forbid`, `Replace`).
    pub concurrency: String,
}

/// One DagRun. `type` is Airflow's `run_type` under the name the UI uses.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub run_id: String,
    pub state: String,
    #[serde(rename = "type")]
    pub run_type: String,
    pub logical_date: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_seconds: Option<f64>,
    pub triggered_by: Option<String>,
    pub note: Option<String>,
}

/// One task instance of a run.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub task_id: String,
    pub state: String,
    pub try_number: u32,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_seconds: Option<f64>,
    pub operator: Option<String>,
}

/// One log message. `sources` is deliberately absent: it names internal object-store paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct LogLine {
    pub timestamp: Option<String>,
    pub level: Option<String>,
    pub text: String,
}

/// One page of a task's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogView {
    pub lines: Vec<LogLine>,
    pub next_token: Option<String>,
    pub truncated: bool,
}

/// The retry policy of a pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct RetriesView {
    pub attempts: i64,
    pub backoff: Option<String>,
}

/// `GET /api/pipelines`.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PipelineListResponse {
    pub pipelines: Vec<PipelineView>,
}

/// `GET /api/pipelines/{namespace}/{name}`: the view, the latest runs and the resource's
/// parameters and retry policy.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PipelineDetail {
    #[serde(flatten)]
    pub pipeline: PipelineView,
    pub runs: Vec<RunView>,
    pub params: BTreeMap<String, String>,
    pub retries: Option<RetriesView>,
}

/// `GET …/runs/{run_id}`: one run and its task instances.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct RunResponse {
    pub run: RunView,
    pub tasks: Vec<TaskView>,
}

fn run_view(run: &DagRun) -> RunView {
    RunView {
        run_id: run.dag_run_id.clone(),
        state: run.state.clone().unwrap_or_default(),
        run_type: run.run_type.clone().unwrap_or_default(),
        logical_date: run.logical_date.clone(),
        started_at: run.start_date.clone(),
        ended_at: run.end_date.clone(),
        duration_seconds: duration_seconds(run.start_date.as_deref(), run.end_date.as_deref()),
        triggered_by: run.triggered_by.clone(),
        note: run.note.clone(),
    }
}

fn task_view(task: &TaskInstance) -> TaskView {
    TaskView {
        task_id: task.task_id.clone(),
        state: task.state.clone().unwrap_or_default(),
        try_number: task.try_number.unwrap_or(0),
        started_at: task.start_date.clone(),
        ended_at: task.end_date.clone(),
        duration_seconds: task.duration,
        operator: task.operator.clone(),
    }
}

fn view_of(
    namespace: &str,
    name: &str,
    spec: &Value,
    dag: Option<&DagSummary>,
    last_run: Option<&DagRun>,
) -> PipelineView {
    let loaded = dag.is_some();
    let paused = loaded && dag.and_then(|d| d.is_paused).unwrap_or(false);
    let state = pipeline_state(paused, loaded, last_run.and_then(|r| r.state.as_deref()));
    PipelineView {
        name: name.to_string(),
        namespace: namespace.to_string(),
        owner_group: spec
            .get("ownerGroup")
            .and_then(Value::as_str)
            .map(str::to_string),
        schedule: schedule_of(spec),
        paused,
        loaded,
        next_run: dag.and_then(|d| d.next_dagrun_run_after.clone()),
        state: state.to_string(),
        last_run: last_run.map(run_view),
        tasks: task_specs(spec),
        run_as: spec
            .get("runAs")
            .and_then(Value::as_str)
            .map(str::to_string),
        concurrency: spec
            .get("concurrency")
            .and_then(Value::as_str)
            .unwrap_or("Forbid")
            .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Logs: drop the marker, strip ANSI, cap the answer
// ---------------------------------------------------------------------------

/// Remove ANSI escape sequences (`\x1b\[[0-9;]*[A-Za-z]`) from a log event. Multi-byte text
/// survives; only the escape sequences are dropped.
fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == 0x1b && bytes.get(at + 1) == Some(&b'[') {
            let mut end = at + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                at = end + 1;
                continue;
            }
        }
        let ch = text[at..].chars().next().expect("a char starts at `at`");
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// The `::group::Log message source details` marker, and anything else that carries
/// `sources`: those values are internal object-store paths and never leave this module.
fn is_group_marker(event: &LogEvent) -> bool {
    !event.sources.is_empty()
        || event
            .event
            .as_deref()
            .is_some_and(|text| text.trim_start().starts_with("::group::"))
}

/// The most log lines one answer carries.
const MAX_LOG_LINES: usize = 2000;
/// The most log text (bytes) one answer carries.
const MAX_LOG_BYTES: usize = 512 * 1024;

fn log_view(page: LogPage) -> LogView {
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for event in page.content {
        if is_group_marker(&event) {
            continue;
        }
        let text = strip_ansi(event.event.as_deref().unwrap_or_default());
        if lines.len() >= MAX_LOG_LINES || bytes + text.len() > MAX_LOG_BYTES {
            truncated = true;
            break;
        }
        bytes += text.len();
        lines.push(LogLine {
            timestamp: event.timestamp,
            level: event.level,
            text,
        });
    }
    LogView {
        lines,
        next_token: page.continuation_token,
        truncated,
    }
}

// ---------------------------------------------------------------------------
// Errors and the ticketing facts
// ---------------------------------------------------------------------------

fn no_cluster() -> Fail {
    Fail::new(StatusCode::SERVICE_UNAVAILABLE, "no cluster connection")
}

fn scheduler_not_configured() -> Fail {
    Fail::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "the scheduler is not configured",
    )
}

fn no_such_pipeline() -> Fail {
    Fail::new(StatusCode::NOT_FOUND, "no such pipeline")
}

fn no_such_run() -> Fail {
    Fail::new(StatusCode::NOT_FOUND, "no such run")
}

fn no_such_task() -> Fail {
    Fail::new(StatusCode::NOT_FOUND, "no such task")
}

/// An Airflow that refused or failed is an unreachable upstream: 502, never 500. The detail
/// goes to the log; the sentence stays one line (and carries no token — the client scrubs).
fn scheduler_fail(e: AirflowError) -> Fail {
    match e {
        AirflowError::NotConfigured => scheduler_not_configured(),
        other => {
            tracing::warn!(error = %other, "Airflow request failed");
            Fail::new(StatusCode::BAD_GATEWAY, "the scheduler did not answer")
        }
    }
}

/// A lookup that answers nothing when Airflow has no such run.
fn run_lookup_fail(e: AirflowError) -> Fail {
    match e {
        AirflowError::NotFound => no_such_run(),
        other => scheduler_fail(other),
    }
}

/// A lookup that answers nothing when Airflow has no such task (or run).
fn task_lookup_fail(e: AirflowError) -> Fail {
    match e {
        AirflowError::NotFound => no_such_task(),
        other => scheduler_fail(other),
    }
}

fn resource_fail(e: ResourceError) -> Fail {
    let ResourceError::Unavailable(message) = e;
    tracing::warn!(error = %message, "pipeline resource request failed");
    Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
}

/// The latest runs the detail page and the run routes read. Nova never asks Airflow for an
/// id the browser sent without finding it in this list first.
const LATEST_RUNS: u32 = 20;

/// The most last-run calls in flight at once.
const LAST_RUN_CONCURRENCY: usize = 8;

/// Find the resource, check the namespace and the caller in that order, and only then answer
/// the DAG id. A hidden pipeline is indistinguishable from an absent one.
async fn load_visible(
    resources: &PipelineResources,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<(String, Value), Fail> {
    if namespace != crate::cluster::namespace() {
        return Err(no_such_pipeline());
    }
    let Some(resource) = resources.get(name).await.map_err(resource_fail)? else {
        return Err(no_such_pipeline());
    };
    if !visible(who, spec_of(&resource)) {
        return Err(no_such_pipeline());
    }
    Ok((dag_id_for(namespace, name), resource))
}

fn scheduler(airflow: Option<&AirflowClient>) -> Result<&AirflowClient, Fail> {
    airflow.ok_or_else(scheduler_not_configured)
}

// ---------------------------------------------------------------------------
// The route logic. These take their inputs, so the tests call them directly.
// ---------------------------------------------------------------------------

/// `GET /api/pipelines`: the visible resources, one DAG-list call, and one last-run call per
/// visible loaded pipeline (at most eight at a time). A failing last-run call leaves that
/// pipeline's `lastRun` empty; it does not fail the list.
async fn list_pipelines(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
) -> Result<PipelineListResponse, Fail> {
    let resources = resources.ok_or_else(no_cluster)?;
    let airflow = scheduler(airflow)?;
    let namespace = crate::cluster::namespace();

    let mut resources = resources.list().await.map_err(resource_fail)?;
    resources.retain(|resource| visible(who, spec_of(resource)));
    resources.sort_by(|a, b| pipeline_name(a).cmp(pipeline_name(b)));
    if resources.is_empty() {
        // Nothing visible: do not tell Airflow (or a timing observer) that there is anything.
        return Ok(PipelineListResponse {
            pipelines: Vec::new(),
        });
    }

    let dags: BTreeMap<String, DagSummary> = airflow
        .list_dags("pipeline", true)
        .await
        .map_err(scheduler_fail)?
        .into_iter()
        .map(|dag| (dag.dag_id.clone(), dag))
        .collect();

    let loaded: Vec<(String, String)> = resources
        .iter()
        .map(|resource| pipeline_name(resource).to_string())
        .map(|name| {
            let dag_id = dag_id_for(&namespace, &name);
            (name, dag_id)
        })
        .filter(|(_, dag_id)| dags.contains_key(dag_id))
        .collect();

    let last_runs: Vec<(String, Option<DagRun>)> = stream::iter(loaded)
        .map(|(name, dag_id)| async move {
            let run = match airflow.runs(&dag_id, 1).await {
                Ok(runs) => runs.into_iter().next(),
                Err(e) => {
                    tracing::warn!(dag = %dag_id, error = %e, "could not read a pipeline's last run");
                    None
                }
            };
            (name, run)
        })
        .buffer_unordered(LAST_RUN_CONCURRENCY)
        .collect()
        .await;
    let last_runs: BTreeMap<String, Option<DagRun>> = last_runs.into_iter().collect();

    let pipelines = resources
        .iter()
        .map(|resource| {
            let name = pipeline_name(resource);
            let dag = dags.get(&dag_id_for(&namespace, name));
            let last_run = last_runs.get(name).and_then(Option::as_ref);
            view_of(&namespace, name, spec_of(resource), dag, last_run)
        })
        .collect();
    Ok(PipelineListResponse { pipelines })
}

/// `GET /api/pipelines/{namespace}/{name}`: the view, the latest runs, params and retries.
/// A pipeline whose DAG does not exist yet is `loaded: false`, `state: "loading"` — not an
/// error. History of a deleted pipeline is not offered: without the resource the answer is 404.
async fn pipeline_detail(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<PipelineDetail, Fail> {
    let resources = resources.ok_or_else(no_cluster)?;
    let airflow = scheduler(airflow)?;
    let (dag_id, resource) = load_visible(resources, who, namespace, name).await?;

    let dag = match airflow.dag(&dag_id).await {
        Ok(dag) => Some(dag),
        Err(AirflowError::NotFound) => None,
        Err(e) => return Err(scheduler_fail(e)),
    };
    let runs = match dag {
        Some(_) => match airflow.runs(&dag_id, LATEST_RUNS).await {
            Ok(runs) => runs,
            // The DAG vanished between the two calls: show it as loading, not as a failure.
            Err(AirflowError::NotFound) => Vec::new(),
            Err(e) => return Err(scheduler_fail(e)),
        },
        None => Vec::new(),
    };
    let spec = spec_of(&resource);
    let view = view_of(namespace, name, spec, dag.as_ref(), runs.first());
    Ok(PipelineDetail {
        pipeline: view,
        runs: runs.iter().map(run_view).collect(),
        params: params_of(spec),
        retries: retries_of(spec),
    })
}

/// `GET …/runs/{run_id}`: the run and its tasks. The run is looked up through this pipeline's
/// DAG id, so a run id of another pipeline answers 404 and never reaches that DAG.
async fn run_detail(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
    run_id: &str,
) -> Result<RunResponse, Fail> {
    let resources = resources.ok_or_else(no_cluster)?;
    let airflow = scheduler(airflow)?;
    let (dag_id, _) = load_visible(resources, who, namespace, name).await?;

    let runs = airflow
        .runs(&dag_id, LATEST_RUNS)
        .await
        .map_err(run_lookup_fail)?;
    let run = runs
        .iter()
        .find(|run| run.dag_run_id == run_id)
        .ok_or_else(no_such_run)?;
    let tasks = airflow
        .task_instances(&dag_id, &run.dag_run_id)
        .await
        .map_err(task_lookup_fail)?;
    Ok(RunResponse {
        run: run_view(run),
        tasks: tasks.iter().map(task_view).collect(),
    })
}

/// `GET …/tasks/{task_id}/logs`: one page of a task's log. `try` defaults to the task's last
/// attempt (read from the run's task instances). The group marker is dropped and the answer is
/// capped; the audit line names the user, pipeline, run and task — never the content.
#[allow(clippy::too_many_arguments)]
async fn task_logs(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
    run_id: &str,
    task_id: &str,
    try_number: Option<u32>,
    token: Option<&str>,
) -> Result<LogView, Fail> {
    let resources = resources.ok_or_else(no_cluster)?;
    let airflow = scheduler(airflow)?;
    let (dag_id, _) = load_visible(resources, who, namespace, name).await?;

    // Confirm the run belongs to this pipeline before any task id is passed on.
    let runs = airflow
        .runs(&dag_id, LATEST_RUNS)
        .await
        .map_err(run_lookup_fail)?;
    let run = runs
        .iter()
        .find(|run| run.dag_run_id == run_id)
        .ok_or_else(no_such_run)?;

    let try_number = match try_number {
        Some(0) => {
            return Err(Fail::new(
                StatusCode::BAD_REQUEST,
                "try must be a positive number",
            ));
        }
        Some(number) => number,
        None => airflow
            .task_instances(&dag_id, &run.dag_run_id)
            .await
            .map_err(task_lookup_fail)?
            .iter()
            .filter(|task| task.task_id == task_id)
            .filter_map(|task| task.try_number)
            .max()
            .unwrap_or(1),
    };

    let page = airflow
        .logs(&dag_id, &run.dag_run_id, task_id, try_number, token)
        .await
        .map_err(task_lookup_fail)?;
    audit::pipeline_log_event(who, namespace, name, &run.dag_run_id, task_id);
    Ok(log_view(page))
}

// ---------------------------------------------------------------------------
// The handlers: sign-in, the two clients, then one of the functions above.
// ---------------------------------------------------------------------------

/// The pipeline page is never cacheable, including the refusals an extractor produces before a
/// handler runs.
pub(crate) async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The resource client for this request; `None` without a cluster connection.
fn resources(state: &AppState) -> Option<PipelineResources> {
    state.kube.clone().map(PipelineResources::cluster)
}

/// The four read routes, all uncacheable.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines", get(list))
        .route("/pipelines/{namespace}/{name}", get(detail))
        .route("/pipelines/{namespace}/{name}/runs/{run_id}", get(run))
        .route(
            "/pipelines/{namespace}/{name}/runs/{run_id}/tasks/{task_id}/logs",
            get(logs),
        )
        .route_layer(axum::middleware::from_fn(no_store))
}

fn answer<T: Serialize>(result: Result<T, Fail>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/api/pipelines",
    tag = "public",
    description = "The pipelines the caller may see: their groups' pipelines, and every pipeline for an administrator. A pipeline without `spec.ownerGroup` is administrators-only. `state` is one of `loading`, `paused`, `running`, `failed`, `succeeded`, `idle`; `loaded` is false while Airflow has no DAG for a new pipeline yet. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "The visible pipelines, sorted by name", body = PipelineListResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(signed: SignedIn) -> Response {
    answer(
        list_pipelines(
            resources(signed.state()).as_ref(),
            signed.state().airflow.as_ref(),
            signed.identity(),
        )
        .await,
    )
}

#[utoipa::path(
    get,
    path = "/api/pipelines/{namespace}/{name}",
    tag = "public",
    description = "One pipeline: the list fields, the latest 20 runs, and the resource's `params` and `retries`. A pipeline whose DAG does not exist yet answers 200 with `loaded: false` and `state: \"loading\"`. A pipeline of another group answers 404. History of a deleted pipeline is not offered in this version: without the resource the answer is 404.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    responses(
        (status = 200, description = "The pipeline, its latest runs, params and retries", body = PipelineDetail),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn detail(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
) -> Response {
    answer(
        pipeline_detail(
            resources(signed.state()).as_ref(),
            signed.state().airflow.as_ref(),
            signed.identity(),
            &namespace,
            &name,
        )
        .await,
    )
}

#[utoipa::path(
    get,
    path = "/api/pipelines/{namespace}/{name}/runs/{run_id}",
    tag = "public",
    description = "One run of a pipeline with its task instances. The run id is looked up through this pipeline's DAG id, so a run id of another pipeline answers 404. Answers `Cache-Control: no-store`.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
        ("run_id" = String, Path, description = "Airflow's run id, for example `manual__2026-10-20T07:23:50.608876+00:00`"),
    ),
    responses(
        (status = 200, description = "The run and its tasks", body = RunResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller, or no such run", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn run(
    signed: SignedIn,
    Path((namespace, name, run_id)): Path<(String, String, String)>,
) -> Response {
    answer(
        run_detail(
            resources(signed.state()).as_ref(),
            signed.state().airflow.as_ref(),
            signed.identity(),
            &namespace,
            &name,
            &run_id,
        )
        .await,
    )
}

#[utoipa::path(
    get,
    path = "/api/pipelines/{namespace}/{name}/runs/{run_id}/tasks/{task_id}/logs",
    tag = "public",
    description = "One page of a task's log: at most 2000 lines and 512 KiB, ANSI escapes removed, the `::group::` source marker dropped (it names internal object-store paths and is never returned). `try` defaults to the task's last attempt; `token` is the `nextToken` of the previous answer. Every read writes one audit line with user, pipeline, run and task — never the content. Answers `Cache-Control: no-store`.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
        ("run_id" = String, Path, description = "Airflow's run id"),
        ("task_id" = String, Path, description = "Task id"),
        ("try" = Option<u32>, Query, description = "Attempt number; defaults to the task's last try"),
        ("token" = Option<String>, Query, description = "Continuation token from a previous answer"),
    ),
    responses(
        (status = 200, description = "The log page", body = LogView),
        (status = 400, description = "`try` is not a positive number", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller, no such run, or no such task", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn logs(
    signed: SignedIn,
    Path((namespace, name, run_id, task_id)): Path<(String, String, String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    let try_number = match query.get("try").map(|value| value.parse::<u32>()) {
        None => None,
        Some(Ok(number)) => Some(number),
        Some(Err(_)) => {
            return Fail::new(StatusCode::BAD_REQUEST, "try must be a positive number")
                .into_response();
        }
    };
    answer(
        task_logs(
            resources(signed.state()).as_ref(),
            signed.state().airflow.as_ref(),
            signed.identity(),
            &namespace,
            &name,
            &run_id,
            &task_id,
            try_number,
            query.get("token").map(String::as_str),
        )
        .await,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};

    use axum::{
        Extension,
        body::Body,
        http::{Request, StatusCode, header},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::airflow::{AirflowConfig, DagRun, DagSummary};
    use crate::auth::{Principal, Secret};
    use crate::secret_store::StoreClient;

    /// A value that must never appear in a log answer: the marker's `sources` path.
    const SOURCES_CANARY: &str = "s3://backups/airflow-logs/dag_id=stardelt__smoke/run_id=manual__2026-10-20T07:23:50.608876+00:00/task_id=insert/attempt=3.log";
    const SMOKE_RUN: &str = "manual__2026-10-20T07:23:50.608876+00:00";
    const OTHER_RUN: &str = "manual__2026-10-20T09:00:00.000000+00:00";

    fn who(sub: &str, groups: &[&str], admin: bool) -> Identity {
        Identity {
            sub: sub.into(),
            display_name: sub.into(),
            email: None,
            name: None,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            roles: vec![],
            groups_overage: false,
            is_admin: admin,
        }
    }

    fn admin() -> Identity {
        who("sub-root", &["stardelt-admins"], true)
    }

    /// One `Pipeline` resource, with the fields the operator's CRD carries.
    fn resource(name: &str, owner_group: Option<&str>) -> Value {
        json!({
            "apiVersion": "stardelt.io/v1alpha1",
            "kind": "Pipeline",
            "metadata": {
                "name": name,
                "namespace": crate::cluster::namespace(),
            },
            "spec": {
                "ownerGroup": owner_group,
                "tasks": [
                    { "name": "schema", "kind": "sql" },
                    { "name": "insert", "kind": "sql" }
                ],
                "schedule": { "cron": "0 3 * * *", "timezone": "Europe/Berlin" },
                "runAs": "pipeline-analytics",
                "params": { "target": "warehouse.smoke" },
                "retries": { "attempts": 2, "backoff": "5m" },
                "concurrency": "Forbid",
                "engine": "airflow"
            }
        })
    }

    fn memory(items: &[(&str, Value)]) -> PipelineResources {
        let items: BTreeMap<String, Value> = items
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        PipelineResources::Memory(Arc::new(Mutex::new(items)))
    }

    fn dag(dag_id: &str) -> Value {
        json!({
            "dag_id": dag_id,
            "dag_display_name": dag_id,
            "is_paused": false,
            "is_stale": false,
            "timetable_description": "0 3 * * *",
            "next_dagrun_run_after": "2026-10-21T03:00:00Z",
            "last_parsed_time": "2026-10-20T07:24:29.819955Z",
            "tags": [{ "name": "pipeline" }, { "name": "stardelt" }]
        })
    }

    fn run(dag_run_id: &str, state: &str) -> Value {
        json!({
            "dag_run_id": dag_run_id,
            "state": state,
            "run_type": "manual",
            "logical_date": "2026-10-20T07:23:50Z",
            "start_date": "2026-10-20T07:23:50.608876Z",
            "end_date": "2026-10-20T07:23:58.876923Z",
            "note": "from the test",
            "triggered_by": "rest_api",
            "triggering_user_name": "nova-api",
            "conf": {},
            "run_after": "2026-10-20T07:23:50.608876Z"
        })
    }

    fn task(task_id: &str, try_number: u32) -> Value {
        json!({
            "task_id": task_id,
            "state": "success",
            "try_number": try_number,
            "start_date": "2026-10-20T07:23:50.608876Z",
            "end_date": "2026-10-20T07:23:58.876923Z",
            "duration": 8.268047,
            "operator": "PythonOperator",
            "map_index": -1
        })
    }

    fn log_page() -> Value {
        json!({
            "content": [
                {
                    "event": "::group::Log message source details",
                    "sources": [SOURCES_CANARY]
                },
                {
                    "timestamp": "2026-10-20T07:23:51.000000Z",
                    "level": "info",
                    "event": "\u{1b}[32minsert\u{1b}[0m done"
                },
                {
                    "timestamp": "2026-10-20T07:23:52.000000Z",
                    "level": "warning",
                    "event": "still here"
                }
            ],
            "continuation_token": null
        })
    }

    /// A fake Airflow with the few routes the read API uses. The marker's `sources` path is a
    /// canary: it must never appear in an answer.
    mod fake_airflow {
        use std::collections::{BTreeMap, BTreeSet};
        use std::sync::{Arc, Mutex};

        use axum::{
            Json, Router,
            extract::{OriginalUri, Path, Query, State},
            http::{HeaderMap, StatusCode, Uri, header},
            routing::{get, post},
        };
        use serde_json::{Value, json};

        use crate::airflow::{AirflowClient, AirflowConfig};
        use crate::auth::Secret;

        use super::{OTHER_RUN, SMOKE_RUN, dag, log_page, run, task};

        const PASSWORD: &str = "pw-canary-pipeline-4a17";
        const TOKEN: &str = "tok-canary-pipeline-4a17";

        type Refusal = (StatusCode, Json<Value>);

        fn refused() -> Refusal {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "detail": "Invalid credentials" })),
            )
        }

        #[derive(Default)]
        pub struct Fake {
            /// `METHOD path?query` of every call except the sign-in.
            pub calls: Vec<String>,
            pub dags: Vec<Value>,
            pub runs: BTreeMap<String, Vec<Value>>,
            pub tasks: BTreeMap<String, Vec<Value>>,
            pub log_page: Value,
            /// DAG ids whose run listing answers `500`.
            pub fail_runs: BTreeSet<String>,
        }

        pub type Shared = Arc<Mutex<Fake>>;

        fn seeded() -> Fake {
            let mut fake = Fake {
                dags: vec![
                    dag("stardelt__smoke"),
                    dag("stardelt__other"),
                    dag("stardelt__hidden"),
                ],
                log_page: log_page(),
                ..Fake::default()
            };
            fake.runs
                .insert("stardelt__smoke".into(), vec![run(SMOKE_RUN, "success")]);
            fake.runs
                .insert("stardelt__other".into(), vec![run(OTHER_RUN, "failed")]);
            fake.runs.insert(
                "stardelt__hidden".into(),
                vec![run("hidden__2026-10-20T08:00:00+00:00", "success")],
            );
            fake.tasks
                .insert(SMOKE_RUN.into(), vec![task("insert", 3), task("schema", 1)]);
            fake.tasks.insert(OTHER_RUN.into(), vec![task("insert", 1)]);
            fake
        }

        fn line(uri: &Uri) -> String {
            uri.path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| uri.path().to_string())
        }

        fn remember(state: &mut Fake, method: &str, uri: &Uri) {
            state.calls.push(format!("{method} {}", line(uri)));
        }

        fn authorize(headers: &HeaderMap) -> Result<(), Refusal> {
            let expected = format!("Bearer {TOKEN}");
            if headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                != Some(expected.as_str())
            {
                return Err(refused());
            }
            Ok(())
        }

        async fn login(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            if body["username"].as_str() == Some("nova-api")
                && body["password"].as_str() == Some(PASSWORD)
            {
                return (StatusCode::CREATED, Json(json!({ "access_token": TOKEN })));
            }
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "detail": "Invalid credentials" })),
            )
        }

        async fn dags(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Query(query): Query<BTreeMap<String, String>>,
            headers: HeaderMap,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "GET", &uri);
            authorize(&headers)?;
            let total = state.dags.len();
            let offset: usize = query
                .get("offset")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let limit: usize = query
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100);
            let page: Vec<Value> = state
                .dags
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect();
            Ok(Json(json!({ "dags": page, "total_entries": total })))
        }

        async fn one_dag(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path(dag_id): Path<String>,
            headers: HeaderMap,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "GET", &uri);
            authorize(&headers)?;
            match state.dags.iter().find(|dag| dag["dag_id"] == dag_id) {
                Some(dag) => Ok(Json(dag.clone())),
                None => Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DAG not found" })),
                )),
            }
        }

        async fn runs(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path(dag_id): Path<String>,
            Query(query): Query<BTreeMap<String, String>>,
            headers: HeaderMap,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "GET", &uri);
            authorize(&headers)?;
            if state.fail_runs.contains(&dag_id) {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "detail": "boom" })),
                ));
            }
            let Some(runs) = state.runs.get(&dag_id) else {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DAG not found" })),
                ));
            };
            let limit: usize = query
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100);
            let page: Vec<Value> = runs.iter().take(limit).cloned().collect();
            Ok(Json(
                json!({ "dag_runs": page, "total_entries": runs.len() }),
            ))
        }

        fn run_belongs(state: &Fake, dag_id: &str, run_id: &str) -> bool {
            state
                .runs
                .get(dag_id)
                .is_some_and(|runs| runs.iter().any(|run| run["dag_run_id"] == run_id))
        }

        async fn tasks(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path((dag_id, run_id)): Path<(String, String)>,
            headers: HeaderMap,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "GET", &uri);
            authorize(&headers)?;
            if !run_belongs(&state, &dag_id, &run_id) {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DagRun not found" })),
                ));
            }
            let tasks = state.tasks.get(&run_id).cloned().unwrap_or_default();
            Ok(Json(
                json!({ "task_instances": tasks, "total_entries": tasks.len() }),
            ))
        }

        async fn logs(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path((dag_id, run_id, _task_id, _try_number)): Path<(String, String, String, u32)>,
            headers: HeaderMap,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "GET", &uri);
            authorize(&headers)?;
            if !run_belongs(&state, &dag_id, &run_id) {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DagRun not found" })),
                ));
            }
            Ok(Json(state.log_page.clone()))
        }

        pub async fn start() -> (AirflowClient, Shared) {
            let shared: Shared = Arc::new(Mutex::new(seeded()));
            let app = Router::new()
                .route("/auth/token", post(login))
                .route("/api/v2/dags", get(dags))
                .route("/api/v2/dags/{dag_id}", get(one_dag))
                .route("/api/v2/dags/{dag_id}/dagRuns", get(runs))
                .route(
                    "/api/v2/dags/{dag_id}/dagRuns/{run_id}/taskInstances",
                    get(tasks),
                )
                .route(
                    "/api/v2/dags/{dag_id}/dagRuns/{run_id}/taskInstances/{task_id}/logs/{try_number}",
                    get(logs),
                )
                .with_state(Arc::clone(&shared));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let config = AirflowConfig {
                url: format!("http://{addr}"),
                user: "nova-api".into(),
                password: Secret::new(PASSWORD),
            };
            (AirflowClient::new(reqwest::Client::new(), config), shared)
        }
    }

    /// A fake Kubernetes API serving the two `Pipeline` routes the resource client uses.
    mod fake_kube {
        use std::sync::{Arc, Mutex};

        use axum::{
            Json, Router,
            extract::{Path, State},
            http::StatusCode,
            routing::get,
        };
        use serde_json::{Value, json};

        struct Fake {
            items: Vec<Value>,
        }

        type Shared = Arc<Mutex<Fake>>;

        async fn list(State(state): State<Shared>) -> Json<Value> {
            let state = state.lock().unwrap();
            Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "PipelineList",
                "metadata": { "resourceVersion": "1" },
                "items": state.items
            }))
        }

        async fn one(
            State(state): State<Shared>,
            Path((_namespace, name)): Path<(String, String)>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let state = state.lock().unwrap();
            match state
                .items
                .iter()
                .find(|item| item["metadata"]["name"] == name)
            {
                Some(item) => Ok(Json(item.clone())),
                None => Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "kind": "Status",
                        "apiVersion": "v1",
                        "status": "Failure",
                        "message": "not found",
                        "reason": "NotFound",
                        "code": 404
                    })),
                )),
            }
        }

        pub async fn start(items: Vec<Value>) -> kube::Client {
            let shared: Shared = Arc::new(Mutex::new(Fake { items }));
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/pipelines",
                    get(list),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/pipelines/{name}",
                    get(one),
                )
                .with_state(shared);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let url = format!("http://{addr}").parse().unwrap();
            kube::Client::try_from(kube::Config::new(url)).expect("a client for the fake API")
        }
    }

    fn test_state(kube: kube::Client, airflow: AirflowClient, store: StoreClient) -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: "http://127.0.0.1:1".into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            warehouse_id: tokio::sync::OnceCell::new(),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(crate::demo::DemoState::default()),
            kube: Some(kube),
            workloads: crate::cluster::WorkloadCache::default(),
            admin_console_url: None,
            db: None,
            store: Some(store),
            break_glass_reads: Mutex::new(std::collections::HashMap::new()),
            airflow: Some(airflow),
        })
    }

    fn principal(identity: Identity) -> Principal {
        Principal {
            identity,
            via: "session",
            access_token: Some(Secret::new("good")),
        }
    }

    // -----------------------------------------------------------------------
    // Visibility
    // -----------------------------------------------------------------------

    #[test]
    fn visibility_follows_the_owner_group() {
        let member = who("sub-ada", &["analytics"], false);
        let outsider = who("sub-bob", &["finance"], false);
        let administrator = who("sub-root", &[], true);
        let owned = json!({ "ownerGroup": "analytics" });

        assert!(
            visible(&member, &owned),
            "a member sees the group's pipeline"
        );
        assert!(!visible(&outsider, &owned), "another group does not");
        assert!(visible(&administrator, &owned), "an administrator sees all");

        let unowned = json!({});
        assert!(
            !visible(&member, &unowned),
            "no ownerGroup is administrators-only"
        );
        assert!(visible(&administrator, &unowned));

        let other = json!({ "ownerGroup": "finance" });
        assert!(
            !visible(&member, &other),
            "membership is by exact group name"
        );
    }

    #[tokio::test]
    async fn list_hides_other_groups_pipelines() {
        let resources = memory(&[
            ("smoke", resource("smoke", Some("analytics"))),
            ("hidden", resource("hidden", Some("finance"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;

        let answer = list_pipelines(
            Some(&resources),
            Some(&airflow),
            &who("sub-ada", &["analytics"], false),
        )
        .await
        .expect("the list succeeds");

        let names: Vec<&str> = answer
            .pipelines
            .iter()
            .map(|pipeline| pipeline.name.as_str())
            .collect();
        assert_eq!(names, ["smoke"], "only the caller's group is listed");

        let calls = fake.lock().unwrap().calls.clone();
        assert!(
            !calls.iter().any(|call| call.contains("stardelt__hidden")),
            "Airflow heard nothing about the hidden pipeline: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|call| call.contains("stardelt__smoke/dagRuns")),
            "the visible pipeline's last run was read: {calls:?}"
        );
    }

    #[tokio::test]
    async fn detail_of_a_hidden_pipeline_is_404_not_403() {
        let resources = memory(&[("hidden", resource("hidden", Some("finance")))]);
        let (airflow, fake) = fake_airflow::start().await;
        let outsider = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let error = pipeline_detail(
            Some(&resources),
            Some(&airflow),
            &outsider,
            &namespace,
            "hidden",
        )
        .await
        .expect_err("hidden");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "no such pipeline");
        assert!(
            fake.lock().unwrap().calls.is_empty(),
            "the hidden pipeline never reaches Airflow"
        );
    }

    #[tokio::test]
    async fn runs_and_logs_of_a_hidden_pipeline_are_404_not_403() {
        let resources = memory(&[("hidden", resource("hidden", Some("finance")))]);
        let (airflow, fake) = fake_airflow::start().await;
        let outsider = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let error = run_detail(
            Some(&resources),
            Some(&airflow),
            &outsider,
            &namespace,
            "hidden",
            "hidden__2026-10-20T08:00:00+00:00",
        )
        .await
        .expect_err("hidden");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);

        let error = task_logs(
            Some(&resources),
            Some(&airflow),
            &outsider,
            &namespace,
            "hidden",
            "hidden__2026-10-20T08:00:00+00:00",
            "insert",
            Some(1),
            None,
        )
        .await
        .expect_err("hidden");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert!(
            fake.lock().unwrap().calls.is_empty(),
            "neither route asks Airflow about a hidden pipeline"
        );
    }

    // -----------------------------------------------------------------------
    // State
    // -----------------------------------------------------------------------

    #[test]
    fn pipeline_state_table() {
        for loaded in [true, false] {
            for paused in [true, false] {
                for last in [
                    None,
                    Some("running"),
                    Some("queued"),
                    Some("failed"),
                    Some("upstream_failed"),
                    Some("success"),
                    Some("skipped"),
                ] {
                    let state = pipeline_state(paused, loaded, last);
                    let expected = if !loaded {
                        "loading"
                    } else if paused {
                        "paused"
                    } else {
                        match last {
                            Some("running") | Some("queued") => "running",
                            Some("failed") | Some("upstream_failed") => "failed",
                            Some("success") => "succeeded",
                            _ => "idle",
                        }
                    };
                    assert_eq!(
                        state, expected,
                        "paused={paused} loaded={loaded} last={last:?}"
                    );
                }
            }
        }
        assert_eq!(
            pipeline_state(true, true, Some("failed")),
            "paused",
            "paused beats a failed last run"
        );
        assert_eq!(
            pipeline_state(false, true, Some("queued")),
            "running",
            "queued counts as running"
        );
        assert_eq!(
            pipeline_state(false, true, None),
            "idle",
            "never ran is idle"
        );
    }

    #[tokio::test]
    async fn a_new_pipeline_without_a_dag_is_loading() {
        let resources = memory(&[("brand-new", resource("brand-new", Some("analytics")))]);
        let (airflow, _fake) = fake_airflow::start().await;

        let detail = pipeline_detail(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "brand-new",
        )
        .await
        .expect("a pipeline without a DAG is not an error");

        assert!(!detail.pipeline.loaded);
        assert_eq!(detail.pipeline.state, "loading");
        assert!(!detail.pipeline.paused);
        assert_eq!(detail.pipeline.next_run, None);
        assert_eq!(detail.pipeline.last_run, None);
        assert!(detail.runs.is_empty());
    }

    #[tokio::test]
    async fn list_calls_airflow_once_for_the_dags() {
        let resources = memory(&[
            ("smoke", resource("smoke", Some("analytics"))),
            ("other", resource("other", Some("analytics"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;

        list_pipelines(
            Some(&resources),
            Some(&airflow),
            &who("sub-ada", &["analytics"], false),
        )
        .await
        .expect("the list succeeds");

        let calls = fake.lock().unwrap().calls.clone();
        let dag_lists = calls
            .iter()
            .filter(|call| call.starts_with("GET /api/v2/dags?"))
            .count();
        let last_runs = calls
            .iter()
            .filter(|call| call.contains("/dagRuns?") && !call.contains("/logs/"))
            .count();
        assert_eq!(
            dag_lists, 1,
            "one DAG list for the whole request: {calls:?}"
        );
        let dag_list = calls
            .iter()
            .find(|call| call.starts_with("GET /api/v2/dags?"))
            .expect("a DAG list call");
        assert!(
            dag_list.contains("tags=pipeline") && dag_list.contains("exclude_stale=false"),
            "the list asks for pipeline DAGs including stale ones: {dag_list}"
        );
        assert_eq!(
            last_runs, 2,
            "one last-run call per loaded pipeline: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_failing_last_run_call_does_not_fail_the_list() {
        let resources = memory(&[
            ("smoke", resource("smoke", Some("analytics"))),
            ("other", resource("other", Some("analytics"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;
        fake.lock()
            .unwrap()
            .fail_runs
            .insert("stardelt__other".into());

        let answer = list_pipelines(
            Some(&resources),
            Some(&airflow),
            &who("sub-ada", &["analytics"], false),
        )
        .await
        .expect("one broken last-run call must not fail the list");

        let smoke = answer
            .pipelines
            .iter()
            .find(|pipeline| pipeline.name == "smoke")
            .expect("smoke");
        let other = answer
            .pipelines
            .iter()
            .find(|pipeline| pipeline.name == "other")
            .expect("other");
        assert!(smoke.last_run.is_some());
        assert_eq!(smoke.state, "succeeded");
        assert_eq!(other.last_run, None, "the failed call leaves lastRun empty");
        assert_eq!(other.state, "idle");
    }

    // -----------------------------------------------------------------------
    // Logs
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn log_lines_drop_the_group_marker_and_ansi() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, _fake) = fake_airflow::start().await;

        let view = task_logs(
            Some(&resources),
            Some(&airflow),
            &who("sub-ada", &["analytics"], false),
            &crate::cluster::namespace(),
            "smoke",
            SMOKE_RUN,
            "insert",
            Some(3),
            None,
        )
        .await
        .expect("the log is readable");

        assert_eq!(view.lines.len(), 2, "the group marker is dropped");
        assert_eq!(view.lines[0].text, "insert done", "ANSI escapes are gone");
        assert_eq!(view.lines[1].text, "still here");

        let serialized = serde_json::to_string(&view).expect("serialises");
        assert!(
            !serialized.contains(SOURCES_CANARY),
            "the marker's object-store path must not be in the answer: {serialized}"
        );
        assert!(!serialized.contains("s3://"));
        assert!(!serialized.contains("::group::"));
        assert!(!serialized.contains("sources"));
    }

    #[tokio::test]
    async fn log_answer_is_capped_and_marked_truncated() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;

        let events: Vec<Value> = (0..MAX_LOG_LINES + 1)
            .map(|index| json!({ "level": "info", "event": format!("line {index}") }))
            .collect();
        fake.lock().unwrap().log_page = json!({
            "content": events,
            "continuation_token": null
        });
        let view = task_logs(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "smoke",
            SMOKE_RUN,
            "insert",
            Some(1),
            None,
        )
        .await
        .expect("the log is readable");
        assert_eq!(view.lines.len(), MAX_LOG_LINES);
        assert!(view.truncated, "more lines existed than the cap allows");

        let big: Vec<Value> = (0..600)
            .map(|_| json!({ "level": "info", "event": "x".repeat(1000) }))
            .collect();
        fake.lock().unwrap().log_page = json!({
            "content": big,
            "continuation_token": null
        });
        let view = task_logs(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "smoke",
            SMOKE_RUN,
            "insert",
            Some(1),
            None,
        )
        .await
        .expect("the log is readable");
        assert!(view.lines.len() < 600, "the byte cap cut the answer");
        assert!(view.truncated);
        let bytes: usize = view.lines.iter().map(|line| line.text.len()).sum();
        assert!(bytes <= MAX_LOG_BYTES);
    }

    #[tokio::test]
    async fn log_token_is_passed_through() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;
        fake.lock().unwrap().log_page = json!({
            "content": [{ "level": "info", "event": "page one" }],
            "continuation_token": "next-page-token"
        });

        let view = task_logs(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "smoke",
            SMOKE_RUN,
            "insert",
            None,
            Some("page-1"),
        )
        .await
        .expect("the log is readable");
        assert_eq!(view.next_token.as_deref(), Some("next-page-token"));

        let calls = fake.lock().unwrap().calls.clone();
        let log_call = calls
            .iter()
            .rev()
            .find(|call| call.contains("/logs/"))
            .expect("a log call");
        assert!(
            log_call.contains("token=page-1"),
            "the continuation token is passed on: {log_call}"
        );
        assert!(
            log_call.contains("/logs/3?"),
            "try defaults to the task's last try (3): {log_call}"
        );
    }

    // -----------------------------------------------------------------------
    // A run id never crosses pipelines
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_run_id_of_another_pipeline_does_not_resolve() {
        let resources = memory(&[
            ("smoke", resource("smoke", Some("analytics"))),
            ("other", resource("other", Some("analytics"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;

        let error = run_detail(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "smoke",
            OTHER_RUN,
        )
        .await
        .expect_err("other's run id must not resolve under smoke");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "no such run");

        let calls = fake.lock().unwrap().calls.clone();
        assert!(
            !calls.iter().any(|call| call.contains("stardelt__other")),
            "the other pipeline's DAG was never addressed: {calls:?}"
        );
        assert!(
            calls.iter().all(|call| call.contains("stardelt__smoke")),
            "every Airflow path used smoke's DAG id: {calls:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Upstreams, headers, view shape
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn airflow_down_is_502_and_not_configured_is_503() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let namespace = crate::cluster::namespace();

        let error = list_pipelines(Some(&resources), None, &admin())
            .await
            .expect_err("no Airflow client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "the scheduler is not configured");

        let error = list_pipelines(None, None, &admin())
            .await
            .expect_err("no cluster client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "no cluster connection");

        let down = AirflowClient::new(
            reqwest::Client::new(),
            AirflowConfig {
                url: "http://127.0.0.1:9".into(),
                user: "nova-api".into(),
                password: Secret::new("x"),
            },
        );
        let error = pipeline_detail(Some(&resources), Some(&down), &admin(), &namespace, "smoke")
            .await
            .expect_err("Airflow is down");
        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(error.message(), "the scheduler did not answer");
    }

    #[tokio::test]
    async fn responses_are_not_cacheable() {
        let (store, _store_fake) = crate::secret_store::fake::start().await;
        let (airflow, _airflow_fake) = fake_airflow::start().await;
        let kube = fake_kube::start(vec![resource("smoke", Some("analytics"))]).await;
        let app = router()
            .with_state(test_state(kube, airflow, store))
            .layer(Extension(principal(admin())));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/pipelines")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "a successful pipeline answer is never cached"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["pipelines"][0]["name"], "smoke");

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/pipelines/{}/no-such-pipeline",
                        crate::cluster::namespace()
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "a refusal is never cached either"
        );
    }

    #[test]
    fn the_view_has_no_raw_airflow_fields() {
        let spec = resource("smoke", Some("analytics"));
        let spec = spec_of(&spec);
        let dag: DagSummary = serde_json::from_value(json!({
            "dag_id": "stardelt__smoke",
            "is_paused": false,
            "is_stale": false,
            "next_dagrun_run_after": "2026-10-21T03:00:00Z",
            "tags": [{ "name": "pipeline" }]
        }))
        .unwrap();
        let run: DagRun = serde_json::from_value(run(SMOKE_RUN, "success")).unwrap();
        let view = view_of("stardelt", "smoke", spec, Some(&dag), Some(&run));

        let value = serde_json::to_value(&view).expect("serialises");
        let keys: BTreeSet<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "name",
                "namespace",
                "ownerGroup",
                "schedule",
                "paused",
                "loaded",
                "nextRun",
                "state",
                "lastRun",
                "tasks",
                "runAs",
                "concurrency",
            ])
        );
        let serialized = serde_json::to_string(&value).unwrap();
        for raw in [
            "dag_id",
            "is_paused",
            "is_stale",
            "next_dagrun_run_after",
            "dag_run_id",
            "run_type",
            "start_date",
            "end_date",
            "try_number",
            "sources",
            "continuation_token",
        ] {
            assert!(
                !serialized.contains(raw),
                "the view must not carry the raw Airflow field {raw}: {serialized}"
            );
        }

        let run = view.last_run.expect("a last run");
        let run = serde_json::to_value(&run).unwrap();
        let keys: BTreeSet<&str> = run
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "runId",
                "state",
                "type",
                "logicalDate",
                "startedAt",
                "endedAt",
                "durationSeconds",
                "triggeredBy",
                "note",
            ])
        );
        let duration = run["durationSeconds"].as_f64().expect("a duration");
        assert!(
            (duration - 8.268047).abs() < 1e-9,
            "the run duration is the difference of its timestamps: {duration}"
        );
    }

    #[tokio::test]
    async fn detail_carries_params_retries_and_schedule() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, _fake) = fake_airflow::start().await;

        let detail = pipeline_detail(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "smoke",
        )
        .await
        .expect("the detail is readable");

        assert_eq!(
            detail.pipeline.schedule.as_deref(),
            Some("0 3 * * * (Europe/Berlin)")
        );
        assert_eq!(
            detail.params.get("target").map(String::as_str),
            Some("warehouse.smoke")
        );
        let retries = detail.retries.expect("a retry policy");
        assert_eq!(retries.attempts, 2);
        assert_eq!(retries.backoff.as_deref(), Some("5m"));
        assert_eq!(detail.runs.len(), 1);
        assert_eq!(detail.pipeline.last_run.as_ref().unwrap().run_id, SMOKE_RUN);
    }

    #[test]
    fn timestamps_parse_with_fractions_and_offsets() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(
            parse_rfc3339("2026-10-20T07:23:50.608876Z"),
            Some(1_792_481_030.608_876)
        );
        assert_eq!(
            parse_rfc3339("2026-10-20T07:23:50+00:00"),
            parse_rfc3339("2026-10-20T07:23:50Z")
        );
        assert_eq!(
            parse_rfc3339("2026-10-20T09:23:50+02:00"),
            parse_rfc3339("2026-10-20T07:23:50Z")
        );
        assert_eq!(parse_rfc3339("not a date"), None);
        assert_eq!(parse_rfc3339("2026-10-20T07:23:50Zjunk"), None);
        let duration = duration_seconds(
            Some("2026-10-20T07:23:50.608876Z"),
            Some("2026-10-20T07:23:58.876923Z"),
        )
        .expect("a duration");
        assert!(
            (duration - 8.268047).abs() < 1e-9,
            "fractions are preserved: {duration}"
        );
        assert_eq!(duration_seconds(None, Some("2026-10-20T07:23:58Z")), None);
    }

    #[test]
    fn ansi_sequences_are_removed_but_text_survives() {
        assert_eq!(strip_ansi("\u{1b}[32minsert\u{1b}[0m done"), "insert done");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("grün \u{1b}[1;31mrot\u{1b}[0m"), "grün rot");
        // The ticket's pattern matches `ESC [ n` as well: the text loses those three bytes.
        assert_eq!(strip_ansi("\u{1b}[not-a-sequence"), "ot-a-sequence");
    }
}
