//! Client for Airflow's REST API (Airflow 3.2 on our cluster): the pipeline UI's read and
//! control path — DAGs, runs, task instances, logs, trigger, pause and mark-a-run-failed.
//!
//! Everything here follows what the spike (stardelt-platform#243) measured against Airflow
//! 3.2.0, not what a generated client would imply:
//!
//! - Sign-in is `POST /auth/token` with `{"username","password"}` and answers **201**
//!   `{"access_token":"<jwt>"}`. The endpoint sits outside `/api/v2` and is missing from
//!   `openapi.json`.
//! - The token lives 24 h and there is **no refresh**: a renewal is another password login (an
//!   old token stays valid until `exp`). [`AirflowClient`] caches one token until five minutes
//!   before `exp`, capped at 23 hours, and repeats a call **once** after a `401`.
//! - The credential is the built-in role `User` (user `nova-api`): enough to read everything,
//!   trigger, pause and mark failed. Airflow cannot scope it to owner groups, so **Nova decides
//!   who may see what**; this client never decides.
//! - The `dag_id` of `Pipeline <namespace>/<name>` is `<namespace>__<name>` (see [`dag_id_for`]).
//! - Triggering **requires** `logical_date` (`422` without it); a paused DAG still answers `200`
//!   and its run waits `queued`, and `concurrency: Forbid` queues a second run instead of
//!   answering `409`.
//! - Logs are JSON (`Accept: text/plain` is refused with `406`), the first item is the
//!   `::group::Log message source details` marker, and a long log continues through the `token`
//!   query parameter.
//! - Run ids carry `:` and `+` (`manual__2026-10-20T07:20:17.516483+00:00`): every path segment
//!   is percent-encoded here, always.
//!
//! Nothing in this module logs a request body, a password or a token, and no error carries one.
//! This is the client only; the HTTP routes that use it land in the follow-up ticket.
#![allow(dead_code)]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::auth::Secret;

/// The API root. The sign-in endpoint sits outside it and is not in the OpenAPI document.
const API: &str = "/api/v2";
/// One page of a listing (`limit`).
const PAGE: &str = "100";
/// A call that has not answered within this time is a failure, not a hanging screen.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Airflow's token lives 24 h; Nova never keeps one longer than this.
const TOKEN_CAP: Duration = Duration::from_secs(23 * 60 * 60);
/// Renew this long before `exp`, so the next call cannot race the expiry.
const EXPIRY_SKEW: Duration = Duration::from_secs(5 * 60);
/// Longest upstream sentence Nova repeats; the rest is cut.
const MAX_DETAIL: usize = 200;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Where Airflow is and how Nova signs in. Absent (`None`) unless all three variables are set:
/// every pipeline view must then say so, never panic.
#[derive(Debug, Clone)]
pub struct AirflowConfig {
    /// Base URL, no trailing slash: `http://airflow-api-server:8080`.
    pub url: String,
    /// The Airflow user Nova signs in as (`nova-api`, built-in role `User`).
    pub user: String,
    /// That user's password. `Debug` prints `[redacted]` (see [`Secret`]).
    pub password: Secret,
}

impl AirflowConfig {
    /// `NOVA_AIRFLOW_URL`, `NOVA_AIRFLOW_USER` and `NOVA_AIRFLOW_PASSWORD`, all three or none.
    ///
    /// The URL loses its trailing slash; the password is taken byte for byte, because a
    /// leading or trailing space can be part of it.
    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let url = get("NOVA_AIRFLOW_URL")?
            .trim()
            .trim_end_matches('/')
            .to_string();
        let user = get("NOVA_AIRFLOW_USER")?.trim().to_string();
        let password = get("NOVA_AIRFLOW_PASSWORD")?;
        if url.is_empty() || user.is_empty() || password.is_empty() {
            return None;
        }
        Some(Self {
            url,
            user,
            password: Secret::new(password),
        })
    }

    pub fn from_env() -> Option<Self> {
        Self::from_env_with(|k| std::env::var(k).ok())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a call failed. `Display` is one sentence for the UI; it never carries the password or a
/// token, and any upstream text is cut and scrubbed before it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AirflowError {
    /// The three `NOVA_AIRFLOW_*` variables are not all set.
    NotConfigured,
    /// Connection failed, timed out (15 s), or a server error (`5xx`).
    Unavailable(String),
    /// `401` after the one retry, or `403`.
    Refused(String),
    /// `404`: no such DAG, run or task (also a DAG that was never created yet).
    NotFound,
    /// `400`, `409` or `422`: Airflow's own `detail`, e.g. the missing `logical_date`.
    Rejected(String),
}

impl std::fmt::Display for AirflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AirflowError::NotConfigured => write!(f, "Airflow is not configured"),
            AirflowError::Unavailable(m) => write!(f, "Airflow is unavailable: {m}"),
            AirflowError::Refused(m) => write!(f, "Airflow refused the request: {m}"),
            AirflowError::NotFound => write!(f, "Airflow has no such object"),
            AirflowError::Rejected(m) => write!(f, "Airflow rejected the request: {m}"),
        }
    }
}

impl std::error::Error for AirflowError {}

// ---------------------------------------------------------------------------
// Documents (raw Airflow fields; the views are built by the route ticket)
// ---------------------------------------------------------------------------

/// One DAG as the list and the detail endpoint report it. Every field a new Airflow might add
/// is ignored; every field Airflow may leave out is optional.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DagSummary {
    pub dag_id: String,
    #[serde(default)]
    pub dag_display_name: Option<String>,
    #[serde(default)]
    pub is_paused: Option<bool>,
    /// True once the `Pipeline` that produced the DAG is gone: the DAG stays readable by id and
    /// disappears from the default list.
    #[serde(default)]
    pub is_stale: Option<bool>,
    #[serde(default)]
    pub timetable_description: Option<String>,
    #[serde(default)]
    pub next_dagrun_run_after: Option<String>,
    #[serde(default)]
    pub last_parsed_time: Option<String>,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

