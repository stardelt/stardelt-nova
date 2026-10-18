//! Sign-in for Nova: `NOVA_AUTH_MODE=dev` (default, no login) or `oidc`.
//!
//! Nova is only ever an OIDC *client* (contract: stardelt-docs
//! `docs/architecture/identity.md`). In `oidc` mode it
//!
//! - discovers the provider from `NOVA_OIDC_ISSUER` and caches its JWKS, refetching
//!   when a token names an unknown `kid` (rate-limited);
//! - runs the authorization-code flow with PKCE (S256), `state` and `nonce`
//!   (`/api/auth/login` → IdP → `/api/auth/callback`);
//! - keeps tokens in an in-memory, server-side session store; the browser only holds a
//!   signed, `HttpOnly` cookie carrying a random session id;
//! - accepts `Authorization: Bearer <access token>` for programmatic access, validated
//!   against the same issuer, audience and keys;
//! - protects every `/api/*` route except `/api/health` and `/api/auth/*`.
//!
//! **Single replica.** Sessions and in-flight logins live in this process' memory: a
//! restart signs everyone out, and two replicas would not share sessions. Run one Nova
//! replica in `oidc` mode (or pin sessions) until a shared store lands.
//!
//! Tokens never appear in a URL, a log line or an error message; `Secret` redacts its
//! `Debug` output and no token-holding type derives `Debug`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{Jwk, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use url::Url;

pub const SESSION_COOKIE: &str = "nova_session";
pub const LOGIN_COOKIE: &str = "nova_login";
const LOGIN_COOKIE_PATH: &str = "/api/auth";
/// Absolute session lifetime. Sessions are not tied to the (often 5-minute) access
/// token lifetime because phase 1 does not forward tokens; see the module docs.
const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);
const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_LOGINS: usize = 10_000;
const MAX_SESSIONS: usize = 100_000;
/// A token with an unknown `kid` triggers a JWKS refetch at most this often.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);
const CLOCK_LEEWAY_SECS: u64 = 60;
const MIN_SESSION_SECRET_BYTES: usize = 32;
const ALLOWED_ALGS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// A credential that must never be printed. `Debug` is redacted on purpose.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How Nova decides who you are.
#[derive(Debug)]
pub enum AuthMode {
    /// No login; `/api/me` reports `NOVA_DEV_USER`. The default, forever.
    Dev,
    Oidc(Box<OidcConfig>),
}

/// Which claims carry roles and which values make an admin.
#[derive(Clone, Debug)]
pub struct ClaimConfig {
    /// `NOVA_OIDC_GROUP_CLAIM`, default `groups`.
    pub group_claim: String,
    /// `NOVA_OIDC_ADMIN_GROUP`, default `stardelt-admins`.
    pub admin_group: String,
    /// `NOVA_OIDC_ADMIN_ROLE`, default = the admin group. Matched against `roles`.
    pub admin_role: String,
}

impl Default for ClaimConfig {
    fn default() -> Self {
        Self {
            group_claim: "groups".into(),
            admin_group: "stardelt-admins".into(),
            admin_role: "stardelt-admins".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OidcConfig {
    /// As configured; discovery must report the same issuer (modulo a trailing `/`).
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Secret,
    pub scopes: String,
    /// Accepted `aud` values for bearer access tokens; default the client id.
    pub audiences: Vec<String>,
    /// Nova's external origin, e.g. `https://nova.example.com` (no trailing slash).
    pub public_url: String,
    pub claims: ClaimConfig,
    pub session_secret: Secret,
}

impl OidcConfig {
    pub fn redirect_uri(&self) -> String {
        format!("{}/api/auth/callback", self.public_url)
    }
    /// Cookies are `Secure` whenever Nova is served over HTTPS.
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

fn is_loopback(u: &Url) -> bool {
    matches!(
        u.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("::1")
    )
}

impl AuthMode {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Parse the auth configuration. Any problem in `oidc` mode is an error: Nova
    /// refuses to start rather than run with a half-configured login.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let required =
            |k: &str| var(k).ok_or_else(|| format!("{k} is required when NOVA_AUTH_MODE=oidc"));

        match var("NOVA_AUTH_MODE").as_deref() {
            None | Some("dev") => return Ok(AuthMode::Dev),
            Some("oidc") => {}
            Some(other) => {
                return Err(format!(
                    "NOVA_AUTH_MODE must be `dev` or `oidc`, got `{other}`"
                ));
            }
        }

        let issuer = required("NOVA_OIDC_ISSUER")?;
        let issuer_url =
            Url::parse(&issuer).map_err(|e| format!("NOVA_OIDC_ISSUER is not a URL: {e}"))?;
        if issuer_url.scheme() != "https"
            && !(issuer_url.scheme() == "http" && is_loopback(&issuer_url))
        {
            return Err(
                "NOVA_OIDC_ISSUER must be https:// (plain http is only accepted for localhost)"
                    .into(),
            );
        }

        let client_id = required("NOVA_OIDC_CLIENT_ID")?;
        let client_secret = Secret::new(required("NOVA_OIDC_CLIENT_SECRET")?);

        let session_secret = required("NOVA_SESSION_SECRET")?;
        if session_secret.len() < MIN_SESSION_SECRET_BYTES {
            return Err(format!(
                "NOVA_SESSION_SECRET must be at least {MIN_SESSION_SECRET_BYTES} bytes (e.g. `openssl rand -base64 48`)"
            ));
        }

        let public_url = required("NOVA_PUBLIC_URL")?;
        let parsed =
            Url::parse(&public_url).map_err(|e| format!("NOVA_PUBLIC_URL is not a URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(
                "NOVA_PUBLIC_URL must be an http(s) origin, e.g. https://nova.example.com".into(),
            );
        }
        if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(
                "NOVA_PUBLIC_URL must be an origin without a path, e.g. https://nova.example.com"
                    .into(),
            );
        }
        let public_url = parsed.origin().ascii_serialization();

        let scopes = var("NOVA_OIDC_SCOPES").unwrap_or_else(|| "openid profile email".into());
        if !scopes.split_whitespace().any(|s| s == "openid") {
            return Err("NOVA_OIDC_SCOPES must include `openid`".into());
        }

        let audiences = var("NOVA_OIDC_AUDIENCES")
            .map(|a| {
                a.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| vec![client_id.clone()]);

        let admin_group = var("NOVA_OIDC_ADMIN_GROUP").unwrap_or_else(|| "stardelt-admins".into());
        let claims = ClaimConfig {
            group_claim: var("NOVA_OIDC_GROUP_CLAIM").unwrap_or_else(|| "groups".into()),
            admin_role: var("NOVA_OIDC_ADMIN_ROLE").unwrap_or_else(|| admin_group.clone()),
            admin_group,
        };

        Ok(AuthMode::Oidc(Box::new(OidcConfig {
            issuer,
            client_id,
            client_secret,
            scopes,
            audiences,
            public_url,
            claims,
            session_secret: Secret::new(session_secret),
        })))
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every variant's message is safe to log and to show: none carries a token.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("not signed in")]
    Unauthenticated,
    #[error("invalid token: {0}")]
    InvalidToken(&'static str),
    #[error("token signed by an unknown key")]
    UnknownKey,
    #[error("identity provider unavailable: {0}")]
    ProviderUnavailable(String),
    #[error("identity provider misconfigured: {0}")]
    Misconfigured(String),
    #[error("internal error: {0}")]
    Internal(&'static str),
    #[error("this view is for platform administrators")]
    Forbidden,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let msg = self.to_string();
        match self {
            AuthError::Unauthenticated | AuthError::InvalidToken(_) | AuthError::UnknownKey => {
                let challenge = if matches!(self, AuthError::Unauthenticated) {
                    "Bearer"
                } else {
                    "Bearer error=\"invalid_token\""
                };
                (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, challenge)],
                    Json(serde_json::json!({
                        "error": "unauthenticated",
                        "detail": msg,
                        "login": "/api/auth/login",
                    })),
                )
                    .into_response()
            }
            AuthError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "forbidden",
                    "detail": msg,
                    "hint": "membership is decided by the identity provider's admin group",
                })),
            )
                .into_response(),
            AuthError::ProviderUnavailable(_) | AuthError::Misconfigured(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response(),
            AuthError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response(),
        }
    }
}

