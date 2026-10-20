//! The Pipelines API: read (list, detail, runs, tasks, logs — stardelt-nova#188) and act (run
//! now, pause, resume, stop a run — stardelt-nova#196); design note `design/scheduled-jobs.md`
//! §6.1, phase J1.
//!
//! Nova is the enforcement point. Airflow's one credential reads every DAG, so every route here
//! first finds the `Pipeline` resource, checks that the caller may see it, and only then asks
//! Airflow. A pipeline the caller may not see answers **404**, exactly like one that does not
//! exist — a refusal that distinguishes "hidden" from "absent" would leak the hidden one.
//!
//! - **Visibility** ([`visible`]): administrators see everything; everybody else sees a pipeline
//!   whose `spec.ownerGroup` is one of their groups. A pipeline without an `ownerGroup` is
//!   administrators-only. Acting uses the same rule ([`may_act`]): v1 has no read-only role, so
//!   whoever may see a pipeline may act on it, and everybody else gets the same 404.
//! - **Airflow's naming**: `Pipeline <namespace>/<name>` has the DAG id `<namespace>__<name>`
//!   ([`airflow::dag_id_for`]). Every Airflow path is built from that id, never from a browser
//!   value, so a run id of another pipeline cannot resolve.
//! - **A new pipeline is loading**: the loader creates the DAG about 25 seconds after the
//!   resource appears. Until then the view says `loaded: false`, `state: "loading"`; after three
//!   minutes without a DAG it says `"not-loaded"` instead (the loader refused this resource).
//! - **Airflow's refusals are Nova's sentences**, not Airflow's silent behaviour:
//!   triggering a paused pipeline would queue a run that never starts (409 here), `Forbid` is not
//!   enforced at trigger time (Nova checks the latest runs itself), a `logical_date` in the past
//!   or future produces a run without tasks (Nova always sends now), and stopping a run marks it
//!   failed — it is not a cancel: in-flight work can finish and commit (see #255).
//! - **Logs**: the `::group::` marker carries internal object-store paths in `sources`; it is
//!   dropped, and no field of an answer can hold a `sources` value. Answers are capped at 2000
//!   lines and 512 KiB and say `truncated: true` beyond that.
//! - **Unreachable upstreams are 502, never 500** (rule U12 of `design/ui-conformance.md`):
//!   without a cluster client or an Airflow client the routes answer 503 and say which one is
//!   missing; an Airflow that refuses or fails answers 502 `the scheduler did not answer`.
//! - **Audit**: every action writes one `audit` line, including a refusal that never reaches
//!   Airflow; no field can hold a note or log text.
//!
//! The route logic sits in plain `async fn`s that take their inputs, so the tests call them
//! without a router; each handler is the thin axum wrapper that checks sign-in and the two
//! clients. The whole surface is uncacheable (`Cache-Control: no-store`).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

#[cfg(test)]
use std::sync::Mutex;

use axum::{
    Json, Router,
    extract::{Path, Query, Request},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
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

/// Who may act on a pipeline: run it now, pause it, resume it, stop one of its runs.
///
/// Exactly who may see it — v1 has no read-only role. Written as a call to [`visible`] on
/// purpose, so the two rules cannot drift; an outsider gets the same 404 as for a read.
pub fn may_act(identity: &Identity, spec: &Value) -> bool {
    visible(identity, spec)
}

/// How long a resource may exist without a DAG before the view calls it `not-loaded`. The loader
/// creates a DAG about 25 seconds after the resource appears; a resource the loader refuses
/// never gets one.
const NOT_LOADED_AFTER_SECONDS: f64 = 3.0 * 60.0;

/// The single pipeline state the UI shows.
///
/// Not loaded: `loading` for the first three minutes after the resource was created (the loader
/// needs about 25 seconds), then `not-loaded` — the loader will not create this DAG, and saying
/// "loading" forever would be a lie. `created_age_seconds` is `None` when the resource carries
/// no `metadata.creationTimestamp`; then the view stays `loading`.
///
/// `paused` wins over everything else (a paused pipeline is not "failed" even if its last run
/// failed). A last run that is `running` or `queued` makes the pipeline `running`;
/// `failed`/`upstream_failed` is `failed`; `success` is `succeeded`; anything else (never ran,
/// `skipped`, …) is `idle`.
pub fn pipeline_state(
    paused: bool,
    loaded: bool,
    last_run_state: Option<&str>,
    created_age_seconds: Option<f64>,
) -> &'static str {
    if !loaded {
        return match created_age_seconds {
            Some(age) if age > NOT_LOADED_AFTER_SECONDS => "not-loaded",
            _ => "loading",
        };
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

/// The civil date for a number of days since 1970-01-01 (the inverse of [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = (days - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for whole seconds since the epoch, always UTC and always with `Z`.
/// Airflow 3 wants a `logical_date`; Nova always sends the current second (`now_rfc3339`).
fn format_rfc3339(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (of_day / 3_600, (of_day % 3_600) / 60, of_day % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Whole seconds since the epoch, as a float for the age comparison.
fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

/// Now, whole seconds, UTC, for Airflow's `logical_date`.
fn now_rfc3339() -> String {
    format_rfc3339(now_seconds() as i64)
}

/// The resource's `metadata.creationTimestamp`, as the cluster wrote it.
fn created_at_of(resource: &Value) -> Option<&str> {
    resource
        .get("metadata")
        .and_then(|metadata| metadata.get("creationTimestamp"))
        .and_then(Value::as_str)
}

/// How long ago the resource was created; `None` without a readable `creationTimestamp`.
fn created_age_seconds(resource: &Value) -> Option<f64> {
    let created = parse_rfc3339(created_at_of(resource)?)?;
    Some((now_seconds() - created).max(0.0))
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
    /// `loading`, `paused`, `running`, `failed`, `succeeded`, `idle`, or `not-loaded` after three
    /// minutes without a DAG.
    pub state: String,
    pub last_run: Option<RunView>,
    pub tasks: Vec<TaskSpecView>,
    pub run_as: Option<String>,
    /// `spec.concurrency` as the resource carries it (`Allow`, `Forbid`, `Replace`).
    pub concurrency: String,
    /// `metadata.creationTimestamp` as the cluster wrote it (RFC 3339); `None` for a malformed
    /// resource.
    pub created_at: Option<String>,
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
    resource: &Value,
    dag: Option<&DagSummary>,
    last_run: Option<&DagRun>,
) -> PipelineView {
    let spec = spec_of(resource);
    let loaded = dag.is_some();
    let paused = loaded && dag.and_then(|d| d.is_paused).unwrap_or(false);
    let state = pipeline_state(
        paused,
        loaded,
        last_run.and_then(|r| r.state.as_deref()),
        created_age_seconds(resource),
    );
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
        created_at: created_at_of(resource).map(str::to_string),
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

/// Airflow has no DAG for the resource yet (or the loader refused it). Acting on a pipeline that
/// does not exist in Airflow is a 409 with a sentence, not a 404: the resource does exist.
fn pipeline_not_loaded() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "the scheduler has not loaded this pipeline yet",
    )
}

/// Triggering a paused pipeline would succeed in Airflow and the run would sit `queued` until
/// someone resumes — it looks like it worked and nothing happens (#255). Nova refuses instead.
fn pipeline_paused() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "the pipeline is paused: resume it first",
    )
}

/// `Forbid` is not enforced by Airflow at trigger time: a second run is created and queued.
fn run_already_active() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "a run is already active: wait for it or stop it",
    )
}

