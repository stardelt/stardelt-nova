//! Client for the platform's secret store (OpenBao in the sandbox; decisions D23 and D27).
//!
//! Nova signs in to the store with **the caller's own token** and reads or writes only the paths
//! the store's policies give that person (their personal prefix, the prefix of each of their
//! groups; administrators write managed values and never read any). A Nova bug can therefore not
//! widen a right, and the store's audit log names the person, not Nova.
//!
//! Values are never logged and never part of an error: an error carries the store's own first
//! message (which does not echo values) or a fixed text. [`StoreToken`] redacts its `Debug`.
//!
//! This module is the client only. The Connections API that uses it comes next, which is why the
//! module silences `dead_code` for now.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::auth::Secret;

/// Where the store is and how Nova signs in. Absent (`None`) when `NOVA_SECRET_STORE_URL` is unset:
/// every feature that needs the store must then answer 503, never panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfig {
    /// `http://openbao.stardelt.svc.cluster.local:8200`, no trailing slash.
    pub url: String,
    /// The JWT sign-in role (`NOVA_SECRET_STORE_ROLE`, default `platform-users`).
    pub role: String,
    /// The KV v2 mount (`NOVA_SECRET_STORE_MOUNT`, default `secret`).
    pub mount: String,
}

impl StoreConfig {
    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let url = get("NOVA_SECRET_STORE_URL")?
            .trim()
            .trim_end_matches('/')
            .to_string();
        if url.is_empty() {
            return None;
        }
        let word = |key: &str, default: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| {
                    !v.is_empty()
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                .unwrap_or_else(|| default.to_string())
        };
        Some(Self {
            url,
            role: word("NOVA_SECRET_STORE_ROLE", "platform-users"),
            mount: word("NOVA_SECRET_STORE_MOUNT", "secret"),
        })
    }

    pub fn from_env() -> Option<Self> {
        Self::from_env_with(|k| std::env::var(k).ok())
    }
}

/// Whose connection a path belongs to (the three scopes of decision D27).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreScope {
    /// One person's own connections: `connections/user/<sub>/`.
    Personal { sub: String },
    /// An owner group's connections: `connections/group/<group>/`.
    Group { group: String },
    /// Administrator-managed connections, write-only for humans: `connections/managed/`.
    Managed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// `NOVA_SECRET_STORE_URL` is not set.
    NotConfigured,
    /// The store refused: the sign-in (expired token, wrong client) or the path (permission denied).
    Refused(String),
    NotFound,
    /// The store did not answer, or answered with a server error.
    Unavailable(String),
    /// The request was refused before it was sent (a name or group that is not allowed in a path).
    Invalid(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotConfigured => write!(f, "the secret store is not configured"),
            StoreError::Refused(m) => write!(f, "the secret store refused the request: {m}"),
            StoreError::NotFound => write!(f, "no such connection secret"),
            StoreError::Unavailable(m) => write!(f, "the secret store is unavailable: {m}"),
            StoreError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

fn valid_sub(s: &str) -> bool {
    // No leading dot: `.` and `..` would be path segments of their own.
    !s.is_empty()
        && !s.starts_with('.')
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'@'))
}

