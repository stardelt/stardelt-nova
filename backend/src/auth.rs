//! OIDC authentication against Keycloak.
//!
//! Nova is an OIDC client: unauthenticated users are redirected to Keycloak
//! (`/auth/login`), which (via the GitHub broker) authenticates them and calls
//! back to `/auth/callback`). A session cookie then carries identity.
//!
//! Cookies are handled with raw `Cookie` / `Set-Cookie` headers — no
//! tower-cookies middleware layer needed. This avoids axum's layer-ordering
//! gotcha (layers only apply to routes already in the router at call time).
//!
//! When the OIDC env (`NOVA_OIDC_ISSUER`) is absent, auth is disabled and Nova
//! falls back to the dev-user stub — preserving the no-SSO dev loop.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use openidconnect::core::{CoreClient, CoreProviderMetadata, CoreResponseType};
use openidconnect::reqwest::async_http_client;
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};

pub const SESSION_COOKIE: &str = "nova_session";

// ---------------------------------------------------------------------------
// Config + client
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub public_url: String,
}

impl OidcConfig {
    pub fn from_env() -> Option<Self> {
        let issuer = std::env::var("NOVA_OIDC_ISSUER").ok()?;
        Some(Self {
            issuer,
            client_id: std::env::var("NOVA_OIDC_CLIENT_ID").unwrap_or_else(|_| "nova".into()),
            client_secret: std::env::var("NOVA_OIDC_CLIENT_SECRET").unwrap_or_default(),
            public_url: std::env::var("NOVA_PUBLIC_URL").unwrap_or_default(),
        })
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.public_url.trim_end_matches('/'))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionUser {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct OidcClient {
    pub client: CoreClient,
}

impl OidcClient {
    pub async fn discover(config: OidcConfig) -> anyhow::Result<Self> {
        let meta = CoreProviderMetadata::discover_async(
            IssuerUrl::new(config.issuer.clone())?,
            async_http_client,
        )
        .await?;
        let client = CoreClient::from_provider_metadata(
            meta,
            ClientId::new(config.client_id.clone()),
            Some(ClientSecret::new(config.client_secret.clone())),
        )
        .set_redirect_uri(RedirectUrl::new(config.redirect_uri())?);
        Ok(Self { client })
    }
}

// ---------------------------------------------------------------------------
// Shared lazy state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OidcState {
    pub sso_configured: bool,
    slot: std::sync::Arc<tokio::sync::RwLock<Option<std::sync::Arc<OidcClient>>>>,
}

impl OidcState {
    pub fn new(sso_configured: bool) -> Self {
        Self {
            sso_configured,
            slot: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    pub async fn get(&self) -> Option<std::sync::Arc<OidcClient>> {
        self.slot.read().await.clone()
    }

    async fn set(&self, client: std::sync::Arc<OidcClient>) {
        *self.slot.write().await = Some(client);
    }

    pub fn spawn_discovery(&self, config: OidcConfig) {
        let state = self.clone();
        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            loop {
                attempt += 1;
                match OidcClient::discover(config.clone()).await {
                    Ok(c) => {
                        state.set(std::sync::Arc::new(c)).await;
                        tracing::info!(attempt, "OIDC discovery succeeded; auth active");
                        return;
                    }
                    Err(e) => {
                        let delay = std::cmp::min(30, 2u64.saturating_mul(attempt as u64));
                        tracing::warn!(
                            attempt,
                            error = %e,
                            retry_in_s = delay,
                            "OIDC discovery failed; retrying"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    }
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Cookie helpers (raw headers — no tower-cookies layer needed)
// ---------------------------------------------------------------------------

fn session_cookie_header(value: &str) -> HeaderValue {
    // HttpOnly + Secure + SameSite=Lax; path=/
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=Lax"
    ))
    .expect("cookie value is ASCII")
}

fn clear_cookie_header() -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; Max-Age=0"
    ))
    .expect("static string")
}

/// Read the session user from the `Cookie` request header, if present.
pub fn current_user(headers: &HeaderMap) -> Option<SessionUser> {
    let cookie_hdr = headers.get(header::COOKIE)?.to_str().ok()?;
    // Find our specific cookie among potentially many.
    let raw = cookie_hdr
        .split(';')
        .map(str::trim)
        .find(|p| p.starts_with(&format!("{SESSION_COOKIE}=")))?
        .trim_start_matches(&format!("{SESSION_COOKIE}="));
    let decoded: String = openidconnect::url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect();
    serde_json::from_str(&decoded).ok()
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    #[allow(dead_code)]
    pub state: Option<String>,
}

/// GET /auth/login — redirect to Keycloak.
pub async fn login(State(state): State<OidcState>) -> Response {
    let oidc = match state.get().await {
        Some(c) => c,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "auth not ready yet",
            )
                .into_response();
        }
    };
    let (url, _csrf, _nonce) = oidc
        .client
        .authorize_url(
            AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("openid".into()))
        .add_scope(Scope::new("email".into()))
        .add_scope(Scope::new("profile".into()))
        .url();
    Redirect::to(url.as_str()).into_response()
}

/// GET /auth/callback — exchange code, set session cookie, redirect home.
pub async fn callback(State(state): State<OidcState>, Query(q): Query<CallbackQuery>) -> Response {
    let oidc = match state.get().await {
        Some(c) => c,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "auth not ready yet",
            )
                .into_response();
        }
    };
    let token = match oidc
        .client
        .exchange_code(AuthorizationCode::new(q.code))
        .request_async(async_http_client)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("token exchange failed: {e}"),
            )
                .into_response();
        }
    };
    let id_token = match token.id_token() {
        Some(t) => t,
        None => return (axum::http::StatusCode::BAD_GATEWAY, "no id_token").into_response(),
    };
    let claims = match id_token.claims(&oidc.client.id_token_verifier(), |_: Option<&Nonce>| Ok(()))
    {
        Ok(c) => c,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("id_token invalid: {e}"),
            )
                .into_response();
        }
    };
    let user = SessionUser {
        sub: claims.subject().to_string(),
        email: claims.email().map(|e| e.to_string()),
        name: claims
            .preferred_username()
            .map(|n| n.to_string())
            .or_else(|| claims.email().map(|e| e.to_string())),
    };
    let payload = serde_json::to_string(&user).unwrap_or_default();
    let encoded =
        openidconnect::url::form_urlencoded::byte_serialize(payload.as_bytes()).collect::<String>();

    let mut response = Redirect::to("/").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie_header(&encoded));
    response
}

