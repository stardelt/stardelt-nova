//! Will a browser render an app inside Nova's frame?
//!
//! Nova embeds Superset and JupyterHub in an iframe, and both ship with headers
//! that refuse exactly that: JupyterHub sends
//! `content-security-policy: frame-ancestors 'none'`, Superset `x-frame-options`.
//! The browser enforces it client-side, so a naive embed yields a blank frame and
//! a console error. Nova asks the app for its headers and tells the reader which
//! header blocks the embed — and what to change — instead of pretending.

use axum::http::HeaderMap;
use serde::Serialize;

/// Whether Nova can frame an app, as observed from the app's own headers.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Embed {
    /// The page a browser loads in the frame (external URL, empty without a base domain).
    pub url: String,
    /// allowed | blocked | unknown
    pub state: String,
    /// The header that refuses framing, e.g. `content-security-policy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    /// That header's value, verbatim — what the operator has to change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The in-cluster page Nova read the headers from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    /// Host a visitor without a session is sent to for sign-in, when the app's
    /// own page redirects off its hosts before it renders. That page (Keycloak
    /// on this platform) refuses framing, so the frame stays empty until the
    /// visitor signs in in a top-level tab; the UI turns this into a call to
    /// action. `None` when the app answers for itself — a login form on its own
    /// host is part of the app and frames fine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_in_host: Option<String>,
}