// ---------------------------------------------------------------------------
// Claims → identity
// ---------------------------------------------------------------------------

/// Who the caller is, as the IdP says. `sub` is the only field to persist.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Identity {
    pub sub: String,
    /// `preferred_username` → `email` → `upn` → `name` → `sub`.
    pub display_name: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub groups: Vec<String>,
    pub roles: Vec<String>,
    /// Entra put a reference instead of the groups (too many to fit the token).
    pub groups_overage: bool,
    pub is_admin: bool,
}

fn claim_str(claims: &Value, key: &str) -> Option<String> {
    claims
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A claim holding a list of strings; a single string counts as a one-item list.
fn claim_list(claims: &Value, key: &str) -> Vec<String> {
    match claims.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Entra ID "group overage": when a user is in more groups than fit the token, Entra
/// omits the group claim and sets `_claim_names.groups` (or `hasgroups: true`).
/// Nova does not call Graph to resolve it; it treats the user as having no groups.
fn groups_overage(claims: &Value, group_claim: &str) -> bool {
    let names = claims.get("_claim_names");
    let named = |k: &str| names.and_then(|n| n.get(k)).is_some();
    let hasgroups = matches!(claims.get("hasgroups"), Some(Value::Bool(true)))
        || claims.get("hasgroups").and_then(Value::as_str) == Some("true");
    named(group_claim) || named("groups") || hasgroups
}

/// Group names match exactly, or with Keycloak's leading `/` (full group path).
fn has_group(groups: &[String], wanted: &str) -> bool {
    groups
        .iter()
        .any(|g| g == wanted || g.strip_prefix('/') == Some(wanted))
}

/// Admin decision. Precedence: the `roles` claim (Entra app roles) is checked first
/// against `admin_role`, then the configured group claim against `admin_group`;
/// either grants admin. An overage never grants anything.
pub fn is_admin(roles: &[String], groups: &[String], cfg: &ClaimConfig) -> bool {
    roles.iter().any(|r| r == &cfg.admin_role) || has_group(groups, &cfg.admin_group)
}

pub fn identity_from_claims(claims: &Value, cfg: &ClaimConfig) -> Result<Identity, AuthError> {
    let sub = claim_str(claims, "sub").ok_or(AuthError::InvalidToken("missing sub"))?;
    let email = claim_str(claims, "email");
    let name = claim_str(claims, "name");
    let display_name = claim_str(claims, "preferred_username")
        .or_else(|| email.clone())
        .or_else(|| claim_str(claims, "upn"))
        .or_else(|| name.clone())
        .unwrap_or_else(|| sub.clone());

    let overage = groups_overage(claims, &cfg.group_claim);
    let groups = if overage {
        warn!(
            sub = %sub,
            claim = %cfg.group_claim,
            "group overage in token (too many groups for the IdP to include) — treating as no groups; \
             use app roles or a group filter on the IdP app registration"
        );
        Vec::new()
    } else {
        claim_list(claims, &cfg.group_claim)
    };
    let roles = claim_list(claims, "roles");
    let is_admin = is_admin(&roles, &groups, cfg);

    Ok(Identity {
        sub,
        display_name,
        email,
        name,
        groups,
        roles,
        groups_overage: overage,
        is_admin,
    })
}

// ---------------------------------------------------------------------------
// JWT verification
// ---------------------------------------------------------------------------

/// What a token must satisfy beyond its signature.
pub struct Expect<'a> {
    pub issuer: &'a str,
    pub audiences: &'a [String],
    pub client_id: &'a str,
    /// `Some` for an ID token from our own login: the `nonce` we sent.
    pub nonce: Option<&'a str>,
}

fn jwt_reason(e: &jsonwebtoken::errors::Error) -> &'static str {
    use jsonwebtoken::errors::ErrorKind as K;
    match e.kind() {
        K::ExpiredSignature => "expired",
        K::ImmatureSignature => "not yet valid",
        K::InvalidIssuer => "wrong issuer",
        K::InvalidAudience => "wrong audience",
        K::InvalidSignature => "bad signature",
        K::InvalidAlgorithm | K::InvalidAlgorithmName => "algorithm not allowed",
        K::MissingRequiredClaim(_) => "missing required claim",
        _ => "malformed",
    }
}

fn select_key<'k>(keys: &'k [Jwk], kid: Option<&str>) -> Result<&'k Jwk, AuthError> {
    let signing = |k: &&Jwk| !matches!(k.common.public_key_use, Some(PublicKeyUse::Encryption));
    match kid {
        Some(kid) => keys
            .iter()
            .filter(signing)
            .find(|k| k.common.key_id.as_deref() == Some(kid))
            .ok_or(AuthError::UnknownKey),
        None => {
            let mut it = keys.iter().filter(signing);
            match (it.next(), it.next()) {
                (Some(only), None) => Ok(only),
                _ => Err(AuthError::UnknownKey),
            }
        }
    }
}

/// Verify signature, `iss`, `aud`, `exp`/`nbf` (with leeway) and, for ID tokens,
/// `nonce` and `azp`. Returns the claims.
pub fn verify_jwt(token: &str, keys: &[Jwk], expect: &Expect<'_>) -> Result<Value, AuthError> {
    let header = decode_header(token).map_err(|_| AuthError::InvalidToken("malformed"))?;
    if !ALLOWED_ALGS.contains(&header.alg) {
        return Err(AuthError::InvalidToken("algorithm not allowed"));
    }
    let jwk = select_key(keys, header.kid.as_deref())?;
    if let Some(key_alg) = jwk.common.key_algorithm
        && key_alg.to_string() != format!("{:?}", header.alg)
    {
        return Err(AuthError::InvalidToken("algorithm does not match key"));
    }
    let key = DecodingKey::from_jwk(jwk).map_err(|_| AuthError::InvalidToken("unusable key"))?;

    let mut v = Validation::new(header.alg);
    v.algorithms = vec![header.alg];
    v.set_issuer(&[expect.issuer]);
    v.set_audience(expect.audiences);
    v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    v.leeway = CLOCK_LEEWAY_SECS;
    v.validate_nbf = true;

    let claims = decode::<Value>(token, &key, &v)
        .map_err(|e| AuthError::InvalidToken(jwt_reason(&e)))?
        .claims;

    match expect.nonce {
        Some(nonce) => {
            if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
                return Err(AuthError::InvalidToken("nonce mismatch"));
            }
            // OIDC Core 3.1.3.7: with several audiences, `azp` must be us.
            let multi_aud = claims
                .get("aud")
                .and_then(Value::as_array)
                .is_some_and(|a| a.len() > 1);
            match claims.get("azp").and_then(Value::as_str) {
                Some(azp) if azp != expect.client_id => {
                    return Err(AuthError::InvalidToken("wrong authorized party"));
                }
                None if multi_aud => return Err(AuthError::InvalidToken("missing azp")),
                _ => {}
            }
        }
        None => {
            // Keycloak marks ID tokens `typ: ID`; they are not API credentials.
            if claims.get("typ").and_then(Value::as_str) == Some("ID") {
                return Err(AuthError::InvalidToken("ID token used as access token"));
            }
        }
    }
    Ok(claims)
}

// ---------------------------------------------------------------------------
// Provider: discovery + JWKS cache
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub end_session_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

impl Discovery {
    /// Check a discovery document against the configured issuer.
    pub fn validate(&self, configured_issuer: &str) -> Result<(), AuthError> {
        if self.issuer.trim_end_matches('/') != configured_issuer.trim_end_matches('/') {
            return Err(AuthError::Misconfigured(format!(
                "discovery reports issuer `{}`, but NOVA_OIDC_ISSUER is `{}` (for Entra ID use \
                 https://login.microsoftonline.com/<tenant-id>/v2.0)",
                self.issuer, configured_issuer
            )));
        }
        let insecure_ok = Url::parse(&self.issuer).is_ok_and(|u| is_loopback(&u));
        for (name, ep) in [
            ("authorization_endpoint", &self.authorization_endpoint),
            ("token_endpoint", &self.token_endpoint),
            ("jwks_uri", &self.jwks_uri),
        ] {
            let u = Url::parse(ep)
                .map_err(|_| AuthError::Misconfigured(format!("discovery {name} is not a URL")))?;
            if u.scheme() != "https" && !(insecure_ok && u.scheme() == "http") {
                return Err(AuthError::Misconfigured(format!(
                    "discovery {name} is not https"
                )));
            }
        }
        Ok(())
    }