/// Marking a finished run failed would rewrite history; only `queued` and `running` can stop.
fn run_not_active() -> Fail {
    Fail::new(StatusCode::CONFLICT, "the run is not active")
}

fn too_many_runs() -> Fail {
    Fail::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too many runs started; wait a minute",
    )
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

/// The latest runs a "run now" checks for activity: enough to catch a `Forbid`/`Replace` DAG
/// whose last run is still queued or running.
const ACTIVE_RUNS: u32 = 5;

/// At most this many runs started per user per minute.
const RUN_STARTS_PER_MINUTE: usize = 10;

/// The in-memory run-start rate limit (`AppState::run_starts`): per signed-in `sub`, the start
/// times inside the last minute. Refusals before the limiter (unloaded, paused) never reach it;
/// only a restart forgets it.
pub type RunStartsLimiter = std::sync::Mutex<std::collections::HashMap<String, VecDeque<Instant>>>;

/// Take one run-start slot for `sub`, or report that the last minute is full.
fn take_run_slot(limiter: &RunStartsLimiter, sub: &str) -> bool {
    let mut map = limiter.lock().expect("run-start limiter poisoned");
    let now = Instant::now();
    let slots = map.entry(sub.to_string()).or_default();
    while slots
        .front()
        .is_some_and(|then| now.duration_since(*then) >= std::time::Duration::from_secs(60))
    {
        slots.pop_front();
    }
    if slots.len() >= RUN_STARTS_PER_MINUTE {
        false
    } else {
        slots.push_back(now);
        true
    }
}

/// The most last-run calls in flight at once.
const LAST_RUN_CONCURRENCY: usize = 8;

/// Find the resource, check the namespace and the caller in that order, and only then answer
/// the DAG id. A hidden pipeline is indistinguishable from an absent one.
async fn load_allowed(
    resources: &PipelineResources,
    who: &Identity,
    namespace: &str,
    name: &str,
    allow: fn(&Identity, &Value) -> bool,
) -> Result<(String, Value), Fail> {
    if namespace != crate::cluster::namespace() {
        return Err(no_such_pipeline());
    }
    let Some(resource) = resources.get(name).await.map_err(resource_fail)? else {
        return Err(no_such_pipeline());
    };
    if !allow(who, spec_of(&resource)) {
        return Err(no_such_pipeline());
    }
    Ok((dag_id_for(namespace, name), resource))
}