impl Embed {
    /// Nova could not read the app's headers — demo mode, an unreachable page, or
    /// no external URL to frame. Not the same as "allowed".
    pub fn unknown(url: String, checked: Option<String>) -> Self {
        Self {
            url,
            state: "unknown".into(),
            blocked_by: None,
            detail: None,
            checked,
            sign_in_host: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum FramePolicy {
    Allowed,
    Blocked { header: &'static str, value: String },
}

/// The host a redirect hands the browser to, when that leaves the app's own
/// hosts — the sign-in detour. The page there (Keycloak's login form on this
/// platform) refuses framing by design, so a signed-out visitor's frame stays
/// empty; the UI uses this to offer "open in a new tab and sign in" instead.
///
/// `request_url` is the URL the response came from (a relative `Location` is
/// resolved against it) and `own_hosts` are the hosts the app answers from: its
/// in-cluster address and its public URL. A redirect between those is still the
/// app; anything else is the detour.
pub fn sign_in_host(location: &str, request_url: &str, own_hosts: &[String]) -> Option<String> {
    let target = url::Url::parse(request_url).ok()?.join(location).ok()?;
    let host = target.host_str()?;
    if own_hosts.iter().any(|own| own == host) {
        return None;
    }
    Some(host.to_string())
}

/// Judge a page's framing policy from its response headers. `base_domain` is the
/// domain Nova itself is served from (`NOVA_BASE_DOMAIN`): a `frame-ancestors`
/// list only permits the embed if it names that origin.
pub fn frame_policy(headers: &HeaderMap, base_domain: &str) -> FramePolicy {
    if let Some(value) = headers.get("x-frame-options").and_then(|v| v.to_str().ok()) {
        let value = value.trim();
        let lowered = value.to_ascii_lowercase();
        // ALLOW-FROM is obsolete and ignored by current browsers — they block.
        if lowered == "deny" || lowered == "sameorigin" || lowered.starts_with("allow-from") {
            return FramePolicy::Blocked {
                header: "x-frame-options",
                value: value.to_string(),
            };
        }
    }
    if let Some(csp) = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        && let Some(sources) = frame_ancestors(csp)
        && !permits(&sources, base_domain)
    {
        return FramePolicy::Blocked {
            header: "content-security-policy",
            value: format!("frame-ancestors {sources}"),
        };
    }
    FramePolicy::Allowed
}

/// The value of the `frame-ancestors` directive, wherever it sits in a policy list.
fn frame_ancestors(csp: &str) -> Option<String> {
    for directive in csp.split(';') {
        let directive = directive.trim();
        let (name, sources) = match directive.split_once(char::is_whitespace) {
            Some((name, sources)) => (name, sources.trim()),
            None => (directive, ""),
        };
        if name.eq_ignore_ascii_case("frame-ancestors") {
            return Some(sources.to_string());
        }
    }
    None
}

/// Does a `frame-ancestors` source list permit Nova's own origin?
fn permits(sources: &str, base_domain: &str) -> bool {
    // A malformed or empty list is ignored by browsers, which then allow framing.
    if sources.is_empty() {
        return true;
    }
    for source in sources.split_whitespace() {
        let source = source.trim_matches('\'');
        if source.is_empty() {
            continue;
        }
        if source.eq_ignore_ascii_case("none") {
            return false;
        }
        // `'self'` is the app's own origin, never Nova's — Nova lives on a
        // different subdomain — so it does not count as permission.
        if source == "*" || (source != "self" && host_of(source) == host_of(base_domain)) {
            return true;
        }
    }
    false
}

/// `https://alpha.dev.stardelt.io` and `*.alpha.dev.stardelt.io` → `alpha.dev.stardelt.io`.
fn host_of(source: &str) -> &str {
    source
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("*.")
        .trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "alpha.dev.stardelt.io";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn a_page_without_frame_headers_may_be_framed() {
        assert_eq!(
            frame_policy(&headers(&[("content-type", "text/html")]), BASE),
            FramePolicy::Allowed
        );
    }

    #[test]
    fn x_frame_options_deny_and_sameorigin_block() {
        for value in [
            "DENY",
            "SAMEORIGIN",
            "sameorigin",
            "allow-from https://other.example",
        ] {
            assert_eq!(
                frame_policy(&headers(&[("x-frame-options", value)]), BASE),
                FramePolicy::Blocked {
                    header: "x-frame-options",
                    value: value.to_string(),
                },
                "{value} must block"
            );
        }
    }

    #[test]
    fn jupyterhubs_actual_header_blocks_framing() {
        let policy = frame_policy(
            &headers(&[(
                "content-security-policy",
                "frame-ancestors 'none'; report-uri /hub/security/csp-report",
            )]),
            BASE,
        );
        assert_eq!(
            policy,
            FramePolicy::Blocked {
                header: "content-security-policy",
                value: "frame-ancestors 'none'".into(),
            }
        );
    }

    #[test]
    fn a_csp_that_does_not_mention_framing_may_be_framed() {
        let policy = frame_policy(
            &headers(&[(
                "content-security-policy",
                "default-src 'self'; script-src 'self' 'unsafe-inline'",
            )]),
            BASE,
        );
        assert_eq!(policy, FramePolicy::Allowed);
    }

    #[test]
    fn self_alone_does_not_allow_nova() {
        let policy = frame_policy(
            &headers(&[("content-security-policy", "frame-ancestors 'self'")]),
            BASE,
        );
        assert!(matches!(policy, FramePolicy::Blocked { .. }));
    }

    #[test]
    fn naming_nova_s_origin_allows_the_embed() {
        for sources in [
            "frame-ancestors 'self' https://alpha.dev.stardelt.io",
            "frame-ancestors *.alpha.dev.stardelt.io",
            "frame-ancestors *",
            "FRAME-ANCESTORS https://alpha.dev.stardelt.io",
        ] {
            assert_eq!(
                frame_policy(&headers(&[("content-security-policy", sources)]), BASE),
                FramePolicy::Allowed,
                "{sources} must allow"
            );
        }
    }

    #[test]
    fn an_unjudgeable_domain_blocks_nothing_but_a_none() {
        // With no base domain configured Nova has no origin to look for; only an
        // explicit 'none' is a definite answer.
        assert_eq!(
            frame_policy(
                &headers(&[("content-security-policy", "frame-ancestors 'none'")]),
                ""
            ),
            FramePolicy::Blocked {
                header: "content-security-policy",
                value: "frame-ancestors 'none'".into(),
            }
        );
        let unknown_origin = frame_policy(
            &headers(&[(
                "content-security-policy",
                "frame-ancestors https://novaless.example",
            )]),
            "",
        );
        assert!(matches!(unknown_origin, FramePolicy::Blocked { .. }));
    }

    #[test]
    fn hosts_are_normalised() {
        assert_eq!(host_of("https://alpha.dev.stardelt.io"), BASE);
        assert_eq!(host_of("*.alpha.dev.stardelt.io"), BASE);
        assert_eq!(host_of("alpha.dev.stardelt.io/"), BASE);
        // Quotes are stripped by `permits`, which is where `'self'` is judged.
        assert_eq!(host_of("'self'"), "'self'");
    }

    fn hosts() -> Vec<String> {
        vec![
            "hub".to_string(),
            "jupyter.alpha.dev.stardelt.io".to_string(),
        ]
    }

    #[test]
    fn a_redirect_between_the_app_s_own_hosts_is_not_a_sign_in() {
        // The hub's relative redirect to its own login page.
        assert_eq!(
            sign_in_host(
                "/hub/login?next=%2Fhub%2Fhome",
                "http://hub:8081/hub/home",
                &hosts()
            ),
            None
        );
        // The same app's public host, if it ever redirects to it.
        assert_eq!(
            sign_in_host(
                "https://jupyter.alpha.dev.stardelt.io/hub/home",
                "http://hub:8081/hub/home",
                &hosts()
            ),
            None
        );
    }

    #[test]
    fn a_redirect_to_the_idp_is_a_sign_in_detour() {
        assert_eq!(
            sign_in_host(
                "https://id.alpha.dev.stardelt.io/realms/stardelt/protocol/openid-connect/auth?client_id=jupyterhub",
                "http://hub:8081/hub/login",
                &hosts()
            ),
            Some("id.alpha.dev.stardelt.io".to_string())
        );
    }

    #[test]
    fn an_unparseable_location_has_no_host() {
        assert_eq!(
            sign_in_host("http://[", "http://hub:8081/hub/home", &hosts()),
            None
        );
    }
}