    /// RFC 8414: absent means `client_secret_basic`. Prefer `post` when offered — it
    /// avoids the form-encoding pitfalls of secrets in a Basic header.
    fn use_secret_post(&self) -> bool {
        self.token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|m| m.iter().any(|x| x == "client_secret_post"))
    }
}

#[derive(Default)]
struct JwksCache {
    keys: Arc<Vec<Jwk>>,
    fetched_at: Option<Instant>,
}

pub struct Provider {
    http: reqwest::Client,
    issuer: String,
    discovery: tokio::sync::RwLock<Option<Arc<Discovery>>>,
    jwks: tokio::sync::RwLock<JwksCache>,
}

impl Provider {
    pub fn new(http: reqwest::Client, issuer: String) -> Self {
        Self {
            http,
            issuer,
            discovery: tokio::sync::RwLock::new(None),
            jwks: tokio::sync::RwLock::new(JwksCache::default()),
        }
    }

    /// The discovery document, fetched once and cached. A failed fetch is retried on
    /// the next call; until then sign-in and bearer access fail (closed).
    pub async fn discovery(&self) -> Result<Arc<Discovery>, AuthError> {
        if let Some(d) = self.discovery.read().await.as_ref() {
            return Ok(d.clone());
        }
        let mut slot = self.discovery.write().await;
        if let Some(d) = slot.as_ref() {
            return Ok(d.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        );
        let doc: Discovery = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AuthError::ProviderUnavailable(format!("discovery at {url}: {e}")))?
            .json()
            .await
            .map_err(|e| {
                AuthError::Misconfigured(format!("discovery at {url} is not valid: {e}"))
            })?;
        doc.validate(&self.issuer)?;
        let doc = Arc::new(doc);
        *slot = Some(doc.clone());
        Ok(doc)
    }

    async fn fetch_jwks(&self, jwks_uri: &str) -> Result<Vec<Jwk>, AuthError> {
        #[derive(Deserialize)]
        struct RawSet {
            keys: Vec<Value>,
        }
        let raw: RawSet = self
            .http
            .get(jwks_uri)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| AuthError::ProviderUnavailable(format!("JWKS: {e}")))?
            .json()
            .await
            .map_err(|e| AuthError::ProviderUnavailable(format!("JWKS is not valid: {e}")))?;
        // Skip keys this library cannot parse (e.g. exotic encryption keys) instead of
        // rejecting the whole set.
        Ok(raw
            .keys
            .into_iter()
            .filter_map(|k| serde_json::from_value::<Jwk>(k).ok())
            .collect())
    }

    /// Cached keys; `refresh` refetches unless the cache is younger than
    /// `JWKS_MIN_REFRESH` (so random `kid`s cannot hammer the IdP).
    async fn keys(&self, disc: &Discovery, refresh: bool) -> Result<Arc<Vec<Jwk>>, AuthError> {
        {
            let c = self.jwks.read().await;
            if let Some(at) = c.fetched_at
                && (!refresh || at.elapsed() < JWKS_MIN_REFRESH)
            {
                return Ok(c.keys.clone());
            }
        }
        let mut c = self.jwks.write().await;
        if let Some(at) = c.fetched_at
            && at.elapsed() < JWKS_MIN_REFRESH
        {
            return Ok(c.keys.clone());
        }
        let keys = Arc::new(self.fetch_jwks(&disc.jwks_uri).await?);
        c.keys = keys.clone();
        c.fetched_at = Some(Instant::now());
        Ok(keys)
    }

    /// Verify a JWT from this provider, refetching the JWKS once on an unknown `kid`.
    pub async fn verify(
        &self,
        token: &str,
        audiences: &[String],
        client_id: &str,
        nonce: Option<&str>,
    ) -> Result<Value, AuthError> {
        let disc = self.discovery().await?;
        let expect = Expect {
            issuer: &disc.issuer,
            audiences,
            client_id,
            nonce,
        };
        let keys = self.keys(&disc, false).await?;
        match verify_jwt(token, &keys, &expect) {
            Err(AuthError::UnknownKey) => {
                let keys = self.keys(&disc, true).await?;
                verify_jwt(token, &keys, &expect)
            }
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Bearer validation (a trait so the middleware can be tested without an IdP)
// ---------------------------------------------------------------------------

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait BearerValidator: Send + Sync {
    fn validate<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Identity, AuthError>>;
}

struct OidcBearer {
    provider: Arc<Provider>,
    audiences: Vec<String>,
    client_id: String,
    claims: ClaimConfig,
}

impl BearerValidator for OidcBearer {
    fn validate<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async move {
            let claims = self
                .provider
                .verify(token, &self.audiences, &self.client_id, None)
                .await?;
            identity_from_claims(&claims, &self.claims)
        })
    }
}

// ---------------------------------------------------------------------------
// Sessions and in-flight logins (in memory, single replica)
// ---------------------------------------------------------------------------

/// Tokens from the code exchange, kept server-side for phase-2 forwarding.
#[allow(dead_code)] // read once identity forwarding (phase 2) lands
struct TokenSet {
    access_token: Secret,
    id_token: Secret,
    refresh_token: Option<Secret>,
}

struct Session {
    identity: Identity,
    expires: Instant,
    #[allow(dead_code)]
    tokens: TokenSet,
}

#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    fn insert(&self, identity: Identity, tokens: TokenSet) -> Result<String, AuthError> {
        let id = random_token(32)?;
        let mut m = self
            .inner
            .lock()
            .map_err(|_| AuthError::Internal("session store"))?;
        let now = Instant::now();
        m.retain(|_, s| s.expires > now);
        if m.len() >= MAX_SESSIONS {
            return Err(AuthError::Internal("too many sessions"));
        }
        m.insert(
            id.clone(),
            Session {
                identity,
                expires: now + SESSION_TTL,
                tokens,
            },
        );
        Ok(id)
    }

    fn get(&self, id: &str) -> Option<(Identity, Option<Secret>)> {
        let mut m = self.inner.lock().ok()?;
        match m.get(id) {
            Some(s) if s.expires > Instant::now() => {
                Some((s.identity.clone(), Some(s.tokens.access_token.clone())))
            }
            Some(_) => {
                m.remove(id);
                None
            }
            None => None,
        }
    }

    fn remove(&self, id: &str) {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(id);
        }
    }
}

struct PendingLogin {
    verifier: Secret,
    nonce: String,
    return_to: String,
    created: Instant,
}

#[derive(Default)]
struct PendingStore {
    inner: Mutex<HashMap<String, PendingLogin>>,
}

impl PendingStore {
    fn insert(&self, state: String, p: PendingLogin) -> Result<(), AuthError> {
        let mut m = self
            .inner
            .lock()
            .map_err(|_| AuthError::Internal("login store"))?;
        m.retain(|_, p| p.created.elapsed() < LOGIN_TTL);
        if m.len() >= MAX_PENDING_LOGINS {
            return Err(AuthError::ProviderUnavailable(
                "too many sign-ins in flight".into(),
            ));
        }
        m.insert(state, p);
        Ok(())
    }

    /// One-shot: a `state` can complete at most one login.
    fn take(&self, state: &str) -> Option<PendingLogin> {
        let p = self.inner.lock().ok()?.remove(state)?;
        (p.created.elapsed() < LOGIN_TTL).then_some(p)
    }
}

