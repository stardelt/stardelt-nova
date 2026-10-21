//! The service token Nova sends to Lakekeeper (`NOVA_CATALOG_*`).
//!
//! Catalog sign-in, step 3 of 5 (stardelt-nova#222; `design/multitenant-catalogs.md`
//! §7.2 P3). Nova authenticates to Lakekeeper **as itself** — the confidential client
//! `svc-catalog-nova` from stardelt-platform#282 — with an OAuth2 client-credentials
//! token whose audience is `lakekeeper`. Lakekeeper's own enforcement is a later step:
//! today it accepts everything (`allowall`), so this changes nothing observable until
//! then, and unset variables leave every call exactly as it was.
//!
//! - All three variables (`NOVA_CATALOG_TOKEN_URL`, `NOVA_CATALOG_CLIENT_ID`,
//!   `NOVA_CATALOG_CLIENT_SECRET`) or none. A partial configuration is a startup error,
//!   not a silent fallback to calling the catalog unauthenticated.
//! - One token in memory, reused until 60 s before `exp`; a caller that arrives while a
//!   fetch is running waits for that fetch instead of starting a second one.
//! - Neither the client secret nor a token is ever logged, returned or part of an error.
//!   Both live in [`Secret`], whose `Debug` prints `[redacted]`, and every sentence in
//!   [`TokenError`] is fixed text: no response body reaches it (a proxy in the middle can
//!   echo anything, including the credentials) and no URL is repeated.

use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::auth::{Secret, sanitize_code};

/// Renew this long before `exp`, so a request cannot race the expiry.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Where the token comes from and which client asks for it.
#[derive(Clone, Debug)]
pub struct CatalogTokenConfig {
    /// The realm's token endpoint, e.g.
    /// `https://id.<domain>/realms/stardelt/protocol/openid-connect/token`.
    pub token_url: String,
    /// `svc-catalog-nova`. Not a secret: it is the same in every install.
    pub client_id: String,
    /// That client's secret. `Debug` prints `[redacted]` (see [`Secret`]).
    pub client_secret: Secret,
}

impl CatalogTokenConfig {
    /// The three `NOVA_CATALOG_*` variables, all or none.
    ///
    /// Empty values count as unset, so a chart that renders an empty string leaves the
    /// feature off exactly as an unset variable does. The secret is taken byte for byte —
    /// a leading or trailing space can be part of it.
    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let word = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let client_id = word("NOVA_CATALOG_CLIENT_ID");
        let token_url = word("NOVA_CATALOG_TOKEN_URL");
        let client_secret = get("NOVA_CATALOG_CLIENT_SECRET").filter(|v| !v.is_empty());

        match (client_id, client_secret, token_url) {
            (None, None, None) => Ok(None),
            (Some(client_id), Some(client_secret), Some(token_url)) => {
                let parsed = url::Url::parse(&token_url)
                    .map_err(|_| "NOVA_CATALOG_TOKEN_URL is not a URL".to_string())?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err("NOVA_CATALOG_TOKEN_URL must be an http(s) URL".into());
                }
                Ok(Some(Self {
                    token_url,
                    client_id,
                    client_secret: Secret::new(client_secret),
                }))
            }
            _ => Err("NOVA_CATALOG_CLIENT_ID, NOVA_CATALOG_CLIENT_SECRET and \
                      NOVA_CATALOG_TOKEN_URL must be set together"
                .into()),
        }
    }

    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_env_with(|k| std::env::var(k).ok())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why the service token could not be obtained.
