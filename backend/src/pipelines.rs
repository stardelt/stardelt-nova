//! The Pipelines API: read (list, detail, runs, tasks, logs — stardelt-nova#188), act (run now,
//! pause, resume, stop a run — stardelt-nova#196), schedule (create, change, delete a
//! pipeline — stardelt-nova#210) and the schedule options (parameters, retries, time limit,
//! concurrency, a statement check, a next-runs preview — stardelt-nova#223); design notes
//! `design/scheduled-jobs.md` §6.1 and `design/scheduling-and-notebooks.md` §2, phase J1.
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
//! - **Writes** (#210): *Schedule* creates a `Pipeline` for one of the caller's **own** groups
//!   (administrators included: they schedule for the groups they belong to) with
//!   `runAs: pipeline-<group>` — the design note's naming convention. Nova validates everything
//!   the loader (`stardelt-demos` `dags/stardelt_pipelines/plan.py`) would refuse, so a pipeline
//!   Nova writes always becomes a DAG; the principal of the group must exist and be ready.
//!   Only pipelines labelled `app.kubernetes.io/managed-by: stardelt-nova` may be changed or
//!   deleted — a `kubectl` or GitOps pipeline answers 409 instead of being rewritten. The
//!   statement is **never** audited, only its length and the cron text. A create needs no
//!   Airflow (the resource is the source of truth); the read routes keep answering 503 without it.
//!
//! - **Options and parameters** (#223): a schedule carries `${params.<name>}` values (at most 16,
//!   lower-case names, values up to 256 characters) and the advanced options the loader reads
//!   (`retries`, `timeout`, `concurrency`). An omitted option keeps today's default, so an old body
//!   writes what it always wrote. Every `${…}` in the statement must be one the loader knows
//!   (`run.date`, `run.id`, `params.<name>`) — an unknown one would fail the run, so it is a 400 at
//!   create time. `POST /api/pipelines/check` asks the engine about a statement with `EXPLAIN`
//!   (never `EXPLAIN ANALYZE`, never the statement itself), and `POST /api/pipelines/preview-schedule`
//!   lists the next fire times in the schedule's zone.
//!
//! - **Notebooks** (#237): the same two routes schedule a published notebook version instead of a
//!   statement. A body gives *either* a non-empty `sql` or a `notebook`
//!   (`{ name, objectVersion, params }`) — both or neither is a 400. The resource's one task is
//!   then `{"name": "run", "kind": "notebook", "config": {notebook, objectVersion, params}}`, where
//!   `params` is the task's JSON object string with sorted keys; everything else (runAs, retries,
//!   timeout, concurrency, schedule) is the SQL shape's. Nova refuses what the CRD or the loader
//!   would refuse: the `.ipynb` name rule, at most 32 parameters named `[A-Za-z_][A-Za-z0-9_]{0,63}`,
//!   values up to 256 characters without NUL, a JSON text of at most 4096 characters — and a version
//!   that is **not in the store**: `notebooks::head_version` asks S3 (503 without `NOVA_S3_*`, 502
//!   when it does not answer), so a schedule always pins bytes that exist. A version is never a
//!   branch or a tag: the loader reads `<ownerGroup>/<notebook>` at exactly that version, so a group
//!   the caller does not belong to can never be named. An update changes the notebook it names, its
//!   version and its parameters, never what the pipeline runs: `sql` on a notebook pipeline is 400
//!   `this pipeline runs a notebook`, a notebook on a statement pipeline 400 `this pipeline runs a
//!   statement`.
//!
//! The route logic sits in plain `async fn`s that take their inputs, so the tests call them
//! without a router; each handler is the thin axum wrapper that checks sign-in and the two
//! clients. The whole surface is uncacheable (`Cache-Control: no-store`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

#[cfg(test)]
use std::sync::Mutex;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, Request},
    http::{HeaderValue, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Datelike, Days, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use futures::stream::{self, StreamExt};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::airflow::{
    AirflowClient, AirflowError, DagRun, DagSummary, LogEvent, LogPage, TaskInstance, dag_id_for,
};
use crate::api_json::ApiJson;
use crate::audit::{self, PipelineWrite};
use crate::auth::{Identity, Principal};
use crate::connection_routes::{Fail, SignedIn};
use crate::notebooks::{self, NotebookStore};

/// The `Pipeline` resources of the platform namespace: the cluster, or memory in tests.
pub enum PipelineResources {
    Kube(Box<ClusterResources>),
    #[cfg(test)]
    Memory(Arc<Mutex<MemoryStore>>),
}

/// The two resource types of Nova's namespace. Boxed, so the enum stays the size of a pointer
/// beside the test double.
pub struct ClusterResources {
    pipelines: Api<DynamicObject>,
    principals: Api<DynamicObject>,
}

/// What the test double holds: the `Pipeline` resources and the `ServicePrincipal` resources of
/// the namespace, both by name.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore {
    pub pipelines: BTreeMap<String, Value>,
    pub principals: BTreeMap<String, Value>,
}

#[derive(Debug)]
pub enum ResourceError {
    /// The name is taken: the answer is 409, never an overwrite.
    Exists,
    /// The resource changed (or vanished) since it was read: 409, never a silent overwrite.
    Conflict,
    /// The cluster did not answer or refused.
    Unavailable(String),
}

impl PipelineResources {
    /// The pipeline resources of Nova's namespace, through the cluster client.
    pub fn cluster(client: kube::Client) -> Self {
        let pipelines = Api::namespaced_with(
            client.clone(),
            &crate::cluster::namespace(),
            &ApiResource::from_gvk(&GroupVersionKind::gvk(
                "stardelt.io",
                "v1alpha1",
                "Pipeline",
            )),
        );
        // The principal of a group, `pipeline-<group>` by convention, is another resource of the
        // same namespace: Nova reads it, never writes it (an administrator creates it).
        let principals = Api::namespaced_with(
            client,
            &crate::cluster::namespace(),
            &ApiResource::from_gvk(&GroupVersionKind::gvk(
                "stardelt.io",
                "v1alpha1",
                "ServicePrincipal",
            )),
        );
        Self::Kube(Box::new(ClusterResources {
            pipelines,
            principals,
        }))
    }

    /// Every `Pipeline` of the namespace, unfiltered; the caller applies [`visible`].
    pub async fn list(&self) -> Result<Vec<Value>, ResourceError> {
        match self {
            Self::Kube(resources) => {
                let objects = resources
                    .pipelines
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
                .pipelines
                .values()
                .cloned()
                .collect()),
        }
    }