fn random_token(bytes: usize) -> Result<String, AuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| AuthError::Internal("no randomness"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// RFC 7636 S256: BASE64URL(SHA256(verifier)).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Only same-origin relative paths: `/x`, never `//host`, `/\host`, `https://…`, or a
/// path back into `/api/auth/`. Anything else becomes `/`.
pub fn safe_return_to(raw: Option<&str>) -> String {
    let Some(p) = raw else { return "/".into() };
    let ok = p.starts_with('/')
        && !p.starts_with("//")
        && !p.contains('\\')
        && !p.chars().any(|c| c.is_control())
        && p.len() <= 2048
        && !p.starts_with("/api/auth/");
    if ok { p.to_string() } else { "/".into() }
}

// ---------------------------------------------------------------------------
// Auth state, cookies, middleware
// ---------------------------------------------------------------------------

/// Who made this request and how. Inserted into request extensions by the middleware.
#[derive(Clone, Debug)]
pub struct Principal {
    pub identity: Identity,
    /// `session` (browser cookie) or `bearer` (Authorization header).
    pub via: &'static str,
    /// The token this principal presented. It is passed upstream **only** when
    /// `NOVA_FORWARD_IDENTITY` is on, and never logged or returned — `Secret` redacts.
    pub access_token: Option<Secret>,
}

pub struct AuthState {
    pub config: OidcConfig,
    pub provider: Arc<Provider>,
    bearer: Arc<dyn BearerValidator>,
    sessions: SessionStore,
    pending: PendingStore,
    key: Key,
}

impl AuthState {
    pub fn new(config: OidcConfig, http: reqwest::Client) -> Arc<Self> {
        let provider = Arc::new(Provider::new(http, config.issuer.clone()));
        let bearer = Arc::new(OidcBearer {
            provider: provider.clone(),
            audiences: config.audiences.clone(),
            client_id: config.client_id.clone(),
            claims: config.claims.clone(),
        });
        Self::with_validator(config, provider, bearer)
    }

    pub fn with_validator(
        config: OidcConfig,
        provider: Arc<Provider>,
        bearer: Arc<dyn BearerValidator>,
    ) -> Arc<Self> {
        // `from_lookup` guarantees >= 32 bytes, which `derive_from` requires.
        let key = Key::derive_from(config.session_secret.expose().as_bytes());
        Arc::new(Self {
            config,
            provider,
            bearer,
            sessions: SessionStore::default(),
            pending: PendingStore::default(),
            key,
        })
    }

    fn jar(&self, headers: &HeaderMap) -> SignedCookieJar {
        SignedCookieJar::from_headers(headers, self.key.clone())
    }

    fn cookie(&self, name: &'static str, value: String, path: &'static str) -> Cookie<'static> {
        Cookie::build((name, value))
            .path(path)
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(self.config.secure_cookies())
            .build()
    }

    fn removal(name: &'static str, path: &'static str) -> Cookie<'static> {
        Cookie::build((name, "")).path(path).build()
    }

    /// Bearer first (an invalid bearer is a 401, never a fallback to the cookie),
    /// then the session cookie.
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, AuthError> {
        if let Some(authz) = headers.get(header::AUTHORIZATION) {
            let value = authz.to_str().map_err(|_| AuthError::Unauthenticated)?;
            let token = match value.split_once(' ') {
                Some((scheme, t))
                    if scheme.eq_ignore_ascii_case("bearer") && !t.trim().is_empty() =>
                {
                    t.trim()
                }
                _ => return Err(AuthError::Unauthenticated),
            };
            let identity = self.bearer.validate(token).await?;
            return Ok(Principal {
                identity,
                via: "bearer",
                access_token: Some(Secret(token.to_string())),
            });
        }
        let jar = self.jar(headers);
        let id = jar.get(SESSION_COOKIE).ok_or(AuthError::Unauthenticated)?;
        let (identity, access_token) = self
            .sessions
            .get(id.value())
            .ok_or(AuthError::Unauthenticated)?;
        Ok(Principal {
            identity,
            via: "session",
            access_token,
        })
    }
}

/// `route_layer` middleware for every protected `/api/*` route in `oidc` mode.
pub async fn require_auth(
    State(auth): State<Arc<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    match auth.authenticate(req.headers()).await {
        Ok(p) => {
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Err(e) => {
            if !matches!(e, AuthError::Unauthenticated) {
                warn!(reason = %e, path = %req.uri().path(), "rejected API request");
            }
            e.into_response()
        }
    }
}

/// Admin-only routes (`/api/apps`, `/api/workloads`, engine details).
///
/// The operator surface is not data-engineering work, and an analyst has no business
/// reading the service inventory or the workload list — so this is enforced here, on the
/// way in, and not only by hiding navigation in the SPA. The decision itself is the same
/// one `/api/me` reports (`is_admin`), so the UI and the API cannot drift apart.
///
/// Only mounted in `oidc` mode: in `dev` there is no IdP and the single user is the
/// operator, which is what CI, `make demo` and the kind gate depend on.
pub async fn require_admin(req: Request, next: Next) -> Response {
    // `require_auth` inserts a `Principal` (identity + how it arrived), not the bare
    // `Identity` — reading the wrong type silently refuses everyone, which is exactly
    // what happened the first time this shipped.
    let principal = req.extensions().get::<Principal>().cloned();
    if principal
        .as_ref()
        .map(|p| p.identity.is_admin)
        .unwrap_or(false)
    {
        return next.run(req).await;
    }
    // The audit layer sits inside this one, so a refusal is recorded here — with the
    // actor when we know it, which is the case that matters.
    match &principal {
        Some(p) => warn!(
            target: "audit",
            actor = %p.identity.display_name,
            sub = %p.identity.sub,
            via = p.via,
            path = %req.uri().path(),
            "refused: not an administrator"
        ),
        None => warn!(target: "audit", path = %req.uri().path(), "refused: no principal"),
    }
    AuthError::Forbidden.into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginParams {
    return_to: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// The token endpoint's answer. No `Debug`: it holds tokens.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    id_token: Option<String>,
    refresh_token: Option<String>,
}

fn no_store<R: IntoResponse>(r: R) -> Response {
    let mut resp = r.into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Plain-text failure page. The reason is ours (or a sanitized IdP error code), never
/// IdP-provided free text and never a token.
fn login_failed(reason: &str) -> Response {
    warn!(reason = %reason, "sign-in failed");
    no_store((
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("Sign-in failed: {reason}.\n\nTry again: /api/auth/login\n"),
    ))
}

fn sanitize_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

async fn login(
    State(auth): State<Arc<AuthState>>,
    Query(q): Query<LoginParams>,
    headers: HeaderMap,
) -> Response {
    let disc = match auth.provider.discovery().await {
        Ok(d) => d,
        Err(e) => {
            warn!(reason = %e, "sign-in unavailable");
            return no_store(e);
        }
    };
    let (state, nonce, verifier) = match (random_token(32), random_token(32), random_token(48)) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        _ => return no_store(AuthError::Internal("no randomness")),
    };
    let mut url = match Url::parse(&disc.authorization_endpoint) {
        Ok(u) => u,
        Err(_) => return no_store(AuthError::Misconfigured("authorization_endpoint".into())),
    };
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &auth.config.client_id)
        .append_pair("redirect_uri", &auth.config.redirect_uri())
        .append_pair("scope", &auth.config.scopes)
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256");

    if let Err(e) = auth.pending.insert(
        state.clone(),
        PendingLogin {
            verifier: Secret::new(verifier),
            nonce,
            return_to: safe_return_to(q.return_to.as_deref()),
            created: Instant::now(),
        },
    ) {
        return no_store(e);
    }
    // Bind the flow to this browser: the callback must present the same `state`.
    let jar = auth
        .jar(&headers)
        .add(auth.cookie(LOGIN_COOKIE, state, LOGIN_COOKIE_PATH));
    no_store((jar, Redirect::to(url.as_str())))
}

async fn exchange_code(
    auth: &AuthState,
    disc: &Discovery,
    code: &str,
    verifier: &Secret,
) -> Result<TokenResponse, String> {
    let redirect_uri = auth.config.redirect_uri();
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", verifier.expose()),
    ];
    let mut req = auth.provider.http.post(&disc.token_endpoint);
    if disc.use_secret_post() {
        form.push(("client_id", &auth.config.client_id));
        form.push(("client_secret", auth.config.client_secret.expose()));
    } else {
        // RFC 6749 §2.3.1: form-encode id and secret before Basic.
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        req = req.basic_auth(
            enc(&auth.config.client_id),
            Some(enc(auth.config.client_secret.expose())),
        );
    }
    let resp = req
        .form(&form)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| "the identity provider's token endpoint could not be reached".to_string())?;
    let status = resp.status();
    if !status.is_success() {
        // Only the RFC 6749 error code is surfaced — never the body.
        let code = resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(sanitize_code))
            .unwrap_or_default();
        return Err(format!("token exchange returned {status} {code}"));
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|_| "token endpoint answered with an unexpected body".to_string())
}