/// A read: who may see the pipeline.
async fn load_visible(
    resources: &PipelineResources,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<(String, Value), Fail> {
    load_allowed(resources, who, namespace, name, visible).await
}

/// An action: who may act on the pipeline (the same rule as seeing it, via [`may_act`]).
async fn load_actionable(
    resources: &PipelineResources,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<(String, Value), Fail> {
    load_allowed(resources, who, namespace, name, may_act).await
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
            view_of(&namespace, name, resource, dag, last_run)
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
    let view = view_of(namespace, name, &resource, dag.as_ref(), runs.first());
    let spec = spec_of(&resource);
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
// Actions: run now, pause, resume, stop a run. Every action writes one audit line.
// ---------------------------------------------------------------------------

/// A refused or failed action: the answer and the audit outcome token (`refused:<why>` or
/// `error`). Keeping the two together is what makes "one audit line per action" true even on the
/// early returns.
struct ActionError {
    fail: Fail,
    outcome: String,
}

impl ActionError {
    fn refused(why: &'static str, fail: Fail) -> Self {
        Self {
            fail,
            outcome: format!("refused:{why}"),
        }
    }

    fn error(fail: Fail) -> Self {
        Self {
            fail,
            outcome: "error".into(),
        }
    }
}

/// `spec.concurrency` as the resource carries it; Airflow's and the CRD's default is `Forbid`.
fn concurrency_of(spec: &Value) -> &str {
    spec.get("concurrency")
        .and_then(Value::as_str)
        .unwrap_or("Forbid")
}

/// `POST …/runs`: start a run now. Refusals happen before Airflow is asked to trigger:
/// not loaded (409), paused (409), rate limit (429), an active run under `Forbid`/`Replace`
/// (409). `Allow` skips the activity check.
async fn run_now(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    limiter: &RunStartsLimiter,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<RunView, Fail> {
    let result = run_now_inner(resources, airflow, limiter, who, namespace, name).await;
    match &result {
        Ok(run) => audit::pipeline_event("run", who, namespace, name, Some(&run.run_id), "ok"),
        Err(e) => audit::pipeline_event("run", who, namespace, name, None, &e.outcome),
    }
    result.map_err(|e| e.fail)
}

async fn run_now_inner(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    limiter: &RunStartsLimiter,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<RunView, ActionError> {
    let resources = resources
        .ok_or_else(no_cluster)
        .map_err(ActionError::error)?;
    let airflow = scheduler(airflow).map_err(ActionError::error)?;
    let (dag_id, resource) = load_actionable(resources, who, namespace, name)
        .await
        .map_err(|fail| ActionError::refused("not-found", fail))?;

    let dag = match airflow.dag(&dag_id).await {
        Ok(dag) => dag,
        Err(AirflowError::NotFound) => {
            return Err(ActionError::refused("not-loaded", pipeline_not_loaded()));
        }
        Err(e) => return Err(ActionError::error(scheduler_fail(e))),
    };
    if dag.is_paused == Some(true) {
        return Err(ActionError::refused("paused", pipeline_paused()));
    }

    // The rate limit comes after the checks that never touch Airflow's trigger: a refusal here
    // means a run was actually going to start.
    if !take_run_slot(limiter, &who.sub) {
        return Err(ActionError::refused("rate-limited", too_many_runs()));
    }

    if concurrency_of(spec_of(&resource)) != "Allow" {
        let runs = match airflow.runs(&dag_id, ACTIVE_RUNS).await {
            Ok(runs) => runs,
            Err(AirflowError::NotFound) => {
                return Err(ActionError::refused("not-loaded", pipeline_not_loaded()));
            }
            Err(e) => return Err(ActionError::error(scheduler_fail(e))),
        };
        if runs
            .iter()
            .any(|run| matches!(run.state.as_deref(), Some("queued") | Some("running")))
        {
            return Err(ActionError::refused("already-active", run_already_active()));
        }
    }

    // Always now, whole seconds, UTC: Airflow accepts any date and makes a run with no tasks for
    // one that is not now (#255).
    let note = format!("Started from Nova by {}", who.display_name);
    match airflow.trigger(&dag_id, &now_rfc3339(), Some(&note)).await {
        Ok(run) => Ok(run_view(&run)),
        Err(e) => Err(ActionError::error(scheduler_fail(e))),
    }
}

/// `POST …/pause`: idempotent (pausing a paused pipeline answers 200) and answers the fresh view.
async fn pause_pipeline(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<PipelineView, Fail> {
    set_paused_action("pause", true, resources, airflow, who, namespace, name).await
}

/// `POST …/resume`: idempotent (resuming a running pipeline answers 200) and answers the fresh
/// view.
async fn resume_pipeline(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<PipelineView, Fail> {
    set_paused_action("resume", false, resources, airflow, who, namespace, name).await
}

#[allow(clippy::too_many_arguments)]
async fn set_paused_action(
    action: &'static str,
    paused: bool,
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<PipelineView, Fail> {
    let result = set_paused_inner(paused, resources, airflow, who, namespace, name).await;
    match &result {
        Ok(_) => audit::pipeline_event(action, who, namespace, name, None, "ok"),
        Err(e) => audit::pipeline_event(action, who, namespace, name, None, &e.outcome),
    }
    result.map_err(|e| e.fail)
}

async fn set_paused_inner(
    paused: bool,
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<PipelineView, ActionError> {
    let resources = resources
        .ok_or_else(no_cluster)
        .map_err(ActionError::error)?;
    let airflow = scheduler(airflow).map_err(ActionError::error)?;
    let (dag_id, resource) = load_actionable(resources, who, namespace, name)
        .await
        .map_err(|fail| ActionError::refused("not-found", fail))?;

    // The patch answers the DAG as Airflow now has it, which is the fresh view the caller gets;
    // `404` is a pipeline the loader has not created yet.
    let dag = match airflow.set_paused(&dag_id, paused).await {
        Ok(dag) => dag,
        Err(AirflowError::NotFound) => {
            return Err(ActionError::refused("not-loaded", pipeline_not_loaded()));
        }
        Err(e) => return Err(ActionError::error(scheduler_fail(e))),
    };
    // A failing last-run read does not fail the pause: the view then has no lastRun, exactly as
    // the list behaves.
    let last_run = airflow
        .runs(&dag_id, 1)
        .await
        .ok()
        .and_then(|runs| runs.into_iter().next());
    Ok(view_of(
        namespace,
        name,
        &resource,
        Some(&dag),
        last_run.as_ref(),
    ))
}

/// `POST …/runs/{run_id}/stop`: mark an active run failed. Not a cancel: in-flight work can
/// finish and commit; the promise is only "the run is marked failed" (#255).
async fn stop_run(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
    run_id: &str,
) -> Result<RunView, Fail> {
    let result = stop_run_inner(resources, airflow, who, namespace, name, run_id).await;
    match &result {
        Ok(run) => audit::pipeline_event("stop", who, namespace, name, Some(&run.run_id), "ok"),
        Err(e) => audit::pipeline_event("stop", who, namespace, name, Some(run_id), &e.outcome),
    }
    result.map_err(|e| e.fail)
}

async fn stop_run_inner(
    resources: Option<&PipelineResources>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
    run_id: &str,
) -> Result<RunView, ActionError> {
    let resources = resources
        .ok_or_else(no_cluster)
        .map_err(ActionError::error)?;
    let airflow = scheduler(airflow).map_err(ActionError::error)?;
    let (dag_id, _) = load_actionable(resources, who, namespace, name)
        .await
        .map_err(|fail| ActionError::refused("not-found", fail))?;

    // Read the run through this pipeline's DAG id first: a run id of another pipeline never
    // resolves, and the PATCH below only ever sees an id found here.
    let runs = match airflow.runs(&dag_id, LATEST_RUNS).await {
        Ok(runs) => runs,
        Err(AirflowError::NotFound) => {
            return Err(ActionError::refused("not-found", no_such_run()));
        }
        Err(e) => return Err(ActionError::error(scheduler_fail(e))),
    };
    let run = runs
        .iter()
        .find(|run| run.dag_run_id == run_id)
        .ok_or_else(|| ActionError::refused("not-found", no_such_run()))?;
    if !matches!(run.state.as_deref(), Some("queued") | Some("running")) {
        return Err(ActionError::refused("not-active", run_not_active()));
    }

    match airflow.mark_run_failed(&dag_id, &run.dag_run_id).await {
        Ok(run) => Ok(run_view(&run)),
        Err(AirflowError::NotFound) => Err(ActionError::refused("not-found", no_such_run())),
        Err(e) => Err(ActionError::error(scheduler_fail(e))),
    }
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

/// The read and action routes, all uncacheable.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines", get(list))
        .route("/pipelines/{namespace}/{name}", get(detail))
        .route("/pipelines/{namespace}/{name}/runs", post(trigger_run))
        .route("/pipelines/{namespace}/{name}/pause", post(pause))
        .route("/pipelines/{namespace}/{name}/resume", post(resume))
        .route("/pipelines/{namespace}/{name}/runs/{run_id}", get(run))
        .route(
            "/pipelines/{namespace}/{name}/runs/{run_id}/stop",
            post(stop),
        )
        .route(
            "/pipelines/{namespace}/{name}/runs/{run_id}/tasks/{task_id}/logs",
            get(logs),
        )
        .route_layer(axum::middleware::from_fn(no_store))
}

fn answer<T: Serialize>(result: Result<T, Fail>) -> Response {
    answer_with(StatusCode::OK, result)
}

fn answer_with<T: Serialize>(status: StatusCode, result: Result<T, Fail>) -> Response {
    match result {
        Ok(value) => (status, Json(value)).into_response(),
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

#[utoipa::path(
    post,
    path = "/api/pipelines/{namespace}/{name}/runs",
    tag = "public",
    description = "Start a run now. Nova refuses instead of passing Airflow's silent behaviour on: a pipeline the scheduler has not loaded yet answers 409, a paused pipeline 409 (Airflow would create a run that waits `queued` until someone resumes), more than ten starts per user per minute 429, and a `Forbid` or `Replace` pipeline whose latest five runs include a queued or running one 409 (`Allow` skips that check). `logicalDate` is always now, UTC, whole seconds, so the run has tasks; the note is `Started from Nova by <display name>`. Every attempt, including a refusal, writes one audit line without note text. Answers `Cache-Control: no-store`.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    responses(
        (status = 201, description = "The new run, queued", body = RunView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The pipeline is not loaded, is paused, or already has an active run", body = crate::openapi::ErrorResponse),
        (status = 429, description = "More than ten runs were started in the last minute", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn trigger_run(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
) -> Response {
    answer_with(
        StatusCode::CREATED,
        run_now(
            resources(signed.state()).as_ref(),
            signed.state().airflow.as_ref(),
            &signed.state().run_starts,
            signed.identity(),
            &namespace,
            &name,
        )
        .await,
    )
}

#[utoipa::path(
    post,
    path = "/api/pipelines/{namespace}/{name}/pause",
    tag = "public",
    description = "Pause a pipeline. Idempotent: pausing a paused pipeline answers 200. A pipeline the scheduler has not loaded answers 409. Answers the fresh `PipelineView` — the DAG as Airflow returned it after the patch — and `Cache-Control: no-store`. The attempt is audited, including a refusal.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    responses(
        (status = 200, description = "The pipeline, now paused", body = PipelineView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The scheduler has not loaded this pipeline yet", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn pause(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
) -> Response {
    answer(
        pause_pipeline(
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
    post,
    path = "/api/pipelines/{namespace}/{name}/resume",
    tag = "public",
    description = "Resume a pipeline. Idempotent: resuming a running pipeline answers 200. A pipeline the scheduler has not loaded answers 409. Answers the fresh `PipelineView` — the DAG as Airflow returned it after the patch — and `Cache-Control: no-store`. The attempt is audited, including a refusal.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    responses(
        (status = 200, description = "The pipeline, now unpaused", body = PipelineView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The scheduler has not loaded this pipeline yet", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn resume(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
) -> Response {
    answer(
        resume_pipeline(
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
    post,
    path = "/api/pipelines/{namespace}/{name}/runs/{run_id}/stop",
    tag = "public",
    description = "Stop a run: mark it failed. This is **not a cancel** — work already in flight can finish and commit its side effects (stardelt-nova#255), so the answer promises only that the run is marked failed. The run id is looked up through this pipeline's DAG id first: a run id of another pipeline answers 404 and is never patched. A run that is not `queued` or `running` answers 409 and is not patched. Answers the fresh `RunView` and `Cache-Control: no-store`. The attempt is audited, including a refusal.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
        ("run_id" = String, Path, description = "Airflow's run id"),
    ),
    responses(
        (status = 200, description = "The run, now failed", body = RunView),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller, or no such run", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The run is not active", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The scheduler or the cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection, or the scheduler is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn stop(
    signed: SignedIn,
    Path((namespace, name, run_id)): Path<(String, String, String)>,
) -> Response {
    answer(
        stop_run(
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
                "creationTimestamp": now_rfc3339(),
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

    /// The same resource with another `spec.concurrency`.
    fn with_concurrency(mut resource: Value, concurrency: &str) -> Value {
        resource["spec"]["concurrency"] = json!(concurrency);
        resource
    }

    /// The same resource, created `seconds` ago (for the `loading`/`not-loaded` boundary).
    fn with_created_ago(mut resource: Value, seconds: i64) -> Value {
        resource["metadata"]["creationTimestamp"] =
            json!(format_rfc3339(now_seconds() as i64 - seconds));
        resource
    }

    fn limiter() -> RunStartsLimiter {
        Mutex::new(std::collections::HashMap::new())
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
            routing::{get, patch, post},
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
            /// Every trigger's JSON body, in order, so a test can read the `logical_date` and
            /// the note Nova sent.
            pub trigger_bodies: Vec<Value>,
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

        async fn trigger(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path(dag_id): Path<String>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Result<(StatusCode, Json<Value>), Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "POST", &uri);
            authorize(&headers)?;
            if !state.dags.iter().any(|dag| dag["dag_id"] == dag_id) {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DAG not found" })),
                ));
            }
            let logical_date = body["logical_date"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let run = json!({
                "dag_run_id": format!("manual__{logical_date}"),
                "state": "queued",
                "run_type": "manual",
                "logical_date": logical_date,
                "start_date": null,
                "end_date": null,
                "note": body.get("note").cloned().unwrap_or(Value::Null),
                "triggered_by": "rest_api",
                "triggering_user_name": "nova-api"
            });
            state.trigger_bodies.push(body);
            state.runs.entry(dag_id).or_default().insert(0, run.clone());
            Ok((StatusCode::CREATED, Json(run)))
        }

        async fn patch_dag(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path(dag_id): Path<String>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "PATCH", &uri);
            authorize(&headers)?;
            let Some(dag) = state.dags.iter_mut().find(|dag| dag["dag_id"] == dag_id) else {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DAG not found" })),
                ));
            };
            if let Some(paused) = body.get("is_paused") {
                dag["is_paused"] = paused.clone();
            }
            Ok(Json(dag.clone()))
        }

        async fn patch_run(
            State(state): State<Shared>,
            OriginalUri(uri): OriginalUri,
            Path((dag_id, run_id)): Path<(String, String)>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> Result<Json<Value>, Refusal> {
            let mut state = state.lock().unwrap();
            remember(&mut state, "PATCH", &uri);
            authorize(&headers)?;
            let Some(runs) = state.runs.get_mut(&dag_id) else {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DAG not found" })),
                ));
            };
            let Some(run) = runs.iter_mut().find(|run| run["dag_run_id"] == run_id) else {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "detail": "DagRun not found" })),
                ));
            };
            if let Some(value) = body.get("state") {
                run["state"] = value.clone();
            }
            Ok(Json(run.clone()))
        }

        pub async fn start() -> (AirflowClient, Shared) {
            let shared: Shared = Arc::new(Mutex::new(seeded()));
            let app = Router::new()
                .route("/auth/token", post(login))
                .route("/api/v2/dags", get(dags))
                .route("/api/v2/dags/{dag_id}", get(one_dag).patch(patch_dag))
                .route("/api/v2/dags/{dag_id}/dagRuns", get(runs).post(trigger))
                .route(
                    "/api/v2/dags/{dag_id}/dagRuns/{run_id}",
                    patch(patch_run),
                )
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
            run_starts: Mutex::new(std::collections::HashMap::new()),
            airflow: Some(airflow),
            table_locks: Mutex::new(std::collections::HashMap::new()),
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
                    let state = pipeline_state(paused, loaded, last, Some(0.0));
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
            pipeline_state(true, true, Some("failed"), Some(0.0)),
            "paused",
            "paused beats a failed last run"
        );
        assert_eq!(
            pipeline_state(false, true, Some("queued"), Some(0.0)),
            "running",
            "queued counts as running"
        );
        assert_eq!(
            pipeline_state(false, true, None, Some(0.0)),
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
    async fn airflow_down_is_502_and_not_configured_is_503_for_every_action() {
        let resources = memory(&[(
            "smoke",
            with_concurrency(resource("smoke", Some("analytics")), "Allow"),
        )]);
        let namespace = crate::cluster::namespace();
        let root = admin();
        let limiter = limiter();

        // No scheduler client: every action answers 503 and names the missing client.
        let error = run_now(Some(&resources), None, &limiter, &root, &namespace, "smoke")
            .await
            .expect_err("no Airflow client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "the scheduler is not configured");
        let error = pause_pipeline(Some(&resources), None, &root, &namespace, "smoke")
            .await
            .expect_err("no Airflow client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "the scheduler is not configured");
        let error = resume_pipeline(Some(&resources), None, &root, &namespace, "smoke")
            .await
            .expect_err("no Airflow client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "the scheduler is not configured");
        let error = stop_run(
            Some(&resources),
            None,
            &root,
            &namespace,
            "smoke",
            SMOKE_RUN,
        )
        .await
        .expect_err("no Airflow client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "the scheduler is not configured");

        // No cluster client at all: the read routes still say which one is missing.
        let error = list_pipelines(None, None, &root)
            .await
            .expect_err("no cluster client");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "no cluster connection");

        // An Airflow that does not answer: 502, never 500, for every action.
        let down = AirflowClient::new(
            reqwest::Client::new(),
            AirflowConfig {
                url: "http://127.0.0.1:9".into(),
                user: "nova-api".into(),
                password: Secret::new("x"),
            },
        );
        for (action, error) in [
            (
                "run",
                run_now(
                    Some(&resources),
                    Some(&down),
                    &limiter,
                    &root,
                    &namespace,
                    "smoke",
                )
                .await
                .expect_err("Airflow is down"),
            ),
            (
                "pause",
                pause_pipeline(Some(&resources), Some(&down), &root, &namespace, "smoke")
                    .await
                    .expect_err("Airflow is down"),
            ),
            (
                "resume",
                resume_pipeline(Some(&resources), Some(&down), &root, &namespace, "smoke")
                    .await
                    .expect_err("Airflow is down"),
            ),
            (
                "stop",
                stop_run(
                    Some(&resources),
                    Some(&down),
                    &root,
                    &namespace,
                    "smoke",
                    SMOKE_RUN,
                )
                .await
                .expect_err("Airflow is down"),
            ),
        ] {
            assert_eq!(error.status(), StatusCode::BAD_GATEWAY, "{action}");
            assert_eq!(error.message(), "the scheduler did not answer", "{action}");
        }
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
            .clone()
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

        let namespace = crate::cluster::namespace();
        for (uri, expected) in [
            (
                format!("/pipelines/{namespace}/smoke/runs"),
                StatusCode::CREATED,
            ),
            (
                format!("/pipelines/{namespace}/smoke/pause"),
                StatusCode::OK,
            ),
            (
                format!("/pipelines/{namespace}/smoke/resume"),
                StatusCode::OK,
            ),
            (
                format!("/pipelines/{namespace}/smoke/runs/{SMOKE_RUN}/stop"),
                StatusCode::CONFLICT,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
                "every action answer, success or refusal, is uncacheable: {uri}"
            );
        }
    }

    #[test]
    fn the_view_has_no_raw_airflow_fields() {
        let resource = resource("smoke", Some("analytics"));
        let dag: DagSummary = serde_json::from_value(json!({
            "dag_id": "stardelt__smoke",
            "is_paused": false,
            "is_stale": false,
            "next_dagrun_run_after": "2026-10-21T03:00:00Z",
            "tags": [{ "name": "pipeline" }]
        }))
        .unwrap();
        let run: DagRun = serde_json::from_value(run(SMOKE_RUN, "success")).unwrap();
        let view = view_of("stardelt", "smoke", &resource, Some(&dag), Some(&run));

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
                "createdAt",
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

    // -----------------------------------------------------------------------
    // Actions: who may act, run now, pause, resume, stop, audit
    // -----------------------------------------------------------------------

    /// Set the fake's DAG paused state, as an operator would through Airflow.
    fn set_dag_paused(fake: &fake_airflow::Shared, dag_id: &str, paused: bool) {
        for dag in &mut fake.lock().unwrap().dags {
            if dag["dag_id"] == dag_id {
                dag["is_paused"] = json!(paused);
            }
        }
    }

    #[tokio::test]
    async fn only_the_owner_group_and_admins_may_act_and_others_get_404() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;
        let outsider = who("sub-bob", &["finance"], false);
        let namespace = crate::cluster::namespace();
        let limiter = limiter();

        for (action, error) in [
            (
                "run",
                run_now(
                    Some(&resources),
                    Some(&airflow),
                    &limiter,
                    &outsider,
                    &namespace,
                    "smoke",
                )
                .await
                .expect_err("an outsider may not run"),
            ),
            (
                "pause",
                pause_pipeline(
                    Some(&resources),
                    Some(&airflow),
                    &outsider,
                    &namespace,
                    "smoke",
                )
                .await
                .expect_err("an outsider may not pause"),
            ),
            (
                "resume",
                resume_pipeline(
                    Some(&resources),
                    Some(&airflow),
                    &outsider,
                    &namespace,
                    "smoke",
                )
                .await
                .expect_err("an outsider may not resume"),
            ),
            (
                "stop",
                stop_run(
                    Some(&resources),
                    Some(&airflow),
                    &outsider,
                    &namespace,
                    "smoke",
                    SMOKE_RUN,
                )
                .await
                .expect_err("an outsider may not stop"),
            ),
        ] {
            assert_eq!(error.status(), StatusCode::NOT_FOUND, "{action}");
            assert_eq!(error.message(), "no such pipeline", "{action}");
        }
        assert!(
            fake.lock().unwrap().calls.is_empty(),
            "a hidden pipeline never reaches Airflow: {:?}",
            fake.lock().unwrap().calls
        );

        let owned = resource("smoke", Some("analytics"));
        let spec = spec_of(&owned);
        assert!(may_act(&who("sub-ada", &["analytics"], false), spec));
        assert!(may_act(&admin(), spec));
        assert!(!may_act(&outsider, spec));
    }

    #[tokio::test]
    async fn run_now_triggers_with_now_and_a_note() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;
        let member = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let started = now_seconds();
        let run = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &member,
            &namespace,
            "smoke",
        )
        .await
        .expect("the run starts");
        assert_eq!(run.state, "queued");
        assert_eq!(run.note.as_deref(), Some("Started from Nova by sub-ada"));
        assert!(run.run_id.starts_with("manual__"));

        let fake = fake.lock().unwrap();
        assert_eq!(fake.trigger_bodies.len(), 1, "exactly one trigger");
        let body = &fake.trigger_bodies[0];
        let logical_date = body["logical_date"].as_str().expect("a logical_date");
        assert!(
            logical_date.ends_with('Z'),
            "always UTC with a Z: {logical_date}"
        );
        assert!(
            !logical_date.contains('.'),
            "whole seconds only: {logical_date}"
        );
        let at = parse_rfc3339(logical_date).expect("RFC 3339");
        assert!(
            (at - started).abs() <= 2.0,
            "logical_date is now, not a guess: {logical_date} vs {started}"
        );
        assert_eq!(body["note"], "Started from Nova by sub-ada");
        assert!(
            fake.calls
                .iter()
                .any(|call| call == "POST /api/v2/dags/stardelt__smoke/dagRuns"),
            "Airflow was asked to trigger: {:?}",
            fake.calls
        );
    }

    #[tokio::test]
    async fn run_now_refuses_a_paused_pipeline_and_does_not_trigger() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;
        set_dag_paused(&fake, "stardelt__smoke", true);

        let error = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &who("sub-ada", &["analytics"], false),
            &crate::cluster::namespace(),
            "smoke",
        )
        .await
        .expect_err("a paused pipeline does not start");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), "the pipeline is paused: resume it first");
        let fake = fake.lock().unwrap();
        assert!(
            !fake.calls.iter().any(|call| call.starts_with("POST ")),
            "Airflow was not asked to trigger: {:?}",
            fake.calls
        );
        assert!(fake.trigger_bodies.is_empty());
    }

    #[tokio::test]
    async fn run_now_refuses_when_a_run_is_active_under_forbid() {
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;
        fake.lock().unwrap().runs.insert(
            "stardelt__smoke".into(),
            vec![run("manual__2026-10-20T09:30:00Z", "running")],
        );

        let error = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &who("sub-ada", &["analytics"], false),
            &crate::cluster::namespace(),
            "smoke",
        )
        .await
        .expect_err("Forbid refuses a second run");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(
            error.message(),
            "a run is already active: wait for it or stop it"
        );
        let fake = fake.lock().unwrap();
        assert!(fake.trigger_bodies.is_empty(), "nothing was triggered");
        assert!(!fake.calls.iter().any(|call| call.starts_with("POST ")));
    }

    #[tokio::test]
    async fn run_now_allows_a_second_run_under_allow() {
        let resources = memory(&[(
            "smoke",
            with_concurrency(resource("smoke", Some("analytics")), "Allow"),
        )]);
        let (airflow, fake) = fake_airflow::start().await;
        fake.lock().unwrap().runs.insert(
            "stardelt__smoke".into(),
            vec![run("manual__2026-10-20T09:30:00Z", "running")],
        );

        let run = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &who("sub-ada", &["analytics"], false),
            &crate::cluster::namespace(),
            "smoke",
        )
        .await
        .expect("Allow skips the activity check");
        assert_eq!(run.state, "queued");
        assert_eq!(fake.lock().unwrap().trigger_bodies.len(), 1);
    }

    #[tokio::test]
    async fn run_now_refuses_an_unloaded_pipeline() {
        let resources = memory(&[("brand-new", resource("brand-new", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;

        let error = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &who("sub-ada", &["analytics"], false),
            &crate::cluster::namespace(),
            "brand-new",
        )
        .await
        .expect_err("no DAG yet");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(
            error.message(),
            "the scheduler has not loaded this pipeline yet"
        );
        assert!(
            fake.lock().unwrap().trigger_bodies.is_empty(),
            "there is nothing to trigger"
        );
    }

    #[tokio::test]
    async fn run_now_is_rate_limited_per_user() {
        let resources = memory(&[
            (
                "smoke",
                with_concurrency(resource("smoke", Some("analytics")), "Allow"),
            ),
            ("other", resource("other", Some("analytics"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;
        set_dag_paused(&fake, "stardelt__other", true);
        let namespace = crate::cluster::namespace();
        let limiter = limiter();
        let ada = who("sub-ada", &["analytics"], false);

        // Refusals before the limiter do not count: ten paused refusals still leave ten slots.
        for _ in 0..10 {
            let error = run_now(
                Some(&resources),
                Some(&airflow),
                &limiter,
                &ada,
                &namespace,
                "other",
            )
            .await
            .expect_err("paused");
            assert_eq!(error.message(), "the pipeline is paused: resume it first");
        }
        for attempt in 0..10 {
            run_now(
                Some(&resources),
                Some(&airflow),
                &limiter,
                &ada,
                &namespace,
                "smoke",
            )
            .await
            .unwrap_or_else(|error| panic!("start {attempt}: {}", error.message()));
        }
        let error = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter,
            &ada,
            &namespace,
            "smoke",
        )
        .await
        .expect_err("the eleventh start in a minute");
        assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error.message(), "too many runs started; wait a minute");

        // Another user has their own ten.
        let run = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter,
            &who("sub-bob", &["analytics"], false),
            &namespace,
            "smoke",
        )
        .await
        .expect("another user is unaffected");
        assert_eq!(run.state, "queued");
    }

    #[tokio::test]
    async fn pause_and_resume_are_idempotent_and_return_the_fresh_view() {
        let resources = memory(&[
            ("smoke", resource("smoke", Some("analytics"))),
            ("brand-new", resource("brand-new", Some("analytics"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;
        let member = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let paused = pause_pipeline(
            Some(&resources),
            Some(&airflow),
            &member,
            &namespace,
            "smoke",
        )
        .await
        .expect("pause");
        assert!(paused.paused);
        assert_eq!(paused.state, "paused");
        let again = pause_pipeline(
            Some(&resources),
            Some(&airflow),
            &member,
            &namespace,
            "smoke",
        )
        .await
        .expect("pausing a paused pipeline is fine");
        assert!(again.paused, "the fresh DAG says paused");

        let resumed = resume_pipeline(
            Some(&resources),
            Some(&airflow),
            &member,
            &namespace,
            "smoke",
        )
        .await
        .expect("resume");
        assert!(!resumed.paused);
        assert_eq!(
            resumed.state, "succeeded",
            "the fresh view carries the last run"
        );
        let again = resume_pipeline(
            Some(&resources),
            Some(&airflow),
            &member,
            &namespace,
            "smoke",
        )
        .await
        .expect("resuming a running pipeline is fine");
        assert!(!again.paused);

        let patches = fake
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call.starts_with("PATCH /api/v2/dags/stardelt__smoke"))
            .count();
        assert_eq!(patches, 4, "two pauses and two resumes reached Airflow");

        let error = pause_pipeline(
            Some(&resources),
            Some(&airflow),
            &member,
            &namespace,
            "brand-new",
        )
        .await
        .expect_err("no DAG yet");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(
            error.message(),
            "the scheduler has not loaded this pipeline yet"
        );
    }

    #[tokio::test]
    async fn stop_marks_only_active_runs_failed() {
        const ACTIVE_RUN: &str = "manual__2026-10-20T09:30:00Z";
        const FAILED_RUN: &str = "manual__2026-10-20T09:00:00Z";
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
        let (airflow, fake) = fake_airflow::start().await;
        fake.lock().unwrap().runs.insert(
            "stardelt__smoke".into(),
            vec![
                run(ACTIVE_RUN, "running"),
                run(FAILED_RUN, "failed"),
                run(SMOKE_RUN, "success"),
            ],
        );
        let namespace = crate::cluster::namespace();
        let member = who("sub-ada", &["analytics"], false);

        for finished in [SMOKE_RUN, FAILED_RUN] {
            let error = stop_run(
                Some(&resources),
                Some(&airflow),
                &member,
                &namespace,
                "smoke",
                finished,
            )
            .await
            .expect_err("a finished run cannot stop");
            assert_eq!(error.status(), StatusCode::CONFLICT, "{finished}");
            assert_eq!(error.message(), "the run is not active");
        }
        assert!(
            !fake
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|call| call.starts_with("PATCH ")),
            "a finished run is not patched"
        );

        let stopped = stop_run(
            Some(&resources),
            Some(&airflow),
            &member,
            &namespace,
            "smoke",
            ACTIVE_RUN,
        )
        .await
        .expect("an active run stops");
        assert_eq!(stopped.state, "failed");
        assert_eq!(stopped.run_id, ACTIVE_RUN);
        let fake = fake.lock().unwrap();
        let patches = fake
            .calls
            .iter()
            .filter(|call| call.starts_with("PATCH /api/v2/dags/stardelt__smoke/dagRuns/"))
            .count();
        assert_eq!(patches, 1, "exactly one PATCH: {:?}", fake.calls);
        assert_eq!(
            fake.runs["stardelt__smoke"]
                .iter()
                .find(|run| run["dag_run_id"] == ACTIVE_RUN)
                .expect("the run")["state"],
            "failed"
        );
    }

    #[tokio::test]
    async fn stop_ignores_a_run_id_of_another_pipeline() {
        let resources = memory(&[
            ("smoke", resource("smoke", Some("analytics"))),
            ("other", resource("other", Some("analytics"))),
        ]);
        let (airflow, fake) = fake_airflow::start().await;

        let error = stop_run(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &crate::cluster::namespace(),
            "smoke",
            OTHER_RUN,
        )
        .await
        .expect_err("another pipeline's run id does not resolve under smoke");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "no such run");
        let fake = fake.lock().unwrap();
        assert!(
            !fake
                .calls
                .iter()
                .any(|call| call.starts_with("PATCH ") || call.contains("stardelt__other")),
            "no PATCH, and the other DAG was never addressed: {:?}",
            fake.calls
        );
    }

    #[tokio::test]
    async fn every_action_is_audited_including_refusals() {
        let resources = memory(&[(
            "smoke",
            with_concurrency(resource("smoke", Some("analytics")), "Allow"),
        )]);
        let (airflow, fake) = fake_airflow::start().await;
        let namespace = crate::cluster::namespace();
        // A `sub` of this test's own: the sink is process-wide, and other tests' records must
        // not be mistaken for these.
        let actor = who("sub-audit-196", &["stardelt-admins"], true);
        let _ = audit::test_sink::take();

        let started = run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &actor,
            &namespace,
            "smoke",
        )
        .await
        .expect("run");
        set_dag_paused(&fake, "stardelt__smoke", true);
        run_now(
            Some(&resources),
            Some(&airflow),
            &limiter(),
            &actor,
            &namespace,
            "smoke",
        )
        .await
        .expect_err("paused");
        pause_pipeline(
            Some(&resources),
            Some(&airflow),
            &actor,
            &namespace,
            "smoke",
        )
        .await
        .expect("pause");
        resume_pipeline(
            Some(&resources),
            Some(&airflow),
            &actor,
            &namespace,
            "smoke",
        )
        .await
        .expect("resume");
        stop_run(
            Some(&resources),
            Some(&airflow),
            &actor,
            &namespace,
            "smoke",
            SMOKE_RUN,
        )
        .await
        .expect_err("the seeded run already succeeded");
        stop_run(
            Some(&resources),
            Some(&airflow),
            &actor,
            &namespace,
            "smoke",
            &started.run_id,
        )
        .await
        .expect("stop the queued run");
        let down = AirflowClient::new(
            reqwest::Client::new(),
            AirflowConfig {
                url: "http://127.0.0.1:9".into(),
                user: "nova-api".into(),
                password: Secret::new("x"),
            },
        );
        run_now(
            Some(&resources),
            Some(&down),
            &limiter(),
            &actor,
            &namespace,
            "smoke",
        )
        .await
        .expect_err("Airflow is down");

        let records: Vec<audit::test_sink::Record> = audit::test_sink::take()
            .into_iter()
            .filter(|record| record.sub == "sub-audit-196")
            .collect();
        let outcomes: Vec<(&str, &str)> = records
            .iter()
            .map(|record| (record.action.as_str(), record.outcome.as_str()))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("run", "ok"),
                ("run", "refused:paused"),
                ("pause", "ok"),
                ("resume", "ok"),
                ("stop", "refused:not-active"),
                ("stop", "ok"),
                ("run", "error"),
            ],
            "one record per action, refusals and errors included"
        );
        assert!(
            records
                .iter()
                .all(|record| record.pipeline == "smoke" && record.namespace == namespace),
            "the record names the pipeline: {records:?}"
        );
        assert_eq!(
            records[0].run.as_deref(),
            Some(started.run_id.as_str()),
            "a started run is named"
        );
        assert_eq!(
            records[1].run, None,
            "a refusal before the trigger has no run"
        );
        assert_eq!(records[4].run.as_deref(), Some(SMOKE_RUN));
        // No field of the record can hold the note or log text: there is none to hold it.
        let serialized = format!("{records:?}");
        assert!(
            !serialized.contains("Started from Nova"),
            "the fixed note text is not repeated in the audit: {serialized}"
        );
    }

    #[tokio::test]
    async fn not_loaded_after_three_minutes() {
        assert_eq!(
            pipeline_state(false, false, None, Some(120.0)),
            "loading",
            "two minutes is still loading"
        );
        assert_eq!(
            pipeline_state(false, false, None, Some(240.0)),
            "not-loaded",
            "four minutes without a DAG is not loading"
        );
        assert_eq!(
            pipeline_state(false, false, None, None),
            "loading",
            "without a creationTimestamp there is nothing to compare"
        );
        assert_eq!(
            pipeline_state(false, true, None, Some(600.0)),
            "idle",
            "a loaded pipeline is never not-loaded"
        );

        let resources = memory(&[
            (
                "two-minutes",
                with_created_ago(resource("two-minutes", Some("analytics")), 120),
            ),
            (
                "four-minutes",
                with_created_ago(resource("four-minutes", Some("analytics")), 240),
            ),
        ]);
        let (airflow, _fake) = fake_airflow::start().await;
        let namespace = crate::cluster::namespace();

        let young = pipeline_detail(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &namespace,
            "two-minutes",
        )
        .await
        .expect("the detail is readable");
        assert_eq!(young.pipeline.state, "loading");
        assert!(young.pipeline.created_at.is_some(), "createdAt is exposed");

        let old = pipeline_detail(
            Some(&resources),
            Some(&airflow),
            &admin(),
            &namespace,
            "four-minutes",
        )
        .await
        .expect("the detail is readable");
        assert!(!old.pipeline.loaded);
        assert_eq!(old.pipeline.state, "not-loaded");
        assert!(old.pipeline.created_at.is_some());
    }
}
