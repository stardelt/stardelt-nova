//! Stage comparison (internal#106 decision 9, slice a of nova#660): what this stage runs against
//! what a peer stage runs. One read, the caller's own token, no stored secret.
//!
//! - **The peer is server configuration**, `NOVA_PEER_STAGES=prod=https://nova.prod.example[,…]`.
//!   A browser never supplies a URL, so this cannot be turned into an SSRF probe.
//! - **The caller's token, not Nova's.** `GET {peer}/api/pipelines` is called with the access
//!   token the caller presented here. Prod filters the answer by that person's groups; if prod
//!   refuses the token (another identity provider) the answer is `status: "unauthorized"` and the
//!   UI says "Sign in to prod to compare" — never an empty stage that looks like "prod has nothing".
//! - **No ahead/behind.** Nova has no Git ancestry; it reports each stage's revision and lets the
//!   page compare them (`modified` says someone changed the pipeline after its deploy).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;
use crate::auth::{Identity, Principal};

/// One configured peer stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Peer {
    /// The name the caller passes as `?peer=`; 1 to 24 lower-case letters, digits and dashes.
    pub name: String,
    /// The peer's base URL, without a trailing slash.
    pub url: String,
}

/// One deployed pipeline of a stage, as the comparison needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageRow {
    pub bundle: String,
    pub target: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The bundle's repository, so the page can link the pull request that promotes a commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageComparison {
    pub peer: String,
    pub peer_url: String,
    /// `ok`, `unauthorized` or `unreachable`.
    pub status: String,
    /// One sentence for the page when `status` is not `ok`; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// This stage's deployed pipelines.
    pub here: Vec<StageRow>,
    /// The peer's, empty when `status` is not `ok`.
    pub there: Vec<StageRow>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CompareQuery {
    /// `None` answers a 400 with a sentence instead of axum's plain-text 400.
    pub peer: Option<String>,
}

fn bad(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// `NOVA_PEER_STAGES`, e.g. `prod=https://nova.prod.example,test=https://nova.test.example`.
/// An entry without a valid name or an `http(s)://` URL is skipped with a warning: a typo in
/// the variable must not stop Nova, and an unreachable peer must show as *unreachable*, not as
/// a stage with nothing deployed.
pub fn peers_from_env() -> Vec<Peer> {
    let raw = std::env::var("NOVA_PEER_STAGES").unwrap_or_default();
    parse_peers(&raw)
}

fn parse_peers(raw: &str) -> Vec<Peer> {
    let mut peers: Vec<Peer> = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((name, url)) = entry.split_once('=') else {
            tracing::warn!(entry, "NOVA_PEER_STAGES: expected name=url, skipped");
            continue;
        };
        let name = name.trim();
        let url = url.trim().trim_end_matches('/');
        let name_ok = !name.is_empty()
            && name.len() <= 24
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !name_ok || !(url.starts_with("http://") || url.starts_with("https://")) {
            tracing::warn!(entry, "NOVA_PEER_STAGES: bad stage name or URL, skipped");
            continue;
        }
        if peers.iter().any(|peer| peer.name == name) {
            tracing::warn!(name, "NOVA_PEER_STAGES: duplicate stage name, skipped");
            continue;
        }
        peers.push(Peer {
            name: name.to_string(),
            url: url.to_string(),
        });
    }
    peers
}