async fn callback(
    State(auth): State<Arc<AuthState>>,
    Query(q): Query<CallbackParams>,
    headers: HeaderMap,
) -> Response {
    if let Some(err) = q.error.as_deref() {
        return login_failed(&format!(
            "the identity provider refused the sign-in ({})",
            sanitize_code(err)
        ));
    }
    let (Some(code), Some(state)) = (q.code.as_deref(), q.state.as_deref()) else {
        return login_failed("missing code or state");
    };
    let jar = auth.jar(&headers);
    if jar
        .get(LOGIN_COOKIE)
        .map(|c| c.value().to_string())
        .as_deref()
        != Some(state)
    {
        return login_failed("state does not match this browser (start again from the login page)");
    }
    let Some(pending) = auth.pending.take(state) else {
        return login_failed("sign-in expired or already used");
    };
    let disc = match auth.provider.discovery().await {
        Ok(d) => d,
        Err(e) => return login_failed(&e.to_string()),
    };
    let tokens = match exchange_code(&auth, &disc, code, &pending.verifier).await {
        Ok(t) => t,
        Err(reason) => return login_failed(&reason),
    };
    let Some(id_token) = tokens.id_token else {
        return login_failed("the identity provider returned no ID token (is `openid` in scope?)");
    };
    let claims = match auth
        .provider
        .verify(
            &id_token,
            std::slice::from_ref(&auth.config.client_id),
            &auth.config.client_id,
            Some(&pending.nonce),
        )
        .await
    {
        Ok(c) => c,
        Err(e) => return login_failed(&format!("ID token rejected: {e}")),
    };
    let identity = match identity_from_claims(&claims, &auth.config.claims) {
        Ok(i) => i,
        Err(e) => return login_failed(&e.to_string()),
    };
    // A fresh session id on every login (no fixation); drop any previous session.
    if let Some(old) = jar.get(SESSION_COOKIE) {
        auth.sessions.remove(old.value());
    }
    info!(sub = %identity.sub, admin = identity.is_admin, "signed in");
    let sid = match auth.sessions.insert(
        identity,
        TokenSet {
            access_token: Secret::new(tokens.access_token),
            id_token: Secret::new(id_token),
            refresh_token: tokens.refresh_token.map(Secret::new),
        },
    ) {
        Ok(s) => s,
        Err(e) => return no_store(e),
    };
    let jar = jar
        .remove(AuthState::removal(LOGIN_COOKIE, LOGIN_COOKIE_PATH))
        .add(auth.cookie(SESSION_COOKIE, sid, "/"));
    no_store((jar, Redirect::to(&pending.return_to)))
}

/// Logout is local *and* at the IdP. A form POST (top-level navigation), answered
/// with 303 to the IdP's end-session endpoint. `id_token_hint` is deliberately not
/// sent — tokens never go into URLs — so `client_id` identifies us instead.
async fn logout(State(auth): State<Arc<AuthState>>, headers: HeaderMap) -> Response {
    if let Some(origin) = headers.get(header::ORIGIN)
        && origin.to_str().ok() != Some(auth.config.public_url.as_str())
    {
        return (StatusCode::FORBIDDEN, "cross-origin logout refused").into_response();
    }
    let jar = auth.jar(&headers);
    if let Some(sid) = jar.get(SESSION_COOKIE) {
        auth.sessions.remove(sid.value());
    }
    let jar = jar.remove(AuthState::removal(SESSION_COOKIE, "/"));
    let target = match auth.provider.discovery().await {
        Ok(d) => d
            .end_session_endpoint
            .as_deref()
            .and_then(|ep| Url::parse(ep).ok())
            .map(|mut u| {
                u.query_pairs_mut()
                    .append_pair("client_id", &auth.config.client_id)
                    .append_pair(
                        "post_logout_redirect_uri",
                        &format!("{}/", auth.config.public_url),
                    );
                u.to_string()
            })
            .unwrap_or_else(|| "/".into()),
        Err(e) => {
            warn!(reason = %e, "signed out locally only: identity provider unreachable");
            "/".into()
        }
    };
    no_store((jar, Redirect::to(&target)))
}

/// `/api/auth/*` in `oidc` mode.
pub fn router<S: Clone + Send + Sync + 'static>(auth: Arc<AuthState>) -> Router<S> {
    Router::new()
        .route("/login", get(login))
        .route("/callback", get(callback))
        .route("/logout", post(logout))
        .with_state(auth)
}