fn valid_group(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The name rule of the `Connection` resource: 1 to 40 lower-case letters, digits and dashes,
/// starting with a letter, not ending with a dash.
pub fn valid_connection_name(s: &str) -> bool {
    let b = s.as_bytes();
    !s.is_empty()
        && s.len() <= 40
        && b[0].is_ascii_lowercase()
        && b[b.len() - 1] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// The folder (below the mount) that holds all connections of a scope.
pub fn scope_prefix(scope: &StoreScope) -> Result<String, StoreError> {
    match scope {
        StoreScope::Personal { sub } if valid_sub(sub) => Ok(format!("connections/user/{sub}")),
        StoreScope::Personal { .. } => Err(StoreError::Invalid(
            "the user id cannot be used in a store path".into(),
        )),
        StoreScope::Group { group } if valid_group(group) => {
            Ok(format!("connections/group/{group}"))
        }
        StoreScope::Group { .. } => Err(StoreError::Invalid(
            "the group name cannot be used in a store path".into(),
        )),
        StoreScope::Managed => Ok("connections/managed".to_string()),
    }
}

/// The path (below the mount) of one connection's secret. The same path the operator derives
/// (`ConnectionSpec::derived_remote_ref`), so what Nova writes is what the sync reads.
pub fn secret_path(scope: &StoreScope, name: &str) -> Result<String, StoreError> {
    if !valid_connection_name(name) {
        return Err(StoreError::Invalid(
            "a connection name is 1 to 40 lower-case letters, digits and dashes, starting with a letter".into(),
        ));
    }
    Ok(format!("{}/{name}", scope_prefix(scope)?))
}

/// What the store knows about a secret besides its fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMeta {
    pub version: u64,
    pub created_time: Option<String>,
    pub updated_time: Option<String>,
    /// Settings Nova stores beside the secret (type, non-secret settings). Never a secret field.
    pub custom_metadata: BTreeMap<String, String>,
}

/// A store token for one person, valid for minutes. `Debug` is redacted.
#[derive(Clone)]
pub struct StoreToken(Secret);

impl StoreToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(Secret::new(token))
    }
}

impl std::fmt::Debug for StoreToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreToken([redacted])")
    }
}

pub struct StoreClient {
    http: reqwest::Client,
    cfg: StoreConfig,
}

fn first_error(body: &Value) -> Option<String> {
    body["errors"]
        .as_array()
        .and_then(|e| e.first())
        .and_then(Value::as_str)
        .map(|s| s.chars().take(200).collect())
}

impl StoreClient {
    pub fn new(http: reqwest::Client, cfg: StoreConfig) -> Self {
        Self { http, cfg }
    }

