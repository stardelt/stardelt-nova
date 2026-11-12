//! The pipeline notice (nova#815 slices c and c2): one message per ending of a pipeline's
//! run, sent by Nova to the destinations its `spec.notify` names for that ending —
//! `onFailure` for a failed run, `onSuccess` for a success, `onRecovered` for the first
//! success after a failure.
//!
//! Design decision 2 of nova#815: Nova sends, from one background collector, so no new
//! credential is put into task pods. The collector reads the newest run of every pipeline
//! from the engine every 30 s, claims the run in `pipeline_notice` *before* it sends, and
//! records every attempt in `destination_delivery` with `subject_kind = 'pipeline_run'`.
//! At most once per run, three attempts inside the tick, no retry on a later tick.
//!
//! A run a person stopped in Nova carries `STOPPED_NOTE` in its Airflow note and is never
//! announced; its claim records `stopped`, so the success after it is a plain success and
//! not a recovery.
//!
//! Without a state store, an engine or a cluster client the collector stays off.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::AppState;
use crate::airflow::{DagRun, dag_id_for};
use crate::destinations::{DeliveryOutcome, post_webhook};
use crate::pipelines::{PipelineResources, duration_seconds};

/// The note every run stopped in Nova carries; such a run is announced to nobody.
pub(crate) const STOPPED_NOTE: &str = "stopped in Nova";

/// The collector's first tick and its period.
const TICK: Duration = Duration::from_secs(30);
/// How long Nova waits before the second and the third attempt of one destination.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

/// `spec.notify`: the destination names of the pipeline's group, by kind of ending — a
/// failed run, a success, and the first success after a failure.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Notify {
    #[serde(default)]
    pub on_failure: Vec<String>,
    #[serde(default)]
    pub on_success: Vec<String>,
    #[serde(default)]
    pub on_recovered: Vec<String>,
}

/// Start the collector. Off — with one line, never a panic — unless the state store, the
/// engine and the cluster client are all configured.
pub fn spawn(state: Arc<AppState>) {
    if state.db.is_none() || state.airflow.is_none() || state.kube.is_none() {
        tracing::info!(
            "pipeline notices are off: the state store, the scheduler or the cluster client is not configured"
        );
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(TICK).await;
        let mut ticks = tokio::time::interval(TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if let Err(e) = run_once(&state).await {
                tracing::warn!(error = %e, "the pipeline notice tick failed");
            }
            // One `run_once` at a time: the tick is awaited in the loop, never overlapped.
            ticks.tick().await;
        }
    });
}