/// The peer's `/api/pipelines` answer, reduced to its deployed pipelines.
fn rows_of_peer(payload: &serde_json::Value) -> Result<Vec<StageRow>, String> {
    let pipelines = payload
        .get("pipelines")
        .and_then(serde_json::Value::as_array)
        .ok_or("the peer answered something that is not a pipeline list")?;
    let mut rows = Vec::new();
    for pipeline in pipelines {
        let Some(deployment) = pipeline.get("deployment") else {
            continue;
        };
        let text = |value: Option<&serde_json::Value>| {
            value
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let (Some(bundle), Some(target), Some(name)) = (
            text(deployment.get("bundle")),
            text(deployment.get("target")),
            text(pipeline.get("name")),
        ) else {
            continue;
        };
        rows.push(StageRow {
            bundle,
            target,
            name,
            revision: text(deployment.get("revision")),
            repository: text(deployment.get("repository")),
            modified: deployment
                .get("modified")
                .and_then(serde_json::Value::as_bool),
        });
    }
    rows.sort_by(|a, b| (&a.bundle, &a.target, &a.name).cmp(&(&b.bundle, &b.target, &b.name)));
    Ok(rows)
}

#[utoipa::path(
    get,
    operation_id = "stages_compare",
    path = "/api/stages/compare",
    tag = "public",
    params(("peer" = String, Query, description = "The peer stage's configured name")),
    description = "This stage's deployed pipelines against a peer stage's, for the Deployments page. The peer is called with the caller's own token (`GET {peer}/api/pipelines`), so the answer is filtered by that person's rights there. `status` is `ok`, `unauthorized` (the peer refused the token — sign in there) or `unreachable` (no answer within 5 s). Nova neither compares Git ancestry nor deploys to the peer. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "Both stages' deployed pipelines", body = StageComparison),
        (status = 400, description = "No `peer` parameter", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No stage with that name is configured", body = crate::openapi::ErrorResponse),
        (status = 401, description = "No signed-in caller, so there is no token to forward", body = crate::openapi::ErrorResponse),
        (status = 502, description = "This stage's cluster did not answer", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn compare(
    State(s): State<Arc<AppState>>,
    Query(query): Query<CompareQuery>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let Some(wanted) = query.peer.as_deref().filter(|p| !p.is_empty()) else {
        return bad(
            StatusCode::BAD_REQUEST,
            "stage comparison needs a peer, e.g. ?peer=prod",
        );
    };
    let Some(peer) = s.stages.iter().find(|peer| peer.name == wanted) else {
        let known = s
            .stages
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return bad(
            StatusCode::NOT_FOUND,
            &if known.is_empty() {
                format!(
                    "no stage named '{wanted}': this Nova has no peer stages configured (NOVA_PEER_STAGES)"
                )
            } else {
                format!("no stage named '{wanted}': this Nova knows {known}")
            },
        );
    };
    let Some(token) = principal.as_ref().and_then(|p| p.access_token.as_ref()) else {
        return bad(
            StatusCode::UNAUTHORIZED,
            "comparing stages forwards your own token to the peer, so it needs a signed-in caller",
        );
    };
    let who: Identity = match principal.as_ref() {
        Some(Extension(principal)) => principal.identity.clone(),
        None => {
            return bad(
                StatusCode::UNAUTHORIZED,
                "comparing stages needs a signed-in caller",
            );
        }
    };

    let here = match crate::pipelines::deployed_rows(&s, &who).await {
        Ok(rows) => rows,
        Err(message) => {
            tracing::warn!(error = %message, "stage comparison could not read this stage");
            return bad(StatusCode::BAD_GATEWAY, "the cluster did not answer");
        }
    };

    let mut comparison = StageComparison {
        peer: peer.name.clone(),
        peer_url: peer.url.clone(),
        status: "ok".to_string(),
        detail: None,
        here,
        there: Vec::new(),
    };
    let answer = s
        .http
        .get(format!("{}/api/pipelines", peer.url))
        .bearer_auth(token.expose())
        .timeout(std::time::Duration::from_secs(5))
        .header("accept", "application/json")
        .send()
        .await;
    match answer {
        Ok(response)
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                || response.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            comparison.status = "unauthorized".to_string();
            comparison.detail = Some(format!(
                "sign in to {} to compare: it refused this session's token",
                peer.name
            ));
        }
        Ok(response) if response.status().is_success() => {
            match response.json::<serde_json::Value>().await {
                Ok(payload) => match rows_of_peer(&payload) {
                    Ok(rows) => comparison.there = rows,
                    Err(message) => {
                        tracing::warn!(peer = %peer.name, error = %message, "stage comparison could not read the peer");
                        comparison.status = "unreachable".to_string();
                        comparison.detail = Some(format!(
                            "{} answered something Nova does not understand",
                            peer.name
                        ));
                    }
                },
                Err(error) => {
                    tracing::warn!(peer = %peer.name, error = %error, "stage comparison could not read the peer");
                    comparison.status = "unreachable".to_string();
                    comparison.detail = Some(format!(
                        "{} answered something Nova does not understand",
                        peer.name
                    ));
                }
            }
        }
        Ok(response) => {
            tracing::warn!(peer = %peer.name, status = %response.status(), "stage comparison: the peer refused");
            comparison.status = "unreachable".to_string();
            comparison.detail = Some(format!("{} answered {}", peer.name, response.status()));
        }
        Err(error) => {
            tracing::warn!(peer = %peer.name, error = %error, "stage comparison: the peer did not answer");
            comparison.status = "unreachable".to_string();
            comparison.detail = Some(format!("{} did not answer within 5 seconds", peer.name));
        }
    }
    Json(comparison).into_response()
}

/// The compare route, uncacheable like the pipeline list it reads.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stages/compare", get(compare))
        .route_layer(axum::middleware::from_fn(crate::pipelines::no_store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn peers_are_name_url_pairs_and_bad_entries_are_skipped() {
        let peers = parse_peers(
            "prod=https://nova.prod.example/, test = http://127.0.0.1:8080 ,prod=http://dup.example,bad,ftp://x.example,=https://y.example",
        );
        assert_eq!(
            peers,
            vec![
                Peer {
                    name: "prod".into(),
                    url: "https://nova.prod.example".into()
                },
                Peer {
                    name: "test".into(),
                    url: "http://127.0.0.1:8080".into()
                },
            ]
        );
    }

    #[test]
    fn an_empty_variable_configures_no_peer() {
        assert!(parse_peers("").is_empty());
        assert!(parse_peers("  ,  ").is_empty());
    }

    #[test]
    fn the_peer_answer_keeps_the_deployed_pipelines_only() {
        let payload = json!({
            "pipelines": [
                {"name": "a__one", "state": "idle", "deployment": {"bundle": "shop", "target": "prod", "revision": "0123abc", "modified": true}},
                {"name": "b__two", "state": "idle"},
                {"name": "c__three", "deployment": {"bundle": "shop", "target": "prod"}}
            ]
        });
        let rows = rows_of_peer(&payload).expect("a pipeline list");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "a__one");
        assert_eq!(rows[0].revision.as_deref(), Some("0123abc"));
        assert_eq!(rows[0].modified, Some(true));
        assert_eq!(rows[1].revision, None);
    }

    #[test]
    fn a_body_that_is_not_a_pipeline_list_is_an_error() {
        assert!(rows_of_peer(&json!({"detail": "nope"})).is_err());
        assert!(rows_of_peer(&json!([])).is_err());
    }
}