///
/// The sentences are fixed text: no secret, no token, no response body, no URL. A caller
/// may show them (the catalog proxy answers them as a 502), so they have to stay safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The endpoint did not answer at all (DNS, TCP, TLS).
    Unreachable,
    /// It answered non-2xx. Carries the status and the RFC 6749 `error` code only.
    Refused(u16, String),
    /// It answered 2xx without the fields a token response must have.
    Malformed,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Unreachable => f.write_str("the catalog token endpoint is not reachable"),
            TokenError::Refused(status, code) if code.is_empty() => {
                write!(
                    f,
                    "the catalog token endpoint refused the client ({status})"
                )
            }
            TokenError::Refused(status, code) => write!(
                f,
                "the catalog token endpoint refused the client ({status} {code})"
            ),
            TokenError::Malformed => {
                f.write_str("the catalog token endpoint answered with an unexpected body")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// One cached client-credentials token and the client that fetches it.
pub struct CatalogToken {
    config: CatalogTokenConfig,
    http: reqwest::Client,
    /// Guarded across the fetch on purpose: a caller that arrives while a fetch is
    /// running waits for that attempt instead of starting a second one.
    cached: Mutex<Option<CachedToken>>,
}

#[derive(Debug)]
struct CachedToken {
    token: Secret,
    /// `exp - 60 s`: from here on the next call fetches a new token.
    refresh_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Seconds the token is valid. Required: without it Nova cannot know when to renew,
    /// and a token cached forever is worse than a loud failure.
    expires_in: u64,
}

impl CatalogToken {
    /// The client and its configuration. `http` is the caller's client (Nova's shared
    /// upstream one, so the platform CA is trusted); its timeouts, if any, govern the
    /// token request too.
    pub fn new(http: reqwest::Client, config: CatalogTokenConfig) -> Self {
        Self {
            config,
            http,
            cached: Mutex::new(None),
        }
    }

    /// `Authorization: Bearer <token>` — the value every Lakekeeper request carries.
    pub async fn bearer(&self) -> Result<String, TokenError> {
        Ok(format!("Bearer {}", self.token().await?.expose()))
    }

    /// The token, fetched once and reused until it is within [`REFRESH_MARGIN`] of `exp`.
    pub async fn token(&self) -> Result<Secret, TokenError> {
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.as_ref()
            && Instant::now() < entry.refresh_at
        {
            return Ok(entry.token.clone());
        }
        let fresh = self.fetch().await?;
        let token = fresh.token.clone();
        *cached = Some(fresh);
        Ok(token)
    }

    async fn fetch(&self) -> Result<CachedToken, TokenError> {
        let response = self
            .http
            .post(&self.config.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.expose()),
            ])
            .send()
            .await
            .map_err(|_| TokenError::Unreachable)?;

        let status = response.status();
        if !status.is_success() {
            // The RFC 6749 error code only. The body is deliberately not read for
            // anything else: it is upstream text, and an intermediary may echo the
            // request (credentials included) into it.
            let code = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(sanitize_code))
                .unwrap_or_default();
            return Err(TokenError::Refused(status.as_u16(), code));
        }

        let body = response
            .json::<TokenResponse>()
            .await
            .map_err(|_| TokenError::Malformed)?;
        let ttl = Duration::from_secs(body.expires_in);
        Ok(CachedToken {
            token: Secret::new(body.access_token),
            refresh_at: Instant::now() + ttl.saturating_sub(REFRESH_MARGIN),
        })
    }
}

