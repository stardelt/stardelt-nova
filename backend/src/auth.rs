//! OIDC authentication against Keycloak.
//!
//! Nova is an OIDC client: unauthenticated users are redirected to Keycloak
//! (`/auth/login`), which (via the GitHub broker) authenticates them and calls
//! back to `/auth/callback`. A session cookie then carries identity.
//!
//! When the OIDC env (`NOVA_OIDC_ISSUER`) is absent, auth is disabled and Nova
//! falls back to the dev-user stub — preserving the no-SSO dev loop.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use openidconnect::core::{CoreClient, CoreProviderMetadata, CoreResponseType};
use openidconnect::reqwest::async_http_client;
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};
use tower_cookies::{Cookie, Cookies};

pub const SESSION_COOKIE: &str = "nova_session";

#[derive(Clone, Debug)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub public_url: String,
}

impl OidcConfig {
    /// Build from env. Returns None when OIDC env is absent (dev / no-sso mode).
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

/// Built OIDC client, stored in app state when SSO is enabled. The redirect URI
/// and credentials are baked into `client` during discovery, so the raw config
/// is not retained.
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

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    #[allow(dead_code)]
    pub state: String,
}

/// GET /auth/login — redirect to Keycloak.
pub async fn login(State(oidc): State<Arc<OidcClient>>) -> Response {
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
pub async fn callback(
    State(oidc): State<Arc<OidcClient>>,
    cookies: Cookies,
    Query(q): Query<CallbackQuery>,
) -> Response {
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
    let mut cookie = Cookie::new(SESSION_COOKIE, encoded);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(true);
    cookies.add(cookie);
    Redirect::to("/").into_response()
}

/// GET /auth/logout — clear the session cookie.
pub async fn logout(cookies: Cookies) -> Response {
    let mut c = Cookie::new(SESSION_COOKIE, "");
    c.set_path("/");
    cookies.remove(c);
    Redirect::to("/").into_response()
}

/// Extract the current user from the session cookie, if any.
pub fn current_user(cookies: &Cookies) -> Option<SessionUser> {
    let raw = cookies.get(SESSION_COOKIE)?.value().to_string();
    let decoded: String = openidconnect::url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect();
    serde_json::from_str(&decoded).ok()
}

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
        // Not setting NOVA_OIDC_ISSUER → None (dev mode). Uses a fresh process env;
        // this is best-effort and only asserts the issuer-absent branch shape.
        unsafe {
            std::env::remove_var("NOVA_OIDC_ISSUER");
        }
        assert!(OidcConfig::from_env().is_none());
    }
}