/// A DAG tag (`stardelt`, `pipeline`). Airflow also sends the owning `dag_id`; it is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tag {
    pub name: String,
}

/// One run of a DAG. Times stay RFC 3339 strings; `logical_date`, `start_date` and `end_date` are
/// null until they exist (a run created outside the API has no `logical_date` at all).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DagRun {
    pub dag_run_id: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub run_type: Option<String>,
    #[serde(default)]
    pub logical_date: Option<String>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Always `rest_api` for Nova's own triggers.
    #[serde(default)]
    pub triggered_by: Option<String>,
    /// Always `nova-api` — Airflow's audit trail does not name the end user.
    #[serde(default)]
    pub triggering_user_name: Option<String>,
}

/// One task instance of a run. `map_index` is `-1` for an unmapped task; `duration` is seconds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TaskInstance {
    pub task_id: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub try_number: Option<u32>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub map_index: Option<i32>,
}

/// One page of a task's log. `continuation_token` is `None` while more text remains only if
/// Airflow hands one out (a long log is read page by page, see [`AirflowClient::logs`]).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LogPage {
    #[serde(default)]
    pub content: Vec<LogEvent>,
    #[serde(default)]
    pub continuation_token: Option<String>,
}

/// One log message. The first item of a page is the `::group::Log message source details`
/// marker, which has `sources` and neither `timestamp` nor `level`; the others carry the
/// message and no `sources`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LogEvent {
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

// ---------------------------------------------------------------------------
// Naming: Pipeline → dag_id and back
// ---------------------------------------------------------------------------

/// The `dag_id` the pipeline loader derives for `Pipeline <namespace>/<name>`. Kubernetes names
/// have no underscore, so the `__` separator is unambiguous.
pub fn dag_id_for(namespace: &str, name: &str) -> String {
    format!("{namespace}__{name}")
}

/// The `(namespace, name)` behind a `dag_id`, or `None` when the id is not a pipeline DAG
/// (Airflow and the operator create DAGs of their own). Splits on the first `__`.
pub fn pipeline_of(dag_id: &str) -> Option<(String, String)> {
    let (namespace, name) = dag_id.split_once("__")?;
    Some((namespace.to_string(), name.to_string()))
}

/// Percent-encode one path segment. Airflow's run ids carry `:` and `+`; only unreserved
/// characters (`A-Z a-z 0-9 - . _ ~`) survive, so `manual__2026-10-20T07:20:17.516483+00:00`
/// becomes `manual__2026-10-20T07%3A20%3A17.516483%2B00%3A00`.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// A token and the instant Nova stops using it. `Debug` would print the token, so it has none.
struct CachedToken {
    token: Secret,
    valid_until: Instant,
}

/// The password sign-in against Airflow, with one cached token shared by every caller.
pub struct AirflowClient {
    http: reqwest::Client,
    cfg: AirflowConfig,
    /// Held across a login on purpose: concurrent callers share one sign-in and one token.
    token: Mutex<Option<CachedToken>>,
}

/// How long a token may be kept, from the token's own `exp`: five minutes before `exp`, never
/// longer than [`TOKEN_CAP`]. An unreadable `exp` (or none) falls back to the cap. The signature
/// is deliberately not verified — Nova only needs the time, and Airflow signed it.
fn cache_duration(jwt: &str) -> Duration {
    let Some(life) = jwt_lifetime(jwt) else {
        return TOKEN_CAP;
    };
    life.min(TOKEN_CAP + EXPIRY_SKEW)
        .saturating_sub(EXPIRY_SKEW)
}

/// The token's remaining life, read from the middle part of the JWT (base64url, no padding).
fn jwt_lifetime(jwt: &str) -> Option<Duration> {
    let payload = jwt.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    let exp = claims.get("exp")?.as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(exp.saturating_sub(now)))
}

/// The `detail` of an Airflow error document, as one sentence. A `422` carries a list of
/// objects (`loc`, `msg`), everything else a string.
fn detail_text(body: &Value) -> Option<String> {
    match body.get("detail")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(|item| {
                    if let Some(text) = item.as_str() {
                        return Some(text.to_string());
                    }
                    let msg = item.get("msg")?.as_str()?;
                    match item.get("loc").and_then(Value::as_array) {
                        Some(loc) => {
                            let loc: Vec<&str> = loc.iter().filter_map(Value::as_str).collect();
                            Some(format!("{}: {msg}", loc.join(".")))
                        }
                        None => Some(msg.to_string()),
                    }
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("; "))
            }
        }
        other => Some(other.to_string()),
    }
}

impl AirflowClient {
    pub fn new(http: reqwest::Client, cfg: AirflowConfig) -> Self {
        Self {
            http,
            cfg,
            token: Mutex::new(None),
        }
    }