/// `/api/auth/*` in `dev` mode: there is nobody to sign in, so both just go home.
pub fn dev_router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    async fn dev_login(Query(q): Query<LoginParams>) -> Redirect {
        Redirect::to(&safe_return_to(q.return_to.as_deref()))
    }
    Router::new()
        .route("/login", get(dev_login))
        .route("/logout", post(|| async { Redirect::to("/") }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The `Authorization` header Nova should send upstream for this caller, if any.
///
/// One place, one rule: forward only when the platform asked for it *and* somebody is
/// signed in *and* holds a token. In `dev` mode there is no token to forward, which is
/// the honest answer — the catalog then sees Nova's service connection, as before.
pub fn upstream_authorization(forward: bool, caller: Option<&Principal>) -> Option<String> {
    if !forward {
        return None;
    }
    let token = caller?.access_token.as_ref()?;
    Some(format!("Bearer {}", token.expose()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal_with_token(token: Option<&str>) -> Principal {
        Principal {
            identity: Identity {
                sub: "s".into(),
                display_name: "u".into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: false,
            },
            via: "session",
            access_token: token.map(Secret::new),
        }
    }

    /// The rule in one place: forward only when asked, only for somebody signed in, and
    /// never a token that is not there. `dev` mode has no token, which is the honest answer.
    #[test]
    fn upstream_authorization_is_gated() {
        let signed_in = principal_with_token(Some("tok"));
        assert_eq!(
            upstream_authorization(true, Some(&signed_in)).as_deref(),
            Some("Bearer tok")
        );
        // Off by default: the platform has to ask.
        assert_eq!(upstream_authorization(false, Some(&signed_in)), None);
        // Nobody signed in (dev mode, or an anonymous call).
        assert_eq!(upstream_authorization(true, None), None);
        // Signed in without a token (a dev session).
        let no_token = principal_with_token(None);
        assert_eq!(upstream_authorization(true, Some(&no_token)), None);
    }

    #[test]
    fn forwarded_tokens_are_redacted_in_debug_output() {
        let p = principal_with_token(Some("super-secret-token"));
        let rendered = format!("{p:?}");
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
    }

    /// The admin middleware must read the extension type `require_auth` actually inserts.
    /// It shipped reading `Identity` instead of `Principal`, which refused *everyone*
    /// including administrators — a bug no other test could see, because the mismatch is
    /// invisible until a request flows through both layers.
    #[tokio::test]
    async fn admin_layer_allows_admins_only() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        fn ident(admin: bool) -> Identity {
            Identity {
                sub: "s".into(),
                display_name: "u".into(),
                email: None,
                name: None,
                groups: vec![],
                roles: vec![],
                groups_overage: false,
                is_admin: admin,
            }
        }

        async fn status_for(identity: Option<Identity>) -> StatusCode {
            let app = Router::new()
                .route("/x", get(|| async { StatusCode::OK }))
                .layer(axum::middleware::from_fn(require_admin))
                // Applied last = outermost, so the extension exists before the check runs —
                // the same order `main` builds.
                .layer(axum::middleware::from_fn(
                    move |mut req: Request<Body>, next: Next| {
                        let identity = identity.clone();
                        async move {
                            if let Some(identity) = identity {
                                req.extensions_mut().insert(Principal {
                                    identity,
                                    via: "session",
                                    access_token: None,
                                });
                            }
                            next.run(req).await
                        }
                    },
                ));
            app.oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }

        assert_eq!(status_for(Some(ident(true))).await, StatusCode::OK);
        assert_eq!(status_for(Some(ident(false))).await, StatusCode::FORBIDDEN);
        assert_eq!(status_for(None).await, StatusCode::FORBIDDEN);
    }
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;
    use tower::ServiceExt;

    // Test-only RSA key (generated for these tests, never used anywhere else).
    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/test-rsa-pkcs1.der");
    const TEST_KEY_N: &str = include_str!("testdata/test-rsa-n.txt");
    const ISS: &str = "https://login.microsoftonline.com/tid/v2.0";
    const CLIENT: &str = "nova";

    fn now() -> u64 {
        jsonwebtoken::get_current_timestamp()
    }

    fn jwks() -> Vec<Jwk> {
        vec![
            serde_json::from_value(json!({
                "kty": "RSA", "use": "sig", "kid": "k1",
                "n": TEST_KEY_N.trim(), "e": "AQAB",
                // Entra adds fields like these; they must not break parsing.
                "x5t": "ignored", "issuer": ISS
            }))
            .unwrap(),
        ]
    }

    fn sign(claims: &Value, kid: Option<&str>) -> String {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = kid.map(str::to_string);
        encode(&h, claims, &EncodingKey::from_rsa_der(TEST_KEY_DER)).unwrap()
    }

    fn base_claims() -> Value {
        json!({ "iss": ISS, "aud": CLIENT, "sub": "u-123", "exp": now() + 300, "nonce": "n0" })
    }

    fn aud() -> Vec<String> {
        vec![CLIENT.to_string()]
    }

    fn expect<'a>(aud: &'a [String], nonce: Option<&'a str>) -> Expect<'a> {
        Expect {
            issuer: ISS,
            audiences: aud,
            client_id: CLIENT,
            nonce,
        }
    }

    // --- JWT validation --------------------------------------------------

    #[test]
    fn valid_id_token_passes() {
        let a = aud();
        let c = verify_jwt(
            &sign(&base_claims(), Some("k1")),
            &jwks(),
            &expect(&a, Some("n0")),
        )
        .unwrap();
        assert_eq!(c["sub"], "u-123");
    }

    #[test]
    fn rejects_wrong_iss_aud_exp_nonce_kid() {
        let a = aud();
        let keys = jwks();
        let check = |claims: Value, kid: Option<&str>, nonce: Option<&str>| {
            verify_jwt(&sign(&claims, kid), &keys, &expect(&a, nonce))
        };
        let mut c = base_claims();
        c["iss"] = json!("https://evil.example/v2.0");
        assert!(matches!(
            check(c, Some("k1"), Some("n0")),
            Err(AuthError::InvalidToken("wrong issuer"))
        ));
        let mut c = base_claims();
        c["aud"] = json!("someone-else");
        assert!(matches!(
            check(c, Some("k1"), Some("n0")),
            Err(AuthError::InvalidToken("wrong audience"))
        ));
        let mut c = base_claims();
        c["exp"] = json!(now() - 3600);
        assert!(matches!(
            check(c, Some("k1"), Some("n0")),
            Err(AuthError::InvalidToken("expired"))
        ));
        assert!(matches!(
            check(base_claims(), Some("k1"), Some("other")),
            Err(AuthError::InvalidToken("nonce mismatch"))
        ));
        assert!(matches!(
            check(base_claims(), Some("k9"), Some("n0")),
            Err(AuthError::UnknownKey)
        ));
        let mut c = base_claims();
        c.as_object_mut().unwrap().remove("sub");
        assert!(check(c, Some("k1"), Some("n0")).is_err());
    }

    #[test]
    fn rejects_bad_signature_and_hmac() {
        let a = aud();
        let token = sign(&base_claims(), Some("k1"));
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut sig = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
        sig[0] ^= 0xff;
        parts[2] = URL_SAFE_NO_PAD.encode(sig);
        assert!(verify_jwt(&parts.join("."), &jwks(), &expect(&a, Some("n0"))).is_err());

        // HS256 signed with the public modulus as a "secret": the classic key-confusion attack.
        let mut h = Header::new(Algorithm::HS256);
        h.kid = Some("k1".into());
        let forged = encode(
            &h,
            &base_claims(),
            &EncodingKey::from_secret(TEST_KEY_N.as_bytes()),
        )
        .unwrap();
        assert!(matches!(
            verify_jwt(&forged, &jwks(), &expect(&a, Some("n0"))),
            Err(AuthError::InvalidToken("algorithm not allowed"))
        ));
    }

    #[test]
    fn id_token_with_foreign_azp_rejected_and_id_token_not_a_bearer() {
        let a = aud();
        let mut c = base_claims();
        c["azp"] = json!("other-client");
        assert!(verify_jwt(&sign(&c, Some("k1")), &jwks(), &expect(&a, Some("n0"))).is_err());
        let mut c = base_claims();
        c["typ"] = json!("ID");
        assert!(verify_jwt(&sign(&c, Some("k1")), &jwks(), &expect(&a, None)).is_err());
    }

    #[test]
    fn kidless_token_uses_single_key() {
        let a = aud();
        assert!(
            verify_jwt(
                &sign(&base_claims(), None),
                &jwks(),
                &expect(&a, Some("n0"))
            )
            .is_ok()
        );
    }

    // --- claims ------------------------------------------------------------

    #[test]
    fn keycloak_groups_with_full_path() {
        let cfg = ClaimConfig::default();
        let c = json!({ "sub": "kc-1", "preferred_username": "admin",
                        "email": "admin@example.com", "groups": ["/stardelt-admins", "/analysts"] });
        let id = identity_from_claims(&c, &cfg).unwrap();
        assert_eq!(id.display_name, "admin");
        assert!(id.is_admin);
        assert!(!id.groups_overage);
        assert_eq!(id.groups.len(), 2);
    }

    #[test]
    fn entra_app_roles_grant_admin_without_groups() {
        let cfg = ClaimConfig {
            admin_role: "Nova.Admin".into(),
            ..ClaimConfig::default()
        };
        let c = json!({ "sub": "e-1", "upn": "ada@contoso.com", "name": "Ada", "roles": ["Nova.Admin"] });
        let id = identity_from_claims(&c, &cfg).unwrap();
        assert!(id.is_admin);
        assert_eq!(id.display_name, "ada@contoso.com");
        assert_eq!(id.roles, vec!["Nova.Admin"]);
    }

    #[test]
    fn entra_group_object_id_as_admin_group() {
        let gid = "8f1c2b7e-0000-4000-8000-000000000001";
        let cfg = ClaimConfig {
            admin_group: gid.into(),
            admin_role: "unused".into(),
            ..ClaimConfig::default()
        };
        let c = json!({ "sub": "e-2", "groups": [gid] });
        assert!(identity_from_claims(&c, &cfg).unwrap().is_admin);
    }

    #[test]
    fn entra_overage_is_no_groups_not_admin() {
        let cfg = ClaimConfig::default();
        let c = json!({
            "sub": "e-3", "preferred_username": "bob@contoso.com",
            "groups": ["stardelt-admins"], // must be ignored once overage is flagged
            "_claim_names": { "groups": "src1" },
            "_claim_sources": { "src1": { "endpoint": "https://graph.microsoft.com/..." } }
        });
        let id = identity_from_claims(&c, &cfg).unwrap();
        assert!(id.groups_overage);
        assert!(id.groups.is_empty());
        assert!(!id.is_admin);

        let c = json!({ "sub": "e-4", "hasgroups": true });
        assert!(identity_from_claims(&c, &cfg).unwrap().groups_overage);
        // Roles still work during an overage.
        let c = json!({ "sub": "e-5", "hasgroups": true, "roles": ["stardelt-admins"] });
        assert!(identity_from_claims(&c, &cfg).unwrap().is_admin);
    }

    #[test]
    fn display_fallbacks_and_sub_required() {
        let cfg = ClaimConfig::default();
        let id = identity_from_claims(&json!({ "sub": "s", "email": "e@x" }), &cfg).unwrap();
        assert_eq!(id.display_name, "e@x");
        let id = identity_from_claims(&json!({ "sub": "s", "name": "N" }), &cfg).unwrap();
        assert_eq!(id.display_name, "N");
        let id = identity_from_claims(&json!({ "sub": "s" }), &cfg).unwrap();
        assert_eq!(id.display_name, "s");
        assert!(!id.is_admin);
        assert!(identity_from_claims(&json!({ "email": "e@x" }), &cfg).is_err());
    }

    #[test]
    fn custom_group_claim() {
        let cfg = ClaimConfig {
            group_claim: "nova_groups".into(),
            ..ClaimConfig::default()
        };
        let c = json!({ "sub": "s", "nova_groups": "stardelt-admins", "groups": [] });
        assert!(identity_from_claims(&c, &cfg).unwrap().is_admin);
    }

    #[test]
    fn group_to_admin_mapping() {
        let cfg = ClaimConfig::default();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(is_admin(&[], &s(&["stardelt-admins"]), &cfg));
        assert!(is_admin(&[], &s(&["/stardelt-admins"]), &cfg));
        assert!(is_admin(&s(&["stardelt-admins"]), &[], &cfg));
        assert!(!is_admin(
            &[],
            &s(&["stardelt-admins-x", "/a/stardelt-admins"]),
            &cfg
        ));
        assert!(!is_admin(&[], &s(&["STARDELT-ADMINS"]), &cfg));
        assert!(!is_admin(&[], &[], &cfg));
    }

    // --- return_to / PKCE ----------------------------------------------------

    #[test]
    fn return_to_only_same_origin_paths() {
        for ok in ["/", "/catalog", "/query?sql=select%201#x", "/apps/trino"] {
            assert_eq!(safe_return_to(Some(ok)), ok);
        }
        for bad in [
            "https://evil.example/",
            "//evil.example",
            "/\\evil.example",
            "\\\\evil",
            "evil",
            "javascript:alert(1)",
            "/api/auth/login",
            "/a\nb",
            "",
        ] {
            assert_eq!(safe_return_to(Some(bad)), "/", "{bad:?}");
        }
        assert_eq!(safe_return_to(None), "/");
    }

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    // --- config ----------------------------------------------------------------

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    const FULL: &[(&str, &str)] = &[
        ("NOVA_AUTH_MODE", "oidc"),
        ("NOVA_OIDC_ISSUER", ISS),
        ("NOVA_OIDC_CLIENT_ID", CLIENT),
        ("NOVA_OIDC_CLIENT_SECRET", "s3cret"),
        ("NOVA_SESSION_SECRET", "0123456789abcdef0123456789abcdef!"),
        ("NOVA_PUBLIC_URL", "https://nova.example.com/"),
    ];

    #[test]
    fn config_defaults_and_fail_closed() {
        assert!(matches!(AuthMode::from_lookup(env(&[])), Ok(AuthMode::Dev)));
        assert!(matches!(
            AuthMode::from_lookup(env(&[("NOVA_AUTH_MODE", "dev")])),
            Ok(AuthMode::Dev)
        ));
        assert!(AuthMode::from_lookup(env(&[("NOVA_AUTH_MODE", "off")])).is_err());

        let Ok(AuthMode::Oidc(c)) = AuthMode::from_lookup(env(FULL)) else {
            panic!()
        };
        assert_eq!(c.public_url, "https://nova.example.com");
        assert_eq!(
            c.redirect_uri(),
            "https://nova.example.com/api/auth/callback"
        );
        assert!(c.secure_cookies());
        assert_eq!(c.scopes, "openid profile email");
        assert_eq!(c.audiences, vec![CLIENT]);
        assert_eq!(c.claims.group_claim, "groups");
        assert_eq!(c.claims.admin_role, "stardelt-admins");
        assert!(!format!("{c:?}").contains("s3cret"));

        for missing in [
            "NOVA_OIDC_ISSUER",
            "NOVA_OIDC_CLIENT_ID",
            "NOVA_OIDC_CLIENT_SECRET",
            "NOVA_SESSION_SECRET",
            "NOVA_PUBLIC_URL",
        ] {
            let v: Vec<_> = FULL
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            let e = AuthMode::from_lookup(env(&v)).unwrap_err();
            assert!(e.contains(missing), "{e}");
        }
        let with = |k: &'static str, val: &'static str| {
            let mut v: Vec<_> = FULL.iter().copied().filter(|(x, _)| *x != k).collect();
            v.push((k, val));
            AuthMode::from_lookup(env(&v))
        };
        assert!(with("NOVA_SESSION_SECRET", "short").is_err());
        assert!(with("NOVA_OIDC_ISSUER", "http://id.example.com/realms/stardelt").is_err());
        assert!(with("NOVA_OIDC_ISSUER", "http://localhost:8081/realms/stardelt").is_ok());
        assert!(with("NOVA_OIDC_SCOPES", "profile email").is_err());
        assert!(with("NOVA_PUBLIC_URL", "https://nova.example.com/sub").is_err());
    }

    #[test]
    fn discovery_issuer_must_match() {
        let d = Discovery {
            issuer: ISS.into(),
            authorization_endpoint: "https://login.microsoftonline.com/tid/oauth2/v2.0/authorize"
                .into(),
            token_endpoint: "https://login.microsoftonline.com/tid/oauth2/v2.0/token".into(),
            jwks_uri: "https://login.microsoftonline.com/tid/discovery/v2.0/keys".into(),
            end_session_endpoint: None,
            token_endpoint_auth_methods_supported: Some(vec!["client_secret_post".into()]),
        };
        assert!(d.validate(ISS).is_ok());
        assert!(d.validate(&format!("{ISS}/")).is_ok());
        assert!(
            d.validate("https://login.microsoftonline.com/other/v2.0")
                .is_err()
        );
        assert!(d.use_secret_post());
        let d = Discovery {
            token_endpoint: "http://plain.example/token".into(),
            ..d
        };
        assert!(d.validate(ISS).is_err());
    }

    // --- session cookie + middleware ----------------------------------------

    struct MockValidator;
    impl BearerValidator for MockValidator {
        fn validate<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Identity, AuthError>> {
            Box::pin(async move {
                match token {
                    "good" => identity_from_claims(
                        &json!({ "sub": "bearer-user" }),
                        &ClaimConfig::default(),
                    ),
                    "idp-down" => Err(AuthError::ProviderUnavailable("down".into())),
                    _ => Err(AuthError::InvalidToken("bad signature")),
                }
            })
        }
    }

    fn test_auth() -> Arc<AuthState> {
        let Ok(AuthMode::Oidc(cfg)) = AuthMode::from_lookup(env(FULL)) else {
            panic!()
        };
        let provider = Arc::new(Provider::new(reqwest::Client::new(), cfg.issuer.clone()));
        AuthState::with_validator(*cfg, provider, Arc::new(MockValidator))
    }

    fn session_for(auth: &AuthState, sub: &str) -> String {
        let id = identity_from_claims(&json!({ "sub": sub }), &auth.config.claims).unwrap();
        auth.sessions
            .insert(
                id,
                TokenSet {
                    access_token: Secret::new("a"),
                    id_token: Secret::new("i"),
                    refresh_token: None,
                },
            )
            .unwrap()
    }

    /// Set-Cookie value (`name=value; attrs`) for a session id, as the browser gets it.
    fn set_cookie_for(auth: &AuthState, sid: String) -> String {
        let jar = SignedCookieJar::new(auth.key.clone()).add(auth.cookie(SESSION_COOKIE, sid, "/"));
        let resp = (jar, "").into_response();
        resp.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn session_cookie_roundtrip_and_tamper() {
        let auth = test_auth();
        let set = set_cookie_for(&auth, "sid-1".into());
        assert!(set.contains("HttpOnly"));
        assert!(set.contains("SameSite=Lax"));
        assert!(set.contains("Secure"));
        assert!(set.contains("Path=/"));
        let pair = set.split(';').next().unwrap().to_string();
        assert!(!pair.ends_with("=sid-1"), "value must be signed");

        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, pair.parse().unwrap());
        assert_eq!(auth.jar(&h).get(SESSION_COOKIE).unwrap().value(), "sid-1");

        // Tampered value, or plain unsigned value: rejected.
        let tampered = pair.replace("sid-1", "sid-2");
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, tampered.parse().unwrap());
        assert!(auth.jar(&h).get(SESSION_COOKIE).is_none());
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "nova_session=sid-1".parse().unwrap());
        assert!(auth.jar(&h).get(SESSION_COOKIE).is_none());

        // A different secret cannot read it.
        let other = Key::derive_from(b"a-completely-different-secret-of-32b!");
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, pair.parse().unwrap());
        assert!(
            SignedCookieJar::from_headers(&h, other)
                .get(SESSION_COOKIE)
                .is_none()
        );
    }

    fn app(auth: Arc<AuthState>) -> Router {
        let protected = Router::new()
            .route(
                "/me",
                get(|axum::Extension(p): axum::Extension<Principal>| async move { p.identity.sub }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                require_auth,
            ));
        let api = Router::new()
            .route("/health", get(|| async { "ok" }))
            .merge(protected)
            .nest("/auth", router(auth));
        Router::new().nest("/api", api)
    }

    async fn call(
        app: &Router,
        uri: &str,
        headers: &[(header::HeaderName, String)],
    ) -> (StatusCode, String) {
        let mut req = HttpRequest::builder().uri(uri);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn middleware_allow_deny() {
        let auth = test_auth();
        let app = app(auth.clone());

        assert_eq!(call(&app, "/api/health", &[]).await.0, StatusCode::OK);
        assert_eq!(call(&app, "/api/me", &[]).await.0, StatusCode::UNAUTHORIZED);

        let bearer = |t: &str| vec![(header::AUTHORIZATION, format!("Bearer {t}"))];
        assert_eq!(
            call(&app, "/api/me", &bearer("good")).await,
            (StatusCode::OK, "bearer-user".into())
        );
        assert_eq!(
            call(&app, "/api/me", &bearer("forged")).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "/api/me", &bearer("idp-down")).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let basic = vec![(header::AUTHORIZATION, "Basic Zm9vOmJhcg==".to_string())];
        assert_eq!(
            call(&app, "/api/me", &basic).await.0,
            StatusCode::UNAUTHORIZED
        );

        let sid = session_for(&auth, "cookie-user");
        let pair = set_cookie_for(&auth, sid.clone())
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let cookie = vec![(header::COOKIE, pair.clone())];
        assert_eq!(
            call(&app, "/api/me", &cookie).await,
            (StatusCode::OK, "cookie-user".into())
        );

        // Invalid bearer does not fall back to a valid cookie.
        let both = vec![
            (header::COOKIE, pair.clone()),
            (header::AUTHORIZATION, "Bearer forged".to_string()),
        ];
        assert_eq!(
            call(&app, "/api/me", &both).await.0,
            StatusCode::UNAUTHORIZED
        );

        // Unsigned cookie with a real session id is not accepted.
        let raw = vec![(header::COOKIE, format!("nova_session={sid}"))];
        assert_eq!(
            call(&app, "/api/me", &raw).await.0,
            StatusCode::UNAUTHORIZED
        );

        // After the session is gone, the cookie is worthless.
        auth.sessions.remove(&sid);
        assert_eq!(
            call(&app, "/api/me", &cookie).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn callback_requires_matching_state_cookie() {
        let auth = test_auth();
        let app = app(auth);
        let (status, body) = call(&app, "/api/auth/callback?code=c&state=s", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("state does not match"));
        let (status, body) = call(
            &app,
            "/api/auth/callback?error=access_denied%3Cscript%3E",
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("access_deniedscript"));
        assert!(!body.contains('<'));
    }

    #[tokio::test]
    async fn login_with_unreachable_idp_fails_closed() {
        let Ok(AuthMode::Oidc(mut cfg)) = AuthMode::from_lookup(env(FULL)) else {
            panic!()
        };
        cfg.issuer = "http://127.0.0.1:9/realms/none".into();
        let auth = AuthState::new(*cfg, reqwest::Client::new());
        let app = app(auth);
        assert_eq!(
            call(&app, "/api/auth/login", &[]).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// The whole browser flow against an in-process mock IdP: login → callback →
    /// session → /api/me → logout. PKCE, state, nonce and client auth are all checked.
    #[tokio::test]
    async fn full_login_flow_against_mock_idp() {
        #[derive(Default)]
        struct Idp {
            challenge: Mutex<Option<String>>,
            nonce: Mutex<Option<String>>,
        }
        let idp = Arc::new(Idp::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());

        let b = base.clone();
        let disc = move || {
            let b = b.clone();
            async move {
                Json(json!({
                    "issuer": b,
                    "authorization_endpoint": format!("{b}/authorize"),
                    "token_endpoint": format!("{b}/token"),
                    "jwks_uri": format!("{b}/jwks"),
                    "end_session_endpoint": format!("{b}/logout"),
                    "token_endpoint_auth_methods_supported": ["client_secret_post"],
                }))
            }
        };
        let b = base.clone();
        let token = move |State(idp): State<Arc<Idp>>,
                          axum::Form(f): axum::Form<HashMap<String, String>>| {
            let b = b.clone();
            async move {
                let challenge = idp.challenge.lock().unwrap().clone().unwrap();
                let ok = f.get("grant_type").map(String::as_str) == Some("authorization_code")
                    && f.get("code").map(String::as_str) == Some("the-code")
                    && f.get("client_id").map(String::as_str) == Some(CLIENT)
                    && f.get("client_secret").map(String::as_str) == Some("s3cret")
                    && f.get("redirect_uri").map(String::as_str)
                        == Some("https://nova.example.com/api/auth/callback")
                    && f.get("code_verifier").map(|v| pkce_challenge(v)) == Some(challenge);
                if !ok {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "invalid_grant"})),
                    );
                }
                let nonce = idp.nonce.lock().unwrap().clone().unwrap();
                let id = sign(
                    &json!({ "iss": b, "aud": CLIENT, "sub": "kc-42", "exp": now() + 300,
                             "nonce": nonce, "preferred_username": "admin",
                             "groups": ["/stardelt-admins"] }),
                    Some("k1"),
                );
                (
                    StatusCode::OK,
                    Json(
                        json!({ "access_token": "opaque", "id_token": id, "token_type": "Bearer" }),
                    ),
                )
            }
        };
        let mock = Router::new()
            .route("/.well-known/openid-configuration", get(disc))
            .route("/jwks", get(|| async { Json(json!({ "keys": jwks() })) }))
            .route("/token", post(token))
            .with_state(idp.clone());
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

        let Ok(AuthMode::Oidc(mut cfg)) = AuthMode::from_lookup(env(FULL)) else {
            panic!()
        };
        cfg.issuer = base.clone();
        let auth = AuthState::new(*cfg, reqwest::Client::new());
        let app = app(auth);

        // 1. login → 303 to the IdP with PKCE + state + nonce, and a login cookie.
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::get("/api/auth/login?return_to=/catalog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let loc = Url::parse(resp.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        assert!(loc.as_str().starts_with(&format!("{base}/authorize?")));
        let q: HashMap<String, String> = loc.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["response_type"], "code");
        *idp.challenge.lock().unwrap() = Some(q["code_challenge"].clone());
        *idp.nonce.lock().unwrap() = Some(q["nonce"].clone());
        let login_cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(login_cookie.contains("Path=/api/auth"));
        let login_pair = login_cookie.split(';').next().unwrap().to_string();

        // 2. callback with the browser's login cookie → session cookie + redirect.
        let cb = format!("/api/auth/callback?code=the-code&state={}", q["state"]);
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::get(&cb)
                    .header(header::COOKIE, &login_pair)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(resp.headers()[header::LOCATION], "/catalog");
        let session_pair = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .find(|c| c.starts_with("nova_session=") && c.contains("HttpOnly"))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        // The state is one-shot: replaying the callback fails.
        let (status, _) = call(&app, &cb, &[(header::COOKIE, login_pair.clone())]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // 3. the session authenticates API calls.
        let cookie = vec![(header::COOKIE, session_pair.clone())];
        assert_eq!(
            call(&app, "/api/me", &cookie).await,
            (StatusCode::OK, "kc-42".into())
        );

        // 4. logout → 303 to the IdP's end-session endpoint, no token in the URL.
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/auth/logout")
                    .header(header::COOKIE, &session_pair)
                    .header(header::ORIGIN, "https://nova.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let loc = resp.headers()[header::LOCATION].to_str().unwrap();
        assert!(loc.starts_with(&format!("{base}/logout?client_id=nova")));
        assert!(!loc.contains("id_token"));
        assert_eq!(
            call(&app, "/api/me", &cookie).await.0,
            StatusCode::UNAUTHORIZED
        );

        // Cross-origin logout is refused.
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/auth/logout")
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