/// `Debug` names the endpoint and the client, prints the secret as `[redacted]`, and
/// never touches the cached token: an operator can log the client, a canary test proves
/// the credentials do not come along.
impl std::fmt::Debug for CatalogToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogToken")
            .field("token_url", &self.config.token_url)
            .field("client_id", &self.config.client_id)
            .field("client_secret", &self.config.client_secret)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{Value, json};

    use super::{CatalogToken, CatalogTokenConfig, TokenError};
    use crate::catalog;

    const CLIENT: &str = "svc-catalog-nova";
    const SECRET_CANARY: &str = "client-secret-canary-9f3a";
    const TOKEN_CANARY: &str = "access-token-canary-77b1";

    /// What the fake upstreams saw.
    #[derive(Default)]
    struct Seen {
        /// One entry per Lakekeeper request: path, `Authorization` header.
        lakekeeper: StdMutex<Vec<(String, Option<String>)>>,
        /// How many tokens were asked for, and with which form.
        token_requests: AtomicUsize,
        forms: StdMutex<Vec<HashMap<String, String>>>,
    }

    impl Seen {
        fn authorizations(&self) -> Vec<(String, Option<String>)> {
            self.lakekeeper.lock().expect("seen").clone()
        }
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn config(token_url: &str, secret: &str) -> CatalogTokenConfig {
        CatalogTokenConfig {
            token_url: token_url.to_string(),
            client_id: CLIENT.into(),
            client_secret: crate::auth::Secret::new(secret),
        }
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        format!("http://{addr}")
    }

    /// A token endpoint that answers with the next token in `tokens` (the last one repeats)
    /// after `delay`, so two callers really overlap.
    async fn token_endpoint(
        seen: Arc<Seen>,
        tokens: &'static [&'static str],
        expires_in: u64,
        delay: std::time::Duration,
    ) -> String {
        let handler =
            move |State(seen): State<Arc<Seen>>,
                  axum::Form(form): axum::Form<HashMap<String, String>>| {
                let seen = seen.clone();
                async move {
                    let n = seen.token_requests.fetch_add(1, Ordering::SeqCst);
                    seen.forms.lock().expect("forms").push(form);
                    tokio::time::sleep(delay).await;
                    let token = tokens
                        .get(n)
                        .or_else(|| tokens.last())
                        .copied()
                        .expect("a token");
                    Json(json!({
                        "access_token": token,
                        "expires_in": expires_in,
                        "token_type": "Bearer",
                    }))
                }
            };
        format!(
            "{}/token",
            serve(
                Router::new()
                    .route("/token", post(handler))
                    .with_state(seen),
            )
            .await
        )
    }

    /// A token endpoint that refuses the client, with a body that (as an intermediary
    /// might) echoes the secret back.
    async fn refusing_token_endpoint() -> String {
        let base = serve(Router::new().route(
            "/token",
            post(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "invalid_client", "echo": SECRET_CANARY})),
                )
            }),
        ))
        .await;
        format!("{base}/token")
    }

    /// A fake Lakekeeper: the management warehouse list and one Iceberg REST route, both
    /// recording the `Authorization` header they were sent.
    async fn lakekeeper(seen: Arc<Seen>) -> String {
        let list = {
            let seen = seen.clone();
            move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    seen.lakekeeper
                        .lock()
                        .expect("seen")
                        .push(("warehouse".into(), authorization(&headers)));
                    Json(json!({"warehouses": [{"id": "wid-1", "name": "warehouse"}]}))
                }
            }
        };
        let namespaces = {
            let seen = seen.clone();
            move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    seen.lakekeeper
                        .lock()
                        .expect("seen")
                        .push(("namespaces".into(), authorization(&headers)));
                    Json(json!({"namespaces": []}))
                }
            }
        };
        serve(
            Router::new()
                .route("/management/v1/warehouse", get(list))
                .route("/catalog/v1/{id}/namespaces", get(namespaces)),
        )
        .await
    }

    fn authorization(headers: &HeaderMap) -> Option<String> {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    /// The catalog token under test, against an in-process token endpoint.
    fn client(token_url: &str, secret: &str) -> Arc<CatalogToken> {
        Arc::new(CatalogToken::new(
            reqwest::Client::new(),
            config(token_url, secret),
        ))
    }

    #[tokio::test]
    async fn token_is_fetched_once_and_cached() {
        let seen = Arc::new(Seen::default());
        // The delay makes the two callers overlap: without the lock across the fetch,
        // both would ask the endpoint.
        let base = token_endpoint(
            seen.clone(),
            &["token-1", "token-2"],
            300,
            std::time::Duration::from_millis(25),
        )
        .await;
        let token = client(&base, SECRET_CANARY);

        let (a, b) = tokio::join!(token.token(), token.token());
        let third = token.token().await.expect("cached token");

        assert_eq!(seen.token_requests.load(Ordering::SeqCst), 1);
        assert_eq!(a.unwrap().expose(), "token-1");
        assert_eq!(b.unwrap().expose(), "token-1");
        assert_eq!(third.expose(), "token-1");

        let forms = seen.forms.lock().expect("forms");
        let form = forms.first().expect("one form");
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("client_credentials")
        );
        assert_eq!(form.get("client_id").map(String::as_str), Some(CLIENT));
        assert_eq!(
            form.get("client_secret").map(String::as_str),
            Some(SECRET_CANARY)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn token_refetched_after_expiry() {
        let seen = Arc::new(Seen::default());
        // expires_in 120 s: the cache is fresh for 60 s (exp - REFRESH_MARGIN).
        let base = token_endpoint(
            seen.clone(),
            &["token-1", "token-2"],
            120,
            std::time::Duration::ZERO,
        )
        .await;
        let token = client(&base, SECRET_CANARY);

        let first = token.token().await.expect("first token");
        tokio::time::advance(std::time::Duration::from_secs(59)).await;
        assert_eq!(
            token.token().await.expect("still cached").expose(),
            first.expose()
        );
        assert_eq!(seen.token_requests.load(Ordering::SeqCst), 1);

        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        let second = token.token().await.expect("refetched token");
        assert_ne!(second.expose(), first.expose());
        assert_eq!(second.expose(), "token-2");
        assert_eq!(seen.token_requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn no_header_when_unconfigured() {
        // None of the three variables: no client at all.
        assert!(
            CatalogTokenConfig::from_env_with(env(&[]))
                .expect("no configuration is not an error")
                .is_none()
        );
        // Empty values count as unset, so a chart rendering "" changes nothing.
        assert!(
            CatalogTokenConfig::from_env_with(env(&[
                ("NOVA_CATALOG_CLIENT_ID", ""),
                ("NOVA_CATALOG_CLIENT_SECRET", ""),
                ("NOVA_CATALOG_TOKEN_URL", "  "),
            ]))
            .expect("empty is unset")
            .is_none()
        );

        // And a Lakekeeper call without a client carries no Authorization header:
        // today's `allowall` behaviour.
        let seen = Arc::new(Seen::default());
        let lakekeeper_base = lakekeeper(seen.clone()).await;
        let state = crate::router_tests::catalog_test_state(&lakekeeper_base, None);
        assert!(
            catalog::lakekeeper_authorization(&state)
                .await
                .unwrap()
                .is_none()
        );

        let response =
            catalog::list_namespaces(State(state), Query(Default::default()), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let seen = seen.authorizations();
        assert_eq!(seen.len(), 2, "warehouse first, then the proxy: {seen:?}");
        for (path, header) in seen {
            assert_eq!(header, None, "{path} must carry no Authorization");
        }
    }

    #[tokio::test]
    async fn header_on_proxy_and_on_warehouse_resolve() {
        let seen = Arc::new(Seen::default());
        let token_base =
            token_endpoint(seen.clone(), &["token-1"], 300, std::time::Duration::ZERO).await;
        let lakekeeper_base = lakekeeper(seen.clone()).await;
        let state = crate::router_tests::catalog_test_state(
            &lakekeeper_base,
            Some(super::CatalogToken::new(
                reqwest::Client::new(),
                config(&token_base, SECRET_CANARY),
            )),
        );

        let id = catalog::resolve_warehouse_id(&state)
            .await
            .expect("warehouse");
        assert_eq!(id, "wid-1");
        let response =
            catalog::list_namespaces(State(state), Query(Default::default()), None).await;
        assert_eq!(response.status(), StatusCode::OK);

        let requests = seen.authorizations();
        assert_eq!(
            requests,
            vec![
                ("warehouse".to_string(), Some("Bearer token-1".to_string())),
                ("namespaces".to_string(), Some("Bearer token-1".to_string())),
            ],
            "every Lakekeeper request carries the service token"
        );
        assert_eq!(
            seen.token_requests.load(Ordering::SeqCst),
            1,
            "one token for both calls"
        );
    }

    #[tokio::test]
    async fn secret_and_token_never_in_debug_or_errors() {
        // Debug of the client: the secret is redacted, the cached token is not printed.
        let seen = Arc::new(Seen::default());
        let base = token_endpoint(seen, &[TOKEN_CANARY], 300, std::time::Duration::ZERO).await;
        let token = client(&base, SECRET_CANARY);
        token.token().await.expect("a token");
        let rendered = format!("{token:?}\n{:?}", token.config);
        assert!(!rendered.contains(SECRET_CANARY), "{rendered}");
        assert!(!rendered.contains(TOKEN_CANARY), "{rendered}");
        assert!(rendered.contains("[redacted]"), "{rendered}");

        // A refused client: the error is a sentence, and the endpoint's body (which here
        // echoes the secret) never reaches it.
        let refusing = refusing_token_endpoint().await;
        let token = client(&refusing, SECRET_CANARY);
        let error = token.token().await.expect_err("refused");
        assert_eq!(
            error,
            TokenError::Refused(401, "invalid_client".into()),
            "only the status and the OAuth error code"
        );
        let rendered = format!("{error}\n{error:?}");
        assert!(!rendered.contains(SECRET_CANARY), "{rendered}");

        // A half-configured client is a startup error that names variables, not values.
        let error = CatalogTokenConfig::from_env_with(env(&[
            ("NOVA_CATALOG_CLIENT_ID", CLIENT),
            ("NOVA_CATALOG_CLIENT_SECRET", SECRET_CANARY),
        ]))
        .expect_err("a partial configuration is refused");
        assert!(!error.contains(SECRET_CANARY), "{error}");
    }

    #[tokio::test]
    async fn failed_token_fetch_is_502_not_500() {
        // The token endpoint is down; the catalog is never asked.
        let seen = Arc::new(Seen::default());
        let refusing = refusing_token_endpoint().await;
        let lakekeeper_base = lakekeeper(seen.clone()).await;
        let state = crate::router_tests::catalog_test_state(
            &lakekeeper_base,
            Some(super::CatalogToken::new(
                reqwest::Client::new(),
                config(&refusing, SECRET_CANARY),
            )),
        );

        let response =
            catalog::list_namespaces(State(state), Query(Default::default()), None).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            body["error"],
            json!("the catalog token endpoint refused the client (401 invalid_client)")
        );
        assert!(
            seen.authorizations().is_empty(),
            "a failed token is not a catalog call"
        );
    }
}