    /// The configuration this client uses (a test reads it; the routes do not need it).
    pub fn config(&self) -> &AirflowConfig {
        &self.cfg
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.cfg.url)
    }

    /// Sign in and cache the token. Called only through [`Self::token`], which holds the lock.
    async fn login(&self) -> Result<Secret, AirflowError> {
        let response = self
            .http
            .post(self.url("/auth/token"))
            .timeout(REQUEST_TIMEOUT)
            .json(&json!({
                "username": self.cfg.user,
                "password": self.cfg.password.expose(),
            }))
            .send()
            .await
            .map_err(|e| AirflowError::Unavailable(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            // Deliberately without the body: a chatty upstream could echo what we sent, and the
            // one thing that must never leave this function is the password.
            return Err(AirflowError::Refused(
                "the sign-in was refused (wrong user or password)".into(),
            ));
        }
        if !(200..=299).contains(&status) {
            return Err(AirflowError::Unavailable(format!(
                "the sign-in answered HTTP {status}"
            )));
        }
        let body: Value = response.json().await.map_err(|_| {
            AirflowError::Unavailable("the sign-in answered no readable token".into())
        })?;
        body["access_token"]
            .as_str()
            .map(Secret::new)
            .ok_or_else(|| AirflowError::Unavailable("the sign-in answered without a token".into()))
    }

    /// The current token, signing in when there is none. The lock is held across the login, so
    /// a burst of callers produces exactly one sign-in.
    async fn token(&self) -> Result<Secret, AirflowError> {
        let mut cached = self.token.lock().await;
        if let Some(entry) = cached.as_ref()
            && entry.valid_until > Instant::now()
        {
            return Ok(entry.token.clone());
        }
        let token = self.login().await?;
        let valid_until = Instant::now() + cache_duration(token.expose());
        *cached = Some(CachedToken {
            token: token.clone(),
            valid_until,
        });
        Ok(token)
    }

    /// Drop the cached token: the next call signs in again.
    async fn forget_token(&self) {
        *self.token.lock().await = None;
    }

    /// Send one request and read its JSON body. A `401` means the cached token is no longer
    /// accepted (Airflow has no refresh): drop it, sign in again and repeat the call once. A
    /// second `401` is a refusal, never a loop.
    async fn send<F>(&self, build: F) -> Result<Value, AirflowError>
    where
        F: Fn(&str) -> reqwest::RequestBuilder,
    {
        let token = self.token().await?;
        let (status, body) = self.perform(build(token.expose())).await?;
        if status != 401 {
            self.accept(status, &body, token.expose())?;
            return Ok(body);
        }
        self.forget_token().await;
        let token = self.token().await?;
        let (status, body) = self.perform(build(token.expose())).await?;
        if status == 401 {
            self.forget_token().await;
            return Err(AirflowError::Refused("Airflow refused the token".into()));
        }
        self.accept(status, &body, token.expose())?;
        Ok(body)
    }

    /// One request, one response body. Bodies are never logged.
    async fn perform(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(u16, Value), AirflowError> {
        let response = request
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| AirflowError::Unavailable(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| AirflowError::Unavailable(e.without_url().to_string()))?;
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    /// Map a status to an error. Any text taken from the upstream body is cut to
    /// [`MAX_DETAIL`] and has the password and the token used for this call removed, so a
    /// talkative upstream cannot leak either through an error the UI shows.
    fn accept(&self, status: u16, body: &Value, token: &str) -> Result<(), AirflowError> {
        let clean = |text: String| {
            let text: String = text.chars().take(MAX_DETAIL).collect();
            let password = self.cfg.password.expose();
            let text = if password.is_empty() {
                text
            } else {
                text.replace(password, "[redacted]")
            };
            if token.is_empty() {
                text
            } else {
                text.replace(token, "[redacted]")
            }
        };
        match status {
            200..=299 => Ok(()),
            401 | 403 => Err(AirflowError::Refused(clean(
                detail_text(body).unwrap_or_else(|| "permission denied".into()),
            ))),
            404 => Err(AirflowError::NotFound),
            400 | 409 | 422 => Err(AirflowError::Rejected(clean(
                detail_text(body).unwrap_or_else(|| format!("HTTP {status}")),
            ))),
            _ => Err(AirflowError::Unavailable(format!("HTTP {status}"))),
        }
    }

    /// Parse a document with the message a route can show. A missing field is an Airflow that
    /// answered something else, not a caller error.
    fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, AirflowError> {
        serde_json::from_value(body).map_err(|e| {
            AirflowError::Unavailable(format!("Airflow answered an unexpected document: {e}"))
        })
    }

    /// The DAGs that carry `tag`, oldest page first. Follows `total_entries` with `offset`
    /// until the list is complete.
    pub async fn list_dags(
        &self,
        tag: &str,
        include_stale: bool,
    ) -> Result<Vec<DagSummary>, AirflowError> {
        let url = self.url(&format!("{API}/dags"));
        let mut dags = Vec::new();
        let mut offset = 0u64;
        loop {
            let mut query = vec![
                ("limit", PAGE.to_string()),
                ("tags", tag.to_string()),
                ("exclude_stale", (!include_stale).to_string()),
            ];
            if offset > 0 {
                query.push(("offset", offset.to_string()));
            }
            let body = self
                .send(|token| self.http.get(&url).bearer_auth(token).query(&query))
                .await?;
            let page: DagsPage = Self::parse(body)?;
            let received = page.dags.len() as u64;
            dags.extend(page.dags);
            offset += received;
            if received == 0 || (page.total_entries > 0 && offset >= page.total_entries) {
                return Ok(dags);
            }
        }
    }

    /// One DAG by id. A DAG whose `Pipeline` is gone still answers (with `is_stale: true`).
    pub async fn dag(&self, dag_id: &str) -> Result<DagSummary, AirflowError> {
        let url = self.url(&format!("{API}/dags/{}", encode_segment(dag_id)));
        let body = self
            .send(|token| self.http.get(&url).bearer_auth(token))
            .await?;
        Self::parse(body)
    }

    /// The newest `limit` runs of a DAG.
    pub async fn runs(&self, dag_id: &str, limit: u32) -> Result<Vec<DagRun>, AirflowError> {
        let url = self.url(&format!("{API}/dags/{}/dagRuns", encode_segment(dag_id)));
        let limit = limit.to_string();
        let body = self
            .send(|token| {
                self.http
                    .get(&url)
                    .bearer_auth(token)
                    .query(&[("order_by", "-start_date"), ("limit", limit.as_str())])
            })
            .await?;
        let page: RunsPage = Self::parse(body)?;
        Ok(page.dag_runs)
    }

    /// The task instances of one run.
    pub async fn task_instances(
        &self,
        dag_id: &str,
        run_id: &str,
    ) -> Result<Vec<TaskInstance>, AirflowError> {
        let url = self.url(&format!(
            "{API}/dags/{}/dagRuns/{}/taskInstances",
            encode_segment(dag_id),
            encode_segment(run_id)
        ));
        let body = self
            .send(|token| self.http.get(&url).bearer_auth(token))
            .await?;
        let page: TasksPage = Self::parse(body)?;
        Ok(page.task_instances)
    }

    /// One page of a task's log. A long log continues through `continuation_token`; pass it back
    /// as `token` for the next page. Airflow serves logs from the object store, so a finished
    /// run is readable after its pod is gone.
    pub async fn logs(
        &self,
        dag_id: &str,
        run_id: &str,
        task_id: &str,
        try_number: u32,
        token: Option<&str>,
    ) -> Result<LogPage, AirflowError> {
        let url = self.url(&format!(
            "{API}/dags/{}/dagRuns/{}/taskInstances/{}/logs/{try_number}",
            encode_segment(dag_id),
            encode_segment(run_id),
            encode_segment(task_id)
        ));
        let body = self
            .send(|bearer| {
                let request = self.http.get(&url).bearer_auth(bearer);
                match token {
                    Some(token) => request.query(&[("token", token)]),
                    None => request,
                }
            })
            .await?;
        Self::parse(body)
    }

    /// Trigger a run. `logical_date` is required by Airflow 3 (`422` without it); `note` is
    /// omitted from the body when there is none. A paused DAG still answers `200` and its run
    /// waits `queued` — this is not a promise that anything runs.
    pub async fn trigger(
        &self,
        dag_id: &str,
        logical_date: &str,
        note: Option<&str>,
    ) -> Result<DagRun, AirflowError> {
        let url = self.url(&format!("{API}/dags/{}/dagRuns", encode_segment(dag_id)));
        let mut payload = serde_json::Map::new();
        payload.insert(
            "logical_date".into(),
            Value::String(logical_date.to_string()),
        );
        if let Some(note) = note {
            payload.insert("note".into(), Value::String(note.to_string()));
        }
        let payload = Value::Object(payload);
        let body = self
            .send(|token| self.http.post(&url).bearer_auth(token).json(&payload))
            .await?;
        Self::parse(body)
    }

    /// Pause or resume a DAG. The `update_mask` is not optional: Airflow rejects the patch
    /// without it.
    pub async fn set_paused(&self, dag_id: &str, paused: bool) -> Result<DagSummary, AirflowError> {
        let url = self.url(&format!("{API}/dags/{}", encode_segment(dag_id)));
        let body = self
            .send(|token| {
                self.http
                    .patch(&url)
                    .bearer_auth(token)
                    .query(&[("update_mask", "is_paused")])
                    .json(&json!({ "is_paused": paused }))
            })
            .await?;
        Self::parse(body)
    }

    /// Mark a run failed. This is metadata, not a cancel: work already in flight keeps running
    /// and can commit its side effects, and the run may end up with an empty log.
    pub async fn mark_run_failed(
        &self,
        dag_id: &str,
        run_id: &str,
    ) -> Result<DagRun, AirflowError> {
        let url = self.url(&format!(
            "{API}/dags/{}/dagRuns/{}",
            encode_segment(dag_id),
            encode_segment(run_id)
        ));
        let body = self
            .send(|token| {
                self.http
                    .patch(&url)
                    .bearer_auth(token)
                    .json(&json!({ "state": "failed" }))
            })
            .await?;
        Self::parse(body)
    }
}

/// The DAG list envelope: `{"dags":[…],"total_entries":N}`.
#[derive(Deserialize)]
struct DagsPage {
    #[serde(default)]
    dags: Vec<DagSummary>,
    #[serde(default)]
    total_entries: u64,
}

/// The run list envelope: `{"dag_runs":[…],"total_entries":N}`.
#[derive(Deserialize)]
struct RunsPage {
    #[serde(default)]
    dag_runs: Vec<DagRun>,
}

/// The task-instance envelope: `{"task_instances":[…],"total_entries":N}`.
#[derive(Deserialize)]
struct TasksPage {
    #[serde(default)]
    task_instances: Vec<TaskInstance>,
}

// ---------------------------------------------------------------------------
// A fake Airflow for the tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fake {
    //! The few Airflow routes the client uses, answering the documents the spike measured.
    //! The password it accepts and the token it hands out are canaries: neither may ever show
    //! up in an error.

    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use axum::{
        Json, Router,
        extract::{OriginalUri, Query, State},
        http::{HeaderMap, StatusCode, Uri, header},
        routing::{get, patch, post},
    };
    use serde_json::{Value, json};

    use super::{AirflowClient, AirflowConfig};
    use crate::auth::Secret;

    pub(super) const PASSWORD: &str = "pw-canary-7f3a";
    pub(super) const TOKEN: &str = "tok-canary-7f3a";

    type Refusal = (StatusCode, Json<Value>);

    fn refused() -> Refusal {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "Invalid credentials" })),
        )
    }

    /// The spike's minimal DAG list entry, including the fields Nova ignores.
    pub(super) fn dag() -> Value {
        json!({
            "dag_id": "stardelt__spike-j1",
            "dag_display_name": "stardelt__spike-j1",
            "is_paused": false,
            "is_stale": true,
            "timetable_description": "Never, external triggers only",
            "next_dagrun_run_after": null,
            "last_parsed_time": "2026-10-20T07:24:29.819955Z",
            "owners": ["airflow"],
            "tags": [
                {"name": "pipeline", "dag_id": "stardelt__spike-j1"},
                {"name": "stardelt", "dag_id": "stardelt__spike-j1"}
            ],
            "description": null,
            "has_import_errors": false
        })
    }

    /// The spike's failed run, including `conf` and `run_after`, which Nova ignores.
    fn run() -> Value {
        json!({
            "dag_run_id": "manual__2026-10-20T07:23:50.608876+00:00",
            "state": "failed",
            "run_type": "manual",
            "logical_date": "2026-10-20T07:23:50Z",
            "start_date": null,
            "end_date": "2026-10-20T07:23:58.876923Z",
            "note": "paused trigger test",
            "conf": {},
            "triggered_by": "rest_api",
            "triggering_user_name": "nova-api",
            "run_after": "2026-10-20T07:23:50.608876Z"
        })
    }

    /// The spike's task instance, including the fields Nova ignores.
    fn task() -> Value {
        json!({
            "task_id": "insert",
            "state": "success",
            "try_number": 1,
            "start_date": "2026-10-19T14:42:15.713056Z",
            "end_date": "2026-10-19T14:42:19.146072Z",
            "duration": 3.433016,
            "operator": "PythonOperator",
            "map_index": -1,
            "max_tries": 0,
            "pool": "default_pool",
            "queued_when": "2026-10-19T14:42:13.445829Z",
            "hostname": "stardelt-smoke-insert-tdfdicc2",
            "dag_version": {"version_number": 1}
        })
    }

    /// The spike's log page: the `::group::` marker first (no timestamp, no level), then the
    /// message (no `sources`).
    fn log_page() -> Value {
        json!({
            "content": [
                {
                    "event": "::group::Log message source details",
                    "sources": ["s3://backups/airflow-logs/dag_id=stardelt__spike-j1/run_id=manual__2026-10-20T07:20:17.516483+00:00/task_id=probe/attempt=1.log"]
                },
                {
                    "timestamp": "2026-10-20T07:21:02.174123Z",
                    "level": "info",
                    "event": "task probe finished: query 20261020_072101_00028_crx9g, 1 rows, update count None"
                }
            ],
            "continuation_token": null
        })
    }

    #[derive(Default)]
    pub(super) struct Fake {
        /// How many sign-in calls arrived.
        pub(super) logins: usize,
        /// The last sign-in body, exactly as it was sent.
        pub(super) login_body: Option<Value>,
        /// `METHOD path?query` and the JSON body of every call except the sign-in.
        pub(super) calls: Vec<(String, Option<Value>)>,
        /// How many more `401` answers to serve before accepting the token.
        pub(super) unauthorized: usize,
        /// How many more `422` answers to serve on a trigger.
        pub(super) rejected: usize,
        /// When set, the `msg` of a `422` repeats it — a talkative upstream.
        pub(super) echo: Option<String>,
        /// The password the fake accepts.
        pub(super) password: String,
        /// The token the fake hands out.
        pub(super) token: String,
        /// The DAGs the list endpoint pages through.
        pub(super) dags: Vec<Value>,
        pub(super) run: Value,
        pub(super) task: Value,
        pub(super) log_page: Value,
    }

    pub(super) type Shared = Arc<Mutex<Fake>>;

    fn seeded() -> Fake {
        Fake {
            password: PASSWORD.into(),
            token: TOKEN.into(),
            dags: vec![dag()],
            run: run(),
            task: task(),
            log_page: log_page(),
            ..Fake::default()
        }
    }

    fn line(uri: &Uri) -> String {
        uri.path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| uri.path().to_string())
    }

    fn remember(state: &mut Fake, method: &str, uri: &Uri, body: Option<Value>) {
        state.calls.push((format!("{method} {}", line(uri)), body));
    }

    /// The fake's permission check: a spent `401` budget answers `401`, a wrong or missing
    /// bearer answers `401` too. Airflow's real answer is the same in both cases.
    fn authorize(state: &mut Fake, headers: &HeaderMap) -> Result<(), Refusal> {
        if state.unauthorized > 0 {
            state.unauthorized -= 1;
            return Err(refused());
        }
        let expected = format!("Bearer {}", state.token);
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
        {
            return Err(refused());
        }
        Ok(())
    }

    async fn login(State(s): State<Shared>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        let mut state = s.lock().unwrap();
        state.logins += 1;
        state.login_body = Some(body.clone());
        let ok = body["username"].as_str() == Some("nova-api")
            && body["password"].as_str() == Some(state.password.as_str());
        if ok {
            return (
                StatusCode::CREATED,
                Json(json!({ "access_token": state.token })),
            );
        }
        // Deliberately chatty: it repeats what was sent, so a client that copies the body into
        // an error would print the password.
        let sent = body["password"].as_str().unwrap_or_default();
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": format!("Invalid credentials for {sent}") })),
        )
    }

    async fn dags(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        Query(q): Query<BTreeMap<String, String>>,
        headers: HeaderMap,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "GET", &uri, None);
        authorize(&mut state, &headers)?;
        let total = state.dags.len();
        let offset: usize = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
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
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "GET", &uri, None);
        authorize(&mut state, &headers)?;
        Ok(Json(state.dags.first().cloned().unwrap_or(Value::Null)))
    }

    async fn pause(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "PATCH", &uri, Some(body.clone()));
        authorize(&mut state, &headers)?;
        let mut dag = state.dags.first().cloned().unwrap_or(Value::Null);
        dag["is_paused"] = body["is_paused"].clone();
        Ok(Json(dag))
    }

    async fn runs(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "GET", &uri, None);
        authorize(&mut state, &headers)?;
        let run = state.run.clone();
        Ok(Json(json!({ "dag_runs": [run], "total_entries": 1 })))
    }

    async fn tasks(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "GET", &uri, None);
        authorize(&mut state, &headers)?;
        let task = state.task.clone();
        Ok(Json(
            json!({ "task_instances": [task], "total_entries": 1 }),
        ))
    }

    async fn logs(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "GET", &uri, None);
        authorize(&mut state, &headers)?;
        Ok(Json(state.log_page.clone()))
    }

    async fn trigger(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "POST", &uri, Some(body.clone()));
        authorize(&mut state, &headers)?;
        if state.rejected > 0 {
            state.rejected -= 1;
            let msg = state
                .echo
                .clone()
                .unwrap_or_else(|| "Field required".to_string());
            // Airflow 3.2's answer to a trigger without `logical_date`.
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "detail": [{
                    "type": "missing",
                    "loc": ["body", "logical_date"],
                    "msg": msg,
                    "input": {}
                }] })),
            ));
        }
        let mut run = state.run.clone();
        run["state"] = json!("queued");
        run["logical_date"] = body["logical_date"].clone();
        run["note"] = body.get("note").cloned().unwrap_or(Value::Null);
        Ok(Json(run))
    }

    async fn fail_run(
        State(s): State<Shared>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<Json<Value>, Refusal> {
        let mut state = s.lock().unwrap();
        remember(&mut state, "PATCH", &uri, Some(body.clone()));
        authorize(&mut state, &headers)?;
        let mut run = state.run.clone();
        run["state"] = body["state"].clone();
        Ok(Json(run))
    }

    /// Start the fake and a client signed in against it.
    pub(super) async fn start() -> (AirflowClient, AirflowConfig, Shared) {
        let shared: Shared = Arc::new(Mutex::new(seeded()));
        let app = Router::new()
            .route("/auth/token", post(login))
            .route("/api/v2/dags", get(dags))
            .route("/api/v2/dags/{dag_id}", get(one_dag).patch(pause))
            .route("/api/v2/dags/{dag_id}/dagRuns", get(runs).post(trigger))
            .route("/api/v2/dags/{dag_id}/dagRuns/{run_id}", patch(fail_run))
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
        let cfg = AirflowConfig {
            url: format!("http://{addr}"),
            user: "nova-api".into(),
            password: Secret::new(PASSWORD),
        };
        (
            AirflowClient::new(reqwest::Client::new(), cfg.clone()),
            cfg,
            shared,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::fake::{self, PASSWORD, TOKEN};
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn all_three() -> Vec<(&'static str, &'static str)> {
        vec![
            ("NOVA_AIRFLOW_URL", "http://airflow-api-server:8080"),
            ("NOVA_AIRFLOW_USER", "nova-api"),
            ("NOVA_AIRFLOW_PASSWORD", PASSWORD),
        ]
    }

    #[tokio::test]
    async fn token_is_requested_once_for_many_calls() {
        let (client, _cfg, fake) = fake::start().await;
        for _ in 0..3 {
            client.dag("stardelt__spike-j1").await.unwrap();
        }
        assert_eq!(
            fake.lock().unwrap().logins,
            1,
            "one sign-in serves every call"
        );
    }

    #[tokio::test]
    async fn token_login_sends_username_and_password_as_json() {
        let (client, _cfg, fake) = fake::start().await;
        client.dag("stardelt__spike-j1").await.unwrap();
        assert_eq!(
            fake.lock().unwrap().login_body,
            Some(json!({ "username": "nova-api", "password": PASSWORD }))
        );
    }

    #[tokio::test]
    async fn a_401_triggers_one_new_login_and_one_retry() {
        let (client, _cfg, fake) = fake::start().await;
        fake.lock().unwrap().unauthorized = 1;
        let dag = client.dag("stardelt__spike-j1").await.unwrap();
        assert_eq!(dag.dag_id, "stardelt__spike-j1");
        let state = fake.lock().unwrap();
        assert_eq!(state.logins, 2, "the 401 costs exactly one new sign-in");
        let reads = state
            .calls
            .iter()
            .filter(|(line, _)| line == "GET /api/v2/dags/stardelt__spike-j1")
            .count();
        assert_eq!(reads, 2, "the call is repeated once");
    }

    #[tokio::test]
    async fn a_second_401_is_refused_not_looped() {
        let (client, _cfg, fake) = fake::start().await;
        fake.lock().unwrap().unauthorized = 99;
        match client.dag("stardelt__spike-j1").await {
            Err(AirflowError::Refused(_)) => {}
            other => panic!("expected Refused, got {other:?}"),
        }
        assert_eq!(
            fake.lock().unwrap().logins,
            2,
            "one retry after the first 401, never a loop"
        );
    }

    #[tokio::test]
    async fn list_dags_asks_for_the_tag_and_stale_flag() {
        let (client, _cfg, fake) = fake::start().await;
        client.list_dags("pipeline", false).await.unwrap();
        client.list_dags("pipeline", true).await.unwrap();
        let state = fake.lock().unwrap();
        let lines: Vec<&str> = state.calls.iter().map(|(line, _)| line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v2/dags?limit=100&tags=pipeline&exclude_stale=true",
                "GET /api/v2/dags?limit=100&tags=pipeline&exclude_stale=false",
            ]
        );
    }

    #[tokio::test]
    async fn list_dags_follows_offsets() {
        let (client, _cfg, fake) = fake::start().await;
        let template = fake.lock().unwrap().dags[0].clone();
        let dags: Vec<Value> = (0..150)
            .map(|i| {
                let mut dag = template.clone();
                dag["dag_id"] = json!(format!("stardelt__dag-{i}"));
                dag
            })
            .collect();
        fake.lock().unwrap().dags = dags;

        let dags = client.list_dags("pipeline", false).await.unwrap();
        assert_eq!(dags.len(), 150, "both pages are collected");
        assert_eq!(dags.last().unwrap().dag_id, "stardelt__dag-149");

        let state = fake.lock().unwrap();
        let lines: Vec<&str> = state.calls.iter().map(|(line, _)| line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v2/dags?limit=100&tags=pipeline&exclude_stale=true",
                "GET /api/v2/dags?limit=100&tags=pipeline&exclude_stale=true&offset=100",
            ]
        );
    }

    #[tokio::test]
    async fn runs_task_instances_and_logs_parse_the_spike_samples() {
        let (client, _cfg, _fake) = fake::start().await;

        let runs = client.runs("stardelt__spike-j1", 20).await.unwrap();
        let run = runs.first().expect("one run");
        assert_eq!(run.dag_run_id, "manual__2026-10-20T07:23:50.608876+00:00");
        assert_eq!(run.state.as_deref(), Some("failed"));
        assert_eq!(run.run_type.as_deref(), Some("manual"));
        assert_eq!(run.logical_date.as_deref(), Some("2026-10-20T07:23:50Z"));
        assert_eq!(run.start_date, None);
        assert_eq!(run.end_date.as_deref(), Some("2026-10-20T07:23:58.876923Z"));
        assert_eq!(run.note.as_deref(), Some("paused trigger test"));
        assert_eq!(run.triggered_by.as_deref(), Some("rest_api"));
        assert_eq!(run.triggering_user_name.as_deref(), Some("nova-api"));

        let tasks = client
            .task_instances("stardelt__spike-j1", &run.dag_run_id)
            .await
            .unwrap();
        let task = tasks.first().expect("one task");
        assert_eq!(task.task_id, "insert");
        assert_eq!(task.state.as_deref(), Some("success"));
        assert_eq!(task.try_number, Some(1));
        assert_eq!(
            task.start_date.as_deref(),
            Some("2026-10-19T14:42:15.713056Z")
        );
        assert_eq!(
            task.end_date.as_deref(),
            Some("2026-10-19T14:42:19.146072Z")
        );
        assert_eq!(task.duration, Some(3.433016));
        assert_eq!(task.operator.as_deref(), Some("PythonOperator"));
        assert_eq!(task.map_index, Some(-1));

        let page = client
            .logs("stardelt__spike-j1", &run.dag_run_id, "probe", 1, None)
            .await
            .unwrap();
        assert_eq!(page.continuation_token, None);
        assert_eq!(page.content.len(), 2);
        assert_eq!(
            page.content[0].event.as_deref(),
            Some("::group::Log message source details")
        );
        assert_eq!(page.content[0].sources.len(), 1);
        assert!(page.content[0].timestamp.is_none());
        assert!(page.content[0].level.is_none());
        assert_eq!(page.content[1].level.as_deref(), Some("info"));
        assert_eq!(
            page.content[1].timestamp.as_deref(),
            Some("2026-10-20T07:21:02.174123Z")
        );
        assert!(
            page.content[1]
                .event
                .as_deref()
                .unwrap_or_default()
                .contains("task probe finished")
        );
        assert!(page.content[1].sources.is_empty());
    }

    fn body_of<'a>(state: &'a fake::Fake, prefix: &str) -> &'a Value {
        state
            .calls
            .iter()
            .rev()
            .find(|(line, _)| line.starts_with(prefix))
            .and_then(|(_, body)| body.as_ref())
            .unwrap_or_else(|| panic!("no call with a body starting {prefix}"))
    }

    #[tokio::test]
    async fn trigger_sends_only_logical_date_when_there_is_no_note() {
        let (client, _cfg, fake) = fake::start().await;
        let run = client
            .trigger("stardelt__spike-j1", "2026-10-20T07:20:17Z", None)
            .await
            .unwrap();
        assert_eq!(run.state.as_deref(), Some("queued"));
        {
            let state = fake.lock().unwrap();
            assert_eq!(
                body_of(&state, "POST /api/v2/dags/stardelt__spike-j1/dagRuns"),
                &json!({ "logical_date": "2026-10-20T07:20:17Z" }),
                "no note key at all"
            );
        }

        client
            .trigger(
                "stardelt__spike-j1",
                "2026-10-20T07:20:17Z",
                Some("run from Nova"),
            )
            .await
            .unwrap();
        let state = fake.lock().unwrap();
        assert_eq!(
            body_of(&state, "POST /api/v2/dags/stardelt__spike-j1/dagRuns"),
            &json!({ "logical_date": "2026-10-20T07:20:17Z", "note": "run from Nova" })
        );
    }

    #[tokio::test]
    async fn trigger_422_is_rejected_with_the_detail() {
        let (client, _cfg, fake) = fake::start().await;
        fake.lock().unwrap().rejected = 1;
        match client
            .trigger("stardelt__spike-j1", "2026-10-20T07:20:17Z", None)
            .await
        {
            Err(AirflowError::Rejected(m)) => {
                assert!(m.contains("logical_date"), "the field must be named: {m}");
                assert!(
                    m.contains("Field required"),
                    "the reason must be quoted: {m}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_paused_uses_the_update_mask() {
        let (client, _cfg, fake) = fake::start().await;
        let dag = client.set_paused("stardelt__spike-j1", true).await.unwrap();
        assert_eq!(dag.is_paused, Some(true));
        let state = fake.lock().unwrap();
        let (line, body) = state
            .calls
            .iter()
            .rev()
            .find(|(line, _)| line.starts_with("PATCH "))
            .expect("a patch");
        assert_eq!(
            line,
            "PATCH /api/v2/dags/stardelt__spike-j1?update_mask=is_paused"
        );
        assert_eq!(body.as_ref(), Some(&json!({ "is_paused": true })));
    }

    #[tokio::test]
    async fn mark_failed_patches_the_state() {
        let (client, _cfg, fake) = fake::start().await;
        let run = client
            .mark_run_failed(
                "stardelt__spike-j1",
                "manual__2026-10-20T07:23:50.608876+00:00",
            )
            .await
            .unwrap();
        assert_eq!(run.state.as_deref(), Some("failed"));
        let state = fake.lock().unwrap();
        let (line, body) = state
            .calls
            .iter()
            .rev()
            .find(|(line, _)| line.starts_with("PATCH "))
            .expect("a patch");
        assert_eq!(
            line,
            "PATCH /api/v2/dags/stardelt__spike-j1/dagRuns/manual__2026-10-20T07%3A23%3A50.608876%2B00%3A00"
        );
        assert_eq!(body.as_ref(), Some(&json!({ "state": "failed" })));
    }

    #[tokio::test]
    async fn run_ids_are_percent_encoded() {
        let (client, _cfg, fake) = fake::start().await;
        client
            .task_instances(
                "stardelt__spike-j1",
                "manual__2026-10-20T07:20:17.516483+00:00",
            )
            .await
            .unwrap();
        let state = fake.lock().unwrap();
        let (line, _) = state.calls.last().expect("a request");
        assert_eq!(
            line,
            "GET /api/v2/dags/stardelt__spike-j1/dagRuns/manual__2026-10-20T07%3A20%3A17.516483%2B00%3A00/taskInstances"
        );
    }

    #[test]
    fn dag_ids_round_trip() {
        assert_eq!(dag_id_for("stardelt", "spike-j1"), "stardelt__spike-j1");
        assert_eq!(
            pipeline_of("stardelt__spike-j1"),
            Some(("stardelt".to_string(), "spike-j1".to_string()))
        );
        assert_eq!(pipeline_of("nyc_taxi_load"), None);
    }

    #[tokio::test]
    async fn the_password_never_appears() {
        let (client, cfg, fake) = fake::start().await;
        assert!(!format!("{cfg:?}").contains(PASSWORD));

        // A sign-in that fails, against a fake that repeats what it was sent.
        fake.lock().unwrap().password = "somebody-rotated-it".into();
        let error = client.dag("stardelt__spike-j1").await.unwrap_err();
        let text = format!("{error}");
        assert!(!text.contains(PASSWORD), "the password leaked: {text}");
        assert!(!text.contains(TOKEN), "the token leaked: {text}");

        // A rejected call whose detail names both.
        {
            let mut state = fake.lock().unwrap();
            state.password = PASSWORD.into();
            state.echo = Some(format!("{PASSWORD} {TOKEN}"));
            state.rejected = 1;
        }
        let error = client
            .trigger("stardelt__spike-j1", "2026-10-20T07:20:17Z", None)
            .await
            .unwrap_err();
        let text = format!("{error}");
        assert!(!text.contains(PASSWORD), "the password leaked: {text}");
        assert!(!text.contains(TOKEN), "the token leaked: {text}");
    }

    #[test]
    fn config_needs_all_three_variables() {
        assert!(AirflowConfig::from_env_with(env(&[])).is_none());
        assert!(AirflowConfig::from_env_with(env(&all_three()[..2])).is_none());
        assert!(
            AirflowConfig::from_env_with(env(&[
                ("NOVA_AIRFLOW_URL", "http://airflow-api-server:8080"),
                ("NOVA_AIRFLOW_PASSWORD", PASSWORD),
            ]))
            .is_none()
        );
        assert!(
            AirflowConfig::from_env_with(env(&[
                ("NOVA_AIRFLOW_USER", "nova-api"),
                ("NOVA_AIRFLOW_PASSWORD", PASSWORD),
            ]))
            .is_none()
        );
        assert!(
            AirflowConfig::from_env_with(env(&[
                ("NOVA_AIRFLOW_URL", "  "),
                ("NOVA_AIRFLOW_USER", "nova-api"),
                ("NOVA_AIRFLOW_PASSWORD", PASSWORD),
            ]))
            .is_none()
        );

        let cfg = AirflowConfig::from_env_with(env(&all_three())).unwrap();
        assert_eq!(cfg.url, "http://airflow-api-server:8080");
        assert_eq!(cfg.user, "nova-api");
        assert_eq!(cfg.password.expose(), PASSWORD);

        // A trailing slash is normal in a chart value and breaks no path.
        let cfg = AirflowConfig::from_env_with(env(&[
            ("NOVA_AIRFLOW_URL", "http://airflow-api-server:8080/"),
            ("NOVA_AIRFLOW_USER", "nova-api"),
            ("NOVA_AIRFLOW_PASSWORD", PASSWORD),
        ]))
        .unwrap();
        assert_eq!(cfg.url, "http://airflow-api-server:8080");
    }

    /// Against the dev cluster's Airflow:
    /// `kubectl -n stardelt port-forward svc/airflow-api-server 18280:8080`, then
    /// `NOVA_TEST_AIRFLOW_URL=http://127.0.0.1:18280 NOVA_TEST_AIRFLOW_USER=nova-api \
    ///  NOVA_TEST_AIRFLOW_PASSWORD=<from Secret airflow-nova-api> cargo test airflow -- --ignored`
    /// Without the three variables the test prints "skipped" and passes, so `cargo test --
    /// --ignored` in CI stays green. The token and the password are never printed.
    #[tokio::test]
    #[ignore = "needs a running Airflow"]
    async fn live_airflow() {
        let (Ok(url), Ok(user), Ok(password)) = (
            std::env::var("NOVA_TEST_AIRFLOW_URL"),
            std::env::var("NOVA_TEST_AIRFLOW_USER"),
            std::env::var("NOVA_TEST_AIRFLOW_PASSWORD"),
        ) else {
            eprintln!(
                "skipped: set NOVA_TEST_AIRFLOW_URL, NOVA_TEST_AIRFLOW_USER and NOVA_TEST_AIRFLOW_PASSWORD to run this test"
            );
            return;
        };
        let client = AirflowClient::new(
            reqwest::Client::new(),
            AirflowConfig {
                url: url.trim().trim_end_matches('/').to_string(),
                user,
                password: Secret::new(password),
            },
        );
        let dags = client
            .list_dags("pipeline", false)
            .await
            .expect("list DAGs");
        let ids: Vec<&str> = dags.iter().map(|d| d.dag_id.as_str()).collect();
        println!("{} DAGs with the tag pipeline: {ids:?}", ids.len());
        assert!(ids.iter().all(|id| !id.is_empty()), "every DAG has an id");
    }
}