    fn url(&self, kind: &str, path: &str) -> String {
        format!("{}/v1/{}/{kind}/{path}", self.cfg.url, self.cfg.mount)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        token: Option<&StoreToken>,
    ) -> Result<(u16, Value), StoreError> {
        let request = match token {
            Some(t) => request.header("X-Vault-Token", t.0.expose()),
            None => request,
        };
        let response = request
            .send()
            .await
            .map_err(|e| StoreError::Unavailable(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| StoreError::Unavailable(e.without_url().to_string()))?;
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status, body))
    }

    fn check(status: u16, body: &Value) -> Result<(), StoreError> {
        match status {
            200..=299 => Ok(()),
            404 => Err(StoreError::NotFound),
            400 | 401 | 403 => Err(StoreError::Refused(
                first_error(body).unwrap_or_else(|| "permission denied".into()),
            )),
            _ => Err(StoreError::Unavailable(format!("HTTP {status}"))),
        }
    }

    /// Sign in with the person's own platform token; the store applies the policies of that
    /// person's groups to the token it returns.
    pub async fn login(&self, user_token: &Secret) -> Result<StoreToken, StoreError> {
        let url = format!("{}/v1/auth/jwt/login", self.cfg.url);
        let request = self
            .http
            .post(url)
            .json(&json!({ "role": self.cfg.role, "jwt": user_token.expose() }));
        let (status, body) = self.send(request, None).await?;
        Self::check(status, &body)?;
        body["auth"]["client_token"]
            .as_str()
            .map(StoreToken::new)
            .ok_or_else(|| StoreError::Unavailable("the sign-in answered without a token".into()))
    }

    /// Write the secret fields (a new version that replaces the old one) and the non-secret
    /// settings beside them.
    pub async fn put(
        &self,
        token: &StoreToken,
        path: &str,
        fields: &BTreeMap<String, String>,
        custom_metadata: &BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        let request = self
            .http
            .post(self.url("data", path))
            .json(&json!({ "data": fields }));
        let (status, body) = self.send(request, Some(token)).await?;
        Self::check(status, &body)?;
        if !custom_metadata.is_empty() {
            let request = self
                .http
                .post(self.url("metadata", path))
                .json(&json!({ "custom_metadata": custom_metadata }));
            let (status, body) = self.send(request, Some(token)).await?;
            Self::check(status, &body)?;
        }
        Ok(())
    }

    /// The secret fields. Only the policies decide who may call this: a person for their own and
    /// their groups' paths; nobody for managed ones.
    pub async fn read(
        &self,
        token: &StoreToken,
        path: &str,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        let (status, body) = self
            .send(self.http.get(self.url("data", path)), Some(token))
            .await?;
        Self::check(status, &body)?;
        let fields = body["data"]["data"]
            .as_object()
            .ok_or(StoreError::NotFound)?;
        Ok(fields
            .iter()
            .map(|(k, v)| {
                let text = v
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string());
                (k.clone(), text)
            })
            .collect())
    }

    /// Version, times and settings, without any secret field (administrators may call this on
    /// every connection).
    pub async fn metadata(&self, token: &StoreToken, path: &str) -> Result<SecretMeta, StoreError> {
        let (status, body) = self
            .send(self.http.get(self.url("metadata", path)), Some(token))
            .await?;
        Self::check(status, &body)?;
        let data = &body["data"];
        let custom_metadata = data["custom_metadata"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let time = |key: &str| {
            data[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Ok(SecretMeta {
            version: data["current_version"].as_u64().unwrap_or(0),
            created_time: time("created_time"),
            updated_time: time("updated_time"),
            custom_metadata,
        })
    }

    /// The connection names below a scope's prefix (folders are not connections and are dropped).
    /// An empty or missing prefix is an empty list.
    pub async fn list(&self, token: &StoreToken, prefix: &str) -> Result<Vec<String>, StoreError> {
        let request = self
            .http
            .get(self.url("metadata", prefix))
            .query(&[("list", "true")]);
        let (status, body) = self.send(request, Some(token)).await?;
        match Self::check(status, &body) {
            Ok(()) => {}
            Err(StoreError::NotFound) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        }
        Ok(body["data"]["keys"]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .filter(|k| !k.ends_with('/'))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Destroy the secret with all its versions.
    pub async fn destroy(&self, token: &StoreToken, path: &str) -> Result<(), StoreError> {
        let (status, body) = self
            .send(self.http.delete(self.url("metadata", path)), Some(token))
            .await?;
        Self::check(status, &body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Json, Router,
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn the_config_needs_a_url_and_has_defaults() {
        assert_eq!(StoreConfig::from_env_with(env(&[])), None);
        assert_eq!(
            StoreConfig::from_env_with(env(&[("NOVA_SECRET_STORE_URL", "  ")])),
            None
        );
        let cfg = StoreConfig::from_env_with(env(&[(
            "NOVA_SECRET_STORE_URL",
            "http://openbao.stardelt.svc:8200/",
        )]))
        .unwrap();
        assert_eq!(cfg.url, "http://openbao.stardelt.svc:8200");
        assert_eq!(
            (cfg.role.as_str(), cfg.mount.as_str()),
            ("platform-users", "secret")
        );
        let cfg = StoreConfig::from_env_with(env(&[
            ("NOVA_SECRET_STORE_URL", "http://x"),
            ("NOVA_SECRET_STORE_ROLE", "other-role"),
            ("NOVA_SECRET_STORE_MOUNT", "bad/mount"),
        ]))
        .unwrap();
        assert_eq!(cfg.role, "other-role");
        assert_eq!(
            cfg.mount, "secret",
            "a mount with a slash falls back to the default"
        );
    }

    #[test]
    fn paths_follow_the_scope_and_match_the_operator() {
        let me = StoreScope::Personal {
            sub: "3f2a-uuid".into(),
        };
        let team = StoreScope::Group {
            group: "analytics".into(),
        };
        assert_eq!(
            secret_path(&me, "pat").unwrap(),
            "connections/user/3f2a-uuid/pat"
        );
        assert_eq!(
            secret_path(&team, "crm-db").unwrap(),
            "connections/group/analytics/crm-db"
        );
        assert_eq!(
            secret_path(&StoreScope::Managed, "sftp").unwrap(),
            "connections/managed/sftp"
        );
        assert_eq!(scope_prefix(&team).unwrap(), "connections/group/analytics");
    }

    #[test]
    fn hostile_names_never_reach_a_path() {
        for name in [
            "",
            "Crm",
            "1crm",
            "a/b",
            "..",
            "a b",
            "crm-",
            &"a".repeat(41),
        ] {
            assert!(
                matches!(
                    secret_path(&StoreScope::Managed, name),
                    Err(StoreError::Invalid(_))
                ),
                "{name}"
            );
        }
        for group in ["", "a/b", "..", "../finance", "-x", "a b", &"g".repeat(65)] {
            let scope = StoreScope::Group {
                group: group.into(),
            };
            assert!(
                matches!(secret_path(&scope, "crm"), Err(StoreError::Invalid(_))),
                "{group}"
            );
        }
        for sub in ["", "a/b", "..", "a b"] {
            let scope = StoreScope::Personal { sub: sub.into() };
            assert!(
                matches!(secret_path(&scope, "crm"), Err(StoreError::Invalid(_))),
                "{sub}"
            );
        }
    }

    #[test]
    fn a_store_token_never_prints() {
        let t = StoreToken::new("hvs.super-secret");
        assert!(!format!("{t:?}").contains("super-secret"));
    }

    // --- a fake store: the few routes the client uses, with the answers the real one gives ---------

    #[derive(Default)]
    struct Fake {
        data: BTreeMap<String, BTreeMap<String, String>>,
        custom: BTreeMap<String, BTreeMap<String, String>>,
        versions: BTreeMap<String, u64>,
    }

    type Shared = Arc<Mutex<Fake>>;

    fn allowed(headers: &HeaderMap, path: &str) -> Result<(), (StatusCode, Json<Value>)> {
        let ok = headers.get("x-vault-token").and_then(|v| v.to_str().ok()) == Some("tok-alice")
            && !path.starts_with("connections/group/finance");
        if ok {
            Ok(())
        } else {
            Err((
                StatusCode::FORBIDDEN,
                Json(json!({ "errors": ["permission denied"] })),
            ))
        }
    }

    async fn fake_login(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        match body["jwt"].as_str() {
            Some("good") => (
                StatusCode::OK,
                Json(json!({ "auth": { "client_token": "tok-alice", "lease_duration": 900 } })),
            ),
            _ => (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({ "errors": ["claim \"azp\" does not match any associated bound claim values"] }),
                ),
            ),
        }
    }

    async fn fake_put(
        State(s): State<Shared>,
        Path(path): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<StatusCode, (StatusCode, Json<Value>)> {
        allowed(&headers, &path)?;
        let fields: BTreeMap<String, String> =
            serde_json::from_value(body["data"].clone()).unwrap();
        let mut s = s.lock().unwrap();
        s.data.insert(path.clone(), fields);
        *s.versions.entry(path).or_insert(0) += 1;
        Ok(StatusCode::OK)
    }

    async fn fake_read(
        State(s): State<Shared>,
        Path(path): Path<String>,
        headers: HeaderMap,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        allowed(&headers, &path)?;
        let s = s.lock().unwrap();
        match s.data.get(&path) {
            Some(d) => Ok(Json(
                json!({ "data": { "data": d, "metadata": { "version": 1 } } }),
            )),
            None => Err((StatusCode::NOT_FOUND, Json(json!({ "errors": [] })))),
        }
    }

    async fn fake_meta_set(
        State(s): State<Shared>,
        Path(path): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Result<StatusCode, (StatusCode, Json<Value>)> {
        allowed(&headers, &path)?;
        let custom: BTreeMap<String, String> =
            serde_json::from_value(body["custom_metadata"].clone()).unwrap();
        s.lock().unwrap().custom.insert(path, custom);
        Ok(StatusCode::NO_CONTENT)
    }

    async fn fake_meta_get(
        State(s): State<Shared>,
        Path(path): Path<String>,
        Query(q): Query<BTreeMap<String, String>>,
        headers: HeaderMap,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        allowed(&headers, &path)?;
        let s = s.lock().unwrap();
        if q.contains_key("list") {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            let mut keys: Vec<String> = s
                .data
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix))
                .map(|rest| match rest.split_once('/') {
                    Some((dir, _)) => format!("{dir}/"),
                    None => rest.to_string(),
                })
                .collect();
            keys.dedup();
            if keys.is_empty() {
                return Err((StatusCode::NOT_FOUND, Json(json!({ "errors": [] }))));
            }
            return Ok(Json(json!({ "data": { "keys": keys } })));
        }
        match s.versions.get(&path) {
            Some(v) => Ok(Json(json!({ "data": {
                "current_version": v,
                "created_time": "2026-10-19T10:00:00Z",
                "updated_time": "2026-10-19T11:00:00Z",
                "custom_metadata": s.custom.get(&path),
            }}))),
            None => Err((StatusCode::NOT_FOUND, Json(json!({ "errors": [] })))),
        }
    }

    async fn fake_delete(
        State(s): State<Shared>,
        Path(path): Path<String>,
        headers: HeaderMap,
    ) -> Result<StatusCode, (StatusCode, Json<Value>)> {
        allowed(&headers, &path)?;
        let mut s = s.lock().unwrap();
        s.data.remove(&path);
        s.custom.remove(&path);
        s.versions.remove(&path);
        Ok(StatusCode::NO_CONTENT)
    }

    async fn start_fake() -> StoreClient {
        let shared: Shared = Arc::default();
        let app = Router::new()
            .route("/v1/auth/jwt/login", post(fake_login))
            .route("/v1/secret/data/{*path}", post(fake_put).get(fake_read))
            .route(
                "/v1/secret/metadata/{*path}",
                get(fake_meta_get).post(fake_meta_set).delete(fake_delete),
            )
            .with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        StoreClient::new(
            reqwest::Client::new(),
            StoreConfig {
                url: format!("http://{addr}"),
                role: "platform-users".into(),
                mount: "secret".into(),
            },
        )
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn a_person_signs_in_and_a_refused_sign_in_says_why_without_a_token() {
        let store = start_fake().await;
        let token = store.login(&Secret::new("good")).await.unwrap();
        assert!(!format!("{token:?}").contains("tok-alice"));
        match store.login(&Secret::new("issued-to-another-client")).await {
            Err(StoreError::Refused(m)) => {
                assert!(m.contains("azp"));
                assert!(!m.contains("issued-to-another-client"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_read_list_describe_and_destroy_round_trip() {
        let store = start_fake().await;
        let token = store.login(&Secret::new("good")).await.unwrap();
        let team = StoreScope::Group {
            group: "analytics".into(),
        };
        let path = secret_path(&team, "crm-db").unwrap();

        store
            .put(
                &token,
                &path,
                &map(&[("user", "etl"), ("password", "p1")]),
                &map(&[("type", "sqlserver"), ("host", "crm.example.com")]),
            )
            .await
            .unwrap();
        assert_eq!(store.read(&token, &path).await.unwrap()["password"], "p1");

        // a rotation is a new version
        store
            .put(
                &token,
                &path,
                &map(&[("user", "etl"), ("password", "p2")]),
                &BTreeMap::new(),
            )
            .await
            .unwrap();
        let meta = store.metadata(&token, &path).await.unwrap();
        assert_eq!(meta.version, 2);
        assert_eq!(meta.custom_metadata["type"], "sqlserver");
        assert_eq!(meta.updated_time.as_deref(), Some("2026-10-19T11:00:00Z"));
        assert!(!format!("{meta:?}").contains("p2"));

        let other = secret_path(&team, "web-api").unwrap();
        store
            .put(&token, &other, &map(&[("token", "t")]), &BTreeMap::new())
            .await
            .unwrap();
        let mut names = store
            .list(&token, &scope_prefix(&team).unwrap())
            .await
            .unwrap();
        names.sort();
        assert_eq!(names, ["crm-db", "web-api"]);
        assert!(
            store
                .list(&token, "connections/group/nobody")
                .await
                .unwrap()
                .is_empty()
        );

        store.destroy(&token, &path).await.unwrap();
        assert_eq!(store.read(&token, &path).await, Err(StoreError::NotFound));
        assert_eq!(
            store.metadata(&token, &path).await,
            Err(StoreError::NotFound)
        );
    }

    #[tokio::test]
    async fn another_groups_path_is_refused_by_the_store_not_by_nova() {
        let store = start_fake().await;
        let token = store.login(&Secret::new("good")).await.unwrap();
        let path = secret_path(
            &StoreScope::Group {
                group: "finance".into(),
            },
            "ledger",
        )
        .unwrap();
        match store.read(&token, &path).await {
            Err(StoreError::Refused(m)) => assert_eq!(m, "permission denied"),
            other => panic!("expected Refused, got {other:?}"),
        }
        assert!(matches!(
            store
                .put(&token, &path, &map(&[("x", "y")]), &BTreeMap::new())
                .await,
            Err(StoreError::Refused(_))
        ));
    }

    #[tokio::test]
    async fn a_store_that_is_down_is_unavailable_and_the_error_has_no_url() {
        let store = StoreClient::new(
            reqwest::Client::new(),
            StoreConfig {
                url: "http://127.0.0.1:9".into(),
                role: "r".into(),
                mount: "secret".into(),
            },
        );
        match store.login(&Secret::new("good")).await {
            Err(StoreError::Unavailable(m)) => assert!(!m.contains("127.0.0.1")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// Against a real store (OpenBao dev mode, KV v2 at `secret/`):
    /// `NOVA_TEST_BAO_URL=http://127.0.0.1:18200 NOVA_TEST_BAO_TOKEN=<root token of the dev server> cargo test secret_store -- --ignored`
    /// Without the two variables the test prints "skipped" and passes, so `cargo test -- --ignored` in CI stays green.
    #[tokio::test]
    #[ignore = "needs a running OpenBao"]
    async fn the_client_works_against_a_real_store() {
        let (Ok(url), Ok(token)) = (
            std::env::var("NOVA_TEST_BAO_URL"),
            std::env::var("NOVA_TEST_BAO_TOKEN"),
        ) else {
            // CI runs every ignored test and has no store: without the variables there is nothing to prove.
            eprintln!("skipped: set NOVA_TEST_BAO_URL and NOVA_TEST_BAO_TOKEN to run this test");
            return;
        };
        let store = StoreClient::new(
            reqwest::Client::new(),
            StoreConfig {
                url,
                role: "platform-users".into(),
                mount: "secret".into(),
            },
        );
        let token = StoreToken::new(token);
        let path = secret_path(
            &StoreScope::Group {
                group: "nova-it".into(),
            },
            "demo",
        )
        .unwrap();
        store
            .put(
                &token,
                &path,
                &map(&[("user", "u"), ("password", "p")]),
                &map(&[("type", "generic")]),
            )
            .await
            .unwrap();
        assert_eq!(store.read(&token, &path).await.unwrap()["user"], "u");
        let meta = store.metadata(&token, &path).await.unwrap();
        assert!(meta.version >= 1);
        assert_eq!(meta.custom_metadata["type"], "generic");
        assert!(
            store
                .list(&token, "connections/group/nova-it")
                .await
                .unwrap()
                .contains(&"demo".to_string())
        );
        store.destroy(&token, &path).await.unwrap();
        assert_eq!(store.read(&token, &path).await, Err(StoreError::NotFound));
    }
}