/// One pass over every pipeline of the namespace. The count is the pipelines a notice went
/// out for; a failed run with no destination left is claimed and counted zero.
pub(crate) async fn run_once(state: &AppState) -> Result<usize, String> {
    let (Some(db), Some(airflow), Some(kube)) = (&state.db, &state.airflow, &state.kube) else {
        return Ok(0);
    };
    let resources = PipelineResources::cluster(kube.clone());
    let pipelines = resources.list().await.map_err(|e| format!("{e:?}"))?;
    let namespace = crate::cluster::namespace();
    let base_domain = std::env::var("NOVA_BASE_DOMAIN").unwrap_or_default();
    let mut noticed = 0;
    for resource in pipelines {
        let Some(spec) = resource.get("spec") else {
            continue;
        };
        let Some(group) = spec["ownerGroup"].as_str() else {
            continue;
        };
        let Some(name) = resource["metadata"]["name"].as_str() else {
            continue;
        };
        let notify: Notify =
            serde_json::from_value(spec.get("notify").cloned().unwrap_or_default())
                .unwrap_or_default();
        if notify.on_failure.is_empty()
            && notify.on_success.is_empty()
            && notify.on_recovered.is_empty()
        {
            continue;
        }
        let dag_id = dag_id_for(&namespace, name);
        let runs = match airflow.runs(&dag_id, 1).await {
            Ok(runs) => runs,
            Err(e) => {
                tracing::warn!(pipeline = %name, error = %e, "the pipeline's runs did not answer");
                continue;
            }
        };
        let Some(run) = runs.first() else {
            continue;
        };
        // A run a person stopped is claimed as `stopped` even though it is announced to
        // nobody: `last_state` stays current, so the success after it is not a recovery.
        let stopped = run
            .note
            .as_deref()
            .is_some_and(|note| note.starts_with(STOPPED_NOTE));
        let claimed_state = if stopped {
            "stopped"
        } else {
            run.state.as_deref().unwrap_or("")
        };
        let claimed = match claim(db, &namespace, name, &run.dag_run_id, claimed_state).await {
            Ok(Some(claim)) => claim,
            // Already handled (or a crash lost it); either way this run is not sent again.
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(pipeline = %name, error = %e, "the run could not be claimed");
                continue;
            }
        };
        let Some(ending) = ending_for(
            &notify,
            &namespace,
            name,
            group,
            run,
            claimed.previous_state.as_deref(),
            &base_domain,
        ) else {
            continue;
        };
        let mut delivered = false;
        for destination in &ending.destinations {
            let found = sqlx::query_as::<_, (i64, String, Value)>(
                "SELECT id, kind, config FROM destination WHERE owner_group = $1 AND name = $2",
            )
            .bind(group)
            .bind(destination)
            .fetch_optional(db)
            .await;
            let (id, kind, config) = match found {
                Ok(Some(row)) => row,
                Ok(None) => {
                    tracing::warn!(
                        pipeline = %name,
                        destination = %destination,
                        "the destination named by spec.notify no longer exists"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(pipeline = %name, error = %e, "the destination did not answer");
                    continue;
                }
            };
            if kind != "webhook" {
                let outcome = DeliveryOutcome {
                    status: "failed",
                    http_status: None,
                    detail: "email delivery needs the platform's SMTP relay, which is not \
                             configured"
                        .into(),
                };
                if let Err(e) = record(db, id, &run.dag_run_id, &outcome).await {
                    tracing::warn!(pipeline = %name, error = %e, "the attempt was not recorded");
                }
                continue;
            }
            let Some(url) = config["url"].as_str() else {
                tracing::warn!(
                    pipeline = %name,
                    destination = %destination,
                    "the webhook destination has no url"
                );
                continue;
            };
            // Three attempts inside this tick, the first at once: a failed webhook is not
            // retried on a later tick, the delivery rows are the record.
            let mut attempts = 0;
            loop {
                attempts += 1;
                let outcome = post_webhook(&state.http, url, &ending.body).await;
                let ok = outcome.status == "delivered";
                if let Err(e) = record(db, id, &run.dag_run_id, &outcome).await {
                    tracing::warn!(pipeline = %name, error = %e, "the attempt was not recorded");
                }
                if ok {
                    delivered = true;
                    break;
                }
                let Some(delay) = RETRY_DELAYS.get(attempts - 1) else {
                    break;
                };
                tracing::debug!(
                    pipeline = %name,
                    attempt = attempts,
                    "the webhook did not deliver; retrying"
                );
                tokio::time::sleep(*delay).await;
            }
        }
        if delivered {
            noticed += 1;
        }
    }
    Ok(noticed)
}

/// What one run means to the pipeline's destinations: the names to tell and the document to
/// send. `None` when this run is announced to nobody.
struct Ending {
    destinations: Vec<String>,
    body: Value,
}

fn ending_for(
    notify: &Notify,
    namespace: &str,
    name: &str,
    group: &str,
    run: &DagRun,
    previous_state: Option<&str>,
    base_domain: &str,
) -> Option<Ending> {
    if run
        .note
        .as_deref()
        .is_some_and(|note| note.starts_with(STOPPED_NOTE))
    {
        return None;
    }
    let (state, recovered) = match run.state.as_deref()? {
        "failed" => ("failed", false),
        "success" => ("success", previous_state == Some("failed")),
        _ => return None,
    };
    // A recovery tells the people who asked for recoveries and the people who asked for every
    // success, once each; a plain success tells `onSuccess` only.
    let destinations = if recovered {
        unique(&notify.on_recovered, &notify.on_success)
    } else if state == "failed" {
        notify.on_failure.clone()
    } else {
        notify.on_success.clone()
    };
    if destinations.is_empty() {
        return None;
    }
    let text = match (state, recovered) {
        ("failed", _) => format!("Pipeline {name} failed: run {}", run.dag_run_id),
        (_, true) => format!(
            "Pipeline {name} recovered after a failure: run {}",
            run.dag_run_id
        ),
        _ => format!("Pipeline {name} succeeded: run {}", run.dag_run_id),
    };
    let mut body = json!({
        "text": text,
        "kind": "pipeline_run",
        "pipeline": name,
        "namespace": namespace,
        "group": group,
        "runId": run.dag_run_id,
        "state": state,
        "trigger": run.run_type.clone().unwrap_or_default(),
        "startedAt": run.start_date,
        "durationSeconds": duration_seconds(run.start_date.as_deref(), run.end_date.as_deref()),
        "apiPath": format!("/api/pipelines/{namespace}/{name}/runs/{}", run.dag_run_id),
    });
    if recovered {
        body["recovered"] = Value::Bool(true);
    }
    if !base_domain.is_empty() {
        body["runUrl"] = Value::String(format!(
            "https://nova.{base_domain}/pipelines/{namespace}/{name}"
        ));
    }
    Some(Ending { destinations, body })
}