/// GET /auth/logout — clear the session cookie and redirect home.
pub async fn logout() -> Response {
    let mut response = Redirect::to("/").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, clear_cookie_header());
    response
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

/// Axum middleware that rejects unauthenticated requests with 401 when SSO is
/// enabled. Read the session from the raw Cookie header — no tower-cookies layer.
pub async fn require_auth(
    axum::extract::State(gate): axum::extract::State<bool>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if gate && current_user(request.headers()).is_none() {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "login": "/auth/login" })),
        )
            .into_response();
    }
    next.run(request).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_is_callback_under_public_url() {
        let c = OidcConfig {
            issuer: "https://auth.lab.stardelt.io/realms/stardelt".into(),
            client_id: "nova".into(),
            client_secret: "x".into(),
            public_url: "https://nova.lab.stardelt.io".into(),
        };
        assert_eq!(
            c.redirect_uri(),
            "https://nova.lab.stardelt.io/auth/callback"
        );
    }

    #[test]
    fn from_env_is_none_without_issuer() {
        unsafe {
            std::env::remove_var("NOVA_OIDC_ISSUER");
        }
        assert!(OidcConfig::from_env().is_none());
    }

    #[test]
    fn current_user_reads_cookie_header() {
        use axum::http::HeaderMap;
        let user = SessionUser {
            sub: "u1".into(),
            email: Some("a@b.com".into()),
            name: Some("Alice".into()),
        };
        let payload = serde_json::to_string(&user).unwrap();
        let encoded: String =
            openidconnect::url::form_urlencoded::byte_serialize(payload.as_bytes()).collect();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={encoded}")).unwrap(),
        );
        let got = current_user(&headers).unwrap();
        assert_eq!(got.sub, "u1");
        assert_eq!(got.name.as_deref(), Some("Alice"));
    }

    #[test]
    fn current_user_returns_none_without_cookie() {
        let headers = HeaderMap::new();
        assert!(current_user(&headers).is_none());
    }
}