    /// One `Pipeline` by name; `None` when it does not exist.
    pub async fn get(&self, name: &str) -> Result<Option<Value>, ResourceError> {
        match self {
            Self::Kube(resources) => resources
                .pipelines
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
                .pipelines
                .get(name)
                .cloned()),
        }
    }

    /// A real create, not an apply: a name that already exists is [`ResourceError::Exists`].
    /// Answers the resource as the cluster stored it.
    pub async fn create(&self, resource: Value) -> Result<Value, ResourceError> {
        match self {
            Self::Kube(resources) => {
                let object: DynamicObject = serde_json::from_value(resource)
                    .map_err(|e| ResourceError::Unavailable(format!("not a resource: {e}")))?;
                let created = resources
                    .pipelines
                    .create(&kube::api::PostParams::default(), &object)
                    .await
                    .map_err(|e| match e {
                        kube::Error::Api(er) if er.code == 409 => ResourceError::Exists,
                        other => ResourceError::Unavailable(other.to_string()),
                    })?;
                serde_json::to_value(created).map_err(|e| ResourceError::Unavailable(e.to_string()))
            }
            #[cfg(test)]
            Self::Memory(store) => {
                let mut store = store.lock().expect("memory pipeline resources poisoned");
                let name = pipeline_name(&resource).to_string();
                if store.pipelines.contains_key(&name) {
                    return Err(ResourceError::Exists);
                }
                store.pipelines.insert(name, resource.clone());
                Ok(resource)
            }
        }
    }

    /// Replace one, carrying the `resourceVersion` it was read with: a concurrent change is
    /// [`ResourceError::Conflict`], never a silent overwrite. Answers the stored resource.
    pub async fn replace(&self, name: &str, resource: Value) -> Result<Value, ResourceError> {
        match self {
            Self::Kube(resources) => {
                let object: DynamicObject = serde_json::from_value(resource)
                    .map_err(|e| ResourceError::Unavailable(format!("not a resource: {e}")))?;
                let replaced = resources
                    .pipelines
                    .replace(name, &kube::api::PostParams::default(), &object)
                    .await
                    .map_err(|e| match e {
                        // A conflict is the API server's `409`; a `404` means somebody deleted it
                        // between the read and the write — both ask the caller to reload.
                        kube::Error::Api(er) if er.code == 409 || er.code == 404 => {
                            ResourceError::Conflict
                        }
                        other => ResourceError::Unavailable(other.to_string()),
                    })?;
                serde_json::to_value(replaced)
                    .map_err(|e| ResourceError::Unavailable(e.to_string()))
            }
            #[cfg(test)]
            Self::Memory(store) => {
                let mut store = store.lock().expect("memory pipeline resources poisoned");
                let Some(current) = store.pipelines.get(name) else {
                    return Err(ResourceError::Conflict);
                };
                let read_with = current
                    .get("metadata")
                    .and_then(|metadata| metadata.get("resourceVersion"));
                let written_with = resource
                    .get("metadata")
                    .and_then(|metadata| metadata.get("resourceVersion"));
                if read_with != written_with {
                    return Err(ResourceError::Conflict);
                }
                store.pipelines.insert(name.to_string(), resource.clone());
                Ok(resource)
            }
        }
    }

    /// Delete one; a resource that is already gone is success (a `404` is not an error).
    pub async fn delete(&self, name: &str) -> Result<(), ResourceError> {
        match self {
            Self::Kube(resources) => {
                match resources
                    .pipelines
                    .delete(name, &kube::api::DeleteParams::default())
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(kube::Error::Api(er)) if er.code == 404 => Ok(()),
                    Err(e) => Err(ResourceError::Unavailable(e.to_string())),
                }
            }
            #[cfg(test)]
            Self::Memory(store) => {
                store
                    .lock()
                    .expect("memory pipeline resources poisoned")
                    .pipelines
                    .remove(name);
                Ok(())
            }
        }
    }

    /// Every `ServicePrincipal` of the namespace, unfiltered; the caller picks a group's one.
    pub async fn principals(&self) -> Result<Vec<Value>, ResourceError> {
        match self {
            Self::Kube(resources) => {
                let objects = resources
                    .principals
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
            Self::Memory(store) => Ok(store
                .lock()
                .expect("memory pipeline resources poisoned")
                .principals
                .values()
                .cloned()
                .collect()),
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

/// The task names and kinds the resource declares, in declaration order. `detailed` adds what
/// only the edit dialog needs — a `sql` task's statement, a notebook task's file name, version
/// and parameters — so the list stays small.
fn task_specs(spec: &Value, detailed: bool) -> Vec<TaskSpecView> {
    spec.get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(|task| {
                    let kind = task.get("kind").and_then(Value::as_str).unwrap_or_default();
                    let config = task.get("config");
                    let notebook = detailed && kind == "notebook";
                    Some(TaskSpecView {
                        name: task.get("name")?.as_str()?.to_string(),
                        kind: kind.to_string(),
                        sql: if detailed && kind == "sql" {
                            task.get("sql").and_then(Value::as_str).map(str::to_string)
                        } else {
                            None
                        },
                        notebook: if notebook {
                            config
                                .and_then(|config| config.get("notebook"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        } else {
                            None
                        },
                        object_version: if notebook {
                            config
                                .and_then(|config| config.get("objectVersion"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        } else {
                            None
                        },
                        notebook_params: if notebook {
                            config
                                .and_then(|config| config.get("params"))
                                .and_then(Value::as_str)
                                .and_then(|params| serde_json::from_str(params).ok())
                        } else {
                            None
                        },
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

/// A duration the CRD accepts (`30s`, `5m`, `2h`, `1d`, the grammar of the loader's `plan.py`)
/// as whole minutes. `None` for anything else, so a hand-written resource never breaks the view.
fn minutes_of(text: &str) -> Option<i64> {
    let text = text.trim();
    let (number, seconds) = ["s", "m", "h", "d"].iter().find_map(|unit| {
        let number = text.strip_suffix(unit)?;
        let seconds = match *unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            _ => 86_400,
        };
        Some((number, seconds))
    })?;
    Some(number.parse::<i64>().ok()? * seconds / 60)
}

/// The options as the resource carries them, defaults when a field is absent (or written in a
/// form Nova does not read): the edit dialog prefills from this.
fn options_of(spec: &Value) -> PipelineOptionsView {
    let retries = spec.get("retries");
    let backoff = retries
        .and_then(|retries| retries.get("backoff"))
        .and_then(Value::as_str)
        .and_then(minutes_of)
        .unwrap_or(DEFAULT_BACKOFF_MINUTES);
    PipelineOptionsView {
        retries: retries
            .and_then(|retries| retries.get("attempts"))
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_RETRIES),
        backoff_minutes: backoff,
        timeout_minutes: spec
            .get("timeout")
            .and_then(Value::as_str)
            .and_then(minutes_of)
            .unwrap_or(DEFAULT_TIMEOUT_MINUTES),
        concurrency: spec
            .get("concurrency")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_CONCURRENCY)
            .to_string(),
    }
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

/// The clock as a UTC date-time, for the statement check's "today" and the preview's "next".
/// Read through `std` on purpose: Nova never asks the machine for its local zone.
fn now_utc() -> DateTime<Utc> {
    DateTime::<Utc>::from(std::time::SystemTime::now())
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
// Writes: what *Schedule* accepts, and the resource it writes
// ---------------------------------------------------------------------------

/// The label that says Nova owns a pipeline. Anything else (kubectl, GitOps) is not ours to
/// rewrite: the answer is 409, never a silent edit of somebody's manifest.
pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
pub const MANAGED_BY_NOVA: &str = "stardelt-nova";

/// The longest statement a scheduled pipeline may carry.
const MAX_STATEMENT_CHARS: usize = 20_000;

/// At most this many pipelines created per user per hour (`AppState::pipeline_creates`).
const CREATES_PER_HOUR: usize = 10;
const CREATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60 * 60);

fn invalid_name() -> &'static str {
    "name must be 1 to 40 lower-case letters, digits and dashes, starting with a letter"
}

fn invalid_sql() -> &'static str {
    "give the statement to run (up to 20,000 characters)"
}

/// The name rules of the CRD and of `ServicePrincipalSpec::validate`: 1 to 40 lower-case
/// letters, digits and dashes, starting with a letter and not ending with a dash.
fn validate_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    let ok = !bytes.is_empty()
        && bytes.len() <= 40
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    if ok {
        Ok(())
    } else {
        Err(invalid_name().into())
    }
}

/// `sql`, trimmed: not empty, at most 20,000 characters, no NUL; one trailing `;` is dropped so
/// the loader does not have to guess. Answers the statement as it is stored.
fn validate_sql(sql: &str) -> Result<String, String> {
    let trimmed = sql.trim();
    let statement = trimmed
        .strip_suffix(';')
        .map(str::trim_end)
        .unwrap_or(trimmed);
    if statement.is_empty()
        || statement.chars().count() > MAX_STATEMENT_CHARS
        || statement.contains('\0')
    {
        return Err(invalid_sql().into());
    }
    Ok(statement.to_string())
}

// ---------------------------------------------------------------------------
// Parameters and advanced options (`Schedule`, #223)
// ---------------------------------------------------------------------------

/// At most this many `${params.<name>}` values per schedule (#223).
const MAX_PARAMS: usize = 16;
/// The longest parameter value the resource may carry.
const MAX_PARAM_VALUE_CHARS: usize = 256;

const PARAM_NAMES: &str = "parameter names are lower-case letters, digits and underscores";
const TOO_MANY_PARAMS: &str = "at most 16 parameters";
const PARAM_VALUE_TOO_LONG: &str = "a parameter value is at most 256 characters";
const PARAM_VALUE_NUL: &str = "a parameter value cannot contain NUL";
const RETRIES_RANGE: &str = "retries are 0 to 5";
const BACKOFF_RANGE: &str = "the wait between retries is 1 to 60 minutes";
const TIMEOUT_RANGE: &str = "the time limit is 1 to 720 minutes";
const CONCURRENCY_RULE: &str = "runs are Forbid or Allow";
const COUNT_RANGE: &str = "count is 1 to 10";

/// Today's defaults for a schedule that sends no `options`: one retry after five minutes, a
/// two-hour time limit, one run at a time (what `pipeline_resource` wrote before #223).
const DEFAULT_RETRIES: i64 = 1;
const DEFAULT_BACKOFF_MINUTES: i64 = 5;
const DEFAULT_TIMEOUT_MINUTES: i64 = 120;
const DEFAULT_CONCURRENCY: &str = "Forbid";

/// `^[a-z][a-z0-9_]{0,31}$`: the names the loader expands as `${params.<name>}`.
fn valid_param_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// The parameter table a schedule may carry: at most 16 values, lower-case names, short values
/// without NUL. `Err` is the sentence the caller sees.
fn validate_params(params: &BTreeMap<String, String>) -> Result<(), String> {
    if params.len() > MAX_PARAMS {
        return Err(TOO_MANY_PARAMS.into());
    }
    for (name, value) in params {
        if !valid_param_name(name) {
            return Err(PARAM_NAMES.into());
        }
        if value.contains('\0') {
            return Err(PARAM_VALUE_NUL.into());
        }
        if value.chars().count() > MAX_PARAM_VALUE_CHARS {
            return Err(PARAM_VALUE_TOO_LONG.into());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The notebook task (`Schedule`, #237)
// ---------------------------------------------------------------------------

/// The most parameters one notebook task may pass to papermill — the CRD's
/// `maxProperties` for `config.params`.
const MAX_NOTEBOOK_PARAMS: usize = 32;
/// The longest one notebook parameter value may be, in characters.
const MAX_NOTEBOOK_PARAM_VALUE_CHARS: usize = 256;
/// The longest the `params` JSON object string may be — the CRD's CEL limit.
const MAX_NOTEBOOK_PARAMS_JSON_CHARS: usize = 4096;

const ONE_OF_STATEMENT_OR_NOTEBOOK: &str = "give a statement or a notebook";
const NOTEBOOK_NAME_RULE: &str = "the notebook name is a .ipynb file name (letters, digits, \
                                  dots, dashes, underscores and spaces)";
const NOTEBOOK_PARAM_NAMES: &str = "parameter names are letters, digits and underscores";
const NOTEBOOK_TOO_MANY_PARAMS: &str = "at most 32 parameters";
const NOTEBOOK_PARAM_VALUE: &str = "a parameter value is at most 256 characters";
const NOTEBOOK_PARAMS_TOO_LONG: &str = "the parameters are too long";
/// An update cannot change what a pipeline runs (#237): the kind is the resource's.
const NOTEBOOK_PIPELINE: &str = "this pipeline runs a notebook";
const STATEMENT_PIPELINE: &str = "this pipeline runs a statement";

/// `^[A-Za-z_][A-Za-z0-9_]{0,63}$`: the parameter names papermill binds. Notebook names are
/// valid under the publish route's rule, so a schedule can only name a notebook a publish
/// accepts (`notebooks::valid_name`, the CRD's own regular expression).
fn valid_notebook_param_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// The notebook a schedule pins, checked the way the CRD and the loader check it. `Err` is the
/// sentence the caller sees; `Ok` is the `config.params` JSON text, keys sorted by
/// [`BTreeMap`], which is what the task carries.
fn validate_notebook(notebook: &NotebookRequest) -> Result<String, String> {
    if !notebooks::valid_name(&notebook.name) {
        return Err(NOTEBOOK_NAME_RULE.into());
    }
    if notebook.params.len() > MAX_NOTEBOOK_PARAMS {
        return Err(NOTEBOOK_TOO_MANY_PARAMS.into());
    }
    for (name, value) in &notebook.params {
        if !valid_notebook_param_name(name) {
            return Err(NOTEBOOK_PARAM_NAMES.into());
        }
        // The one sentence covers both: a value that would not fit papermill's call.
        if value.chars().count() > MAX_NOTEBOOK_PARAM_VALUE_CHARS || value.contains('\0') {
            return Err(NOTEBOOK_PARAM_VALUE.into());
        }
    }
    let params = serde_json::to_string(&notebook.params)
        .map_err(|_| NOTEBOOK_PARAMS_TOO_LONG.to_string())?;
    if params.chars().count() > MAX_NOTEBOOK_PARAMS_JSON_CHARS {
        return Err(NOTEBOOK_PARAMS_TOO_LONG.into());
    }
    Ok(params)
}

/// The `run` task of a Nova-written pipeline as a write stores it (#210, #237): exactly one of a
/// statement and a pinned notebook version.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Statement {
    /// The statement as stored: trimmed, one trailing `;` dropped.
    Sql(String),
    /// The notebook fields as stored: the file name, the immutable object version and the
    /// parameter JSON text with sorted keys.
    Notebook {
        name: String,
        object_version: String,
        params: String,
    },
}

/// The body's statement with the rules of its kind applied: the SQL ones (statement, template
/// parameters, placeholders), or the notebook ones (name, parameters, and a pinned version that
/// exists in the group). `Err` is the sentence and the audit's reason. A body gives exactly one
/// of a non-empty `sql` and a `notebook`; both or neither is the same sentence (#237).
async fn checked_statement(
    sql: &str,
    notebook: Option<&NotebookRequest>,
    template_params: &BTreeMap<String, String>,
    notebooks: Option<&NotebookStore>,
    group: &str,
) -> Result<Statement, ActionError> {
    let refused = |reason: &'static str, message: String| {
        ActionError::refused(reason, Fail::new(StatusCode::BAD_REQUEST, message))
    };
    match (sql.trim().is_empty(), notebook) {
        (false, None) => {
            let sql = validate_sql(sql).map_err(|message| refused("sql", message))?;
            validate_params(template_params).map_err(|message| refused("params", message))?;
            check_placeholders(&sql, template_params)
                .map_err(|message| refused("params", message))?;
            Ok(Statement::Sql(sql))
        }
        (true, Some(notebook)) => {
            let params =
                validate_notebook(notebook).map_err(|message| refused("notebook", message))?;
            if !notebooks::object_version_ok(&notebook.object_version) {
                // Nothing with a space in it, or longer than 128 characters, ever came out of
                // a publish: it cannot be in the store.
                return Err(refused("version", no_such_version_message(group)));
            }
            match notebooks::head_version(
                notebooks,
                group,
                &notebook.name,
                &notebook.object_version,
            )
            .await
            {
                Ok(true) => Ok(Statement::Notebook {
                    name: notebook.name.clone(),
                    object_version: notebook.object_version.clone(),
                    params,
                }),
                Ok(false) => Err(refused("version", no_such_version_message(group))),
                Err(fail) => Err(ActionError::refused("store", fail)),
            }
        }
        _ => Err(refused("body", ONE_OF_STATEMENT_OR_NOTEBOOK.into())),
    }
}

/// The sentence a version that is not in the store answers (#237).
fn no_such_version_message(group: &str) -> String {
    format!("that notebook version does not exist in group {group}")
}

/// One `${…}` the loader's `expand.py` would replace: the offset of its `${`, the offset just
/// past its `}`, and the name inside. `from` is where the search starts.
///
/// The grammar is deliberately the loader's (`\$\{[A-Za-z_][A-Za-z0-9_.]*\}`): a `${` its regex
/// does not match is no placeholder, and the loader leaves it in the statement — Nova must not
/// refuse a statement the loader happily runs.
fn next_placeholder(statement: &str, from: usize) -> Option<(usize, usize, &str)> {
    let mut at = from;
    while let Some(found) = statement.get(at..)?.find("${") {
        let start = at + found;
        let body = start + 2;
        let rest = statement.get(body..)?;
        let length = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.')
            .count();
        let name = rest.get(..length)?;
        let first_ok = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        let closes = rest.as_bytes().get(length) == Some(&b'}');
        if first_ok && closes {
            return Some((start, body + length + 1, name));
        }
        at = body;
    }
    None
}

/// Every `${…}` in the statement must be one the loader can fill: `run.date`, `run.id`, or
/// `params.<name>` for a parameter this schedule defines. An unknown one fails the run
/// (`expand.py` raises), so Nova refuses the write instead.
fn check_placeholders(statement: &str, params: &BTreeMap<String, String>) -> Result<(), String> {
    let mut at = 0;
    while let Some((_, end, name)) = next_placeholder(statement, at) {
        let known = name == "run.date"
            || name == "run.id"
            || name
                .strip_prefix("params.")
                .is_some_and(|key| params.contains_key(key));
        if !known {
            return Err(format!("unknown placeholder ${{{name}}}"));
        }
        at = end;
    }
    Ok(())
}

/// The statement with every placeholder replaced, in one pass so a value that itself contains
/// `${…}` is never expanded again. Unknown placeholders are refused before this runs.
fn fill_placeholders(
    statement: &str,
    run_date: &str,
    run_id: &str,
    params: &BTreeMap<String, String>,
) -> String {
    let mut out = String::with_capacity(statement.len());
    let mut at = 0;
    while let Some((start, end, name)) = next_placeholder(statement, at) {
        out.push_str(statement.get(at..start).unwrap_or_default());
        let value = match name {
            "run.date" => Some(run_date),
            "run.id" => Some(run_id),
            other => other
                .strip_prefix("params.")
                .and_then(|key| params.get(key))
                .map(String::as_str),
        };
        match value {
            Some(value) => out.push_str(value),
            // Unreachable after `check_placeholders`; leaving the text alone is the safe answer.
            None => out.push_str(statement.get(start..end).unwrap_or_default()),
        }
        at = end;
    }
    out.push_str(statement.get(at..).unwrap_or_default());
    out
}

const CRON_FIELDS: &str = "a schedule has five fields: minute hour day month weekday";
const CRON_RANGE: &str = "a field is out of range";
const CRON_TOO_OFTEN: &str = "at most one run every 5 minutes";

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// One number or name in a cron field, mapped to its value (months count from 1, weekdays from
/// Sunday = 0) and checked against the field's range.
fn cron_value(text: &str, min: u32, max: u32, names: &[&str]) -> Result<u32, String> {
    let value = if let Ok(number) = text.parse::<u32>() {
        number
    } else if let Some(index) = names
        .iter()
        .position(|name| name.eq_ignore_ascii_case(text))
    {
        min + index as u32
    } else {
        return Err(CRON_RANGE.into());
    };
    if value < min || value > max {
        return Err(CRON_RANGE.into());
    }
    Ok(value)
}

/// One field: a comma list of `*`, `*/n`, `a`, `a-b` and `a-b/n`, with every number in range.
fn cron_field(field: &str, min: u32, max: u32, names: &[&str]) -> Result<BTreeSet<u32>, String> {
    let mut values = BTreeSet::new();
    for part in field.split(',') {
        if part.is_empty() {
            return Err(CRON_RANGE.into());
        }
        let (body, step) = match part.split_once('/') {
            Some((body, step)) if !step.contains('/') => (body, Some(step)),
            Some(_) => return Err(CRON_RANGE.into()),
            None => (part, None),
        };
        let step = match step {
            Some(step) => step
                .parse::<u32>()
                .ok()
                .filter(|step| *step >= 1)
                .ok_or_else(|| CRON_RANGE.to_string())?,
            None => 1,
        };
        let (start, end) = if body == "*" {
            (min, max)
        } else if let Some((start, end)) = body.split_once('-') {
            let start = cron_value(start, min, max, names)?;
            let end = cron_value(end, min, max, names)?;
            if start > end {
                return Err(CRON_RANGE.into());
            }
            (start, end)
        } else {
            let single = cron_value(body, min, max, names)?;
            // `a/n` is not one of the five shapes: only `*/n` and `a-b/n` take a step.
            if step != 1 {
                return Err(CRON_RANGE.into());
            }
            (single, single)
        };
        let mut value = start;
        while value <= end {
            values.insert(value);
            value += step;
        }
    }
    Ok(values)
}

/// The minutes of an hour a cron fires on, from its minute field.
fn cron_minutes(field: &str) -> Result<BTreeSet<u32>, String> {
    cron_field(field, 0, 59, &[])
}

/// `true` when two fires are closer than five minutes, or more than twelve fire in an hour.
/// Only the minute field can make an hourly schedule fire more often than hourly; a set with a
/// gap under five minutes (including the wrap from the last minute to the first) is refused.
fn fires_too_often(minutes: &BTreeSet<u32>) -> bool {
    if minutes.len() > 12 {
        return true;
    }
    let values: Vec<u32> = minutes.iter().copied().collect();
    let adjacently_too_close = values.windows(2).any(|pair| pair[1] - pair[0] < 5);
    let wrap_too_close = match (values.first(), values.last()) {
        (Some(first), Some(last)) if values.len() > 1 => (60 - last) + first < 5,
        _ => false,
    };
    adjacently_too_close || wrap_too_close
}

/// The schedule rules of the loader: five fields, in range, no `@daily` shortcut, no seconds,
/// and never more often than every five minutes. `Err` is the sentence the caller sees.
pub fn validate_cron(cron: &str) -> Result<(), String> {
    parse_cron(cron).map(|_| ())
}

/// A parsed cron: the values each field names. The preview walks it (#223); `validate_cron` is
/// this parser's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CronSchedule {
    minutes: BTreeSet<u32>,
    hours: BTreeSet<u32>,
    days: BTreeSet<u32>,
    months: BTreeSet<u32>,
    weekdays: BTreeSet<u32>,
}

/// Parse and validate in one place, so the preview and the create route cannot drift: the field
/// sets, the `7` = Sunday spelling, and the same sentences.
fn parse_cron(cron: &str) -> Result<CronSchedule, String> {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(CRON_FIELDS.into());
    }
    let minutes = cron_minutes(fields[0])?;
    let hours = cron_field(fields[1], 0, 23, &[])?;
    let days = cron_field(fields[2], 1, 31, &[])?;
    let months = cron_field(fields[3], 1, 12, &MONTH_NAMES)?;
    let mut weekdays = cron_field(fields[4], 0, 7, &WEEKDAY_NAMES)?;
    if weekdays.remove(&7) {
        weekdays.insert(0);
    }
    if fires_too_often(&minutes) {
        return Err(CRON_TOO_OFTEN.into());
    }
    Ok(CronSchedule {
        minutes,
        hours,
        days,
        months,
        weekdays,
    })
}

/// The next `count` times the schedule fires in `zone`, strictly after `now`.
///
/// Walks the local days the cron names (at most 366 of them, so a schedule that can never fire —
/// `0 0 31 2 *` — answers fewer runs instead of spinning), then their hours and minutes. Like
/// croniter, a day matches when both day fields are restricted and either matches, and when one
/// of them is unrestricted and the other matches. A wall-clock time the zone skips at a DST jump
/// does not exist and is not a run; a wall-clock time that happens twice (the clocks going back)
/// is answered with its first occurrence.
fn next_runs(
    schedule: &CronSchedule,
    zone: Tz,
    count: usize,
    now: DateTime<Utc>,
) -> Vec<DateTime<Tz>> {
    let mut runs = Vec::with_capacity(count);
    // `*` and any expression that selects every day mean the same thing; the dom/dow rule only
    // cares whether a field actually restricts the day.
    let day_restricted = schedule.days.len() < 31;
    let weekday_restricted = schedule.weekdays.len() < 7;
    let today = now.with_timezone(&zone).date_naive();
    for offset in 0..=366 {
        if runs.len() == count {
            break;
        }
        let Some(day) = today.checked_add_days(Days::new(offset)) else {
            break;
        };
        if !schedule.months.contains(&day.month()) {
            continue;
        }
        let day_of_month = schedule.days.contains(&day.day());
        let day_of_week = schedule
            .weekdays
            .contains(&day.weekday().num_days_from_sunday());
        let matches = if day_restricted && weekday_restricted {
            day_of_month || day_of_week
        } else {
            day_of_month && day_of_week
        };
        if !matches {
            continue;
        }
        for hour in &schedule.hours {
            for minute in &schedule.minutes {
                let Some(local) = day.and_hms_opt(*hour, *minute, 0) else {
                    continue;
                };
                let fires_at = match zone.from_local_datetime(&local) {
                    LocalResult::None => continue,
                    LocalResult::Single(fires_at) => fires_at,
                    LocalResult::Ambiguous(first, _) => first,
                };
                if fires_at.with_timezone(&Utc) > now {
                    runs.push(fires_at);
                    if runs.len() == count {
                        return runs;
                    }
                }
            }
        }
    }
    runs
}

/// The validated schedule: the cron text and the IANA zone, as they go into the resource.
fn validate_schedule(
    schedule: Option<&ScheduleRequest>,
) -> Result<Option<(String, String)>, String> {
    let Some(schedule) = schedule else {
        return Ok(None);
    };
    let cron = schedule.cron.trim();
    validate_cron(cron)?;
    let timezone = schedule.timezone.trim();
    if Tz::from_str(timezone).is_err() {
        return Err("unknown timezone".into());
    }
    Ok(Some((cron.to_string(), timezone.to_string())))
}

/// The sentence for a group whose principal an administrator has not created yet. It is the
/// `principals` route's message and the create/update refusal, word for word.
fn no_principal_message(group: &str) -> String {
    format!(
        "No principal for this group yet: an administrator creates one with \
         make sync-principal NAME=pipeline-{group} GROUP={group}"
    )
}

/// Why a principal is not ready: its `Ready` condition's message, or `no status yet`.
fn principal_message(principal: &Value) -> String {
    principal
        .get("status")
        .and_then(|status| status.get("conditions"))
        .and_then(Value::as_array)
        .and_then(|conditions| {
            conditions
                .iter()
                .find(|condition| condition.get("type").and_then(Value::as_str) == Some("Ready"))
        })
        .and_then(|condition| condition.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .unwrap_or("no status yet")
        .to_string()
}

/// One group's principal: `pipeline-<group>` when a `ServicePrincipal` of that name with
/// `spec.ownerGroup` equal to the group exists, its `status.ready`, and why not when it is not.
fn principal_view(principals: &[Value], group: &str) -> PrincipalView {
    let name = format!("pipeline-{group}");
    let found = principals.iter().find(|principal| {
        principal
            .get("metadata")
            .and_then(|metadata| metadata.get("name"))
            .and_then(Value::as_str)
            == Some(name.as_str())
            && principal
                .get("spec")
                .and_then(|spec| spec.get("ownerGroup"))
                .and_then(Value::as_str)
                == Some(group)
    });
    match found {
        Some(principal) => {
            let ready = principal
                .get("status")
                .and_then(|status| status.get("ready"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            PrincipalView {
                group: group.to_string(),
                principal: Some(name),
                ready,
                message: (!ready).then(|| principal_message(principal)),
            }
        }
        None => PrincipalView {
            group: group.to_string(),
            principal: None,
            ready: false,
            message: Some(no_principal_message(group)),
        },
    }
}

/// Whether Nova wrote this pipeline (and may therefore change or delete it).
fn nova_managed(resource: &Value) -> bool {
    resource
        .get("metadata")
        .and_then(|metadata| metadata.get("labels"))
        .and_then(|labels| labels.get(MANAGED_BY_LABEL))
        .and_then(Value::as_str)
        == Some(MANAGED_BY_NOVA)
}

/// The kind of the pipeline's single `run` task when the form may edit it — `sql` or
/// `notebook` (#210, #237). Anything else is a shape the form does not edit, and an update
/// never changes the kind.
fn run_task_kind(resource: &Value) -> Option<&str> {
    let task = resource
        .get("spec")?
        .get("tasks")?
        .as_array()
        .filter(|tasks| tasks.len() == 1)?
        .first()?;
    if task.get("name").and_then(Value::as_str) != Some("run") {
        return None;
    }
    match task.get("kind").and_then(Value::as_str)? {
        kind @ ("sql" | "notebook") => Some(kind),
        _ => None,
    }
}

/// The `Pipeline` *Schedule* writes: the ticket's shape (#210), the caller's parameters and
/// options (#223), a statement or a pinned notebook version (#237), nothing else.
fn pipeline_resource(
    name: &str,
    group: &str,
    statement: &Statement,
    schedule: Option<&(String, String)>,
    params: &BTreeMap<String, String>,
    options: &PipelineOptionsView,
    created_by: &str,
) -> Value {
    let task = match statement {
        Statement::Sql(sql) => json!({ "name": "run", "kind": "sql", "sql": sql }),
        Statement::Notebook {
            name,
            object_version,
            params,
        } => json!({
            "name": "run",
            "kind": "notebook",
            "config": { "notebook": name, "objectVersion": object_version, "params": params },
        }),
    };
    let mut spec = json!({
        "ownerGroup": group,
        "runAs": format!("pipeline-{group}"),
        "tasks": [task],
        "concurrency": options.concurrency,
        "retries": { "attempts": options.retries, "backoff": format!("{}m", options.backoff_minutes) },
        "timeout": format!("{}m", options.timeout_minutes),
    });
    if !params.is_empty() {
        spec["params"] = json!(params);
    }
    if let Some((cron, timezone)) = schedule {
        spec["schedule"] = json!({ "cron": cron, "timezone": timezone });
    }
    let mut labels = serde_json::Map::new();
    labels.insert(MANAGED_BY_LABEL.to_string(), json!(MANAGED_BY_NOVA));
    labels.insert("stardelt.io/owner-group".to_string(), json!(group));
    json!({
        "apiVersion": "stardelt.io/v1alpha1",
        "kind": "Pipeline",
        "metadata": {
            "name": name,
            "namespace": crate::cluster::namespace(),
            "labels": Value::Object(labels),
            "annotations": { "stardelt.io/created-by": created_by },
        },
        "spec": spec,
    })
}

/// The in-memory create rate limit (`AppState::pipeline_creates`): per signed-in `sub`, the
/// creation times inside the last hour.
pub type CreateLimiter = std::sync::Mutex<HashMap<String, VecDeque<Instant>>>;

/// Take one creation slot for `sub`, or report that the hour is full. Refusals before the
/// limiter (bad name, foreign group, missing principal, bad body, name taken) never reach it;
/// only a restart forgets it.
fn take_create_slot(limiter: &CreateLimiter, sub: &str) -> bool {
    let mut map = limiter.lock().expect("create limiter poisoned");
    let now = Instant::now();
    let slots = map.entry(sub.to_string()).or_default();
    while slots
        .front()
        .is_some_and(|then| now.duration_since(*then) >= CREATE_WINDOW)
    {
        slots.pop_front();
    }
    if slots.len() >= CREATES_PER_HOUR {
        false
    } else {
        slots.push_back(now);
        true
    }
}

// ---------------------------------------------------------------------------
// Request and answer shapes (`Schedule`, #210)
// ---------------------------------------------------------------------------

/// A cron expression and its IANA zone; both required when a `schedule` object is sent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
pub struct ScheduleRequest {
    pub cron: String,
    pub timezone: String,
}

/// The advanced options of a schedule (#223), all optional: an omitted field keeps today's
/// default (one retry, five minutes between retries, a two-hour time limit, one run at a time).
/// `Replace` is deliberately not offered — the loader treats it as `Forbid`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PipelineOptions {
    /// Extra attempts after a failure, 0 to 5.
    #[serde(default)]
    pub retries: Option<i64>,
    /// The wait between attempts, 1 to 60 minutes.
    #[serde(default)]
    pub backoff_minutes: Option<i64>,
    /// The longest a whole run may take, 1 to 720 minutes.
    #[serde(default)]
    pub timeout_minutes: Option<i64>,
    /// `Forbid` (one run at a time, the default) or `Allow` (runs side by side).
    #[serde(default)]
    pub concurrency: Option<String>,
}

/// The options as the resource carries them, defaults filled in — what the edit dialog prefills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PipelineOptionsView {
    pub retries: i64,
    pub backoff_minutes: i64,
    pub timeout_minutes: i64,
    pub concurrency: String,
}

impl PipelineOptions {
    /// The options with their defaults, checked. `Err` is the sentence the caller sees; the
    /// order is the order of the fields (retries, the wait, the time limit, the run mode).
    fn resolve(&self) -> Result<PipelineOptionsView, String> {
        let retries = self.retries.unwrap_or(DEFAULT_RETRIES);
        if !(0..=5).contains(&retries) {
            return Err(RETRIES_RANGE.into());
        }
        let backoff_minutes = self.backoff_minutes.unwrap_or(DEFAULT_BACKOFF_MINUTES);
        if !(1..=60).contains(&backoff_minutes) {
            return Err(BACKOFF_RANGE.into());
        }
        let timeout_minutes = self.timeout_minutes.unwrap_or(DEFAULT_TIMEOUT_MINUTES);
        if !(1..=720).contains(&timeout_minutes) {
            return Err(TIMEOUT_RANGE.into());
        }
        let concurrency = self
            .concurrency
            .as_deref()
            .map(str::trim)
            .unwrap_or(DEFAULT_CONCURRENCY);
        if concurrency != "Allow" && concurrency != "Forbid" {
            return Err(CONCURRENCY_RULE.into());
        }
        Ok(PipelineOptionsView {
            retries,
            backoff_minutes,
            timeout_minutes,
            concurrency: concurrency.to_string(),
        })
    }
}

/// The published notebook version a schedule pins (#237): the `.ipynb` file name, the immutable
/// object version a publish answered, and the parameters the run passes to papermill.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotebookRequest {
    /// `<group>/<name>` is the object key; the group is the pipeline's `ownerGroup`, so a
    /// notebook of another group can never be named.
    pub name: String,
    /// The S3 object version, never a branch or a tag: exactly the bytes a person published.
    pub object_version: String,
    /// Parameter values by name; at most 32, names `[A-Za-z_][A-Za-z0-9_]{0,63}`, values up to
    /// 256 characters. Written as the task's `config.params` JSON object string.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

/// `POST /api/pipelines`: schedule a statement for one of the caller's own groups.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatePipelineRequest {
    pub name: String,
    pub owner_group: String,
    /// The statement to run; exactly one of this and `notebook` is given.
    #[serde(default)]
    pub sql: String,
    /// A published notebook version to run instead of a statement (#237).
    #[serde(default)]
    pub notebook: Option<NotebookRequest>,
    #[serde(default)]
    pub schedule: Option<ScheduleRequest>,
    /// Values for `${params.<name>}` in the statement; at most 16. A notebook's values are in
    /// `notebook.params`, not here.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Retries, the wait between them, the time limit and the run mode; omitted fields keep the
    /// defaults.
    #[serde(default)]
    pub options: Option<PipelineOptions>,
}

/// `PUT /api/pipelines/{namespace}/{name}`: change the statement, the schedule, the parameters
/// and the options — or, for a notebook pipeline, the notebook, its version and its parameters.
/// Every other field of the resource stays as it is, and what the pipeline runs never changes
/// kind.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePipelineRequest {
    /// The statement of a statement pipeline; a notebook pipeline refuses it.
    #[serde(default)]
    pub sql: String,
    /// The notebook, its version and its parameters for a notebook pipeline; a statement
    /// pipeline refuses it.
    #[serde(default)]
    pub notebook: Option<NotebookRequest>,
    /// Omitted means no schedule: the pipeline then runs on demand only.
    #[serde(default)]
    pub schedule: Option<ScheduleRequest>,
    /// The whole parameter table; omitted means none.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// The whole option set; omitted means the defaults.
    #[serde(default)]
    pub options: Option<PipelineOptions>,
}

/// `POST /api/pipelines/check`: what the engine says about a statement, without running it.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct CheckStatementRequest {
    pub sql: String,
    /// The parameters the statement's `${params.<name>}` placeholders are filled with.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

/// The engine's verdict. `checked: false` means this kind of statement cannot be checked here;
/// `ok` is absent then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CheckStatementResponse {
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// The engine's sentence when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `POST /api/pipelines/preview-schedule`: the next fire times of a cron in its zone.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct PreviewScheduleRequest {
    pub cron: String,
    pub timezone: String,
    /// How many runs to list, 1 to 10; 5 when omitted.
    #[serde(default)]
    pub count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct PreviewScheduleResponse {
    /// The next fire times in the schedule's zone, RFC 3339 with that offset.
    pub runs: Vec<String>,
}

/// One of the caller's groups and its pipeline principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct PrincipalView {
    pub group: String,
    /// `pipeline-<group>` when such a `ServicePrincipal` of this group exists.
    pub principal: Option<String>,
    /// The principal's `status.ready`.
    pub ready: bool,
    /// `null` when ready; otherwise the `Ready` condition's message, or `no status yet`, or the
    /// `make sync-principal` command for a group without a principal.
    pub message: Option<String>,
}

/// `GET /api/pipelines/principals`: every group of the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct PrincipalsResponse {
    pub groups: Vec<PrincipalView>,
}

// ---------------------------------------------------------------------------
// Views (serde camelCase; no raw Airflow field names and no `sources`)
// ---------------------------------------------------------------------------

/// One task as the resource declares it. A `sql` task's statement and a `notebook` task's file
/// name, version and parameters are carried only by the detail route (Schedule's edit dialog
/// prefills from them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskSpecView {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    /// The `.ipynb` file name of a `kind: "notebook"` task (#237).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notebook: Option<String>,
    /// The immutable object version a notebook task pins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_version: Option<String>,
    /// The parameters a notebook task passes, parsed back from `config.params`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notebook_params: Option<BTreeMap<String, String>>,
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
/// parameters, retry policy and options.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PipelineDetail {
    #[serde(flatten)]
    pub pipeline: PipelineView,
    pub runs: Vec<RunView>,
    pub params: BTreeMap<String, String>,
    pub retries: Option<RetriesView>,
    /// Retries, the wait between them, the time limit and the run mode as the resource carries
    /// them, with the defaults filled in when a field is absent.
    pub options: PipelineOptionsView,
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
        tasks: task_specs(spec, false),
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

/// A `Pipeline` of that name already exists; a create never overwrites one.
fn pipeline_exists() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "a pipeline with this name already exists",
    )
}

/// More than ten creates in an hour: the answer is 429, not a silently growing namespace.
fn too_many_creates() -> Fail {
    Fail::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too many pipelines created this hour; wait before creating another",
    )
}

/// A pipeline Nova did not write is somebody's manifest: change it there, not through the form.
fn managed_outside() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "this pipeline is managed outside Nova (kubectl or Git); change it there",
    )
}