/// The names of both lists, in order, each once (a name may be in both).
fn unique(first: &[String], second: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(first.len() + second.len());
    for name in first.iter().chain(second.iter()) {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// The state the previous run was announced with, or `None` for the first run of a pipeline.
struct Claim {
    previous_state: Option<String>,
}

/// Remember which run was announced, before anything is sent: `Some` means this is the first
/// time Nova sees it, `None` means it already handled that run id.
///
/// `state` is what the next claim compares against: the Airflow state, except that a run a
/// person stopped is recorded as `stopped`, so the success after it is not called a recovery.
async fn claim(
    pool: &PgPool,
    namespace: &str,
    pipeline: &str,
    run_id: &str,
    state: &str,
) -> Result<Option<Claim>, sqlx::Error> {
    let row = sqlx::query_as::<_, (Option<String>, i64)>(
        "WITH before AS ( \
             SELECT last_state FROM pipeline_notice \
              WHERE namespace = $1 AND pipeline = $2 \
         ), upsert AS ( \
             INSERT INTO pipeline_notice (namespace, pipeline, last_run_id, last_state, noticed_at) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (namespace, pipeline) DO UPDATE \
               SET last_run_id = EXCLUDED.last_run_id, \
                   last_state = EXCLUDED.last_state, \
                   noticed_at = EXCLUDED.noticed_at \
               WHERE pipeline_notice.last_run_id <> EXCLUDED.last_run_id \
             RETURNING 1 \
         ) \
         SELECT (SELECT last_state FROM before), (SELECT count(*) FROM upsert)",
    )
    .bind(namespace)
    .bind(pipeline)
    .bind(run_id)
    .bind(state)
    .bind(crate::jobs::now_ms() as i64)
    .fetch_one(pool)
    .await?;
    let (previous_state, claimed) = row;
    Ok((claimed > 0).then_some(Claim { previous_state }))
}

/// One `destination_delivery` row per attempt, tagged with the run it belonged to.
async fn record(
    pool: &PgPool,
    destination_id: i64,
    subject_id: &str,
    outcome: &DeliveryOutcome,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO destination_delivery \
         (destination_id, subject_kind, subject_id, status, http_status, detail, attempted_at) \
         VALUES ($1, 'pipeline_run', $2, $3, $4, $5, $6)",
    )
    .bind(destination_id)
    .bind(subject_id)
    .bind(outcome.status)
    .bind(outcome.http_status.map(i32::from))
    .bind(&outcome.detail)
    .bind(crate::jobs::now_ms() as i64)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notify_with(failure: &[&str], success: &[&str], recovered: &[&str]) -> Notify {
        Notify {
            on_failure: failure.iter().map(|s| s.to_string()).collect(),
            on_success: success.iter().map(|s| s.to_string()).collect(),
            on_recovered: recovered.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn run(state: &str, note: Option<&str>) -> DagRun {
        DagRun {
            dag_run_id: "manual__2026-11-12T03:00:00+00:00".into(),
            state: Some(state.into()),
            run_type: Some("manual".into()),
            logical_date: None,
            start_date: Some("2026-11-12T03:00:00+00:00".into()),
            end_date: Some("2026-11-12T03:00:42.500000+00:00".into()),
            note: note.map(str::to_string),
            triggered_by: None,
            triggering_user_name: None,
        }
    }

    #[test]
    fn a_stopped_run_is_not_announced() {
        let stopped = run("failed", Some("stopped in Nova by Ada"));
        let what = notify_with(&["on-call"], &["digest"], &["digest"]);
        assert!(
            ending_for(&what, "stardelt", "daily", "analytics", &stopped, None, "").is_none(),
            "a run a person stopped must not be announced"
        );
    }

    #[test]
    fn a_failed_run_tells_on_failure() {
        let failed = run("failed", None);
        let ending = ending_for(
            &notify_with(&["on-call"], &[], &[]),
            "stardelt",
            "daily",
            "analytics",
            &failed,
            None,
            "",
        )
        .expect("a failed run is announced to its failure list");
        assert_eq!(ending.destinations, ["on-call"]);
        assert_eq!(ending.body["state"], "failed");
        assert!(
            ending.body.get("recovered").is_none(),
            "a failure is not a recovery: {}",
            ending.body
        );
    }

    #[test]
    fn a_success_tells_on_success() {
        let success = run("success", None);
        let ending = ending_for(
            &notify_with(&[], &["digest"], &[]),
            "stardelt",
            "daily",
            "analytics",
            &success,
            None,
            "",
        )
        .expect("a success is announced to its success list");
        assert_eq!(ending.destinations, ["digest"]);
        assert_eq!(ending.body["state"], "success");
        assert!(
            ending.body.get("recovered").is_none(),
            "a first success is not a recovery: {}",
            ending.body
        );
    }

    #[test]
    fn a_success_after_a_failure_is_recovered_and_tells_both_lists_once() {
        let success = run("success", None);
        let ending = ending_for(
            &notify_with(&[], &["digest"], &["on-call", "digest"]),
            "stardelt",
            "daily",
            "analytics",
            &success,
            Some("failed"),
            "",
        )
        .expect("the first success after a failure is announced");
        assert_eq!(ending.destinations, ["on-call", "digest"]);
        assert_eq!(ending.body["recovered"], true);
        assert_eq!(ending.body["state"], "success");
        let text = ending.body["text"].as_str().expect("a text");
        assert!(
            text.starts_with("Pipeline daily recovered after a failure"),
            "{text}"
        );
    }

    #[test]
    fn nothing_to_tell_is_no_notice() {
        let failed = run("failed", None);
        assert!(
            ending_for(
                &notify_with(&[], &[], &[]),
                "stardelt",
                "daily",
                "analytics",
                &failed,
                None,
                ""
            )
            .is_none(),
            "a failure nobody asked about is announced to nobody"
        );
        let success = run("success", None);
        assert!(
            ending_for(
                &notify_with(&["on-call"], &[], &[]),
                "stardelt",
                "daily",
                "analytics",
                &success,
                Some("failed"),
                ""
            )
            .is_none(),
            "a recovery nobody asked about is announced to nobody"
        );
    }

    #[test]
    fn a_failed_run_is_noticed_once_with_the_facts() {
        let failed = run("failed", None);
        let body = ending_for(
            &notify_with(&["on-call"], &[], &[]),
            "stardelt",
            "daily",
            "analytics",
            &failed,
            None,
            "",
        )
        .expect("a failed run is announced")
        .body;
        assert_eq!(body["kind"], "pipeline_run");
        assert_eq!(body["pipeline"], "daily");
        assert_eq!(body["runId"], "manual__2026-11-12T03:00:00+00:00");
        assert_eq!(
            body["apiPath"],
            "/api/pipelines/stardelt/daily/runs/manual__2026-11-12T03:00:00+00:00"
        );
        assert_eq!(body["namespace"], "stardelt");
        assert_eq!(body["group"], "analytics");
        assert_eq!(body["state"], "failed");
        assert_eq!(body["trigger"], "manual");
        assert_eq!(body["durationSeconds"], 42.5);
        assert!(
            body.get("runUrl").is_none(),
            "no base domain, no link: {body}"
        );

        let linked = ending_for(
            &notify_with(&["on-call"], &[], &[]),
            "stardelt",
            "daily",
            "analytics",
            &failed,
            None,
            "dev.example.io",
        )
        .expect("a failed run is announced")
        .body;
        assert_eq!(
            linked["runUrl"],
            "https://nova.dev.example.io/pipelines/stardelt/daily"
        );
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn a_run_is_claimed_once() {
        let pool = crate::db::test_support::migrated_pool().await;
        let first = claim(&pool, "stardelt", "daily", "run-1", "failed")
            .await
            .expect("claim")
            .expect("the first sight of a run is claimed");
        assert_eq!(
            first.previous_state, None,
            "the first run of a pipeline has no state before it"
        );
        assert!(
            claim(&pool, "stardelt", "daily", "run-1", "failed")
                .await
                .expect("claim")
                .is_none(),
            "the same run is not sent twice"
        );
        let next = claim(&pool, "stardelt", "daily", "run-2", "success")
            .await
            .expect("claim")
            .expect("a new run is claimed");
        assert_eq!(
            next.previous_state.as_deref(),
            Some("failed"),
            "the state of the run the previous claim recorded"
        );
    }

    #[tokio::test]
    #[ignore = "needs NOVA_TEST_DATABASE_URL"]
    async fn an_attempt_is_recorded() {
        let pool = crate::db::test_support::migrated_pool().await;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO destination (owner_group, name, kind, config, created_at, updated_at) \
             VALUES ('analytics', 'on-call', 'webhook', $1, 0, 0) RETURNING id",
        )
        .bind(json!({ "url": "https://example.invalid/hook" }))
        .fetch_one(&pool)
        .await
        .expect("insert a destination");
        let outcome = DeliveryOutcome {
            status: "delivered",
            http_status: Some(204),
            detail: "HTTP 204 No Content".into(),
        };
        record(&pool, id, "run-1", &outcome).await.expect("record");
        let row: (String, Option<String>, String) = sqlx::query_as(
            "SELECT subject_kind, subject_id, status FROM destination_delivery \
             WHERE destination_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("read the delivery back");
        assert_eq!(row.0, "pipeline_run");
        assert_eq!(row.1.as_deref(), Some("run-1"));
        assert_eq!(row.2, "delivered");
    }
}