/// The resource changed between the read and the write: reload instead of overwriting.
fn pipeline_changed() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "the pipeline changed while you edited it: reload",
    )
}

/// The form edits exactly one `sql` task named `run`; anything else is not its shape.
fn shape_not_editable() -> Fail {
    Fail::new(
        StatusCode::CONFLICT,
        "this pipeline has a shape the form cannot edit",
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

/// The status for a read of a resource: the sentence stays generic (a kube error names the API
/// server), the detail goes to the log.
fn resource_fail(e: ResourceError) -> Fail {
    match e {
        ResourceError::Unavailable(message) => {
            tracing::warn!(error = %message, "pipeline resource request failed");
            Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
        }
        // A read cannot take a name or hit a conflict; both are upstream failures here.
        ResourceError::Exists | ResourceError::Conflict => {
            Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
        }
    }
}

/// The status for a write: the two facts a read never has a sentence for become the caller's
/// 409, everything else is the generic upstream failure.
fn resource_write_fail(e: ResourceError) -> Fail {
    match e {
        ResourceError::Exists => pipeline_exists(),
        ResourceError::Conflict => pipeline_changed(),
        ResourceError::Unavailable(message) => {
            tracing::warn!(error = %message, "pipeline resource write failed");
            Fail::new(StatusCode::BAD_GATEWAY, "the cluster did not answer")
        }
    }
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
    let mut view = view_of(namespace, name, &resource, dag.as_ref(), runs.first());
    let spec = spec_of(&resource);
    // The statement goes only on the detail route: the list stays small, and the caller has
    // already passed the same visibility rule that `load_visible` applies to an edit.
    view.tasks = task_specs(spec, true);
    Ok(PipelineDetail {
        pipeline: view,
        runs: runs.iter().map(run_view).collect(),
        params: params_of(spec),
        retries: retries_of(spec),
        options: options_of(spec),
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
        Ok(run) => {
            audit::pipeline_event("run", who, namespace, name, Some(&run.run_id), "ok", None)
        }
        Err(e) => audit::pipeline_event("run", who, namespace, name, None, &e.outcome, None),
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
        Ok(_) => audit::pipeline_event(action, who, namespace, name, None, "ok", None),
        Err(e) => audit::pipeline_event(action, who, namespace, name, None, &e.outcome, None),
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
        Ok(run) => {
            audit::pipeline_event("stop", who, namespace, name, Some(&run.run_id), "ok", None)
        }
        Err(e) => {
            audit::pipeline_event("stop", who, namespace, name, Some(run_id), &e.outcome, None)
        }
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
// Writes: the principals route, create, update and delete. Every write is audited.
// ---------------------------------------------------------------------------

/// `GET /api/pipelines/principals`: every group of the caller and its pipeline principal.
async fn principal_views(
    resources: Option<&PipelineResources>,
    who: &Identity,
) -> Result<PrincipalsResponse, Fail> {
    let resources = resources.ok_or_else(no_cluster)?;
    let principals = resources.principals().await.map_err(resource_fail)?;
    let groups: BTreeSet<&str> = who
        .groups
        .iter()
        .map(String::as_str)
        .filter(|group| !group.is_empty())
        .collect();
    Ok(PrincipalsResponse {
        groups: groups
            .into_iter()
            .map(|group| principal_view(&principals, group))
            .collect(),
    })
}

/// The name an audit line can carry: the request's, capped so a malformed one cannot flood the
/// log (the rest of the request is refused by [`validate_name`] anyway).
fn audit_name(name: &str) -> String {
    name.chars().take(64).collect()
}

/// The facts a create or update records beside the action (#210, #223, #237): the statement's
/// **length**, the cron text and — for a notebook — the group, the file name and the pinned
/// version. Never the statement, never a parameter value.
struct WriteFacts {
    statement_chars: usize,
    cron: Option<String>,
    kind: &'static str,
    notebook: Option<(String, String, String)>,
}

impl WriteFacts {
    fn of(
        sql: &str,
        notebook: Option<&NotebookRequest>,
        schedule: Option<&ScheduleRequest>,
        group: &str,
    ) -> Self {
        Self {
            statement_chars: sql.trim().chars().count(),
            cron: schedule.map(|schedule| schedule.cron.trim().to_string()),
            kind: if notebook.is_some() {
                "notebook"
            } else {
                "sql"
            },
            notebook: notebook.map(|notebook| {
                (
                    group.to_string(),
                    notebook.name.clone(),
                    notebook.object_version.clone(),
                )
            }),
        }
    }

    fn write(&self) -> PipelineWrite<'_> {
        PipelineWrite {
            statement_chars: self.statement_chars,
            schedule: self.cron.as_deref(),
            kind: self.kind,
            notebook: self
                .notebook
                .as_ref()
                .map(|(group, name, version)| audit::NotebookTarget {
                    group,
                    name,
                    version,
                }),
        }
    }
}

/// The owner group of a pipeline, for an audit line: empty when the resource cannot be read
/// (the caller's answer is then a refusal anyway, and an audit line may not hold a guess).
async fn audit_group(resources: Option<&PipelineResources>, name: &str) -> String {
    let Some(resources) = resources else {
        return String::new();
    };
    let Ok(Some(resource)) = resources.get(name).await else {
        return String::new();
    };
    spec_of(&resource)
        .get("ownerGroup")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `POST /api/pipelines`: create the resource, then answer its view (`loaded: false`, because
/// Airflow has not seen it yet). Audited whatever happens.
async fn create_pipeline(
    resources: Option<&PipelineResources>,
    notebooks: Option<&NotebookStore>,
    limiter: &CreateLimiter,
    who: &Identity,
    body: CreatePipelineRequest,
) -> Result<PipelineView, Fail> {
    let namespace = crate::cluster::namespace();
    let pipeline = audit_name(&body.name);
    let facts = WriteFacts::of(
        &body.sql,
        body.notebook.as_ref(),
        body.schedule.as_ref(),
        &body.owner_group,
    );
    let result = create_pipeline_inner(resources, notebooks, limiter, who, body).await;
    match &result {
        Ok(_) => audit::pipeline_event(
            "create",
            who,
            &namespace,
            &pipeline,
            None,
            "ok",
            Some(facts.write()),
        ),
        Err(e) => audit::pipeline_event(
            "create",
            who,
            &namespace,
            &pipeline,
            None,
            &e.outcome,
            Some(facts.write()),
        ),
    }
    result.map_err(|e| e.fail)
}

async fn create_pipeline_inner(
    resources: Option<&PipelineResources>,
    notebooks_store: Option<&NotebookStore>,
    limiter: &CreateLimiter,
    who: &Identity,
    body: CreatePipelineRequest,
) -> Result<PipelineView, ActionError> {
    let resources = resources
        .ok_or_else(no_cluster)
        .map_err(ActionError::error)?;

    // 1. The name the CRD (and the loader) accepts.
    validate_name(&body.name).map_err(|message| {
        ActionError::refused("name", Fail::new(StatusCode::BAD_REQUEST, message))
    })?;

    // 2. One of the caller's own groups — an administrator schedules for their groups too.
    if !who.groups.iter().any(|group| group == &body.owner_group) {
        return Err(ActionError::refused(
            "not-member",
            Fail::new(
                StatusCode::FORBIDDEN,
                format!("you are not a member of the group {}", body.owner_group),
            ),
        ));
    }

    // 3. The principal of the group must exist and be ready: the loader refuses otherwise, and
    //    a pipeline the loader refuses would sit in the list as `not-loaded`.
    let principals = resources
        .principals()
        .await
        .map_err(|e| ActionError::error(resource_fail(e)))?;
    let principal = principal_view(&principals, &body.owner_group);
    if !principal.ready {
        return Err(ActionError::refused(
            "no-principal",
            Fail::new(StatusCode::CONFLICT, principal.message.unwrap_or_default()),
        ));
    }

    // 4. What it runs: exactly one of a statement and a notebook (#237), with the rules of its
    //    kind — the statement's parameters and placeholders, or the notebook's name,
    //    parameters and a pinned version that exists in the group.
    let statement = checked_statement(
        &body.sql,
        body.notebook.as_ref(),
        &body.params,
        notebooks_store,
        &body.owner_group,
    )
    .await?;

    // 5. The advanced options, defaults filled in.
    let options = body
        .options
        .clone()
        .unwrap_or_default()
        .resolve()
        .map_err(|message| {
            ActionError::refused("options", Fail::new(StatusCode::BAD_REQUEST, message))
        })?;

    // 6. The schedule (or none, for on demand).
    let schedule = validate_schedule(body.schedule.as_ref()).map_err(|message| {
        ActionError::refused("schedule", Fail::new(StatusCode::BAD_REQUEST, message))
    })?;

    // 7. The name is free.
    if resources
        .get(&body.name)
        .await
        .map_err(|e| ActionError::error(resource_fail(e)))?
        .is_some()
    {
        return Err(ActionError::refused("exists", pipeline_exists()));
    }

    // 8. The rate limit comes last: a refusal above never consumes a slot.
    if !take_create_slot(limiter, &who.sub) {
        return Err(ActionError::refused("rate-limited", too_many_creates()));
    }

    let resource = pipeline_resource(
        &body.name,
        &body.owner_group,
        &statement,
        schedule.as_ref(),
        &body.params,
        &options,
        &who.display_name,
    );
    let written = resources.create(resource).await.map_err(|e| match e {
        // Another Nova or a `kubectl apply` won the race between the read and the create.
        ResourceError::Exists => ActionError::refused("exists", pipeline_exists()),
        other => ActionError::error(resource_write_fail(other)),
    })?;
    let mut view = view_of(
        &crate::cluster::namespace(),
        &body.name,
        &written,
        None,
        None,
    );
    // The answer is the resource as written: the loader has not seen it yet, whatever the
    // cluster's clock says about its age.
    view.loaded = false;
    view.state = "loading".to_string();
    Ok(view)
}

/// The view after a write: the DAG and last run when Airflow is configured (a failed lookup only
/// leaves them empty), the resource alone otherwise — a write never requires Airflow.
async fn view_after_write(
    airflow: Option<&AirflowClient>,
    namespace: &str,
    name: &str,
    resource: &Value,
) -> PipelineView {
    let Some(airflow) = airflow else {
        return view_of(namespace, name, resource, None, None);
    };
    let dag_id = dag_id_for(namespace, name);
    let dag = match airflow.dag(&dag_id).await {
        Ok(dag) => Some(dag),
        Err(AirflowError::NotFound) => None,
        Err(e) => {
            tracing::warn!(dag = %dag_id, error = %e, "could not read the DAG after a write");
            None
        }
    };
    let last_run = match dag {
        Some(_) => airflow
            .runs(&dag_id, 1)
            .await
            .ok()
            .and_then(|runs| runs.into_iter().next()),
        None => None,
    };
    view_of(namespace, name, resource, dag.as_ref(), last_run.as_ref())
}

/// `PUT /api/pipelines/{namespace}/{name}`: change the statement and the schedule of a statement
/// pipeline, or the version and parameters of a notebook pipeline, and nothing else. Audited
/// whatever happens.
async fn update_pipeline(
    resources: Option<&PipelineResources>,
    notebooks: Option<&NotebookStore>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
    body: UpdatePipelineRequest,
) -> Result<PipelineView, Fail> {
    // The update keeps the pipeline's group, so the audit's group is read from the resource.
    let group = audit_group(resources, name).await;
    let facts = WriteFacts::of(
        &body.sql,
        body.notebook.as_ref(),
        body.schedule.as_ref(),
        &group,
    );
    let result =
        update_pipeline_inner(resources, notebooks, airflow, who, namespace, name, body).await;
    match &result {
        Ok(_) => audit::pipeline_event(
            "update",
            who,
            namespace,
            name,
            None,
            "ok",
            Some(facts.write()),
        ),
        Err(e) => audit::pipeline_event(
            "update",
            who,
            namespace,
            name,
            None,
            &e.outcome,
            Some(facts.write()),
        ),
    }
    result.map_err(|e| e.fail)
}

async fn update_pipeline_inner(
    resources: Option<&PipelineResources>,
    notebooks_store: Option<&NotebookStore>,
    airflow: Option<&AirflowClient>,
    who: &Identity,
    namespace: &str,
    name: &str,
    body: UpdatePipelineRequest,
) -> Result<PipelineView, ActionError> {
    let resources = resources
        .ok_or_else(no_cluster)
        .map_err(ActionError::error)?;
    let (_, resource) = load_actionable(resources, who, namespace, name)
        .await
        .map_err(|fail| ActionError::refused("not-found", fail))?;

    // Only what Nova wrote may be rewritten; a manifest stays its author's.
    if !nova_managed(&resource) {
        return Err(ActionError::refused("not-nova", managed_outside()));
    }

    // The pipeline's group: both the refusal sentence and the notebook's object key.
    let group = spec_of(&resource)
        .get("ownerGroup")
        .and_then(Value::as_str)
        .filter(|group| !group.is_empty())
        .ok_or_else(|| ActionError::refused("shape", shape_not_editable()))?
        .to_string();

    // A body names exactly one of a statement and a notebook (#237); an update never changes
    // what the pipeline runs, only the text or the pinned version.
    let requested_kind = match (body.sql.trim().is_empty(), body.notebook.is_some()) {
        (false, false) => "sql",
        (true, true) => "notebook",
        _ => {
            return Err(ActionError::refused(
                "body",
                Fail::new(StatusCode::BAD_REQUEST, ONE_OF_STATEMENT_OR_NOTEBOOK),
            ));
        }
    };
    match (run_task_kind(&resource), requested_kind) {
        (Some("notebook"), "sql") => {
            return Err(ActionError::refused(
                "kind",
                Fail::new(StatusCode::BAD_REQUEST, NOTEBOOK_PIPELINE),
            ));
        }
        (Some("sql"), "notebook") => {
            return Err(ActionError::refused(
                "kind",
                Fail::new(StatusCode::BAD_REQUEST, STATEMENT_PIPELINE),
            ));
        }
        _ => {}
    }

    let statement = checked_statement(
        &body.sql,
        body.notebook.as_ref(),
        &body.params,
        notebooks_store,
        &group,
    )
    .await?;

    let options = body
        .options
        .clone()
        .unwrap_or_default()
        .resolve()
        .map_err(|message| {
            ActionError::refused("options", Fail::new(StatusCode::BAD_REQUEST, message))
        })?;
    let schedule = validate_schedule(body.schedule.as_ref()).map_err(|message| {
        ActionError::refused("schedule", Fail::new(StatusCode::BAD_REQUEST, message))
    })?;

    // The principal of the pipeline's own group must still be ready, or the change would make a
    // pipeline the loader refuses.
    let principals = resources
        .principals()
        .await
        .map_err(|e| ActionError::error(resource_fail(e)))?;
    let principal = principal_view(&principals, &group);
    if !principal.ready {
        return Err(ActionError::refused(
            "no-principal",
            Fail::new(StatusCode::CONFLICT, principal.message.unwrap_or_default()),
        ));
    }

    // The form edits exactly one `run` task, of the kind it already has.
    if run_task_kind(&resource).is_none() {
        return Err(ActionError::refused("shape", shape_not_editable()));
    }

    let mut updated = resource.clone();
    match &statement {
        Statement::Sql(sql) => updated["spec"]["tasks"][0]["sql"] = json!(sql),
        // The body describes the whole notebook state: the version and the parameters are
        // written, not merged.
        Statement::Notebook {
            name,
            object_version,
            params,
        } => {
            updated["spec"]["tasks"][0]["config"] = json!({
                "notebook": name,
                "objectVersion": object_version,
                "params": params,
            });
        }
    }
    // The body describes the whole editable state: the options are written, not merged.
    updated["spec"]["concurrency"] = json!(options.concurrency);
    updated["spec"]["retries"] = json!({
        "attempts": options.retries,
        "backoff": format!("{}m", options.backoff_minutes),
    });
    updated["spec"]["timeout"] = json!(format!("{}m", options.timeout_minutes));
    if body.params.is_empty() {
        if let Some(spec) = updated.get_mut("spec").and_then(Value::as_object_mut) {
            spec.remove("params");
        }
    } else {
        updated["spec"]["params"] = json!(body.params);
    }
    match &schedule {
        Some((cron, timezone)) => {
            updated["spec"]["schedule"] = json!({ "cron": cron, "timezone": timezone });
        }
        None => {
            if let Some(spec) = updated.get_mut("spec").and_then(Value::as_object_mut) {
                spec.remove("schedule");
            }
        }
    }

    // The resource still carries the `resourceVersion` read above: a concurrent change is a
    // conflict, not a silent overwrite.
    let written = resources
        .replace(name, updated)
        .await
        .map_err(|e| match e {
            ResourceError::Conflict => ActionError::refused("conflict", pipeline_changed()),
            other => ActionError::error(resource_write_fail(other)),
        })?;
    Ok(view_after_write(airflow, namespace, name, &written).await)
}

/// `DELETE /api/pipelines/{namespace}/{name}`: remove a pipeline Nova wrote. Airflow is not
/// called — the resource is the source of truth, and the loader removes the DAG.
async fn delete_pipeline(
    resources: Option<&PipelineResources>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<(), Fail> {
    let result = delete_pipeline_inner(resources, who, namespace, name).await;
    match &result {
        Ok(()) => audit::pipeline_event("delete", who, namespace, name, None, "ok", None),
        Err(e) => audit::pipeline_event("delete", who, namespace, name, None, &e.outcome, None),
    }
    result.map_err(|e| e.fail)
}

async fn delete_pipeline_inner(
    resources: Option<&PipelineResources>,
    who: &Identity,
    namespace: &str,
    name: &str,
) -> Result<(), ActionError> {
    let resources = resources
        .ok_or_else(no_cluster)
        .map_err(ActionError::error)?;
    let (_, resource) = load_actionable(resources, who, namespace, name)
        .await
        .map_err(|fail| ActionError::refused("not-found", fail))?;
    if !nova_managed(&resource) {
        return Err(ActionError::refused("not-nova", managed_outside()));
    }
    resources
        .delete(name)
        .await
        .map_err(|e| ActionError::error(resource_write_fail(e)))
}

// ---------------------------------------------------------------------------
// The statement check and the next-runs preview (#223): no resource, no store.
// ---------------------------------------------------------------------------

/// The engine's `source` for the Schedule dialog's statement check. Trino records it in the
/// query events, so a check reads differently from a real query or a pipeline run.
const SOURCE_SCHEDULE_CHECK: &str = "nova-schedule-check";

/// A signed-in caller for the two routes that touch no `Pipeline` and no secret store. They
/// still refuse an anonymous caller: in `oidc` mode `require_auth` refuses first, and a router
/// mounted without that middleware — as the tests mount one — answers the same 401.
pub(crate) struct Caller {
    state: Arc<AppState>,
    principal: Principal,
}

impl Caller {
    fn state(&self) -> &AppState {
        &self.state
    }

    fn principal(&self) -> &Principal {
        &self.principal
    }
}

impl FromRequestParts<Arc<AppState>> for Caller {
    type Rejection = Fail;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let principal = parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or_else(|| Fail::new(StatusCode::UNAUTHORIZED, "not signed in"))?;
        Ok(Self {
            state: Arc::clone(state),
            principal,
        })
    }
}

/// The keywords `EXPLAIN` accepts here: what Nova may ask the engine about.
const CHECKABLE_KEYWORDS: [&str; 8] = [
    "select", "with", "insert", "merge", "delete", "update", "values", "create",
];

/// The statement's first word, as written.
fn first_keyword(statement: &str) -> &str {
    let start = statement.trim_start();
    let end = start
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(start.len());
    &start[..end]
}

/// Whether the engine can plan this statement. `create` only when it creates from a query
/// (`CREATE … AS …`): a bare `CREATE TABLE t (a int)` has no plan, and `EXPLAIN` refuses it.
fn checkable(statement: &str) -> bool {
    let keyword = first_keyword(statement);
    if !CHECKABLE_KEYWORDS
        .iter()
        .any(|checkable| keyword.eq_ignore_ascii_case(checkable))
    {
        return false;
    }
    if keyword.eq_ignore_ascii_case("create") {
        return statement.to_ascii_lowercase().contains(" as ");
    }
    true
}

/// `POST /api/pipelines/check`: fill the placeholders and ask the engine to *plan* the
/// statement — `EXPLAIN`, as the caller, under the check's own source. It never sends
/// `EXPLAIN ANALYZE` and never the statement itself, so nothing runs and nothing is written.
/// A statement kind that cannot be explained answers `checked: false`.
async fn check_statement(
    state: &AppState,
    caller: &Principal,
    body: CheckStatementRequest,
) -> Result<CheckStatementResponse, Fail> {
    let statement =
        validate_sql(&body.sql).map_err(|message| Fail::new(StatusCode::BAD_REQUEST, message))?;
    validate_params(&body.params).map_err(|message| Fail::new(StatusCode::BAD_REQUEST, message))?;
    check_placeholders(&statement, &body.params)
        .map_err(|message| Fail::new(StatusCode::BAD_REQUEST, message))?;
    if !checkable(&statement) {
        return Ok(CheckStatementResponse {
            checked: false,
            ok: None,
            message: None,
        });
    }
    let today = now_utc().format("%Y-%m-%d").to_string();
    let sql = format!(
        "EXPLAIN {}",
        fill_placeholders(&statement, &today, "check", &body.params)
    );
    match crate::trino::execute_from(state, sql, Some(caller), SOURCE_SCHEDULE_CHECK).await {
        Ok(_) => Ok(CheckStatementResponse {
            checked: true,
            ok: Some(true),
            message: None,
        }),
        // The engine did not answer at all: 502, like every other unreachable upstream (U12).
        Err(message) if message == crate::trino::ENGINE_UNREACHABLE => {
            Err(Fail::new(StatusCode::BAD_GATEWAY, message))
        }
        // A refused statement is the engine's sentence, not a Nova error.
        Err(message) => Ok(CheckStatementResponse {
            checked: true,
            ok: Some(false),
            message: Some(message),
        }),
    }
}

/// `POST /api/pipelines/preview-schedule`: the next `count` fire times of a cron in its zone,
/// RFC 3339 with that zone's offset. `now` is a parameter so the tests can pin the clock.
fn preview_schedule(
    body: PreviewScheduleRequest,
    now: DateTime<Utc>,
) -> Result<PreviewScheduleResponse, Fail> {
    let schedule = ScheduleRequest {
        cron: body.cron,
        timezone: body.timezone,
    };
    let (cron, timezone) = validate_schedule(Some(&schedule))
        .map_err(|message| Fail::new(StatusCode::BAD_REQUEST, message))?
        .expect("a schedule was given");
    let count = body.count.unwrap_or(5);
    if !(1..=10).contains(&count) {
        return Err(Fail::new(StatusCode::BAD_REQUEST, COUNT_RANGE));
    }
    let zone: Tz = timezone
        .parse()
        .expect("validate_schedule checked the zone");
    let parsed = parse_cron(&cron).expect("validate_schedule checked the cron");
    Ok(PreviewScheduleResponse {
        runs: next_runs(&parsed, zone, count, now)
            .into_iter()
            .map(|run| run.to_rfc3339())
            .collect(),
    })
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

/// The read, action and write routes, all uncacheable.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/pipelines", get(list).post(create))
        // The caller's groups and their pipeline principals, before any `{namespace}/{name}`
        // route can see the word "principals" as a name.
        .route("/pipelines/principals", get(principals))
        // The Schedule dialog's two helpers (#223): no resource and no store, but a signed-in
        // caller; placed before `{namespace}/{name}` like `principals`.
        .route("/pipelines/check", post(check))
        .route("/pipelines/preview-schedule", post(preview))
        .route(
            "/pipelines/{namespace}/{name}",
            get(detail).put(update).delete(remove),
        )
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
    path = "/api/pipelines/principals",
    tag = "public",
    description = "The caller's own groups and the pipeline principal of each. `principal` is `pipeline-<group>` when a `ServicePrincipal` of that name with `spec.ownerGroup` equal to the group exists; `ready` is its `status.ready`. `message` is `null` when it is ready, otherwise the `Ready` condition's message (or `no status yet`), and for a group without a principal the `make sync-principal NAME=pipeline-<group> GROUP=<group>` command that creates one. A group without a ready principal cannot schedule. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "One entry per group of the caller, sorted by group", body = PrincipalsResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn principals(signed: SignedIn) -> Response {
    answer(principal_views(resources(signed.state()).as_ref(), signed.identity()).await)
}

#[utoipa::path(
    post,
    path = "/api/pipelines/check",
    tag = "public",
    description = "What the engine says about a statement, without running it: Nova fills the statement's `${run.date}` (today, UTC), `${run.id}` (`check`) and `${params.<name>}` placeholders and sends `EXPLAIN <statement>` to the engine as the caller, under the source `nova-schedule-check`. It never sends `EXPLAIN ANALYZE` and never the statement itself. `checked: false` means this kind of statement cannot be checked here (only `select`, `with`, `insert`, `merge`, `delete`, `update`, `values` and `create … as …` are). A refused statement answers 200 with `ok: false` and the engine's sentence; an engine that does not answer is 502. Refusals before the engine: an empty, over-20,000-character or NUL-carrying statement 400, a parameter table or a placeholder as on create 400. Answers `Cache-Control: no-store`.",
    request_body = CheckStatementRequest,
    responses(
        (status = 200, description = "The engine's verdict, or that this statement cannot be checked", body = CheckStatementResponse),
        (status = 400, description = "The statement, a parameter or a placeholder is invalid", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The query engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn check(
    caller: Caller,
    ApiJson(body): ApiJson<CheckStatementRequest>,
) -> Response {
    answer(check_statement(caller.state(), caller.principal(), body).await)
}

#[utoipa::path(
    post,
    path = "/api/pipelines/preview-schedule",
    tag = "public",
    description = "The next fire times of a cron in its zone, RFC 3339 with that zone's offset (for example `2026-10-21T03:00:00+02:00`), so the Schedule dialog can show what a cadence means before it is saved. A field or a zone as on create (400 with the same sentences); `count` is 1 to 10, 5 when omitted. Like croniter, a day matches when both day fields are restricted and either matches, and when one is unrestricted and the other matches. A wall-clock time the zone skips at a DST jump is not a run; a time that happens twice is answered with its first occurrence. A cron that can never fire answers fewer runs (at most 366 days are searched), not an error. Answers `Cache-Control: no-store`.",
    request_body = PreviewScheduleRequest,
    responses(
        (status = 200, description = "The next runs in the schedule's zone", body = PreviewScheduleResponse),
        (status = 400, description = "The cron, the timezone or the count is invalid", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn preview(
    _caller: Caller,
    ApiJson(body): ApiJson<PreviewScheduleRequest>,
) -> Response {
    answer(preview_schedule(body, now_utc()))
}

#[utoipa::path(
    post,
    path = "/api/pipelines",
    tag = "public",
    description = "Schedule a statement or a published notebook: create a `Pipeline` for one of the caller's owner groups (`runAs: pipeline-<group>`), validated so the loader accepts it. A body gives exactly one of a non-empty `sql` and a `notebook` (`{ name, objectVersion, params }`, #237) — both or neither is 400 `give a statement or a notebook`. Refusals, in this order: a name outside 1 to 40 lower-case letters, digits and dashes starting with a letter 400; a group the caller is not a member of 403 (`you are not a member of the group <g>` — administrators included); a group without a ready `ServicePrincipal` 409 with the `principals` route's message; an empty, over-20,000-character or NUL-carrying statement 400; a parameter name, a parameter count or value, or a `${…}` placeholder the loader cannot fill 400; a notebook name outside `^[A-Za-z0-9][A-Za-z0-9._ -]{0,127}\\.ipynb$` 400, more than 32 notebook parameters, a value over 256 characters or NUL, or a `params` JSON text over 4096 characters 400, and a version that is not in the store 400 `that notebook version does not exist in group <g>` (503 when `NOVA_S3_*` is unset, 502 when S3 does not answer); a retry, wait, time-limit or run-mode option out of range 400; a cron that is not five in-range fields or fires more often than every five minutes 400; an unknown IANA timezone 400; a name that already exists 409; more than ten creations per user per hour 429. `options` and `params` are optional: an omitted option keeps its default (1 retry after 5 minutes, a 120-minute time limit, `Forbid`); `params` are the statement's `${params.<name>}` values, a notebook's values are in `notebook.params`. Airflow is not needed: the resource is the source of truth, and the answer is the new pipeline with `loaded: false`, `state: \"loading\"`. The audit line carries the statement's length, the cron and, for a notebook, the group, the file name and the pinned version — never the statement and never a parameter value. Answers `Cache-Control: no-store`.",
    request_body = CreatePipelineRequest,
    responses(
        (status = 201, description = "The resource as written", body = PipelineView),
        (status = 400, description = "The name, the statement, the cron or the timezone is invalid", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller is not a member of the owner group", body = crate::openapi::ErrorResponse),
        (status = 409, description = "No ready principal for the group, or the name is taken", body = crate::openapi::ErrorResponse),
        (status = 429, description = "More than ten creations in the last hour", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn create(
    signed: SignedIn,
    ApiJson(body): ApiJson<CreatePipelineRequest>,
) -> Response {
    answer_with(
        StatusCode::CREATED,
        create_pipeline(
            resources(signed.state()).as_ref(),
            signed.state().notebooks.as_ref(),
            &signed.state().pipeline_creates,
            signed.identity(),
            body,
        )
        .await,
    )
}

#[utoipa::path(
    put,
    path = "/api/pipelines/{namespace}/{name}",
    tag = "public",
    description = "Change a pipeline Nova created (`app.kubernetes.io/managed-by: stardelt-nova`) and nothing else. A statement pipeline's body carries `sql`; a notebook pipeline's carries `notebook` (`{ name, objectVersion, params }`), and an update never changes which of the two it is: `sql` on a notebook pipeline is 400 `this pipeline runs a notebook`, a notebook on a statement pipeline 400 `this pipeline runs a statement`, and both or neither 400 `give a statement or a notebook`. The body describes the whole editable state: an omitted `schedule` removes it, an omitted `params` removes the table, an omitted option keeps its default, and a notebook body's file name, version and parameters replace the task's `config`. Refusals: a pipeline the caller may not act on 404; a pipeline Nova did not write, or one whose task shape is not exactly one `run` task of kind `sql` or `notebook` 409; the statement, a parameter, a placeholder, a notebook name, a parameter value or count, or a version that is not in the store (503 without `NOVA_S3_*`, 502 when S3 does not answer), an option, the cron or the timezone as on create 400; the group's principal no longer ready 409 with the `principals` route's message; a concurrent change to the resource 409 `the pipeline changed while you edited it: reload`. The audit line carries the statement's length, the cron and, for a notebook, the group, the file name and the pinned version — never the statement and never a parameter value. Answers `Cache-Control: no-store`.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    request_body = UpdatePipelineRequest,
    responses(
        (status = 200, description = "The resource as written", body = PipelineView),
        (status = 400, description = "The statement, the cron or the timezone is invalid", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller", body = crate::openapi::ErrorResponse),
        (status = 409, description = "Managed outside Nova, an uneditable shape, no ready principal, or a conflict", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn update(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
    ApiJson(body): ApiJson<UpdatePipelineRequest>,
) -> Response {
    answer(
        update_pipeline(
            resources(signed.state()).as_ref(),
            signed.state().notebooks.as_ref(),
            signed.state().airflow.as_ref(),
            signed.identity(),
            &namespace,
            &name,
            body,
        )
        .await,
    )
}

#[utoipa::path(
    delete,
    path = "/api/pipelines/{namespace}/{name}",
    tag = "public",
    description = "Delete a pipeline Nova created. Only a pipeline the caller may act on and that carries `app.kubernetes.io/managed-by: stardelt-nova` may be deleted: a `kubectl` or GitOps pipeline answers 409 instead. Airflow is not called — the resource is the source of truth; the DAG leaves the scheduler's default list after about 90 seconds while its history stays readable. Answers 204 with `Cache-Control: no-store`; the attempt is audited.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    responses(
        (status = 204, description = "Deleted (or already gone)"),
        (status = 401, description = "No token to sign in with", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such pipeline for this caller", body = crate::openapi::ErrorResponse),
        (status = 409, description = "The pipeline is managed outside Nova", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The cluster did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "No cluster connection", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn remove(
    signed: SignedIn,
    Path((namespace, name)): Path<(String, String)>,
) -> Response {
    match delete_pipeline(
        resources(signed.state()).as_ref(),
        signed.identity(),
        &namespace,
        &name,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/api/pipelines/{namespace}/{name}",
    tag = "public",
    description = "One pipeline: the list fields, the latest 20 runs, and the resource's `params`, `retries` and `options` (defaults filled in when a field is absent, so the edit dialog can prefill). A pipeline whose DAG does not exist yet answers 200 with `loaded: false` and `state: \"loading\"`. A pipeline of another group answers 404. History of a deleted pipeline is not offered in this version: without the resource the answer is 404.",
    params(
        ("namespace" = String, Path, description = "The Pipeline's namespace (Nova's own)"),
        ("name" = String, Path, description = "Pipeline name"),
    ),
    responses(
        (status = 200, description = "The pipeline, its latest runs, params, retries and options", body = PipelineDetail),
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
    use crate::notebooks::NotebookStore;
    use crate::notebooks::fake::{FakeS3, notebook_store};
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
        memory_with(items, &[])
    }

    fn memory_with(items: &[(&str, Value)], principals: &[(&str, Value)]) -> PipelineResources {
        let pipelines: BTreeMap<String, Value> = items
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        let principals: BTreeMap<String, Value> = principals
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        PipelineResources::Memory(Arc::new(Mutex::new(MemoryStore {
            pipelines,
            principals,
        })))
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

    /// A fake Kubernetes API serving the `Pipeline` and `ServicePrincipal` routes the resource
    /// client uses, including the writes and the `resourceVersion` check of a replace.
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
            principals: Vec<Value>,
            version: u64,
        }

        type Shared = Arc<Mutex<Fake>>;

        fn not_found() -> (StatusCode, Json<Value>) {
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "message": "not found",
                    "reason": "NotFound",
                    "code": 404
                })),
            )
        }

        fn conflict() -> (StatusCode, Json<Value>) {
            (
                StatusCode::CONFLICT,
                Json(json!({
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "message": "the object has been modified",
                    "reason": "Conflict",
                    "code": 409
                })),
            )
        }

        async fn list(State(state): State<Shared>) -> Json<Value> {
            let state = state.lock().unwrap();
            Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "PipelineList",
                "metadata": { "resourceVersion": "1" },
                "items": state.items
            }))
        }

        async fn principal_list(State(state): State<Shared>) -> Json<Value> {
            let state = state.lock().unwrap();
            Json(json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "ServicePrincipalList",
                "metadata": { "resourceVersion": "1" },
                "items": state.principals
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
                None => Err(not_found()),
            }
        }

        async fn create(
            State(state): State<Shared>,
            Path(_namespace): Path<String>,
            Json(body): Json<Value>,
        ) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
            let mut state = state.lock().unwrap();
            let name = body["metadata"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state
                .items
                .iter()
                .any(|item| item["metadata"]["name"] == name)
            {
                return Err(conflict());
            }
            state.version += 1;
            let resource_version = state.version.to_string();
            let mut stored = body;
            stored["metadata"]["resourceVersion"] = json!(resource_version);
            stored["metadata"]["creationTimestamp"] = json!("2026-10-20T10:00:00Z");
            state.items.push(stored.clone());
            Ok((StatusCode::CREATED, Json(stored)))
        }

        async fn replace(
            State(state): State<Shared>,
            Path((_namespace, name)): Path<(String, String)>,
            Json(body): Json<Value>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .items
                .iter()
                .position(|item| item["metadata"]["name"] == name)
            else {
                return Err(not_found());
            };
            if state.items[index]["metadata"]["resourceVersion"]
                != body["metadata"]["resourceVersion"]
            {
                return Err(conflict());
            }
            state.version += 1;
            let resource_version = state.version.to_string();
            let mut stored = body;
            stored["metadata"]["resourceVersion"] = json!(resource_version);
            state.items[index] = stored.clone();
            Ok(Json(stored))
        }

        async fn delete_one(
            State(state): State<Shared>,
            Path((_namespace, name)): Path<(String, String)>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .items
                .iter()
                .position(|item| item["metadata"]["name"] == name)
            else {
                return Err(not_found());
            };
            state.items.remove(index);
            Ok(Json(json!({
                "kind": "Status",
                "apiVersion": "v1",
                "status": "Success",
                "code": 200
            })))
        }

        pub async fn start(items: Vec<Value>) -> kube::Client {
            start_with(items, vec![]).await
        }

        pub async fn start_with(items: Vec<Value>, principals: Vec<Value>) -> kube::Client {
            let shared: Shared = Arc::new(Mutex::new(Fake {
                items,
                principals,
                version: 1,
            }));
            let app = Router::new()
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/pipelines",
                    get(list).post(create),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/pipelines/{name}",
                    get(one).put(replace).delete(delete_one),
                )
                .route(
                    "/apis/stardelt.io/v1alpha1/namespaces/{namespace}/serviceprincipals",
                    get(principal_list),
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

    /// A fake Trino coordinator with the one endpoint the statement check uses. It records the
    /// statement text and the headers, so a test can prove what Nova sent — and what it never sent.
    mod fake_trino {
        use std::sync::{Arc, Mutex};

        use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
        use serde_json::{Value, json};

        #[derive(Default)]
        pub struct Fake {
            /// The body of every `/v1/statement` POST, in order.
            pub statements: Vec<String>,
            /// The `X-Trino-Source` of every POST.
            pub sources: Vec<String>,
            /// The `X-Trino-User` of every POST: who the check ran as.
            pub users: Vec<String>,
            /// Answer this error document instead of a result.
            pub error: Option<Value>,
        }

        pub type Shared = Arc<Mutex<Fake>>;

        async fn statement(
            State(state): State<Shared>,
            headers: HeaderMap,
            body: String,
        ) -> Json<Value> {
            let mut fake = state.lock().expect("fake trino poisoned");
            fake.statements.push(body);
            fake.sources.push(
                headers
                    .get("x-trino-source")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
            );
            fake.users.push(
                headers
                    .get("x-trino-user")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
            );
            match &fake.error {
                // No `nextUri`: the client stops on the first document.
                Some(error) => Json(json!({ "id": "q-check", "error": error, "stats": {} })),
                None => Json(json!({
                    "id": "q-check",
                    "columns": [{ "name": "plan", "type": "varchar" }],
                    "data": [["Output[columnNames = [x]]"]],
                    "stats": {}
                })),
            }
        }

        pub async fn start(error: Option<Value>) -> (String, Shared) {
            let shared: Shared = Arc::new(Mutex::new(Fake {
                error,
                ..Fake::default()
            }));
            let app = Router::new()
                .route("/v1/statement", post(statement))
                .with_state(shared.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (format!("http://{addr}"), shared)
        }
    }

    fn test_state(
        kube: Option<kube::Client>,
        airflow: AirflowClient,
        store: StoreClient,
    ) -> Arc<AppState> {
        test_state_at(kube, airflow, store, "http://127.0.0.1:1")
    }

    fn test_state_at(
        kube: Option<kube::Client>,
        airflow: AirflowClient,
        store: StoreClient,
        trino_url: &str,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: trino_url.into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            // No catalog client in these tests: Lakekeeper is called as before (#222).
            catalog_token: None,
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            catalog_ids: std::sync::RwLock::new(std::collections::HashMap::new()),
            catalog_access: std::sync::Mutex::new(std::collections::HashMap::new()),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(crate::demo::DemoState::default()),
            kube,
            workloads: crate::cluster::WorkloadCache::default(),
            admin_console_url: None,
            db: None,
            store: Some(store),
            break_glass_reads: Mutex::new(std::collections::HashMap::new()),
            run_starts: Mutex::new(std::collections::HashMap::new()),
            pipeline_creates: Mutex::new(std::collections::HashMap::new()),
            airflow: Some(airflow),
            table_locks: Mutex::new(std::collections::HashMap::new()),
            people_touched: Mutex::new(std::collections::HashMap::new()),
            notebooks: None,
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
            .with_state(test_state(Some(kube), airflow, store))
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

    #[tokio::test]
    async fn detail_carries_the_statement() {
        let mut smoke = resource("smoke", Some("analytics"));
        smoke["spec"]["tasks"] = json!([{ "name": "run", "kind": "sql", "sql": "select 1" }]);
        let resources = memory(&[("smoke", smoke)]);
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

        assert_eq!(detail.pipeline.tasks.len(), 1);
        assert_eq!(detail.pipeline.tasks[0].sql.as_deref(), Some("select 1"));
    }

    #[tokio::test]
    async fn list_does_not_carry_the_statement() {
        let mut smoke = resource("smoke", Some("analytics"));
        smoke["spec"]["tasks"] = json!([{ "name": "run", "kind": "sql", "sql": "select 1" }]);
        let resources = memory(&[("smoke", smoke)]);
        let (airflow, _fake) = fake_airflow::start().await;

        let answer = list_pipelines(Some(&resources), Some(&airflow), &admin())
            .await
            .expect("the list succeeds");

        let tasks = &answer.pipelines[0].tasks;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].sql, None);
        let value = serde_json::to_value(&tasks[0]).expect("serialises");
        assert!(
            value.get("sql").is_none(),
            "the list does not carry the statement: {value}"
        );
    }

    #[tokio::test]
    async fn detail_task_without_statement_has_none() {
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

        assert!(
            !detail.pipeline.tasks.is_empty(),
            "the fixture declares tasks"
        );
        for task in &detail.pipeline.tasks {
            assert_eq!(task.sql, None, "{} has no statement", task.name);
        }
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
        let _ = audit::test_sink::take_for("sub-audit-196");

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

        let records: Vec<audit::test_sink::Record> = audit::test_sink::take_for("sub-audit-196");
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

    // -----------------------------------------------------------------------
    // Writes: principals, create, update, delete (#210)
    // -----------------------------------------------------------------------

    const NOT_READY_MESSAGE: &str = "the client credentials are not written yet";

    /// A `ServicePrincipal` as the operator writes it. `None` leaves `status` off, as a resource
    /// the operator has not seen yet.
    fn service_principal(name: &str, group: &str, ready: Option<bool>) -> Value {
        let mut resource = json!({
            "apiVersion": "stardelt.io/v1alpha1",
            "kind": "ServicePrincipal",
            "metadata": { "name": name, "namespace": crate::cluster::namespace() },
            "spec": { "ownerGroup": group },
        });
        if let Some(ready) = ready {
            resource["status"] = json!({
                "ready": ready,
                "conditions": [{
                    "type": "Ready",
                    "status": if ready { "True" } else { "False" },
                    "reason": if ready { "Ready" } else { "Pending" },
                    "message": if ready { "the client exists" } else { NOT_READY_MESSAGE },
                }],
            });
        }
        resource
    }

    /// A namespace with the `pipeline-analytics` principal ready: what a create needs.
    fn analytics_ready() -> PipelineResources {
        memory_with(
            &[],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(true)),
            )],
        )
    }

    fn create_body(name: &str, group: &str, sql: &str) -> CreatePipelineRequest {
        CreatePipelineRequest {
            name: name.into(),
            owner_group: group.into(),
            sql: sql.into(),
            notebook: None,
            schedule: Some(ScheduleRequest {
                cron: "0 3 * * *".into(),
                timezone: "Europe/Berlin".into(),
            }),
            params: BTreeMap::new(),
            options: None,
        }
    }

    fn update_body(sql: &str) -> UpdatePipelineRequest {
        UpdatePipelineRequest {
            sql: sql.into(),
            notebook: None,
            schedule: None,
            params: BTreeMap::new(),
            options: None,
        }
    }

    /// The defaults `pipeline_resource` writes when a body sends no options.
    fn default_options() -> PipelineOptionsView {
        PipelineOptions::default()
            .resolve()
            .expect("the defaults are in range")
    }

    fn create_limiter() -> CreateLimiter {
        CreateLimiter::default()
    }

    /// A pipeline Nova wrote, carrying the `resourceVersion` a read would see.
    fn nova_pipeline(name: &str, group: &str) -> Value {
        let mut resource = pipeline_resource(
            name,
            group,
            &Statement::Sql("select 1".into()),
            Some(&("0 3 * * *".to_string(), "Europe/Berlin".to_string())),
            &BTreeMap::new(),
            &default_options(),
            "sub-ada",
        );
        resource["metadata"]["resourceVersion"] = json!("7");
        resource
    }

    /// The sentence the ticket specifies, spelled out rather than built by the function under
    /// test, so a typo in the code cannot hide behind the test.
    fn missing_principal_sentence(group: &str) -> String {
        format!(
            "No principal for this group yet: an administrator creates one with \
             make sync-principal NAME=pipeline-{group} GROUP={group}"
        )
    }

    async fn stored_pipeline(resources: &PipelineResources, name: &str) -> Option<Value> {
        resources.get(name).await.expect("memory read")
    }

    #[test]
    fn validate_cron_table() {
        for cron in [
            "0 3 * * *",
            "*/15 * * * *",
            "0 9 * * 1-5",
            "30 6 1 * *",
            "0 8 * * mon",
            "0 */2 * * *",
            "5,35 * * * *",
        ] {
            assert_eq!(validate_cron(cron), Ok(()), "{cron} must be accepted");
        }
        for (cron, sentence) in [
            ("* * * * *", CRON_TOO_OFTEN),
            ("*/2 * * * *", CRON_TOO_OFTEN),
            ("0 3 * *", CRON_FIELDS),
            ("@daily", CRON_FIELDS),
            ("0 3 * * * *", CRON_FIELDS),
            ("0 25 * * *", CRON_RANGE),
            ("61 * * * *", CRON_RANGE),
            ("a b c d e", CRON_RANGE),
            ("*/0 * * * *", CRON_RANGE),
            ("5-1 * * * *", CRON_RANGE),
            ("", CRON_FIELDS),
        ] {
            assert_eq!(
                validate_cron(cron).expect_err(cron),
                sentence,
                "{cron:?} must be refused with the field sentence"
            );
        }
        // No shortcuts and no seconds, named months and weekdays in range, `a/n` not in the
        // grammar, a step larger than the field is still one fire.
        for cron in [
            "@hourly",
            "0 3 * * * *",
            "0 3 * * mon-fri/0",
            "0 3 * * jan",
            "5/10 * * * *",
            "0 0 * * 8",
        ] {
            assert!(validate_cron(cron).is_err(), "{cron} must be refused");
        }
        assert_eq!(validate_cron("0 3 1 jan-mar sun"), Ok(()));
        assert_eq!(validate_cron("0 3 * * 0-7"), Ok(()));
    }

    #[tokio::test]
    async fn create_writes_the_resource_with_owner_runas_labels_and_defaults() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);

        let view = create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &ada,
            create_body("daily-trips", "analytics", "select 1;"),
        )
        .await
        .expect("the pipeline is created");

        assert_eq!(view.name, "daily-trips");
        assert_eq!(view.owner_group.as_deref(), Some("analytics"));
        assert_eq!(view.run_as.as_deref(), Some("pipeline-analytics"));
        assert!(!view.loaded, "Airflow has not seen it yet");
        assert_eq!(view.state, "loading");
        assert_eq!(view.schedule.as_deref(), Some("0 3 * * * (Europe/Berlin)"));

        let stored = stored_pipeline(&resources, "daily-trips")
            .await
            .expect("the resource is written");
        assert_eq!(
            stored,
            json!({
                "apiVersion": "stardelt.io/v1alpha1",
                "kind": "Pipeline",
                "metadata": {
                    "name": "daily-trips",
                    "namespace": crate::cluster::namespace(),
                    "labels": {
                        "app.kubernetes.io/managed-by": "stardelt-nova",
                        "stardelt.io/owner-group": "analytics"
                    },
                    "annotations": { "stardelt.io/created-by": "sub-ada" }
                },
                "spec": {
                    "ownerGroup": "analytics",
                    "runAs": "pipeline-analytics",
                    "schedule": { "cron": "0 3 * * *", "timezone": "Europe/Berlin" },
                    "tasks": [ { "name": "run", "kind": "sql", "sql": "select 1" } ],
                    "concurrency": "Forbid",
                    "retries": { "attempts": 1, "backoff": "5m" },
                    // The default 120 minutes in the loader's `<n>m` grammar (#223).
                    "timeout": "120m"
                }
            }),
            "the whole resource, exactly as the ticket specifies it"
        );
    }

    #[tokio::test]
    async fn create_without_schedule_omits_it() {
        let resources = analytics_ready();
        let mut body = create_body("on-demand", "analytics", "select 1");
        body.schedule = None;

        let view = create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &who("sub-ada", &["analytics"], false),
            body,
        )
        .await
        .expect("an on-demand pipeline is created");

        assert_eq!(view.schedule, None);
        let stored = stored_pipeline(&resources, "on-demand")
            .await
            .expect("the resource is written");
        assert!(
            stored["spec"].get("schedule").is_none(),
            "an on-demand pipeline carries no schedule: {stored}"
        );
        assert_eq!(stored["spec"]["tasks"][0]["sql"], "select 1");
    }

    #[tokio::test]
    async fn create_refuses_a_group_the_caller_is_not_in() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let error = create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &ada,
            create_body("smoke", "finance", "select 1"),
        )
        .await
        .expect_err("not a member of finance");
        assert_eq!(error.status(), StatusCode::FORBIDDEN);
        assert_eq!(error.message(), "you are not a member of the group finance");
        assert!(
            resources.list().await.unwrap().is_empty(),
            "nothing is written for a group the caller is not in"
        );

        // An administrator is no substitute: they schedule for the groups they belong to.
        let root = who("sub-root", &["stardelt-admins"], true);
        let error = create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &root,
            create_body("smoke", "analytics", "select 1"),
        )
        .await
        .expect_err("an administrator is not a member of analytics");
        assert_eq!(error.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            error.message(),
            "you are not a member of the group analytics"
        );
        assert!(resources.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_needs_a_ready_principal_of_the_same_group() {
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();

        // No principal at all.
        let resources = memory(&[]);
        let error = create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("smoke", "analytics", "select 1"),
        )
        .await
        .expect_err("no principal");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), missing_principal_sentence("analytics"));
        assert!(resources.list().await.unwrap().is_empty());

        // The principal exists but is not ready: the condition's message is the sentence.
        let resources = memory_with(
            &[],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(false)),
            )],
        );
        let error = create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("smoke", "analytics", "select 1"),
        )
        .await
        .expect_err("not ready");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), NOT_READY_MESSAGE);
        assert!(resources.list().await.unwrap().is_empty());

        // The right name, but another group's principal: treated as absent.
        let resources = memory_with(
            &[],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "finance", Some(true)),
            )],
        );
        let error = create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("smoke", "analytics", "select 1"),
        )
        .await
        .expect_err("another group's principal");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), missing_principal_sentence("analytics"));
        assert!(resources.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_validates_name_sql_timezone() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();
        let long_name = "a".repeat(41);

        for name in [
            "",
            "Analytics",
            "1analytics",
            "analytics-",
            long_name.as_str(),
        ] {
            let error = create_pipeline(
                Some(&resources),
                None,
                &limiter,
                &ada,
                create_body(name, "analytics", "select 1"),
            )
            .await
            .expect_err("invalid name");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{name:?}");
            assert_eq!(error.message(), invalid_name(), "{name:?}");
        }

        // A blank statement is neither a statement nor a notebook (#237).
        let error = create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("smoke", "analytics", "   "),
        )
        .await
        .expect_err("no statement");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), ONE_OF_STATEMENT_OR_NOTEBOOK);

        for sql in [
            "x".repeat(MAX_STATEMENT_CHARS + 1),
            "select 1\0".to_string(),
            ";".to_string(),
        ] {
            let error = create_pipeline(
                Some(&resources),
                None,
                &limiter,
                &ada,
                create_body("smoke", "analytics", &sql),
            )
            .await
            .expect_err("invalid statement");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST);
            assert_eq!(error.message(), invalid_sql());
        }

        let mut body = create_body("smoke", "analytics", "select 1");
        body.schedule = Some(ScheduleRequest {
            cron: "0 3 * * *".into(),
            timezone: "Mars/Olympus".into(),
        });
        let error = create_pipeline(Some(&resources), None, &limiter, &ada, body)
            .await
            .expect_err("not an IANA zone");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), "unknown timezone");

        for (cron, sentence) in [("* * * * *", CRON_TOO_OFTEN), ("0 3 * *", CRON_FIELDS)] {
            let mut body = create_body("smoke", "analytics", "select 1");
            body.schedule = Some(ScheduleRequest {
                cron: cron.into(),
                timezone: "UTC".into(),
            });
            let error = create_pipeline(Some(&resources), None, &limiter, &ada, body)
                .await
                .expect_err("invalid cron");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST);
            assert_eq!(error.message(), sentence);
        }
        assert!(
            resources.list().await.unwrap().is_empty(),
            "no refusal wrote anything"
        );

        // One trailing `;` is dropped from the stored statement; the rest is trimmed.
        create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("trailing", "analytics", "  select 42 ;  "),
        )
        .await
        .expect("a trailing semicolon is fine");
        let stored = stored_pipeline(&resources, "trailing")
            .await
            .expect("written");
        assert_eq!(stored["spec"]["tasks"][0]["sql"], "select 42");
    }

    #[tokio::test]
    async fn create_twice_is_a_conflict() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();

        create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("smoke", "analytics", "select 1"),
        )
        .await
        .expect("the first create wins");

        let error = create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("smoke", "analytics", "select 2"),
        )
        .await
        .expect_err("the name is taken");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), "a pipeline with this name already exists");
        let stored = stored_pipeline(&resources, "smoke")
            .await
            .expect("still there");
        assert_eq!(
            stored["spec"]["tasks"][0]["sql"], "select 1",
            "the first pipeline is never overwritten"
        );
    }

    #[tokio::test]
    async fn create_is_rate_limited() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();

        for index in 0..CREATES_PER_HOUR {
            create_pipeline(
                Some(&resources),
                None,
                &limiter,
                &ada,
                create_body(&format!("daily-{index}"), "analytics", "select 1"),
            )
            .await
            .unwrap_or_else(|error| panic!("create {index}: {}", error.message()));
        }
        let error = create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &ada,
            create_body("one-more", "analytics", "select 1"),
        )
        .await
        .expect_err("the eleventh create in an hour");
        assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            error.message(),
            "too many pipelines created this hour; wait before creating another"
        );
        assert!(
            stored_pipeline(&resources, "one-more").await.is_none(),
            "a rate-limited create writes nothing"
        );
        assert_eq!(resources.list().await.unwrap().len(), CREATES_PER_HOUR);

        // Another person has their own ten.
        create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &who("sub-bob", &["analytics"], false),
            create_body("bobs-job", "analytics", "select 1"),
        )
        .await
        .expect("another user is unaffected");
    }

    #[tokio::test]
    async fn update_changes_the_editable_fields_only() {
        let mut original = nova_pipeline("smoke", "analytics");
        original["spec"]["params"] = json!({ "days": "1" });
        original["spec"]["engine"] = json!("airflow");
        let resources = memory_with(
            &[("smoke", original.clone())],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(true)),
            )],
        );
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        // The body carries the whole editable state: statement, schedule, params, options.
        let update = UpdatePipelineRequest {
            sql: "select 2;".into(),
            notebook: None,
            schedule: None,
            params: BTreeMap::from([
                ("days".to_string(), "7".to_string()),
                ("region".to_string(), "eu".to_string()),
            ]),
            options: Some(PipelineOptions {
                retries: Some(0),
                timeout_minutes: Some(30),
                ..PipelineOptions::default()
            }),
        };
        let view = update_pipeline(
            Some(&resources),
            None,
            None,
            &ada,
            &namespace,
            "smoke",
            update,
        )
        .await
        .expect("the update succeeds");
        assert_eq!(view.schedule, None);

        let stored = stored_pipeline(&resources, "smoke").await.expect("written");
        assert_eq!(stored["spec"]["tasks"][0]["sql"], "select 2");
        assert!(
            stored["spec"].get("schedule").is_none(),
            "an omitted schedule is removed: {stored}"
        );
        assert_eq!(
            stored["spec"]["params"],
            json!({ "days": "7", "region": "eu" }),
            "the parameter table is replaced, not merged"
        );
        assert_eq!(
            stored["spec"]["retries"],
            json!({ "attempts": 0, "backoff": "5m" })
        );
        assert_eq!(stored["spec"]["timeout"], json!("30m"));
        assert_eq!(stored["spec"]["concurrency"], json!("Forbid"));
        assert_eq!(
            stored["spec"]["engine"], "airflow",
            "every field the form does not edit stays"
        );
        assert_eq!(stored["spec"]["runAs"], "pipeline-analytics");
        assert_eq!(stored["metadata"]["resourceVersion"], "7");
        assert_eq!(
            stored["metadata"]["annotations"]["stardelt.io/created-by"],
            "sub-ada"
        );

        // A new schedule goes back in; the statement changes again.
        let view = update_pipeline(
            Some(&resources),
            None,
            None,
            &ada,
            &namespace,
            "smoke",
            UpdatePipelineRequest {
                sql: "select 3".into(),
                notebook: None,
                schedule: Some(ScheduleRequest {
                    cron: "30 6 * * mon-fri".into(),
                    timezone: "UTC".into(),
                }),
                params: BTreeMap::new(),
                options: None,
            },
        )
        .await
        .expect("the second update succeeds");
        assert_eq!(view.schedule.as_deref(), Some("30 6 * * mon-fri (UTC)"));
        let stored = stored_pipeline(&resources, "smoke").await.expect("written");
        assert_eq!(stored["spec"]["tasks"][0]["sql"], "select 3");
        assert_eq!(
            stored["spec"]["schedule"],
            json!({ "cron": "30 6 * * mon-fri", "timezone": "UTC" })
        );
        assert!(
            stored["spec"].get("params").is_none(),
            "an omitted table is removed: {stored}"
        );
        assert_eq!(stored["spec"]["engine"], "airflow");
    }

    #[tokio::test]
    async fn update_refuses_a_pipeline_not_created_by_nova() {
        let mut legacy = nova_pipeline("legacy", "analytics");
        legacy["metadata"]["labels"]
            .as_object_mut()
            .unwrap()
            .remove(MANAGED_BY_LABEL);
        let resources = memory_with(
            &[("legacy", legacy.clone())],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(true)),
            )],
        );
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        for caller in [&ada, &admin()] {
            let error = update_pipeline(
                Some(&resources),
                None,
                None,
                caller,
                &namespace,
                "legacy",
                update_body("select 2"),
            )
            .await
            .expect_err("kubectl's pipeline is not Nova's to rewrite");
            assert_eq!(error.status(), StatusCode::CONFLICT);
            assert_eq!(
                error.message(),
                "this pipeline is managed outside Nova (kubectl or Git); change it there"
            );
        }
        assert_eq!(
            stored_pipeline(&resources, "legacy")
                .await
                .expect("unchanged"),
            legacy
        );
    }

    #[tokio::test]
    async fn update_refuses_a_shape_it_cannot_edit() {
        let mut two_tasks = nova_pipeline("two-tasks", "analytics");
        two_tasks["spec"]["tasks"] = json!([
            { "name": "run", "kind": "sql", "sql": "select 1" },
            { "name": "check", "kind": "sql", "sql": "select 2" }
        ]);
        let mut other_name = nova_pipeline("other-name", "analytics");
        other_name["spec"]["tasks"] = json!([
            { "name": "load", "kind": "sql", "sql": "select 1" }
        ]);
        let resources = memory_with(
            &[("two-tasks", two_tasks), ("other-name", other_name)],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(true)),
            )],
        );
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        for name in ["two-tasks", "other-name"] {
            let error = update_pipeline(
                Some(&resources),
                None,
                None,
                &ada,
                &namespace,
                name,
                update_body("select 2"),
            )
            .await
            .expect_err("the form cannot edit this shape");
            assert_eq!(error.status(), StatusCode::CONFLICT, "{name}");
            assert_eq!(
                error.message(),
                "this pipeline has a shape the form cannot edit",
                "{name}"
            );
        }
        assert_eq!(
            stored_pipeline(&resources, "two-tasks").await.unwrap()["spec"]["tasks"]
                .as_array()
                .unwrap()
                .len(),
            2,
            "a refused update changes nothing"
        );
    }

    #[tokio::test]
    async fn update_conflict_on_a_changed_resource_version() {
        let original = nova_pipeline("smoke", "analytics");
        let resources = memory_with(
            &[("smoke", original.clone())],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(true)),
            )],
        );

        // A replace carrying the version read before somebody else wrote is a conflict.
        let mut stale = original.clone();
        stale["metadata"]["resourceVersion"] = json!("6");
        stale["spec"]["tasks"][0]["sql"] = json!("select 9");
        let error = resources
            .replace("smoke", stale)
            .await
            .expect_err("a stale resourceVersion is a conflict");
        assert!(matches!(error, ResourceError::Conflict));
        assert_eq!(
            stored_pipeline(&resources, "smoke").await.unwrap()["spec"]["tasks"][0]["sql"],
            "select 1",
            "the stale write changed nothing"
        );

        // A resource deleted between the read and the write is the same answer for the caller.
        assert!(matches!(
            resources.replace("absent", original).await,
            Err(ResourceError::Conflict)
        ));

        let fail = resource_write_fail(ResourceError::Conflict);
        assert_eq!(fail.status(), StatusCode::CONFLICT);
        assert_eq!(
            fail.message(),
            "the pipeline changed while you edited it: reload"
        );
    }

    #[tokio::test]
    async fn update_rechecks_the_principal() {
        let namespace = crate::cluster::namespace();
        let ada = who("sub-ada", &["analytics"], false);

        // The principal exists but stopped being ready.
        let resources = memory_with(
            &[("smoke", nova_pipeline("smoke", "analytics"))],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(false)),
            )],
        );
        let error = update_pipeline(
            Some(&resources),
            None,
            None,
            &ada,
            &namespace,
            "smoke",
            update_body("select 2"),
        )
        .await
        .expect_err("not ready any more");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), NOT_READY_MESSAGE);
        assert_eq!(
            stored_pipeline(&resources, "smoke").await.unwrap()["spec"]["tasks"][0]["sql"],
            "select 1"
        );

        // The principal is gone (an administrator deleted it).
        let resources = memory_with(&[("smoke", nova_pipeline("smoke", "analytics"))], &[]);
        let error = update_pipeline(
            Some(&resources),
            None,
            None,
            &ada,
            &namespace,
            "smoke",
            update_body("select 2"),
        )
        .await
        .expect_err("no principal");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.message(), missing_principal_sentence("analytics"));
    }

    #[tokio::test]
    async fn delete_removes_only_nova_managed_pipelines_the_caller_may_act_on() {
        let mut legacy = nova_pipeline("legacy", "analytics");
        legacy["metadata"]["labels"]
            .as_object_mut()
            .unwrap()
            .remove(MANAGED_BY_LABEL);
        let resources = memory_with(
            &[
                ("mine", nova_pipeline("mine", "analytics")),
                ("legacy", legacy),
                ("foreign", nova_pipeline("foreign", "finance")),
            ],
            &[],
        );
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        delete_pipeline(Some(&resources), &ada, &namespace, "mine")
            .await
            .expect("the owner group deletes its own Nova pipeline");
        assert!(stored_pipeline(&resources, "mine").await.is_none());
        let error = delete_pipeline(Some(&resources), &ada, &namespace, "mine")
            .await
            .expect_err("a deleted pipeline is gone");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);

        let error = delete_pipeline(Some(&resources), &ada, &namespace, "legacy")
            .await
            .expect_err("kubectl's pipeline is not Nova's to delete");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(
            error.message(),
            "this pipeline is managed outside Nova (kubectl or Git); change it there"
        );
        assert!(
            stored_pipeline(&resources, "legacy").await.is_some(),
            "a refused delete removes nothing"
        );

        let error = delete_pipeline(Some(&resources), &admin(), &namespace, "legacy")
            .await
            .expect_err("an administrator does not delete somebody's manifest either");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert!(stored_pipeline(&resources, "legacy").await.is_some());
    }

    #[tokio::test]
    async fn delete_of_a_hidden_pipeline_is_404() {
        let resources = memory_with(&[("foreign", nova_pipeline("foreign", "finance"))], &[]);
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let error = delete_pipeline(Some(&resources), &ada, &namespace, "foreign")
            .await
            .expect_err("another group's pipeline");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "no such pipeline");
        assert!(
            stored_pipeline(&resources, "foreign").await.is_some(),
            "a hidden pipeline is not deleted"
        );
    }

    #[tokio::test]
    async fn principals_route_reports_each_group() {
        let resources = memory_with(
            &[],
            &[
                (
                    "pipeline-analytics",
                    service_principal("pipeline-analytics", "analytics", Some(true)),
                ),
                (
                    "pipeline-finance",
                    service_principal("pipeline-finance", "finance", Some(false)),
                ),
            ],
        );
        let who = who("sub-ada", &["analytics", "finance", "ops"], false);

        let answer = principal_views(Some(&resources), &who)
            .await
            .expect("the principals are readable");
        assert_eq!(
            answer.groups,
            [
                PrincipalView {
                    group: "analytics".into(),
                    principal: Some("pipeline-analytics".into()),
                    ready: true,
                    message: None,
                },
                PrincipalView {
                    group: "finance".into(),
                    principal: Some("pipeline-finance".into()),
                    ready: false,
                    message: Some(NOT_READY_MESSAGE.into()),
                },
                PrincipalView {
                    group: "ops".into(),
                    principal: None,
                    ready: false,
                    message: Some(missing_principal_sentence("ops")),
                },
            ],
            "one entry per group, sorted, with the sentence of the ticket"
        );
    }

    #[tokio::test]
    async fn audit_has_lengths_not_text() {
        let resources = analytics_ready();
        let actor = who("sub-audit-210", &["analytics"], false);
        let namespace = crate::cluster::namespace();
        let limiter = create_limiter();
        const CANARY: &str = "select 'canary-7f3a'";
        let _ = audit::test_sink::take_for("sub-audit-210");

        create_pipeline(
            Some(&resources),
            None,
            &limiter,
            &actor,
            create_body("canary", "analytics", CANARY),
        )
        .await
        .expect("create");
        update_pipeline(
            Some(&resources),
            None,
            None,
            &actor,
            &namespace,
            "canary",
            update_body(CANARY),
        )
        .await
        .expect("update");
        delete_pipeline(Some(&resources), &actor, &namespace, "canary")
            .await
            .expect("delete");

        let records = audit::test_sink::take_for("sub-audit-210");
        assert_eq!(
            records
                .iter()
                .map(|record| record.action.as_str())
                .collect::<Vec<_>>(),
            ["create", "update", "delete"]
        );
        assert_eq!(
            records[0].statement_chars,
            Some(CANARY.chars().count()),
            "the create records the statement's length"
        );
        assert_eq!(records[0].schedule.as_deref(), Some("0 3 * * *"));
        assert_eq!(
            records[1].statement_chars,
            Some(CANARY.chars().count()),
            "the update records the statement's length"
        );
        assert_eq!(
            records[2].statement_chars, None,
            "a delete has no statement"
        );
        let serialized = format!("{records:?}");
        assert!(
            !serialized.contains("canary-7f3a"),
            "the statement is never audited: {serialized}"
        );
    }

    #[tokio::test]
    async fn cluster_down_is_503_and_responses_are_not_cacheable() {
        let (store, _store_fake) = crate::secret_store::fake::start().await;
        let (airflow, _airflow_fake) = fake_airflow::start().await;
        let namespace = crate::cluster::namespace();
        let app = router()
            .with_state(test_state(None, airflow, store))
            .layer(Extension(principal(admin())));

        let create = json!({
            "name": "nightly",
            "ownerGroup": "stardelt-admins",
            "sql": "select 1",
            "schedule": { "cron": "0 3 * * *", "timezone": "Europe/Berlin" }
        });
        let update = json!({ "sql": "select 2", "schedule": null });
        let routes: [(&str, String, Option<Value>); 4] = [
            ("POST", "/pipelines".to_string(), Some(create)),
            ("GET", "/pipelines/principals".to_string(), None),
            (
                "PUT",
                format!("/pipelines/{namespace}/nightly"),
                Some(update),
            ),
            ("DELETE", format!("/pipelines/{namespace}/nightly"), None),
        ];
        for (method, uri, body) in routes {
            let mut request = Request::builder().method(method).uri(&uri);
            let body = match body {
                Some(body) => {
                    request = request.header("content-type", "application/json");
                    Body::from(body.to_string())
                }
                None => Body::empty(),
            };
            let response = app
                .clone()
                .oneshot(request.body(body).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {uri}"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
                "{method} {uri} is never cached"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["error"], "no cluster connection", "{method} {uri}");
        }

        // The success statuses through the router and the real client (a fake API server).
        let kube = fake_kube::start_with(
            vec![],
            vec![service_principal(
                "pipeline-analytics",
                "analytics",
                Some(true),
            )],
        )
        .await;
        let (store, _store_fake) = crate::secret_store::fake::start().await;
        let (airflow, _airflow_fake) = fake_airflow::start().await;
        let app = router()
            .with_state(test_state(Some(kube), airflow, store))
            .layer(Extension(principal(who("sub-ada", &["analytics"], false))));

        // principals: 200 and the ready entry.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/pipelines/principals")
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
            Some("no-store")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["groups"][0]["principal"], "pipeline-analytics");
        assert_eq!(value["groups"][0]["ready"], true);

        // create: 201 with the new pipeline as `loading`.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/pipelines")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "nightly",
                            "ownerGroup": "analytics",
                            "sql": "select 1",
                            "schedule": { "cron": "0 3 * * *", "timezone": "Europe/Berlin" }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["name"], "nightly");
        assert_eq!(value["loaded"], false);
        assert_eq!(value["state"], "loading");

        // update: 200.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/pipelines/{namespace}/nightly"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "sql": "select 2", "schedule": null }).to_string(),
                    ))
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
            Some("no-store")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schedule"], Value::Null);

        // delete: 204 and no body.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/pipelines/{namespace}/nightly"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(bytes.is_empty(), "204 carries no body");
    }

    // -----------------------------------------------------------------------
    // Schedule options, the statement check and the next-runs preview (#223)
    // -----------------------------------------------------------------------

    /// A state whose engine is the fake, for the two routes that need no cluster.
    async fn check_state(trino_url: &str) -> Arc<AppState> {
        let (store, _store_fake) = crate::secret_store::fake::start().await;
        let (airflow, _airflow_fake) = fake_airflow::start().await;
        test_state_at(None, airflow, store, trino_url)
    }

    fn check_body(sql: &str) -> CheckStatementRequest {
        CheckStatementRequest {
            sql: sql.into(),
            params: BTreeMap::new(),
        }
    }

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[tokio::test]
    async fn params_and_options_written_to_the_resource() {
        let resources = analytics_ready();
        let mut body = create_body("daily-trips", "analytics", "select '${params.day}'");
        body.params = params(&[("day", "2026-10-01")]);
        body.options = Some(PipelineOptions {
            retries: Some(3),
            backoff_minutes: Some(10),
            timeout_minutes: Some(360),
            concurrency: Some("Allow".into()),
        });

        create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &who("sub-ada", &["analytics"], false),
            body,
        )
        .await
        .expect("the pipeline is created");

        let stored = stored_pipeline(&resources, "daily-trips")
            .await
            .expect("the resource is written");
        assert_eq!(
            stored["spec"]["params"],
            json!({ "day": "2026-10-01" }),
            "the parameter table is written as given: {stored}"
        );
        assert_eq!(
            stored["spec"]["retries"],
            json!({ "attempts": 3, "backoff": "10m" }),
            "the CRD's retries shape: {stored}"
        );
        assert_eq!(
            stored["spec"]["timeout"],
            json!("360m"),
            "the loader's duration grammar: {stored}"
        );
        assert_eq!(stored["spec"]["concurrency"], json!("Allow"));
        assert_eq!(
            stored["spec"]["tasks"][0]["sql"], "select '${params.day}'",
            "the statement is stored unexpanded: the loader fills it when the run starts"
        );
    }

    #[tokio::test]
    async fn defaults_when_omitted() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);

        create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &ada,
            create_body("plain", "analytics", "select 1"),
        )
        .await
        .expect("a create without params and options is fine");

        let stored = stored_pipeline(&resources, "plain").await.expect("written");
        assert_eq!(
            stored["spec"]["retries"],
            json!({ "attempts": 1, "backoff": "5m" })
        );
        assert_eq!(stored["spec"]["timeout"], json!("120m"));
        assert_eq!(stored["spec"]["concurrency"], json!("Forbid"));
        assert!(
            stored["spec"].get("params").is_none(),
            "no parameter table is written when none is given: {stored}"
        );

        // The update body describes the whole editable state: an omitted option is the default,
        // never "keep whatever was there".
        let mut update = update_body("select 2");
        update.options = Some(PipelineOptions {
            retries: Some(4),
            ..PipelineOptions::default()
        });
        update_pipeline(
            Some(&resources),
            None,
            None,
            &ada,
            &crate::cluster::namespace(),
            "plain",
            update,
        )
        .await
        .expect("the update succeeds");
        let stored = stored_pipeline(&resources, "plain").await.expect("written");
        assert_eq!(
            stored["spec"]["retries"],
            json!({ "attempts": 4, "backoff": "5m" })
        );
        assert_eq!(stored["spec"]["timeout"], json!("120m"));
        assert_eq!(stored["spec"]["concurrency"], json!("Forbid"));
    }

    #[tokio::test]
    async fn option_ranges_refused_with_sentences() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();
        let too_many: BTreeMap<String, String> = (0..17)
            .map(|index| (format!("p{index}"), "1".to_string()))
            .collect();
        let long_value = "x".repeat(MAX_PARAM_VALUE_CHARS + 1);

        let cases: Vec<(BTreeMap<String, String>, PipelineOptions, &str)> = vec![
            (
                BTreeMap::new(),
                PipelineOptions {
                    retries: Some(-1),
                    ..PipelineOptions::default()
                },
                RETRIES_RANGE,
            ),
            (
                BTreeMap::new(),
                PipelineOptions {
                    retries: Some(6),
                    ..PipelineOptions::default()
                },
                RETRIES_RANGE,
            ),
            (
                BTreeMap::new(),
                PipelineOptions {
                    backoff_minutes: Some(0),
                    ..PipelineOptions::default()
                },
                BACKOFF_RANGE,
            ),
            (
                BTreeMap::new(),
                PipelineOptions {
                    backoff_minutes: Some(61),
                    ..PipelineOptions::default()
                },
                BACKOFF_RANGE,
            ),
            (
                BTreeMap::new(),
                PipelineOptions {
                    timeout_minutes: Some(0),
                    ..PipelineOptions::default()
                },
                TIMEOUT_RANGE,
            ),
            (
                BTreeMap::new(),
                PipelineOptions {
                    timeout_minutes: Some(721),
                    ..PipelineOptions::default()
                },
                TIMEOUT_RANGE,
            ),
            (
                BTreeMap::new(),
                PipelineOptions {
                    concurrency: Some("Replace".into()),
                    ..PipelineOptions::default()
                },
                CONCURRENCY_RULE,
            ),
            (
                params(&[("Day", "1")]),
                PipelineOptions::default(),
                PARAM_NAMES,
            ),
            (
                params(&[("day", &long_value)]),
                PipelineOptions::default(),
                PARAM_VALUE_TOO_LONG,
            ),
            (
                params(&[("day", "a\0b")]),
                PipelineOptions::default(),
                PARAM_VALUE_NUL,
            ),
            (too_many, PipelineOptions::default(), TOO_MANY_PARAMS),
        ];

        for (index, (parameters, options, sentence)) in cases.into_iter().enumerate() {
            let mut body = create_body(&format!("refused-{index}"), "analytics", "select 1");
            body.params = parameters;
            body.options = Some(options);
            let error = create_pipeline(Some(&resources), None, &limiter, &ada, body)
                .await
                .expect_err("the option is out of range");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{sentence}");
            assert_eq!(error.message(), sentence);
        }
        assert!(
            resources.list().await.unwrap().is_empty(),
            "a refused option writes nothing"
        );
    }

    #[tokio::test]
    async fn unknown_placeholder_refused() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();

        for (sql, params, sentence) in [
            (
                "select '${params.missing}'",
                params(&[("day", "1")]),
                "unknown placeholder ${params.missing}",
            ),
            (
                "select '${run.when}'",
                BTreeMap::new(),
                "unknown placeholder ${run.when}",
            ),
        ] {
            let mut body = create_body("typo", "analytics", sql);
            body.params = params;
            let error = create_pipeline(Some(&resources), None, &limiter, &ada, body)
                .await
                .expect_err("the loader would fail this run");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{sql}");
            assert_eq!(error.message(), sentence, "{sql}");
        }
        assert!(resources.list().await.unwrap().is_empty());

        // The three the loader fills pass, and so does a `${…}` its regex does not match (it
        // leaves such text in the statement, so Nova must not refuse it).
        let statement = "select '${run.date}', '${run.id}', '${params.day}', '${a-b}'";
        let mut body = create_body("defined", "analytics", statement);
        body.params = params(&[("day", "2026-10-01")]);
        create_pipeline(Some(&resources), None, &limiter, &ada, body)
            .await
            .expect("every placeholder is one the loader fills");
        let stored = stored_pipeline(&resources, "defined")
            .await
            .expect("written");
        assert_eq!(stored["spec"]["tasks"][0]["sql"], statement);
    }

    #[tokio::test]
    async fn detail_returns_params_and_options() {
        let mut smoke = resource("smoke", Some("analytics"));
        smoke["spec"]["params"] = json!({ "day": "2026-10-01" });
        smoke["spec"]["retries"] = json!({ "attempts": 3, "backoff": "30m" });
        smoke["spec"]["timeout"] = json!("6h");
        smoke["spec"]["concurrency"] = json!("Allow");
        let resources = memory(&[("smoke", smoke)]);
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
            detail.params.get("day").map(String::as_str),
            Some("2026-10-01")
        );
        assert_eq!(
            detail.options,
            PipelineOptionsView {
                retries: 3,
                backoff_minutes: 30,
                timeout_minutes: 360,
                concurrency: "Allow".into(),
            }
        );

        // A resource without the fields answers the defaults, so the dialog always has values.
        let resources = memory(&[("smoke", resource("smoke", Some("analytics")))]);
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
            detail.options,
            PipelineOptionsView {
                retries: 2,
                backoff_minutes: 5,
                // `spec.timeout` is absent in the fixture: the default two hours.
                timeout_minutes: 120,
                concurrency: "Forbid".into(),
            }
        );
    }

    #[tokio::test]
    async fn check_sends_explain_never_analyze() {
        let (trino, fake) = fake_trino::start(None).await;
        let state = check_state(&trino).await;
        let ada = principal(who("sub-ada", &["analytics"], false));

        let answer = check_statement(&state, &ada, check_body("select 1"))
            .await
            .expect("the engine accepts the statement");
        assert_eq!(
            answer,
            CheckStatementResponse {
                checked: true,
                ok: Some(true),
                message: None,
            }
        );

        let fake = fake.lock().expect("fake trino");
        assert_eq!(fake.statements, vec!["EXPLAIN select 1".to_string()]);
        assert_eq!(fake.sources, vec![SOURCE_SCHEDULE_CHECK.to_string()]);
        assert_eq!(
            fake.users,
            vec!["sub-ada".to_string()],
            "the check runs as the caller, so the engine's query log names them"
        );
        assert!(
            !fake.statements[0].to_ascii_uppercase().contains("ANALYZE"),
            "never EXPLAIN ANALYZE: it would run the statement"
        );
    }

    #[tokio::test]
    async fn check_fills_placeholders() {
        let (trino, fake) = fake_trino::start(None).await;
        let state = check_state(&trino).await;
        let ada = principal(who("sub-ada", &["analytics"], false));

        let statement = "select '${run.date}' as d, '${run.id}' as i, '${params.day}' as p";
        let mut body = check_body(statement);
        body.params = params(&[("day", "2026-01-01")]);
        check_statement(&state, &ada, body)
            .await
            .expect("the placeholders are filled");

        let today = now_utc().format("%Y-%m-%d").to_string();
        let fake = fake.lock().expect("fake trino");
        assert_eq!(
            fake.statements,
            vec![format!(
                "EXPLAIN select '{today}' as d, 'check' as i, '2026-01-01' as p"
            )],
            "run.date is today (UTC), run.id is `check`, params as given"
        );
        assert!(
            !fake.statements[0].contains("${"),
            "no placeholder reaches the engine: {}",
            fake.statements[0]
        );
    }

    #[tokio::test]
    async fn check_skips_unsupported_kinds() {
        let (trino, fake) = fake_trino::start(None).await;
        let state = check_state(&trino).await;
        let ada = principal(who("sub-ada", &["analytics"], false));

        for sql in [
            "drop table x",
            "selec 1",
            "show tables",
            "create table t (a int)",
            "grant select on t to public",
        ] {
            let answer = check_statement(&state, &ada, check_body(sql))
                .await
                .unwrap_or_else(|error| panic!("{sql}: {}", error.message()));
            assert_eq!(
                answer,
                CheckStatementResponse {
                    checked: false,
                    ok: None,
                    message: None,
                },
                "{sql} cannot be checked here"
            );
        }
        assert!(
            fake.lock().expect("fake trino").statements.is_empty(),
            "a kind of statement Nova cannot check never reaches the engine"
        );

        // `CREATE … AS …` has a plan, so it is checked.
        let answer = check_statement(&state, &ada, check_body("CREATE TABLE t AS SELECT 1"))
            .await
            .expect("a create-from-query is checkable");
        assert_eq!(answer.ok, Some(true));
        assert_eq!(
            fake.lock().expect("fake trino").statements,
            vec!["EXPLAIN CREATE TABLE t AS SELECT 1".to_string()]
        );
    }

    #[tokio::test]
    async fn check_returns_engine_message_on_error() {
        let error = json!({
            "errorName": "SYNTAX_ERROR",
            "message": "line 1:1: mismatched input 'selec'. Expecting: 'ALTER', 'ANALYZE', ...",
        });
        let (trino, _fake) = fake_trino::start(Some(error)).await;
        let state = check_state(&trino).await;
        let ada = principal(who("sub-ada", &["analytics"], false));

        let answer = check_statement(&state, &ada, check_body("select 1"))
            .await
            .expect("a refused statement is an answer, not a failure");
        assert!(answer.checked);
        assert_eq!(answer.ok, Some(false));
        assert_eq!(
            answer.message.as_deref(),
            Some(
                "SYNTAX_ERROR: line 1:1: mismatched input 'selec'. Expecting: 'ALTER', 'ANALYZE', ..."
            ),
            "the engine's own sentence reaches the dialog"
        );
        assert!(
            !answer
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("failureInfo"),
            "no stack trace in an answer"
        );

        // An engine that does not answer is 502 (U12), not "the engine said no".
        let state = check_state("http://127.0.0.1:1").await;
        let error = check_statement(&state, &ada, check_body("select 1"))
            .await
            .expect_err("the engine is unreachable");
        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(error.message(), crate::trino::ENGINE_UNREACHABLE);
    }

    #[test]
    fn preview_next_runs_table() {
        let now = "2026-10-20T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("a fixed clock");
        let table: [(&str, [&str; 5]); 4] = [
            // Europe/Berlin: the clocks go back on 25 October, so the fifth run is CET.
            (
                "0 3 * * *",
                [
                    "2026-10-21T03:00:00+02:00",
                    "2026-10-22T03:00:00+02:00",
                    "2026-10-23T03:00:00+02:00",
                    "2026-10-24T03:00:00+02:00",
                    "2026-10-25T03:00:00+01:00",
                ],
            ),
            (
                "*/15 * * * *",
                [
                    "2026-10-20T14:15:00+02:00",
                    "2026-10-20T14:30:00+02:00",
                    "2026-10-20T14:45:00+02:00",
                    "2026-10-20T15:00:00+02:00",
                    "2026-10-20T15:15:00+02:00",
                ],
            ),
            (
                "0 9 * * 1-5",
                [
                    "2026-10-21T09:00:00+02:00",
                    "2026-10-22T09:00:00+02:00",
                    "2026-10-23T09:00:00+02:00",
                    "2026-10-26T09:00:00+01:00",
                    "2026-10-27T09:00:00+01:00",
                ],
            ),
            (
                "30 6 1 * *",
                [
                    "2026-11-01T06:30:00+01:00",
                    "2026-12-01T06:30:00+01:00",
                    "2027-01-01T06:30:00+01:00",
                    "2027-02-01T06:30:00+01:00",
                    "2027-03-01T06:30:00+01:00",
                ],
            ),
        ];

        for (cron, expected) in table {
            let answer = preview_schedule(
                PreviewScheduleRequest {
                    cron: cron.into(),
                    timezone: "Europe/Berlin".into(),
                    count: None,
                },
                now,
            )
            .unwrap_or_else(|error| panic!("{cron}: {}", error.message()));
            assert_eq!(
                answer.runs,
                expected
                    .iter()
                    .map(|run| (*run).to_string())
                    .collect::<Vec<_>>(),
                "{cron}"
            );
        }

        // 02:30 does not exist on 29 March 2026 (the clocks jump 02:00 → 03:00): it is skipped.
        let before_the_jump = "2026-03-28T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("a fixed clock");
        let answer = preview_schedule(
            PreviewScheduleRequest {
                cron: "30 2 * * *".into(),
                timezone: "Europe/Berlin".into(),
                count: Some(3),
            },
            before_the_jump,
        )
        .expect("a valid cron");
        assert_eq!(
            answer.runs,
            vec![
                "2026-03-30T02:30:00+02:00".to_string(),
                "2026-03-31T02:30:00+02:00".to_string(),
                "2026-04-01T02:30:00+02:00".to_string(),
            ]
        );

        // Both day fields restricted: a day matches when either does (croniter's rule).
        let answer = preview_schedule(
            PreviewScheduleRequest {
                cron: "0 4 1 * 1".into(),
                timezone: "Europe/Berlin".into(),
                count: Some(4),
            },
            now,
        )
        .expect("a valid cron");
        assert_eq!(
            answer.runs,
            vec![
                "2026-10-26T04:00:00+01:00".to_string(),
                "2026-11-01T04:00:00+01:00".to_string(),
                "2026-11-02T04:00:00+01:00".to_string(),
                "2026-11-09T04:00:00+01:00".to_string(),
            ]
        );

        // A cron that can never fire answers no runs (at most 366 days are searched), not an error.
        let answer = preview_schedule(
            PreviewScheduleRequest {
                cron: "0 0 31 2 *".into(),
                timezone: "Europe/Berlin".into(),
                count: Some(5),
            },
            now,
        )
        .expect("the cron itself is valid");
        assert!(answer.runs.is_empty(), "31 February never comes");
    }

    #[test]
    fn preview_rejects_what_create_rejects() {
        let now = "2026-10-20T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("a fixed clock");

        for (cron, timezone, sentence) in [
            ("* * * * *", "UTC", CRON_TOO_OFTEN),
            ("0 3 * *", "UTC", CRON_FIELDS),
            ("0 25 * * *", "UTC", CRON_RANGE),
            ("0 3 * * *", "Mars/Olympus", "unknown timezone"),
        ] {
            let error = preview_schedule(
                PreviewScheduleRequest {
                    cron: cron.into(),
                    timezone: timezone.into(),
                    count: None,
                },
                now,
            )
            .unwrap_err();
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{cron}",);
            assert_eq!(error.message(), sentence, "{cron}");
        }

        let error = preview_schedule(
            PreviewScheduleRequest {
                cron: "0 3 * * *".into(),
                timezone: "UTC".into(),
                count: Some(11),
            },
            now,
        )
        .unwrap_err();
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), COUNT_RANGE);

        let answer = preview_schedule(
            PreviewScheduleRequest {
                cron: "0 3 * * *".into(),
                timezone: "UTC".into(),
                count: Some(1),
            },
            now,
        )
        .expect("count 1 is fine");
        assert_eq!(answer.runs.len(), 1);
    }

    #[tokio::test]
    async fn check_and_preview_need_a_signed_in_person() {
        let (store, _store_fake) = crate::secret_store::fake::start().await;
        let (airflow, _airflow_fake) = fake_airflow::start().await;
        let (trino, fake) = fake_trino::start(None).await;
        let app = router().with_state(test_state_at(None, airflow, store, &trino));

        for (uri, body) in [
            ("/pipelines/check", json!({ "sql": "select 1" })),
            (
                "/pipelines/preview-schedule",
                json!({ "cron": "0 3 * * *", "timezone": "Europe/Berlin" }),
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
                "a refusal is never cached either: {uri}"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["error"], "not signed in", "{uri}");
        }
        assert!(
            fake.lock().expect("fake trino").statements.is_empty(),
            "an anonymous caller never reaches the engine"
        );
    }

    // -----------------------------------------------------------------------
    // Notebook schedules (#237)
    // -----------------------------------------------------------------------

    /// A create body that schedules `notebook` at `version` for `group`, no statement.
    fn notebook_body(
        name: &str,
        group: &str,
        notebook: &str,
        version: &str,
    ) -> CreatePipelineRequest {
        CreatePipelineRequest {
            name: name.into(),
            owner_group: group.into(),
            sql: String::new(),
            notebook: Some(NotebookRequest {
                name: notebook.into(),
                object_version: version.into(),
                params: BTreeMap::new(),
            }),
            schedule: Some(ScheduleRequest {
                cron: "0 3 * * *".into(),
                timezone: "Europe/Berlin".into(),
            }),
            params: BTreeMap::new(),
            options: None,
        }
    }

    fn update_notebook_body(notebook: &str, version: &str) -> UpdatePipelineRequest {
        UpdatePipelineRequest {
            sql: String::new(),
            notebook: Some(NotebookRequest {
                name: notebook.into(),
                object_version: version.into(),
                params: BTreeMap::new(),
            }),
            schedule: None,
            params: BTreeMap::new(),
            options: None,
        }
    }

    /// A notebook pipeline Nova wrote, as a `head_version` needs nothing: the resource only.
    fn stored_notebook_pipeline(name: &str, group: &str, notebook: &str, version: &str) -> Value {
        let mut resource = pipeline_resource(
            name,
            group,
            &Statement::Notebook {
                name: notebook.into(),
                object_version: version.into(),
                params: "{}".into(),
            },
            Some(&("0 3 * * *".to_string(), "Europe/Berlin".to_string())),
            &BTreeMap::new(),
            &default_options(),
            "sub-ada",
        );
        resource["metadata"]["resourceVersion"] = json!("7");
        resource
    }

    #[tokio::test]
    async fn notebook_request_builds_the_notebook_task() {
        let resources = analytics_ready();
        let s3 = FakeS3::start().await;
        let version = s3.put_raw("analytics/report.ipynb", b"{\"nbformat\":4}", "ada");
        let store = notebook_store(&s3.base);
        let ada = who("sub-ada", &["analytics"], false);

        let mut body = notebook_body("nightly-report", "analytics", "report.ipynb", &version);
        // Deliberately not sorted: the task's `params` must be, whatever the body's order.
        body.notebook.as_mut().expect("notebook").params = BTreeMap::from([
            ("region".to_string(), "eu".to_string()),
            ("days".to_string(), "7".to_string()),
        ]);

        let view = create_pipeline(
            Some(&resources),
            Some(&store),
            &create_limiter(),
            &ada,
            body,
        )
        .await
        .expect("a notebook schedule");

        assert!(!view.loaded, "the loader has not seen it yet");
        assert_eq!(view.state, "loading");
        assert_eq!(view.tasks.len(), 1);
        assert_eq!(view.tasks[0].kind, "notebook");
        assert_eq!(view.tasks[0].sql, None);

        let stored = stored_pipeline(&resources, "nightly-report")
            .await
            .expect("written");
        assert_eq!(
            stored["spec"],
            json!({
                "ownerGroup": "analytics",
                "runAs": "pipeline-analytics",
                "tasks": [{
                    "name": "run",
                    "kind": "notebook",
                    "config": {
                        "notebook": "report.ipynb",
                        "objectVersion": version,
                        "params": "{\"days\":\"7\",\"region\":\"eu\"}",
                    },
                }],
                "concurrency": "Forbid",
                "retries": { "attempts": 1, "backoff": "5m" },
                "timeout": "120m",
                "schedule": { "cron": "0 3 * * *", "timezone": "Europe/Berlin" },
            }),
            "the notebook task exactly: one config object, params as a sorted JSON string, no sql"
        );
        assert!(
            stored["spec"]["tasks"][0].get("sql").is_none(),
            "a notebook task carries no statement: {stored}"
        );
        assert!(
            stored["spec"].get("params").is_none(),
            "the notebook's values live in config.params, not in spec.params: {stored}"
        );
        assert_eq!(
            stored["metadata"]["labels"],
            json!({
                MANAGED_BY_LABEL: MANAGED_BY_NOVA,
                "stardelt.io/owner-group": "analytics",
            }),
            "the label set stays what it was: no kind label is added"
        );
        assert_eq!(
            stored["metadata"]["annotations"]["stardelt.io/created-by"],
            "sub-ada"
        );
    }

    #[tokio::test]
    async fn exactly_one_of_sql_and_notebook() {
        const SENTENCE: &str = "give a statement or a notebook";
        let resources = analytics_ready();
        let s3 = FakeS3::start().await;
        let version = s3.put_raw("analytics/report.ipynb", b"{}", "ada");
        let store = notebook_store(&s3.base);
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();

        // Neither: no statement and no notebook.
        let mut body = notebook_body("either", "analytics", "report.ipynb", &version);
        body.notebook = None;
        let error = create_pipeline(Some(&resources), Some(&store), &limiter, &ada, body)
            .await
            .expect_err("neither");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), SENTENCE);

        // Both: the body would have to choose.
        let mut body = notebook_body("either", "analytics", "report.ipynb", &version);
        body.sql = "select 1".into();
        let error = create_pipeline(Some(&resources), Some(&store), &limiter, &ada, body)
            .await
            .expect_err("both");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), SENTENCE);

        // An update says the same: it changes one or the other, never both.
        let resources_with_pipeline = memory_with(
            &[(
                "nightly",
                stored_notebook_pipeline("nightly", "analytics", "report.ipynb", &version),
            )],
            &[(
                "pipeline-analytics",
                service_principal("pipeline-analytics", "analytics", Some(true)),
            )],
        );
        let mut update = update_notebook_body("report.ipynb", &version);
        update.sql = "select 1".into();
        let error = update_pipeline(
            Some(&resources_with_pipeline),
            Some(&store),
            None,
            &ada,
            &crate::cluster::namespace(),
            "nightly",
            update,
        )
        .await
        .expect_err("an update gives one of them too");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), SENTENCE);

        // A blank statement is no statement: the notebook is what runs.
        let mut body = notebook_body("blank-sql", "analytics", "report.ipynb", &version);
        body.sql = "  \n\t ".into();
        create_pipeline(Some(&resources), Some(&store), &limiter, &ada, body)
            .await
            .expect("a blank statement next to a notebook is not a second one");
        let stored = stored_pipeline(&resources, "blank-sql")
            .await
            .expect("written");
        assert_eq!(stored["spec"]["tasks"][0]["kind"], "notebook");
    }

    /// One refused notebook body, with the sentence the ticket names.
    async fn create_refused(
        resources: &PipelineResources,
        store: &NotebookStore,
        limiter: &CreateLimiter,
        who: &Identity,
        notebook: NotebookRequest,
        expected: &str,
    ) {
        let printed = format!("{notebook:?}");
        let mut body = notebook_body("smoke", "analytics", "report.ipynb", "v1");
        body.notebook = Some(notebook);
        let error = create_pipeline(Some(resources), Some(store), limiter, who, body)
            .await
            .expect_err(&printed);
        assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{expected}");
        assert_eq!(error.message(), expected);
    }

    #[tokio::test]
    async fn bad_names_and_params_refused_with_sentences() {
        let resources = analytics_ready();
        let s3 = FakeS3::start().await;
        let version = s3.put_raw("analytics/report.ipynb", b"{}", "ada");
        let store = notebook_store(&s3.base);
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();

        for name in [
            "report",
            "report.txt",
            "-report.ipynb",
            "re/port.ipynb",
            ".ipynb",
        ] {
            create_refused(
                &resources,
                &store,
                &limiter,
                &ada,
                NotebookRequest {
                    name: name.into(),
                    object_version: version.clone(),
                    params: BTreeMap::new(),
                },
                NOTEBOOK_NAME_RULE,
            )
            .await;
        }
        create_refused(
            &resources,
            &store,
            &limiter,
            &ada,
            NotebookRequest {
                name: format!("{}.ipynb", "a".repeat(129)),
                object_version: version.clone(),
                params: BTreeMap::new(),
            },
            NOTEBOOK_NAME_RULE,
        )
        .await;

        for name in ["re-gion", "re.gion", "", "1region", "reg ion", "region!"] {
            create_refused(
                &resources,
                &store,
                &limiter,
                &ada,
                NotebookRequest {
                    name: "report.ipynb".into(),
                    object_version: version.clone(),
                    params: BTreeMap::from([(name.to_string(), "eu".to_string())]),
                },
                NOTEBOOK_PARAM_NAMES,
            )
            .await;
        }

        let too_many: BTreeMap<String, String> = (0..=MAX_NOTEBOOK_PARAMS)
            .map(|i| (format!("p{i}"), "1".to_string()))
            .collect();
        create_refused(
            &resources,
            &store,
            &limiter,
            &ada,
            NotebookRequest {
                name: "report.ipynb".into(),
                object_version: version.clone(),
                params: too_many,
            },
            NOTEBOOK_TOO_MANY_PARAMS,
        )
        .await;

        create_refused(
            &resources,
            &store,
            &limiter,
            &ada,
            NotebookRequest {
                name: "report.ipynb".into(),
                object_version: version.clone(),
                params: BTreeMap::from([(
                    "region".to_string(),
                    "x".repeat(MAX_NOTEBOOK_PARAM_VALUE_CHARS + 1),
                )]),
            },
            NOTEBOOK_PARAM_VALUE,
        )
        .await;
        create_refused(
            &resources,
            &store,
            &limiter,
            &ada,
            NotebookRequest {
                name: "report.ipynb".into(),
                object_version: version.clone(),
                params: BTreeMap::from([("region".to_string(), "eu\0us".to_string())]),
            },
            NOTEBOOK_PARAM_VALUE,
        )
        .await;

        // Every value fits, the JSON text does not: 32 × 200 characters is over 4096.
        let long: BTreeMap<String, String> = (0..MAX_NOTEBOOK_PARAMS)
            .map(|i| (format!("p{i}"), "y".repeat(200)))
            .collect();
        assert!(
            serde_json::to_string(&long).expect("json").chars().count()
                > MAX_NOTEBOOK_PARAMS_JSON_CHARS
        );
        create_refused(
            &resources,
            &store,
            &limiter,
            &ada,
            NotebookRequest {
                name: "report.ipynb".into(),
                object_version: version.clone(),
                params: long,
            },
            NOTEBOOK_PARAMS_TOO_LONG,
        )
        .await;

        assert!(
            stored_pipeline(&resources, "smoke").await.is_none(),
            "a refused body writes nothing"
        );
    }

    #[tokio::test]
    async fn missing_version_refused_400() {
        let resources = analytics_ready();
        let s3 = FakeS3::start().await;
        let version = s3.put_raw("analytics/report.ipynb", b"{}", "ada");
        let store = notebook_store(&s3.base);
        let ada = who("sub-ada", &["analytics"], false);
        let limiter = create_limiter();
        let sentence = "that notebook version does not exist in group analytics";

        // A deleted (or never published) version of a notebook that does exist.
        for pinned in ["v99".to_string(), "v1 ".to_string(), "v".repeat(129)] {
            let body = notebook_body("smoke", "analytics", "report.ipynb", &pinned);
            let error = create_pipeline(Some(&resources), Some(&store), &limiter, &ada, body)
                .await
                .expect_err("no such version");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{pinned:?}");
            assert_eq!(error.message(), sentence, "{pinned:?}");
        }

        // A notebook nobody published.
        let body = notebook_body("smoke", "analytics", "missing.ipynb", &version);
        let error = create_pipeline(Some(&resources), Some(&store), &limiter, &ada, body)
            .await
            .expect_err("no such notebook");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), sentence);

        assert!(
            stored_pipeline(&resources, "smoke").await.is_none(),
            "nothing is pinned to bytes that do not exist"
        );
        assert!(!s3.is_empty());
    }

    #[tokio::test]
    async fn s3_unconfigured_503() {
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        // No `NOVA_S3_*`: Nova cannot prove the version exists, so it schedules nothing.
        let body = notebook_body("smoke", "analytics", "report.ipynb", "v1");
        let error = create_pipeline(Some(&resources), None, &create_limiter(), &ada, body)
            .await
            .expect_err("no store");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.message(), "notebook storage is not configured");
        assert!(stored_pipeline(&resources, "smoke").await.is_none());

        // A statement pipeline needs no store at all: nothing changed for it.
        create_pipeline(
            Some(&resources),
            None,
            &create_limiter(),
            &ada,
            create_body("statement", "analytics", "select 1"),
        )
        .await
        .expect("a statement schedule still works without S3");
    }

    #[tokio::test]
    async fn foreign_group_403() {
        let resources = analytics_ready();
        let s3 = FakeS3::start().await;
        let version = s3.put_raw("finance/budget.ipynb", b"{}", "fin");
        let store = notebook_store(&s3.base);
        let ada = who("sub-ada", &["analytics"], false);

        // The owner group must be one of the caller's own — an administrator included.
        for caller in [&ada, &admin()] {
            let body = notebook_body("their-budget", "finance", "budget.ipynb", &version);
            let error = create_pipeline(
                Some(&resources),
                Some(&store),
                &create_limiter(),
                caller,
                body,
            )
            .await
            .expect_err("another group's notebook");
            assert_eq!(error.status(), StatusCode::FORBIDDEN);
            assert_eq!(error.message(), "you are not a member of the group finance");
        }
        assert!(stored_pipeline(&resources, "their-budget").await.is_none());

        // A notebook pipeline of another group is hidden: 404, exactly like an absent one,
        // so a schedule never tells an outsider that the notebook exists.
        let resources = memory_with(
            &[(
                "budget",
                stored_notebook_pipeline("budget", "finance", "budget.ipynb", &version),
            )],
            &[],
        );
        let error = update_pipeline(
            Some(&resources),
            Some(&store),
            None,
            &ada,
            &crate::cluster::namespace(),
            "budget",
            update_notebook_body("budget.ipynb", &version),
        )
        .await
        .expect_err("hidden");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "no such pipeline");
    }

    #[tokio::test]
    async fn update_changes_version_and_params_not_kind() {
        let s3 = FakeS3::start().await;
        let first = s3.put_raw("analytics/report.ipynb", b"{\"v\":1}", "ada");
        let second = s3.put_raw("analytics/report.ipynb", b"{\"v\":2}", "ada");
        let store = notebook_store(&s3.base);
        let resources = analytics_ready();
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let mut body = notebook_body("nightly-report", "analytics", "report.ipynb", &first);
        body.notebook.as_mut().expect("notebook").params =
            BTreeMap::from([("days".to_string(), "1".to_string())]);
        create_pipeline(
            Some(&resources),
            Some(&store),
            &create_limiter(),
            &ada,
            body,
        )
        .await
        .expect("create");

        // The version and the parameters change; the notebook and the kind stay.
        let mut update = update_notebook_body("report.ipynb", &second);
        update.notebook.as_mut().expect("notebook").params = BTreeMap::from([
            ("region".to_string(), "eu".to_string()),
            ("days".to_string(), "7".to_string()),
        ]);
        update.schedule = Some(ScheduleRequest {
            cron: "30 6 * * mon-fri".into(),
            timezone: "UTC".into(),
        });
        let view = update_pipeline(
            Some(&resources),
            Some(&store),
            None,
            &ada,
            &namespace,
            "nightly-report",
            update,
        )
        .await
        .expect("the update succeeds");
        assert_eq!(view.schedule.as_deref(), Some("30 6 * * mon-fri (UTC)"));

        let stored = stored_pipeline(&resources, "nightly-report")
            .await
            .expect("written");
        assert_eq!(
            stored["spec"]["tasks"][0],
            json!({
                "name": "run",
                "kind": "notebook",
                "config": {
                    "notebook": "report.ipynb",
                    "objectVersion": second,
                    "params": "{\"days\":\"7\",\"region\":\"eu\"}",
                },
            }),
            "the whole notebook state is written, and the kind does not change"
        );
        assert!(
            stored["spec"]["tasks"][0].get("sql").is_none(),
            "a notebook pipeline never grows a statement: {stored}"
        );

        // `sql` on a notebook pipeline: the schema would have to change, so it is refused.
        let error = update_pipeline(
            Some(&resources),
            Some(&store),
            None,
            &ada,
            &namespace,
            "nightly-report",
            update_body("select 2"),
        )
        .await
        .expect_err("a notebook pipeline runs a notebook");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), NOTEBOOK_PIPELINE);
        assert_eq!(
            stored_pipeline(&resources, "nightly-report")
                .await
                .expect("unchanged")["spec"]["tasks"][0]["config"]["objectVersion"],
            second,
            "a refused update changes nothing"
        );

        // And a statement pipeline does not turn into a notebook one.
        let mut statement = create_body("nightly-sql", "analytics", "select 1");
        statement.schedule = None;
        create_pipeline(
            Some(&resources),
            Some(&store),
            &create_limiter(),
            &ada,
            statement,
        )
        .await
        .expect("create a statement pipeline");
        let error = update_pipeline(
            Some(&resources),
            Some(&store),
            None,
            &ada,
            &namespace,
            "nightly-sql",
            update_notebook_body("report.ipynb", &second),
        )
        .await
        .expect_err("a statement pipeline runs a statement");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), STATEMENT_PIPELINE);
        assert_eq!(
            stored_pipeline(&resources, "nightly-sql")
                .await
                .expect("unchanged")["spec"]["tasks"][0]["sql"],
            "select 1"
        );
    }

    #[tokio::test]
    async fn detail_returns_notebook_fields() {
        let version = "v-7f3a";
        let resources = memory_with(
            &[(
                "nightly",
                stored_notebook_pipeline("nightly", "analytics", "report.ipynb", version),
            )],
            &[],
        );
        let (airflow, _fake) = fake_airflow::start().await;
        let ada = who("sub-ada", &["analytics"], false);
        let namespace = crate::cluster::namespace();

        let detail = pipeline_detail(
            Some(&resources),
            Some(&airflow),
            &ada,
            &namespace,
            "nightly",
        )
        .await
        .expect("the detail is readable");
        assert_eq!(
            detail.pipeline.tasks,
            vec![TaskSpecView {
                name: "run".into(),
                kind: "notebook".into(),
                sql: None,
                notebook: Some("report.ipynb".into()),
                object_version: Some(version.into()),
                notebook_params: Some(BTreeMap::new()),
            }],
            "the edit dialog gets the version and the parameters it must prefill"
        );

        // The list stays small: the fields are the detail's, like `sql` since #218.
        let list = list_pipelines(Some(&resources), Some(&airflow), &ada)
            .await
            .expect("the list succeeds");
        assert_eq!(
            list.pipelines[0].tasks,
            vec![TaskSpecView {
                name: "run".into(),
                kind: "notebook".into(),
                sql: None,
                notebook: None,
                object_version: None,
                notebook_params: None,
            }]
        );

        // A statement pipeline keeps carrying its `sql` and none of the notebook fields.
        let resources = memory(&[("smoke", nova_pipeline("smoke", "analytics"))]);
        let detail = pipeline_detail(Some(&resources), Some(&airflow), &ada, &namespace, "smoke")
            .await
            .expect("the detail is readable");
        assert_eq!(
            detail.pipeline.tasks,
            vec![TaskSpecView {
                name: "run".into(),
                kind: "sql".into(),
                sql: Some("select 1".into()),
                notebook: None,
                object_version: None,
                notebook_params: None,
            }]
        );
    }

    #[tokio::test]
    async fn audit_has_no_param_values() {
        const CANARY: &str = "CANARY-param-4b1d";
        const SUB: &str = "sub-audit-237";
        let s3 = FakeS3::start().await;
        let version = s3.put_raw("analytics/report.ipynb", b"{}", "ada");
        let store = notebook_store(&s3.base);
        let resources = analytics_ready();
        let ada = who(SUB, &["analytics"], false);
        let namespace = crate::cluster::namespace();
        let _ = audit::test_sink::take_for(SUB);

        let mut body = notebook_body("nightly-report", "analytics", "report.ipynb", &version);
        body.notebook.as_mut().expect("notebook").params = BTreeMap::from([
            ("region".to_string(), CANARY.to_string()),
            ("days".to_string(), "7".to_string()),
        ]);
        create_pipeline(
            Some(&resources),
            Some(&store),
            &create_limiter(),
            &ada,
            body,
        )
        .await
        .expect("create");

        let mut update = update_notebook_body("report.ipynb", &version);
        update.notebook.as_mut().expect("notebook").params =
            BTreeMap::from([("region".to_string(), CANARY.to_string())]);
        update_pipeline(
            Some(&resources),
            Some(&store),
            None,
            &ada,
            &namespace,
            "nightly-report",
            update,
        )
        .await
        .expect("update");

        let records = audit::test_sink::take_for(SUB);
        assert_eq!(
            records
                .iter()
                .map(|record| record.action.as_str())
                .collect::<Vec<_>>(),
            ["create", "update"]
        );
        for record in &records {
            assert_eq!(record.kind, "notebook", "the audit says what it runs");
            assert_eq!(record.notebook_group.as_deref(), Some("analytics"));
            assert_eq!(record.notebook.as_deref(), Some("report.ipynb"));
            assert_eq!(record.version.as_deref(), Some(version.as_str()));
            assert_eq!(
                record.statement_chars,
                Some(0),
                "a notebook has no statement to measure"
            );
        }
        assert_eq!(records[0].schedule.as_deref(), Some("0 3 * * *"));
        assert_eq!(
            records[1].schedule, None,
            "the update's body carries no schedule, so it removes it"
        );
        let serialized = format!("{records:?}");
        assert!(
            !serialized.contains(CANARY),
            "no parameter value is ever audited: {serialized}"
        );
        assert!(
            !serialized.contains("region"),
            "not even a parameter name: {serialized}"
        );
        let stored = stored_pipeline(&resources, "nightly-report")
            .await
            .expect("written");
        assert!(
            stored["spec"]["tasks"][0]["config"]["params"]
                .as_str()
                .expect("params")
                .contains(CANARY),
            "the value is in the resource, where papermill reads it"
        );
    }
}
