//! Published notebooks: immutable object versions in the platform's S3 bucket
//! (stardelt-nova#225; design note `design/scheduling-and-notebooks.md` §3; spike
//! stardelt-platform#268).
//!
//! A scheduled notebook must run exactly the bytes a person approved, so publishing
//! writes a **new version** to the versioned bucket `notebooks` (created by
//! stardelt-platform#290) and answers the version id a `Pipeline` can pin. The store
//! keeps every version; a later publish, a delete or an S3-gateway restart does not
//! change what a pinned version returns.
//!
//! - **The person is the publisher.** Publishing needs a signed-in person — a browser
//!   session or an `Authorization: Bearer` token — and the group is checked against
//!   *their* groups: `403 you are not a member of the group <g>`. The hub token already
//!   carries audience `nova` (platform#290), which is the client id here, so a notebook
//!   can call Nova with the token it holds. No token is forwarded to S3: Nova signs
//!   every object-store request with the platform identity it was given
//!   (`NOVA_S3_*`), and the *only* thing that reaches S3 from a request is the checked
//!   `{group}/<name>` key.
//! - **Outputs never reach the store.** A notebook is parsed as nbformat 4 and every
//!   cell's `outputs` and `execution_count`, plus the notebook's widget metadata, are
//!   removed before the bytes are written. A cell that printed `os.environ` (the
//!   platform#268 canary: a token and a client secret) therefore stores no trace of it.
//!   The stored bytes are compact JSON, and the answer's `sha256` identifies them.
//! - **Parameters** are the `name = literal` lines of the first cell tagged
//!   `parameters` (papermill's convention): at most 32, the default exactly as it would
//!   run (a string literal without its quotes), never evaluated. The Schedule dialog
//!   reads them for one pinned version with `GET …/parameters` (#237), so it offers
//!   exactly the form the bytes it will run declare.
//! - **Visibility** is owner-group scoped: a person lists and versions the notebooks of
//!   their own groups; an administrator gets no wider view here, because the platform's
//!   administrators are not automatically members of a tenant's group.
//! - **Why presigned URLs.** `rusty-s3` implements SigV4 as presigned URLs; the shared
//!   `reqwest` client sends them. A signed URL *is* a credential (short-lived, five
//!   minutes), so no error, log line or response ever contains one: [`StoreError`]'s
//!   sentences are fixed text, and a `reqwest` error is logged only after
//!   `without_url()`. One call is the exception: SeaweedFS refuses the presigned
//!   `?versions` list that `rusty-s3`'s `sign` produces (`403 SignatureDoesNotMatch`),
//!   so that listing is signed in the headers instead (stardelt-nova#245) — same
//!   credentials, no signature in the URL at all.
//! - **Failure sentences.** Unset `NOVA_S3_*` is `503 notebook storage is not
//!   configured`; an S3 that is down or refuses is `502`, never `500` (rule U12 of
//!   `design/ui-conformance.md`); a 61st publish by one person in an hour is `429`.
//!   The audit line carries the group, the name, the version and the size — never a
//!   byte of the content.
//!
//! Answers are `application/json` with `Cache-Control: no-store`. Names are rendered
//! text-only downstream; this module never returns notebook content.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use futures::stream::{self, StreamExt};
use hmac::{Hmac, KeyInit as _, Mac as _};
use instant_xml::FromXml;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use rusty_s3::S3Action;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::AppState;
use crate::auth::{Identity, Principal, Secret};
use crate::connection_routes::Fail;

/// The largest notebook body Nova accepts, before stripping. A notebook is source
/// code, not data: 5 MiB is generous for cells and still bounds one request.
const MAX_NOTEBOOK_BYTES: usize = 5 * 1024 * 1024;
/// At most this many parameters are offered as the schedule form (ticket #225).
const MAX_PARAMETERS: usize = 32;
/// At most this many versions are listed for one notebook.
const MAX_VERSIONS: usize = 50;
/// Publishes per person per rolling hour. In memory, like the other rate limits:
/// only a restart forgets it.
const PUBLISHES_PER_HOUR: usize = 60;
const PUBLISH_WINDOW: Duration = Duration::from_secs(60 * 60);
/// One S3 request may take this long; a gateway that accepts TCP and never answers
/// must not hold a UI request open indefinitely.
const S3_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a presigned URL may be used. The request is sent immediately, so this is
/// only clock-skew headroom.
const PRESIGN_TTL: Duration = Duration::from_secs(300);
/// SeaweedFS accepts any SigV4 region; AWS SDKs default to this one.
const S3_REGION: &str = "us-east-1";
/// `ListObjects*` pages hold at most 1000 entries. Ten pages bound one listing.
const MAX_PAGES: usize = 10;
const NOTEBOOK_CONTENT_TYPE: &str = "application/x-ipynb+json";
const S3_XML_NS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";
/// The SHA-256 of an empty body — the value `x-amz-content-sha256` carries on every
/// request here (nothing this module sends has a body).
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// The headers a SigV4 header-signed request signs, in canonical order.
const SIGNED_HEADERS: &str = "host;x-amz-content-sha256;x-amz-date";
/// RFC 3986 §2.3: `A–Z a–z 0–9 - . _ ~` are the only characters a canonical query
/// leaves alone. Everything else is percent-encoded, a space as `%20` and never `+`
/// (which `form_urlencoded` would produce, and which AWS does not sign the same way).
const QUERY_UNSAFE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

// ---------------------------------------------------------------------------
// Configuration and the store
// ---------------------------------------------------------------------------

/// Where published notebooks live: `NOVA_S3_URL`, `NOVA_S3_ACCESS_KEY`,
/// `NOVA_S3_SECRET_KEY` and `NOVA_S3_NOTEBOOK_BUCKET`, all four or none.
///
/// A partial configuration is a startup error, not a silent fallback: the platform
/// either wires notebook publishing or it does not. Empty values count as unset, so a
/// chart that renders an empty string leaves the feature off exactly as an unset
/// variable does.
#[derive(Clone)]
pub struct NotebookConfig {
    pub endpoint: Url,
    pub access_key: String,
    pub secret_key: Secret,
    pub bucket: String,
}

impl NotebookConfig {
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_env_with(|k| std::env::var(k).ok())
    }

    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let word = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let url = word("NOVA_S3_URL");
        let access_key = word("NOVA_S3_ACCESS_KEY");
        let secret_key = get("NOVA_S3_SECRET_KEY").filter(|v| !v.is_empty());
        let bucket = word("NOVA_S3_NOTEBOOK_BUCKET");

        match (url, access_key, secret_key, bucket) {
            (None, None, None, None) => Ok(None),
            (Some(url), Some(access_key), Some(secret_key), Some(bucket)) => {
                let parsed = Url::parse(url.trim_end_matches('/'))
                    .map_err(|_| "NOVA_S3_URL is not a URL".to_string())?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err("NOVA_S3_URL must be an http(s) URL".into());
                }
                if parsed.path() != "/" {
                    return Err("NOVA_S3_URL must be an origin without a path".into());
                }
                Ok(Some(Self {
                    endpoint: parsed,
                    access_key,
                    secret_key: Secret::new(secret_key),
                    bucket,
                }))
            }
            _ => Err("NOVA_S3_URL, NOVA_S3_ACCESS_KEY, NOVA_S3_SECRET_KEY and \
                      NOVA_S3_NOTEBOOK_BUCKET must be set together"
                .into()),
        }
    }
}

/// The S3 client for published notebooks. `None` in `AppState` when the four
/// `NOVA_S3_*` variables are unset; every route here then answers 503.
pub struct NotebookStore {
    http: reqwest::Client,
    bucket: rusty_s3::Bucket,
    credentials: rusty_s3::Credentials,
    /// Per-person publish times inside the last hour.
    publishes: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl NotebookStore {
    pub fn new(http: reqwest::Client, config: NotebookConfig) -> Result<Self, String> {
        let bucket = rusty_s3::Bucket::new(
            config.endpoint,
            rusty_s3::UrlStyle::Path,
            config.bucket,
            S3_REGION,
        )
        .map_err(|e| format!("the S3 endpoint or bucket is not usable: {e}"))?;
        Ok(Self {
            http,
            bucket,
            credentials: rusty_s3::Credentials::new(config.access_key, config.secret_key.expose()),
            publishes: Mutex::new(HashMap::new()),
        })
    }

    /// Take one publish slot for `sub`, or report that the hour is full.
    fn take_publish_slot(&self, sub: &str) -> bool {
        self.take_publish_slot_at(sub, Instant::now())
    }

    fn take_publish_slot_at(&self, sub: &str, now: Instant) -> bool {
        let mut map = self.publishes.lock().expect("notebook limiter poisoned");
        let slots = map.entry(sub.to_string()).or_default();
        while slots
            .front()
            .is_some_and(|then| now.duration_since(*then) >= PUBLISH_WINDOW)
        {
            slots.pop_front();
        }
        if slots.len() >= PUBLISHES_PER_HOUR {
            false
        } else {
            slots.push_back(now);
            true
        }
    }

    /// Write one version and return the version id S3 assigned. Refuses a bucket
    /// without versioning: a schedule must pin bytes, and an unversioned object can
    /// never prove which bytes it was.
    async fn put(
        &self,
        key: &str,
        body: Vec<u8>,
        uploaded_by: &str,
        sha256: &str,
    ) -> Result<String, StoreError> {
        let uploaded_by = metadata_value(uploaded_by);
        let mut action = self.bucket.put_object(Some(&self.credentials), key);
        // Signed headers: presigning covers them, so the gateway gets the metadata as
        // part of the same immutable object version.
        action
            .headers_mut()
            .insert("content-type", NOTEBOOK_CONTENT_TYPE);
        action
            .headers_mut()
            .insert("x-amz-meta-uploaded-by", uploaded_by.clone());
        action
            .headers_mut()
            .insert("x-amz-meta-sha256", sha256.to_string());
        let url = action.sign(PRESIGN_TTL);
        let response = self
            .send(
                self.http
                    .put(url)
                    .header("content-type", NOTEBOOK_CONTENT_TYPE)
                    .header("x-amz-meta-uploaded-by", uploaded_by.as_str())
                    .header("x-amz-meta-sha256", sha256)
                    .body(body),
            )
            .await?;
        version_id(&response).ok_or(StoreError::Unversioned)
    }

    /// One object's metadata: the version, its size and time, and the user metadata
    /// the publish wrote.
    async fn head(&self, key: &str, version: Option<&str>) -> Result<HeadObject, StoreError> {
        let mut action = self.bucket.head_object(Some(&self.credentials), key);
        if let Some(version) = version {
            action.query_mut().insert("versionId", version.to_string());
        }
        let url = action.sign(PRESIGN_TTL);
        let response = self.send(self.http.head(url)).await?;
        let headers = response.headers();
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let object_version = text("x-amz-version-id").ok_or(StoreError::Unversioned)?;
        let size = text("content-length")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(StoreError::Malformed)?;
        let uploaded_at = text("last-modified").ok_or(StoreError::Malformed)?;
        Ok(HeadObject {
            object_version,
            size,
            uploaded_at,
            uploaded_by: text("x-amz-meta-uploaded-by")
                .map(|value| metadata_text(&value))
                .unwrap_or_default(),
            sha256: text("x-amz-meta-sha256").unwrap_or_default(),
        })
    }

    /// One object version's bytes and its version id: `version: None` reads the latest.
    /// The version comes from the response, so a read of the latest still names the bytes
    /// it answered (#237). A body over [`MAX_NOTEBOOK_BYTES`] is refused rather than
    /// buffered: the bucket is shared, and a foreign key must not become a 5 MiB read.
    async fn get(&self, key: &str, version: Option<&str>) -> Result<(Vec<u8>, String), StoreError> {
        let mut action = self.bucket.get_object(Some(&self.credentials), key);
        if let Some(version) = version {
            action.query_mut().insert("versionId", version.to_string());
        }
        let url = action.sign(PRESIGN_TTL);
        let response = self.send(self.http.get(url)).await?;
        let object_version = version_id(&response).ok_or(StoreError::Unversioned)?;
        let too_large = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|size| size > MAX_NOTEBOOK_BYTES as u64);
        if too_large {
            return Err(StoreError::Malformed);
        }
        let body = response.bytes().await.map_err(|_| StoreError::Malformed)?;
        if body.len() > MAX_NOTEBOOK_BYTES {
            return Err(StoreError::Malformed);
        }
        Ok((body.to_vec(), object_version))
    }

    /// Every object key under `prefix`, latest version only (one page after another, up
    /// to [`MAX_PAGES`]).
    async fn list_objects(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            action.with_prefix(prefix.to_string());
            action.with_max_keys(1000);
            if let Some(token) = &token {
                action.with_continuation_token(token.clone());
            }
            let url = action.sign(PRESIGN_TTL);
            let response = self.send(self.http.get(url)).await?;
            let body = response.text().await.map_err(|_| StoreError::Malformed)?;
            let page = rusty_s3::actions::ListObjectsV2::parse_response(&body)
                .map_err(|_| StoreError::Malformed)?;
            out.extend(page.contents.into_iter().map(|object| object.key));
            match page.next_continuation_token {
                Some(next) if !next.is_empty() => token = Some(next),
                _ => return Ok(out),
            }
        }
        Ok(out)
    }

    /// Every version under `prefix`, newest first as S3 returns them, up to
    /// [`MAX_PAGES`] pages.
    ///
    /// SeaweedFS refuses the presigned `?versions` list that `rusty-s3`'s `sign`
    /// produces (`403 SignatureDoesNotMatch`), so this one request is signed in its
    /// headers instead of its URL (stardelt-nova#245). Every other call here keeps the
    /// presigned form.
    async fn list_versions(&self, prefix: &str) -> Result<Vec<ListedVersion>, StoreError> {
        let mut out = Vec::new();
        let mut key_marker: Option<String> = None;
        let mut version_marker: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut query: Vec<(&str, &str)> =
                vec![("versions", ""), ("prefix", prefix), ("max-keys", "1000")];
            if let Some(marker) = &key_marker {
                query.push(("key-marker", marker));
            }
            if let Some(marker) = &version_marker {
                query.push(("version-id-marker", marker));
            }
            // The URL carries the parameters in the same RFC 3986 encoding that is
            // signed, so no `+` for a space can make the signature disagree with what
            // the gateway parses.
            let mut url = self.bucket.base_url().clone();
            url.set_query(Some(&canonical_query_string(query.into_iter())));
            let mut request = self.http.get(url.clone());
            for (name, value) in sign_headers(
                "GET",
                &url,
                S3_REGION,
                self.credentials.key(),
                self.credentials.secret(),
                jiff::Timestamp::now(),
            ) {
                request = request.header(name, value);
            }
            let response = self.send(request).await?;
            let body = response.text().await.map_err(|_| StoreError::Malformed)?;
            let page: ListVersionsResult =
                instant_xml::from_str(&body).map_err(|_| StoreError::Malformed)?;
            let truncated = page.is_truncated.as_deref() == Some("true");
            out.extend(page.versions.into_iter().map(|version| ListedVersion {
                key: version.key,
                object_version: version.version_id,
                size: version.size,
                last_modified: version.last_modified,
            }));
            if !truncated {
                return Ok(out);
            }
            match (page.next_key_marker, page.next_version_id_marker) {
                (Some(key), version) => {
                    key_marker = Some(key);
                    version_marker = version;
                }
                (None, _) => return Ok(out),
            }
        }
        Ok(out)
    }

    /// The rows of one group, latest versions only, without a particular order.
    async fn list_group(&self, group: &str) -> Result<Vec<NotebookSummary>, StoreError> {
        let prefix = format!("{group}/");
        let objects = self.list_objects(&prefix).await?;
        let heads = stream::iter(objects.into_iter().map(|key| async move {
            let head = self.head(&key, None).await;
            (key, head)
        }))
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;

        let mut rows = Vec::new();
        for (key, head) in heads {
            let head = match head {
                Ok(head) => head,
                // The object was deleted between the list and the head: it is not in
                // this listing anymore, which is exactly what the caller asks.
                Err(StoreError::Refused(404)) => continue,
                Err(e) => return Err(e),
            };
            let Some(name) = key.strip_prefix(&prefix) else {
                continue;
            };
            // Only direct children of the group prefix and only real notebook names:
            // the bucket is shared with the platform identity, and a key written by
            // something else must not become a row with a path in it.
            if name.contains('/') || !valid_name(name) {
                continue;
            }
            rows.push(NotebookSummary {
                group: group.to_string(),
                name: name.to_string(),
                object_version: head.object_version,
                size: head.size,
                uploaded_at: rfc3339_utc(&head.uploaded_at),
                uploaded_by: head.uploaded_by,
            });
        }
        Ok(rows)
    }

    /// The versions of exactly `key`, newest first, at most [`MAX_VERSIONS`].
    async fn versions_of(&self, key: &str) -> Result<Vec<NotebookVersion>, StoreError> {
        let listed = self.list_versions(key).await?;
        let listed: Vec<ListedVersion> = listed
            .into_iter()
            .filter(|version| version.key == key)
            .take(MAX_VERSIONS)
            .collect();
        let heads = stream::iter(listed.into_iter().map(|version| async move {
            let head = self.head(&version.key, Some(&version.object_version)).await;
            (version, head)
        }))
        // `buffered` keeps the store's newest-first order; `buffer_unordered` would let
        // a slow HEAD reorder the versions in the answer.
        .buffered(8)
        .collect::<Vec<_>>()
        .await;

        let mut rows = Vec::new();
        for (version, head) in heads {
            let head = match head {
                Ok(head) => head,
                Err(StoreError::Refused(404)) => continue,
                Err(e) => return Err(e),
            };
            rows.push(NotebookVersion {
                object_version: version.object_version,
                size: version.size,
                uploaded_at: rfc3339_utc(&version.last_modified),
                uploaded_by: head.uploaded_by,
                sha256: head.sha256,
            });
        }
        Ok(rows)
    }

    /// Send one signed request. Never log the URL (it carries the signature) and never
    /// put a response body into an error: S3 can echo anything.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, StoreError> {
        let response = request.timeout(S3_TIMEOUT).send().await.map_err(|e| {
            tracing::warn!(error = %e.without_url(), "notebook store request failed");
            StoreError::Unreachable
        })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            tracing::warn!(status, "notebook store refused a request");
            return Err(StoreError::Refused(status));
        }
        Ok(response)
    }
}

fn version_id(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get("x-amz-version-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// SigV4 header signing (stardelt-nova#245)
// ---------------------------------------------------------------------------

/// The SigV4 headers for a GET without a body: `Authorization`, `x-amz-date` and
/// `x-amz-content-sha256`. `now` is a parameter so the test pins the exact date.
///
/// Only `ListObjectVersions` uses this. `rusty-s3` has no action for that list and its
/// public `sign` presigns the URL, which is exactly the shape SeaweedFS refuses for
/// `?versions`; the header form authenticates the same request the same way.
fn sign_headers(
    method: &str,
    url: &Url,
    region: &str,
    access_key: &str,
    secret_key: &str,
    now: jiff::Timestamp,
) -> Vec<(String, String)> {
    let amz_date = now.strftime("%Y%m%dT%H%M%SZ").to_string();
    let date = now.strftime("%Y%m%d").to_string();
    let canonical_request = format!(
        "{method}\n{}\n{}\nhost:{}\nx-amz-content-sha256:{EMPTY_SHA256}\nx-amz-date:{amz_date}\n\n{SIGNED_HEADERS}\n{EMPTY_SHA256}",
        url.path(),
        canonical_query(url),
        host_header(url),
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    // The signing key is `AWS4` + the secret, then a chain of HMACs down to the request.
    let mut prefix = b"AWS4".to_vec();
    prefix.extend_from_slice(secret_key.as_bytes());
    let date_key = hmac_sha256(&prefix, date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, b"s3");
    let signing_key = hmac_sha256(&service_key, b"aws4_request");
    let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    vec![
        (
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, \
                 SignedHeaders={SIGNED_HEADERS}, Signature={signature}"
            ),
        ),
        ("x-amz-date".to_string(), amz_date),
        ("x-amz-content-sha256".to_string(), EMPTY_SHA256.to_string()),
    ]
}

/// The canonical query of `url`: the parameters sorted by name, names and values
/// percent-encoded per RFC 3986. A parameter written without a value signs as
/// `name=` — which is how `?versions` is signed.
fn canonical_query(url: &Url) -> String {
    let params: Vec<(&str, &str)> = url
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|item| !item.is_empty())
        .map(|item| item.split_once('=').unwrap_or((item, "")))
        .collect();
    canonical_query_string(params.into_iter())
}

/// Write `params` as a canonical query: sorted by name, percent-encoded per RFC 3986.
/// One function builds the URL's query and the signed query, so the two cannot differ.
fn canonical_query_string<'a>(params: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut params: Vec<(String, String)> = params
        .map(|(name, value)| {
            (
                percent_decode_str(name).decode_utf8_lossy().into_owned(),
                percent_decode_str(value).decode_utf8_lossy().into_owned(),
            )
        })
        .collect();
    params.sort();
    params
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                utf8_percent_encode(name, QUERY_UNSAFE),
                utf8_percent_encode(value, QUERY_UNSAFE)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// The `Host` header a request to `url` carries: the port only when it is not the
/// scheme's default.
fn host_header(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match (url.scheme(), url.port()) {
        ("http" | "https", None) | ("http", Some(80)) | ("https", Some(443)) => host.to_string(),
        (_, Some(port)) => format!("{host}:{port}"),
        (_, None) => host.to_string(),
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// S3 answers two date shapes: RFC 1123 (`Wed, 21 Oct 2026 01:24:28 GMT`, what SeaweedFS
/// writes into `Last-Modified` and its version listing) and RFC 3339 (what some gateways
/// list). Every row answers RFC 3339 in UTC with milliseconds, so one shape reaches the
/// UI; a value in neither shape is passed through rather than failing the whole listing.
fn rfc3339_utc(value: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(value));
    match parsed {
        Ok(time) => time
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        Err(_) => value.to_string(),
    }
}

/// Why an object-store request failed. Every sentence is fixed text: no URL, no
/// signature, no response body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StoreError {
    /// DNS, TCP or TLS: the gateway did not answer.
    Unreachable,
    /// It answered non-2xx.
    Refused(u16),
    /// A 2xx without the fields this module needs.
    Malformed,
    /// A write or a read of an object without a version id: versioning is off.
    Unversioned,
}

fn store_fail(error: StoreError) -> Fail {
    match error {
        StoreError::Unreachable => {
            Fail::new(StatusCode::BAD_GATEWAY, "the notebook store did not answer")
        }
        StoreError::Refused(status) => Fail::new(
            StatusCode::BAD_GATEWAY,
            format!("the notebook store refused the request (HTTP {status})"),
        ),
        StoreError::Malformed => Fail::new(
            StatusCode::BAD_GATEWAY,
            "the notebook store sent an unexpected answer",
        ),
        StoreError::Unversioned => Fail::new(
            StatusCode::BAD_GATEWAY,
            "the notebook store did not return an object version",
        ),
    }
}

/// Whether `version` of `group/name` exists in the store — the check a notebook schedule
/// makes before it pins one (stardelt-nova#237). `Ok(false)` means the store answered
/// `404`: the version was never published, or it is gone. `Err` is the sentence the
/// caller sees: unset `NOVA_S3_*` is 503, an S3 that is down or refuses is 502.
pub(crate) async fn head_version(
    store: Option<&NotebookStore>,
    group: &str,
    name: &str,
    version: &str,
) -> Result<bool, Fail> {
    let store = store.ok_or_else(no_storage)?;
    // No such key can exist: the caller's own rules already refuse these names, and a
    // version with whitespace in it could not have been written by a publish.
    if !valid_group(group) || !valid_name(name) || !object_version_ok(version) {
        return Ok(false);
    }
    match store.head(&object_key(group, name), Some(version)).await {
        Ok(_) => Ok(true),
        Err(StoreError::Refused(404)) => Ok(false),
        Err(error) => Err(store_fail(error)),
    }
}

/// `^\S{1,128}$`: the object-version shape the CRD accepts (`config.objectVersion`).
pub(crate) fn object_version_ok(version: &str) -> bool {
    let count = version.chars().count();
    (1..=128).contains(&count) && !version.chars().any(char::is_whitespace)
}

struct HeadObject {
    object_version: String,
    size: u64,
    uploaded_at: String,
    uploaded_by: String,
    sha256: String,
}

struct ListedVersion {
    key: String,
    object_version: String,
    size: u64,
    last_modified: String,
}

/// `<ListVersionsResult>` from `GET /<bucket>?versions` (the one S3 action
/// `rusty-s3` does not model). `IsTruncated` stays text on purpose: a strict `bool`
/// parser would fail the whole listing over an unexpected spelling.
#[derive(Debug, FromXml)]
#[xml(rename = "ListVersionsResult", ns(S3_XML_NS))]
struct ListVersionsResult {
    #[xml(rename = "Version")]
    versions: Vec<ListVersionsVersion>,
    #[xml(rename = "IsTruncated")]
    is_truncated: Option<String>,
    #[xml(rename = "NextKeyMarker")]
    next_key_marker: Option<String>,
    #[xml(rename = "NextVersionIdMarker")]
    next_version_id_marker: Option<String>,
}

#[derive(Debug, FromXml)]
#[xml(rename = "Version", ns(S3_XML_NS))]
struct ListVersionsVersion {
    #[xml(rename = "Key")]
    key: String,
    #[xml(rename = "VersionId")]
    version_id: String,
    #[xml(rename = "LastModified")]
    last_modified: String,
    #[xml(rename = "Size")]
    size: u64,
}

// ---------------------------------------------------------------------------
// Request and answer shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct NotebookParameter {
    pub name: String,
    /// The default as the cell would run it: a string literal without its quotes, any
    /// other literal exactly as written. Never evaluated.
    pub default: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublishedNotebook {
    pub name: String,
    pub group: String,
    /// The immutable S3 version id a schedule pins.
    pub object_version: String,
    /// Lower-case hex SHA-256 of the stored bytes (outputs already stripped).
    pub sha256: String,
    pub size: u64,
    pub parameters: Vec<NotebookParameter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotebookSummary {
    pub group: String,
    pub name: String,
    /// The latest version.
    pub object_version: String,
    pub size: u64,
    /// The version's `LastModified` as RFC 3339 in UTC (`2026-10-21T01:24:28.000Z`),
    /// whichever of S3's two date shapes the gateway answered.
    pub uploaded_at: String,
    /// The `x-amz-meta-uploaded-by` the publisher wrote (their display name).
    pub uploaded_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotebookVersion {
    pub object_version: String,
    pub size: u64,
    /// The version's `LastModified` as RFC 3339 in UTC (`2026-10-21T01:24:28.000Z`),
    /// whichever of S3's two date shapes the gateway answered.
    pub uploaded_at: String,
    pub uploaded_by: String,
    pub sha256: String,
}

/// `GET /api/notebooks/{group}/{name}/parameters`: the parameters of one pinned version
/// (stardelt-nova#237).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotebookParameters {
    /// The `name = literal` lines of the first cell tagged `parameters`, in order, at most
    /// 32; empty when no cell carries the tag.
    pub parameters: Vec<NotebookParameter>,
    /// The version the answer describes — the one asked for, or the latest when the query
    /// named none. A schedule pins this exact id.
    pub object_version: String,
}

/// The `?version=` of the parameters route: omitted means the latest version.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
pub struct ParametersQuery {
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// `^[A-Za-z0-9][A-Za-z0-9._ -]{0,127}\.ipynb$`, without a regex dependency: the
/// suffix is literal, the stem is 1 to 128 ASCII characters, and it cannot contain a
/// slash (so a name can never escape its group's prefix). Shared with the schedule rules
/// (`pipelines.rs`), so a notebook a publish accepts is one a schedule can name.
pub(crate) fn valid_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".ipynb") else {
        return false;
    };
    if stem.is_empty() || stem.len() > 128 {
        return false;
    }
    let mut chars = stem.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    stem.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-'))
}

/// A group name is used as an object key prefix. Groups come from the IdP and are
/// checked against the caller's groups first; this keeps a group that contains a slash
/// (some IdPs report full paths) from escaping the prefix.
fn valid_group(group: &str) -> bool {
    !group.is_empty()
        && group.len() <= 253
        && group
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && group
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn object_key(group: &str, name: &str) -> String {
    format!("{group}/{name}")
}

/// S3 user metadata is US-ASCII. A display name can be anything the IdP reports
/// (`Jörg Müller`), and `HeaderValue` rejects non-ASCII, so the value is
/// percent-encoded on the way in and decoded on the way out. A plain name is
/// unchanged.
const META_UNSAFE: &AsciiSet = &CONTROLS.add(b'%').add(b'"').add(b'\\').add(b' ');

fn metadata_value(value: &str) -> String {
    utf8_percent_encode(value, META_UNSAFE).to_string()
}

fn metadata_text(value: &str) -> String {
    percent_decode_str(value).decode_utf8_lossy().into_owned()
}

fn bad_name() -> Fail {
    Fail::new(
        StatusCode::BAD_REQUEST,
        "a notebook name must start with a letter or digit, end in .ipynb, and contain only letters, digits, dots, underscores, spaces and dashes",
    )
}

fn not_a_notebook() -> Fail {
    Fail::new(
        StatusCode::BAD_REQUEST,
        "the body is not an nbformat 4 notebook (it needs `nbformat: 4` and a `cells` array)",
    )
}

fn no_storage() -> Fail {
    Fail::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "notebook storage is not configured",
    )
}

fn sign_in_again() -> Fail {
    Fail::new(StatusCode::UNAUTHORIZED, "sign in again")
}

fn forbidden_group(group: &str) -> Fail {
    Fail::new(
        StatusCode::FORBIDDEN,
        format!("you are not a member of the group {group}"),
    )
}

fn too_many_publishes() -> Fail {
    Fail::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too many notebooks published this hour; wait before publishing another",
    )
}

/// The version the caller named is not in the store (#237): never published, or gone.
fn no_such_version() -> Fail {
    Fail::new(StatusCode::NOT_FOUND, "no such notebook version")
}

// ---------------------------------------------------------------------------
// Notebook transformation
// ---------------------------------------------------------------------------

/// Strip what must never reach the object store: every cell's `outputs` and
/// `execution_count`, and the notebook's widget metadata (whose state can hold the
/// output of a widget). The source, tags and the rest of the metadata stay.
fn strip_outputs(notebook: &mut serde_json::Value) {
    if let Some(cells) = notebook
        .get_mut("cells")
        .and_then(|cells| cells.as_array_mut())
    {
        for cell in cells {
            if let Some(cell) = cell.as_object_mut() {
                cell.remove("outputs");
                cell.remove("execution_count");
            }
        }
    }
    if let Some(metadata) = notebook.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        metadata.remove("widgets");
    }
}

/// The source of a cell, which nbformat allows as one string or an array of lines.
fn cell_source(cell: &serde_json::Value) -> String {
    match cell.get("source") {
        Some(serde_json::Value::String(source)) => source.clone(),
        Some(serde_json::Value::Array(lines)) => lines
            .iter()
            .filter_map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The default as text. A single-quoted or double-quoted string loses its quotes
/// (that is the value the cell would bind); every other literal stays exactly as
/// written. Nothing is evaluated, so a default cannot execute code.
fn default_text(value: &str) -> String {
    if value.len() >= 2
        && !value.starts_with("\"\"\"")
        && !value.starts_with("'''")
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        let quote = value.chars().next().expect("a quote");
        // Only a single string literal: `"a" + "b"` is an expression, and stripping
        // its outer quotes would invent a value the cell never binds.
        if !value[1..value.len() - 1].contains(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// The `name = literal` lines of a parameter cell, in order, first name wins, at most
/// [`MAX_PARAMETERS`].
fn parse_parameters(source: &str) -> Vec<NotebookParameter> {
    let mut out: Vec<NotebookParameter> = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if !is_identifier(name) || value.starts_with('=') {
            continue;
        }
        if out.iter().any(|parameter| parameter.name == name) {
            continue;
        }
        out.push(NotebookParameter {
            name: name.to_string(),
            default: default_text(value),
        });
        if out.len() == MAX_PARAMETERS {
            break;
        }
    }
    out
}

/// The parameters from the first cell tagged `parameters`; none when no cell carries
/// the tag.
fn parameters_of(notebook: &serde_json::Value) -> Vec<NotebookParameter> {
    let Some(cells) = notebook.get("cells").and_then(|cells| cells.as_array()) else {
        return Vec::new();
    };
    for cell in cells {
        let tagged = cell
            .get("metadata")
            .and_then(|metadata| metadata.get("tags"))
            .and_then(|tags| tags.as_array())
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some("parameters")));
        if tagged {
            return parse_parameters(&cell_source(cell));
        }
    }
    Vec::new()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lower-case hex of a digest or a signature.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// The three routes, all uncacheable (the answer carries a person's notebooks).
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/notebooks", get(list))
        .route("/notebooks/{group}/{name}", put(publish))
        .route("/notebooks/{group}/{name}/versions", get(versions))
        .route("/notebooks/{group}/{name}/parameters", get(parameters))
        .route_layer(axum::middleware::from_fn(no_store))
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `PUT /api/notebooks/{group}/{name}`: validate, strip, store one immutable version.
#[utoipa::path(
    put,
    path = "/api/notebooks/{group}/{name}",
    tag = "public",
    description = "Publish a notebook as a new immutable object version in the `notebooks` bucket. Needs a signed-in person whose groups include `{group}` (403 `you are not a member of the group <g>`; administrators are not exempt). `{name}` must match `^[A-Za-z0-9][A-Za-z0-9._ -]{0,127}\\.ipynb$` and the body must be an nbformat 4 notebook of at most 5 MiB. Every cell's `outputs` and `execution_count` and the notebook's widget metadata are removed before storing — no cell output reaches the object store. `sha256` is of the stored (stripped) bytes. `parameters` are the `name = literal` lines of the first cell tagged `parameters`, at most 32; defaults are text and are never evaluated. A 61st publish by one person in an hour is 429. Unset `NOVA_S3_*` is 503 `notebook storage is not configured`; an S3 that is down or refuses is 502. Each publish writes one audit line with group, name, version and size, never content.",
    params(
        ("group" = String, Path, description = "One of the caller's groups"),
        ("name" = String, Path, description = "Notebook file name, ending in .ipynb"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "The stored version and its parsed parameters", body = PublishedNotebook),
        (status = 400, description = "Bad name or not an nbformat 4 notebook", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not a member of the group", body = crate::openapi::ErrorResponse),
        (status = 413, description = "Larger than 5 MiB", body = crate::openapi::ErrorResponse),
        (status = 429, description = "More than 60 publishes in an hour", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The notebook store did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Notebook storage is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn publish(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((group, name)): Path<(String, String)>,
    request: Request,
) -> Response {
    let Some(store) = state.notebooks.as_ref() else {
        return no_storage().into_response();
    };
    let Some(Extension(principal)) = principal else {
        return sign_in_again().into_response();
    };
    if !principal.identity.groups.iter().any(|g| g == &group) {
        return forbidden_group(&group).into_response();
    }
    if !valid_group(&group) {
        return Fail::new(
            StatusCode::BAD_REQUEST,
            "the group name cannot be used as a notebook prefix",
        )
        .into_response();
    }
    if !valid_name(&name) {
        return bad_name().into_response();
    }

    let body = match read_body(request).await {
        Ok(body) => body,
        Err(fail) => return fail.into_response(),
    };
    let mut notebook: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(notebook) => notebook,
        Err(_) => {
            return Fail::new(
                StatusCode::BAD_REQUEST,
                "the body is not a notebook: it is not valid JSON",
            )
            .into_response();
        }
    };
    if notebook.get("nbformat").and_then(serde_json::Value::as_u64) != Some(4)
        || !notebook
            .get("cells")
            .is_some_and(serde_json::Value::is_array)
    {
        return not_a_notebook().into_response();
    }

    let parameters = parameters_of(&notebook);
    strip_outputs(&mut notebook);
    let stored = match serde_json::to_vec(&notebook) {
        Ok(stored) => stored,
        Err(_) => {
            return Fail::new(StatusCode::BAD_REQUEST, "the notebook cannot be serialized")
                .into_response();
        }
    };
    let sha256 = sha256_hex(&stored);
    let size = stored.len() as u64;

    // A valid publish consumes a slot; every refusal above did not.
    if !store.take_publish_slot(&principal.identity.sub) {
        return too_many_publishes().into_response();
    }

    let key = object_key(&group, &name);
    let version = match store
        .put(&key, stored, &principal.identity.display_name, &sha256)
        .await
    {
        Ok(version) => version,
        Err(error) => return store_fail(error).into_response(),
    };

    crate::audit::notebook_publish_event(&principal.identity, &group, &name, &version, size);

    Json(PublishedNotebook {
        name,
        group,
        object_version: version,
        sha256,
        size,
        parameters,
    })
    .into_response()
}

/// `GET /api/notebooks`: the latest version of every notebook in the caller's groups,
/// newest first. Administrators see their own groups only.
#[utoipa::path(
    get,
    path = "/api/notebooks",
    tag = "public",
    description = "The latest published version of every notebook in the caller's groups, newest first. Each row carries the group, the name, the latest `objectVersion`, its size, `uploadedAt` as an RFC 3339 timestamp in UTC (SeaweedFS's RFC 1123 `LastModified` normalised) and the publisher's `uploadedBy` display name. An administrator gets no wider view than their own groups. Unset `NOVA_S3_*` is 503; an S3 that is down or refuses is 502. Answers `Cache-Control: no-store`.",
    responses(
        (status = 200, description = "One row per notebook", body = [NotebookSummary]),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The notebook store did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Notebook storage is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Response {
    let Some(store) = state.notebooks.as_ref() else {
        return no_storage().into_response();
    };
    let Some(Extension(principal)) = principal else {
        return sign_in_again().into_response();
    };

    let groups = caller_groups(&principal.identity);
    let mut rows = Vec::new();
    for group in groups {
        match store.list_group(&group).await {
            Ok(mut group_rows) => rows.append(&mut group_rows),
            Err(error) => return store_fail(error).into_response(),
        }
    }
    rows.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at));
    Json(rows).into_response()
}

/// `GET /api/notebooks/{group}/{name}/versions`: the versions of one notebook, newest
/// first, at most 50.
#[utoipa::path(
    get,
    path = "/api/notebooks/{group}/{name}/versions",
    tag = "public",
    description = "The published versions of one notebook, newest first, at most 50. Needs a member of `{group}` (403 `you are not a member of the group <g>`; administrators are not exempt). Each row carries the immutable `objectVersion`, its size, `uploadedAt` as an RFC 3339 timestamp in UTC (SeaweedFS's RFC 1123 `LastModified` normalised), the publisher's `uploadedBy` and the `sha256` of the stored bytes. A group with no such notebook answers an empty list. Unset `NOVA_S3_*` is 503; an S3 that is down or refuses is 502. Answers `Cache-Control: no-store`.",
    params(
        ("group" = String, Path, description = "One of the caller's groups"),
        ("name" = String, Path, description = "Notebook file name, ending in .ipynb"),
    ),
    responses(
        (status = 200, description = "The versions, newest first", body = [NotebookVersion]),
        (status = 400, description = "Bad name", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not a member of the group", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The notebook store did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Notebook storage is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn versions(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((group, name)): Path<(String, String)>,
) -> Response {
    let Some(store) = state.notebooks.as_ref() else {
        return no_storage().into_response();
    };
    let Some(Extension(principal)) = principal else {
        return sign_in_again().into_response();
    };
    if !principal.identity.groups.iter().any(|g| g == &group) {
        return forbidden_group(&group).into_response();
    }
    if !valid_group(&group) {
        return Fail::new(
            StatusCode::BAD_REQUEST,
            "the group name cannot be used as a notebook prefix",
        )
        .into_response();
    }
    if !valid_name(&name) {
        return bad_name().into_response();
    }

    match store.versions_of(&object_key(&group, &name)).await {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => store_fail(error).into_response(),
    }
}

/// `GET /api/notebooks/{group}/{name}/parameters`: the parameters of one version, so the
/// Schedule dialog can offer the form the notebook declares (stardelt-nova#237).
#[utoipa::path(
    get,
    path = "/api/notebooks/{group}/{name}/parameters",
    tag = "public",
    description = "The parameters of one published version of a notebook, read from the stored bytes: the `name = literal` lines of the first cell tagged `parameters`, in order, at most 32, defaults as text and never evaluated. `?version=<id>` names the immutable version; omitted, the latest is read and its id answered. Needs a member of `{group}` (403 `you are not a member of the group <g>`; administrators are not exempt). An unknown version is 404 `no such notebook version`. The answer carries `parameters` and the `objectVersion` it describes — a schedule pins that id. Unset `NOVA_S3_*` is 503 `notebook storage is not configured`; an S3 that is down or refuses is 502. Answers `Cache-Control: no-store`.",
    params(
        ("group" = String, Path, description = "One of the caller's groups"),
        ("name" = String, Path, description = "Notebook file name, ending in .ipynb"),
        ParametersQuery,
    ),
    responses(
        (status = 200, description = "The parameters and the version they belong to", body = NotebookParameters),
        (status = 400, description = "Bad name", body = crate::openapi::ErrorResponse),
        (status = 401, description = "Not signed in", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Not a member of the group", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such notebook version", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The notebook store did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Notebook storage is not configured", body = crate::openapi::ErrorResponse),
    )
)]
pub(crate) async fn parameters(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path((group, name)): Path<(String, String)>,
    Query(query): Query<ParametersQuery>,
) -> Response {
    let Some(store) = state.notebooks.as_ref() else {
        return no_storage().into_response();
    };
    let Some(Extension(principal)) = principal else {
        return sign_in_again().into_response();
    };
    if !principal.identity.groups.iter().any(|g| g == &group) {
        return forbidden_group(&group).into_response();
    }
    if !valid_group(&group) {
        return Fail::new(
            StatusCode::BAD_REQUEST,
            "the group name cannot be used as a notebook prefix",
        )
        .into_response();
    }
    if !valid_name(&name) {
        return bad_name().into_response();
    }
    let version = query
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if version.is_some_and(|version| !object_version_ok(version)) {
        // A version with whitespace or over 128 characters never came from a publish.
        return no_such_version().into_response();
    }

    let (bytes, object_version) = match store.get(&object_key(&group, &name), version).await {
        Ok(read) => read,
        Err(StoreError::Refused(404)) => return no_such_version().into_response(),
        Err(error) => return store_fail(error).into_response(),
    };
    // The stored bytes are Nova's own compact JSON (outputs already stripped), so this is a
    // parse, not a validation: a key something else wrote is not a notebook here, and the
    // store's answer is what was unexpected.
    let notebook: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(notebook) => notebook,
        Err(_) => return store_fail(StoreError::Malformed).into_response(),
    };
    Json(NotebookParameters {
        parameters: parameters_of(&notebook),
        object_version,
    })
    .into_response()
}

/// The caller's groups, deduplicated and sorted, for a deterministic listing order
/// before the rows are sorted by time.
fn caller_groups(identity: &Identity) -> Vec<String> {
    identity
        .groups
        .iter()
        .filter(|group| valid_group(group))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Read at most [`MAX_NOTEBOOK_BYTES`] bytes; one byte more is enough to know the body
/// is too large.
async fn read_body(request: Request) -> Result<Vec<u8>, Fail> {
    match axum::body::to_bytes(request.into_body(), MAX_NOTEBOOK_BYTES + 1).await {
        Ok(body) if body.len() <= MAX_NOTEBOOK_BYTES => Ok(body.to_vec()),
        _ => Err(Fail::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "a notebook must be 5 MiB or smaller",
        )),
    }
}

/// The fake S3 gateway the notebook tests use (stardelt-nova#225), shared with the
/// schedule tests in `pipelines.rs` (#237): PUT, HEAD, GET of one version, ListObjectsV2
/// and ListObjectVersions, all in memory.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    use std::collections::HashMap as Map;

    use axum::body::Bytes;
    use axum::extract::DefaultBodyLimit;
    use axum::http::{HeaderMap, Method, Uri};
    use axum::routing::any;

    // -----------------------------------------------------------------------
    // A fake S3 gateway: PUT, HEAD, ListObjectsV2 and ListObjectVersions
    // -----------------------------------------------------------------------

    #[derive(Clone)]
    pub(crate) struct FakeS3 {
        pub(crate) state: Arc<Mutex<FakeState>>,
        pub(crate) base: String,
    }

    #[derive(Default)]
    pub(crate) struct FakeState {
        pub(crate) objects: Vec<StoredObject>,
        pub(crate) next: u64,
    }

    #[derive(Clone)]
    pub(crate) struct StoredObject {
        pub(crate) key: String,
        pub(crate) version: String,
        pub(crate) body: Vec<u8>,
        pub(crate) content_type: String,
        pub(crate) uploaded_by: String,
        pub(crate) sha256: String,
        pub(crate) last_modified: String,
        pub(crate) seq: u64,
    }

    impl FakeS3 {
        pub(crate) async fn start() -> Self {
            let state = Arc::new(Mutex::new(FakeState::default()));
            let app = Router::new()
                .fallback(any(fake_s3))
                // Bigger than the publish limit, so the 413 comes from Nova, not the fake.
                .layer(DefaultBodyLimit::max(MAX_NOTEBOOK_BYTES + 4096))
                .with_state(state.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind fake S3");
            let base = format!("http://{}", listener.local_addr().expect("fake S3 addr"));
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Self { state, base }
        }

        /// Write a version directly, for tests that only need a listing to exist.
        pub(crate) fn put_raw(&self, key: &str, body: &[u8], uploaded_by: &str) -> String {
            let mut state = self.state.lock().expect("fake S3 state");
            state.next += 1;
            let seq = state.next;
            let version = format!("v{seq}");
            state.objects.push(StoredObject {
                key: key.to_string(),
                version: version.clone(),
                body: body.to_vec(),
                content_type: NOTEBOOK_CONTENT_TYPE.into(),
                uploaded_by: uploaded_by.to_string(),
                sha256: sha256_hex(body),
                last_modified: modified(seq),
                seq,
            });
            version
        }

        pub(crate) fn objects(&self) -> Vec<StoredObject> {
            self.state.lock().expect("fake S3 state").objects.clone()
        }

        pub(crate) fn object(&self, key: &str) -> StoredObject {
            self.objects()
                .into_iter()
                .rev()
                .find(|object| object.key == key)
                .unwrap_or_else(|| panic!("no stored object {key}"))
        }

        pub(crate) fn is_empty(&self) -> bool {
            self.state.lock().expect("fake S3 state").objects.is_empty()
        }
    }

    fn modified(seq: u64) -> String {
        format!("2026-10-20T12:{:02}:{:02}.000Z", (seq / 60) % 60, seq % 60)
    }

    fn xml_escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// One handler for every S3 method the store sends. Signatures are not verified (the
    /// real gateway does that), but *where* the signature sits is: a `?versions` list
    /// must be header-signed, as SeaweedFS requires (stardelt-nova#245).
    async fn fake_s3(
        State(state): State<Arc<Mutex<FakeState>>>,
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response {
        let path = percent_encoding::percent_decode_str(uri.path())
            .decode_utf8_lossy()
            .into_owned();
        let path = path.trim_start_matches('/');
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        if bucket != "notebooks" {
            return StatusCode::NOT_FOUND.into_response();
        }
        let query: Map<String, String> =
            url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
                .into_owned()
                .collect();

        if method == Method::PUT {
            let mut state = state.lock().expect("fake S3 state");
            state.next += 1;
            let seq = state.next;
            let version = format!("v{seq}");
            state.objects.push(StoredObject {
                key: key.to_string(),
                version: version.clone(),
                body: body.to_vec(),
                content_type: header_text(&headers, "content-type").unwrap_or_default(),
                uploaded_by: header_text(&headers, "x-amz-meta-uploaded-by").unwrap_or_default(),
                sha256: header_text(&headers, "x-amz-meta-sha256").unwrap_or_default(),
                last_modified: modified(seq),
                seq,
            });
            return (StatusCode::OK, [("x-amz-version-id", version)]).into_response();
        }

        if method == Method::HEAD {
            let state = state.lock().expect("fake S3 state");
            let wanted = query.get("versionId");
            let object = state.objects.iter().rev().find(|object| {
                object.key == key && wanted.is_none_or(|version| object.version == *version)
            });
            let Some(object) = object else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let mut answer = HeaderMap::new();
            answer.insert(
                "x-amz-version-id",
                HeaderValue::from_str(&object.version).expect("version header"),
            );
            answer.insert(
                "content-length",
                HeaderValue::from_str(&object.body.len().to_string()).expect("size header"),
            );
            answer.insert(
                "last-modified",
                HeaderValue::from_str(&object.last_modified).expect("date header"),
            );
            answer.insert(
                "x-amz-meta-uploaded-by",
                HeaderValue::from_str(&object.uploaded_by).expect("uploader header"),
            );
            answer.insert(
                "x-amz-meta-sha256",
                HeaderValue::from_str(&object.sha256).expect("sha header"),
            );
            return (StatusCode::OK, answer).into_response();
        }

        if method == Method::GET && !key.is_empty() && !query.contains_key("versions") {
            // One object version (the parameters route, #237): the body and the version id
            // a real gateway answers in `x-amz-version-id`.
            let state = state.lock().expect("fake S3 state");
            let wanted = query.get("versionId");
            let object = state.objects.iter().rev().find(|object| {
                object.key == key && wanted.is_none_or(|version| object.version == *version)
            });
            let Some(object) = object else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let mut answer = HeaderMap::new();
            answer.insert(
                "x-amz-version-id",
                HeaderValue::from_str(&object.version).expect("version header"),
            );
            answer.insert(
                "content-length",
                HeaderValue::from_str(&object.body.len().to_string()).expect("size header"),
            );
            answer.insert(
                "content-type",
                HeaderValue::from_str(&object.content_type).expect("type header"),
            );
            return (StatusCode::OK, answer, object.body.clone()).into_response();
        }

        if method == Method::GET && query.contains_key("versions") {
            // SeaweedFS refuses the presigned `?versions` URL `rusty-s3` produced
            // (`403 SignatureDoesNotMatch`) and accepts the header-signed form, so the
            // fake does exactly that: the signature must sit in `Authorization`, never in
            // the query (stardelt-nova#245). A regression back to a presigned URL must
            // fail here.
            let header_signed = header_text(&headers, "authorization")
                .is_some_and(|value| value.starts_with("AWS4-HMAC-SHA256 "));
            let query_signed = query
                .keys()
                .any(|name| name.eq_ignore_ascii_case("X-Amz-Signature"));
            if !header_signed || query_signed {
                return StatusCode::FORBIDDEN.into_response();
            }
            let prefix = query.get("prefix").cloned().unwrap_or_default();
            let state = state.lock().expect("fake S3 state");
            let mut objects: Vec<&StoredObject> = state
                .objects
                .iter()
                .filter(|object| object.key.starts_with(&prefix))
                .collect();
            // S3 groups by key and returns the newest version of each key first.
            objects.sort_by(|a, b| a.key.cmp(&b.key).then(b.seq.cmp(&a.seq)));
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
            );
            xml.push_str(&format!(
                "<Name>notebooks</Name><Prefix>{}</Prefix><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>",
                xml_escape(&prefix)
            ));
            for object in objects {
                xml.push_str(&format!(
                    "<Version><Key>{}</Key><VersionId>{}</VersionId><IsLatest>{}</IsLatest><LastModified>{}</LastModified><ETag>\"etag\"</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Version>",
                    xml_escape(&object.key),
                    xml_escape(&object.version),
                    object.seq
                        == state
                            .objects
                            .iter()
                            .filter(|other| other.key == object.key)
                            .map(|other| other.seq)
                            .max()
                            .unwrap_or_default(),
                    object.last_modified,
                    object.body.len(),
                ));
            }
            xml.push_str("</ListVersionsResult>");
            return (StatusCode::OK, [("content-type", "application/xml")], xml).into_response();
        }

        if method == Method::GET {
            let prefix = query.get("prefix").cloned().unwrap_or_default();
            let state = state.lock().expect("fake S3 state");
            let mut latest: Map<String, &StoredObject> = Map::new();
            for object in &state.objects {
                if object.key.starts_with(&prefix) {
                    latest.insert(object.key.clone(), object);
                }
            }
            let mut objects: Vec<&&StoredObject> = latest.values().collect();
            objects.sort_by(|a, b| a.key.cmp(&b.key));
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
            );
            xml.push_str(&format!(
                "<Name>notebooks</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>",
                xml_escape(&prefix),
                objects.len()
            ));
            for object in objects {
                xml.push_str(&format!(
                    "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>\"etag\"</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                    xml_escape(&object.key),
                    object.last_modified,
                    object.body.len(),
                ));
            }
            xml.push_str("</ListBucketResult>");
            return (StatusCode::OK, [("content-type", "application/xml")], xml).into_response();
        }

        StatusCode::METHOD_NOT_ALLOWED.into_response()
    }

    pub(crate) fn notebook_store(base: &str) -> NotebookStore {
        NotebookStore::new(
            reqwest::Client::new(),
            NotebookConfig {
                endpoint: Url::parse(base).expect("fake S3 URL"),
                access_key: "test-access-key".into(),
                secret_key: Secret::new("test-secret-key"),
                bucket: "notebooks".into(),
            },
        )
        .expect("notebook store")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use jsonwebtoken::jwk::Jwk;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::auth::{
        AuthError, AuthState, BearerValidator, BoxFuture, ClaimConfig, Expect, OidcConfig, Provider,
    };
    use crate::cluster::WorkloadCache;
    use crate::demo::DemoState;

    use super::fake::{FakeS3, notebook_store};

    const TEST_KEY_DER: &[u8] = include_bytes!("testdata/test-rsa-pkcs1.der");
    const TEST_KEY_N: &str = include_str!("testdata/test-rsa-n.txt");
    const ISSUER: &str = "https://id.example.com/realms/stardelt";
    const CLIENT_ID: &str = "nova";

    // -----------------------------------------------------------------------
    // App and request helpers
    // -----------------------------------------------------------------------

    fn state(store: Option<NotebookStore>) -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: "http://127.0.0.1:1".into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            catalog_token: None,
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            catalog_ids: std::sync::RwLock::new(std::collections::HashMap::new()),
            catalog_access: std::sync::Mutex::new(std::collections::HashMap::new()),
            catalog_metrics: std::sync::Mutex::new(std::collections::HashMap::new()),
            catalog_layers: std::sync::Mutex::new(None),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(DemoState::default()),
            kube: None,
            workloads: WorkloadCache::default(),
            store: None,
            admin_console_url: None,
            db: None,
            break_glass_reads: Mutex::new(HashMap::new()),
            run_starts: Mutex::new(HashMap::new()),
            pipeline_creates: Mutex::new(HashMap::new()),
            catalog_access_changes: Mutex::new(HashMap::new()),
            airflow: None,
            table_locks: Mutex::new(HashMap::new()),
            people_touched: Mutex::new(HashMap::new()),
            notebooks: store,
        })
    }

    fn principal(sub: &str, groups: &[&str], admin: bool) -> Principal {
        Principal {
            identity: Identity {
                sub: sub.into(),
                display_name: format!("{sub}-name"),
                email: None,
                name: None,
                groups: groups.iter().map(|group| group.to_string()).collect(),
                roles: Vec::new(),
                groups_overage: false,
                is_admin: admin,
            },
            via: "session",
            access_token: Some(Secret::new("caller-token")),
        }
    }

    fn app(state: Arc<AppState>, who: Option<Principal>) -> Router {
        let router = Router::new().merge(router());
        let router = match who {
            Some(who) => router.layer(Extension(who)),
            None => router,
        };
        router.with_state(state)
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut request = HttpRequest::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        // A 401 comes from `require_auth`, which sits outside the notebook route's
        // no-store layer (the whole app adds it in `main`); every answer the notebook
        // handlers produce is uncacheable, including their refusals.
        if status != StatusCode::UNAUTHORIZED {
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
                "every notebook answer is uncacheable"
            );
        }
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn code_cell(source: Value) -> Value {
        json!({
            "cell_type": "code",
            "execution_count": 7,
            "metadata": {},
            "outputs": [
                {"output_type": "stream", "name": "stdout", "text": ["leaked-output\n"]},
                {"output_type": "display_data", "data": {"text/plain": ["leaked-data"]}, "metadata": {}}
            ],
            "source": source
        })
    }

    fn notebook(cells: Vec<Value>) -> Value {
        json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {
                "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
                "widgets": {"application/vnd.jupyter.widget-state+json": {"state": {"leaked-widget": {}}}}
            },
            "cells": cells
        })
    }

    fn body(notebook: &Value) -> String {
        serde_json::to_string(notebook).expect("notebook JSON")
    }

    // -----------------------------------------------------------------------
    // The tests named by stardelt-nova#225
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn publish_strips_outputs_and_returns_version() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let app = app(state, Some(principal("alice", &["analytics"], false)));
        let canary = "CANARY-4b1d-output-secret";
        let published = notebook(vec![
            code_cell(json!(["import os\n", "print(os.environ)\n"])),
            json!({"cell_type": "markdown", "metadata": {}, "source": ["# Notes\n"]}),
        ]);
        let mut body_value = published.clone();
        body_value["cells"][0]["outputs"][0]["text"] = json!([canary]);
        let body = serde_json::to_string(&body_value).unwrap();

        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/my%20notebook.ipynb",
            Some(&body),
            &[],
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["name"], "my notebook.ipynb");
        assert_eq!(answer["group"], "analytics");
        assert_eq!(answer["objectVersion"], "v1");

        let object = s3.object("analytics/my notebook.ipynb");
        assert_eq!(object.version, "v1");
        assert_eq!(object.content_type, NOTEBOOK_CONTENT_TYPE);
        assert_eq!(object.uploaded_by, "alice-name");
        let stored: Value = serde_json::from_slice(&object.body).expect("stored JSON");
        assert_eq!(
            stored["cells"][0]["source"],
            json!(["import os\n", "print(os.environ)\n"])
        );
        assert!(
            stored["cells"][0].get("outputs").is_none(),
            "outputs are gone"
        );
        assert!(
            stored["cells"][0].get("execution_count").is_none(),
            "execution_count is gone"
        );
        assert!(
            stored["metadata"].get("widgets").is_none(),
            "widget metadata is gone"
        );
        let stored_text = String::from_utf8(object.body.clone()).unwrap();
        assert!(
            !stored_text.contains(canary),
            "no output reaches the object store"
        );
        assert!(!stored_text.contains("leaked-data"));
        assert!(!stored_text.contains("leaked-widget"));

        assert_eq!(answer["size"].as_u64(), Some(object.body.len() as u64));
        assert_eq!(
            answer["sha256"].as_str(),
            Some(sha256_hex(&object.body).as_str())
        );
        assert_eq!(object.sha256, sha256_hex(&object.body));
        assert_eq!(answer["parameters"], json!([]));
    }

    #[tokio::test]
    async fn uploaded_by_metadata_survives_a_non_ascii_name() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let mut who = principal("joerg", &["analytics"], false);
        who.identity.display_name = "Jörg Müller".into();
        let app = app(state, Some(who));
        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/umlaut.ipynb",
            Some(&body(&notebook(vec![code_cell(json!(["x = 1\n"]))]))),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");

        // S3 user metadata is US-ASCII; the header must not carry the raw umlaut.
        let object = s3.object("analytics/umlaut.ipynb");
        assert!(object.uploaded_by.is_ascii(), "{}", object.uploaded_by);
        assert_ne!(object.uploaded_by, "Jörg Müller");

        // The listing decodes it back to the name a person recognizes.
        let (status, rows) = call(&app, "GET", "/notebooks", None, &[]).await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        assert_eq!(rows[0]["uploadedBy"], "Jörg Müller");
    }

    #[tokio::test]
    async fn publish_refuses_other_groups_and_bad_names_and_non_notebooks() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let app = app(state, Some(principal("alice", &["analytics"], false)));
        let good = body(&notebook(vec![code_cell(json!(["x = 1\n"]))]));

        let (status, answer) =
            call(&app, "PUT", "/notebooks/other/x.ipynb", Some(&good), &[]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["error"], "you are not a member of the group other");

        for name in ["not-a-notebook", ".ipynb", "-leading.ipynb", "trailing.txt"] {
            let uri = format!("/notebooks/analytics/{name}");
            let (status, answer) = call(&app, "PUT", &uri, Some(&good), &[]).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {answer}");
            assert!(
                answer["error"].as_str().unwrap().contains("notebook name"),
                "{name}: {answer}"
            );
        }
        let too_long = format!("{}.ipynb", "a".repeat(129));
        let (status, _) = call(
            &app,
            "PUT",
            &format!("/notebooks/analytics/{too_long}"),
            Some(&good),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        for body in [
            "{not json",
            r#"{"nbformat":3,"cells":[]}"#,
            r#"{"nbformat":4}"#,
            r#"{"nbformat":4,"cells":{}}"#,
            r#"[]"#,
        ] {
            let (status, answer) =
                call(&app, "PUT", "/notebooks/analytics/x.ipynb", Some(body), &[]).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {answer}");
        }
        assert!(s3.is_empty(), "a refused publish stores nothing");
    }

    #[tokio::test]
    async fn size_cap() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let app = app(state, Some(principal("alice", &["analytics"], false)));
        // A valid notebook whose source alone is one byte over the cap.
        let big = notebook(vec![code_cell(json!("x".repeat(MAX_NOTEBOOK_BYTES)))]);
        let body = body(&big);
        assert!(body.len() > MAX_NOTEBOOK_BYTES);

        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/big.ipynb",
            Some(&body),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{answer}");
        assert_eq!(answer["error"], "a notebook must be 5 MiB or smaller");
        assert!(s3.is_empty());
    }

    #[tokio::test]
    async fn parameters_parsed_from_the_tagged_cell() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let app = app(state, Some(principal("alice", &["analytics"], false)));

        let mut lines = vec![
            "a = 1".to_string(),
            "b = \"hello\"".to_string(),
            "c = True".to_string(),
            "# d = 4".to_string(),
            "e = 'x'".to_string(),
            "a = 2".to_string(),
            "not an assignment".to_string(),
            "f = [1, 2]".to_string(),
            "g = h == 1".to_string(),
        ];
        for i in 0..40 {
            lines.push(format!("p{i} = {i}"));
        }
        let source = lines.join("\n") + "\n";
        let published = notebook(vec![
            // A second assignment block must not shadow the first name.
            json!({"cell_type": "markdown", "metadata": {"tags": ["not-parameters"]}, "source": ["x = 99\n"]}),
            code_cell(json!(source)),
            code_cell(json!(["z = 3\n"])),
        ]);
        let mut tagged = published.clone();
        tagged["cells"][1]["metadata"]["tags"] = json!(["parameters"]);

        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/params.ipynb",
            Some(&body(&tagged)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let parameters = answer["parameters"].as_array().expect("parameters");
        assert_eq!(parameters.len(), MAX_PARAMETERS, "at most 32");
        assert_eq!(parameters[0], json!({"name": "a", "default": "1"}));
        assert_eq!(parameters[1], json!({"name": "b", "default": "hello"}));
        assert_eq!(parameters[2], json!({"name": "c", "default": "True"}));
        assert_eq!(parameters[3], json!({"name": "e", "default": "x"}));
        assert_eq!(parameters[4], json!({"name": "f", "default": "[1, 2]"}));
        assert_eq!(parameters[5], json!({"name": "g", "default": "h == 1"}));
        assert_eq!(
            parameters[6],
            json!({"name": "p0", "default": "0"}),
            "first 32 names, in order"
        );
        assert!(
            !parameters.iter().any(|parameter| parameter["name"] == "x"),
            "an untagged cell's assignment is not a parameter"
        );
        assert!(
            !parameters.iter().any(|parameter| parameter["name"] == "d"),
            "a comment is not a parameter"
        );

        // No tagged cell: no parameters.
        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/untagged.ipynb",
            Some(&body(&published)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["parameters"], json!([]));
    }

    /// stardelt-nova#237: the Schedule dialog reads the parameters of the exact version it
    /// will pin, and only for its own groups.
    #[tokio::test]
    async fn parameters_route_parses_the_tagged_cell_and_filters_groups() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let alice = app(
            Arc::clone(&state),
            Some(principal("alice", &["analytics"], false)),
        );

        fn tagged(source: &str) -> Value {
            let mut cell = code_cell(json!(source));
            cell["metadata"]["tags"] = json!(["parameters"]);
            cell
        }

        let first_notebook = notebook(vec![
            json!({"cell_type": "markdown", "metadata": {}, "source": ["# Report\n"]}),
            tagged("region = \"eu\"\ndays = 7\n# ignored = 1\n"),
        ]);
        let (status, published) = call(
            &alice,
            "PUT",
            "/notebooks/analytics/report.ipynb",
            Some(&body(&first_notebook)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{published}");
        let first = published["objectVersion"]
            .as_str()
            .expect("the first version")
            .to_string();

        // A later publish of the same name: a different version, different parameters.
        let second_notebook = notebook(vec![tagged("region = \"us\"\n")]);
        let (status, published) = call(
            &alice,
            "PUT",
            "/notebooks/analytics/report.ipynb",
            Some(&body(&second_notebook)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{published}");
        let second = published["objectVersion"]
            .as_str()
            .expect("the second version")
            .to_string();
        assert_ne!(first, second);

        // No `version`: the latest, and the answer names it.
        let (status, answer) = call(
            &alice,
            "GET",
            "/notebooks/analytics/report.ipynb/parameters",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["objectVersion"], second.as_str());
        assert_eq!(
            answer["parameters"],
            json!([{"name": "region", "default": "us"}])
        );

        // A pinned version: exactly its parameters, never the latest one's.
        let (status, answer) = call(
            &alice,
            "GET",
            &format!("/notebooks/analytics/report.ipynb/parameters?version={first}"),
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["objectVersion"], first.as_str());
        assert_eq!(
            answer["parameters"],
            json!([
                {"name": "region", "default": "eu"},
                {"name": "days", "default": "7"},
            ]),
            "the tagged cell's assignments, in order, the comment skipped"
        );

        // An unknown version is a 404, never an empty list a schedule could pin.
        for version in ["nope", "v999"] {
            let uri = format!("/notebooks/analytics/report.ipynb/parameters?version={version}");
            let (status, answer) = call(&alice, "GET", &uri, None, &[]).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{version}: {answer}");
            assert_eq!(answer["error"], "no such notebook version");
        }

        // A notebook nobody published is the same 404.
        let (status, _) = call(
            &alice,
            "GET",
            "/notebooks/analytics/missing.ipynb/parameters",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Another group's notebook stays hidden: 403 here, and a schedule of it is refused
        // with the same sentence, so its bytes and its parameters never leak.
        let (status, answer) = call(
            &alice,
            "GET",
            "/notebooks/other/secret.ipynb/parameters",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["error"], "you are not a member of the group other");

        // An administrator gets no wider view: being one does not make them a member.
        let admin = app(
            state,
            Some(principal("root", &["analytics", "stardelt-admins"], true)),
        );
        let (status, answer) = call(
            &admin,
            "GET",
            "/notebooks/other/secret.ipynb/parameters",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["error"], "you are not a member of the group other");
    }

    #[tokio::test]
    async fn list_and_versions_filtered_to_own_groups() {
        let s3 = FakeS3::start().await;
        let first = s3.put_raw("analytics/a.ipynb", b"{\"v\":1}", "alice-name");
        let second = s3.put_raw("analytics/a.ipynb", b"{\"v\":2}", "alice-name");
        s3.put_raw("analytics/b.ipynb", b"{\"b\":1}", "bob-name");
        s3.put_raw("other/secret.ipynb", b"{\"secret\":1}", "mallory-name");
        for _ in 0..55 {
            s3.put_raw("analytics/many.ipynb", b"{}", "alice-name");
        }
        let newest_many = s3.object("analytics/many.ipynb").version;

        let state = state(Some(notebook_store(&s3.base)));
        let alice = app(
            Arc::clone(&state),
            Some(principal("alice", &["analytics"], false)),
        );
        let admin = app(
            Arc::clone(&state),
            Some(principal("root", &["analytics", "stardelt-admins"], true)),
        );

        for (who, app) in [("alice", &alice), ("admin", &admin)] {
            let (status, rows) = call(app, "GET", "/notebooks", None, &[]).await;
            assert_eq!(status, StatusCode::OK, "{who}: {rows}");
            let rows = rows.as_array().expect("array");
            assert_eq!(
                rows.len(),
                3,
                "{who}: a, b and many; never other's notebook"
            );
            assert!(rows.iter().all(|row| row["group"] == "analytics"));
            assert!(
                !serde_json::to_string(rows)
                    .unwrap()
                    .contains("secret.ipynb"),
                "{who}: another group's notebook is absent"
            );
            let a = rows
                .iter()
                .find(|row| row["name"] == "a.ipynb")
                .expect("a.ipynb");
            assert_eq!(a["objectVersion"], second.as_str());
            assert_eq!(a["uploadedBy"], "alice-name");
            assert_eq!(a["size"], 7);
            assert!(a["uploadedAt"].as_str().unwrap().ends_with('Z'));
        }

        // Newest first across the listing: `many` has the newest timestamp.
        let (_, rows) = call(&alice, "GET", "/notebooks", None, &[]).await;
        assert_eq!(rows[0]["name"], "many.ipynb");
        assert_eq!(rows[0]["objectVersion"], newest_many.as_str());

        let (status, versions) = call(
            &alice,
            "GET",
            "/notebooks/analytics/a.ipynb/versions",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{versions}");
        let versions = versions.as_array().expect("array");
        assert_eq!(versions.len(), 2);
        assert_eq!(
            versions[0]["objectVersion"],
            second.as_str(),
            "newest first"
        );
        assert_eq!(versions[1]["objectVersion"], first.as_str());
        assert_eq!(versions[0]["sha256"], sha256_hex(b"{\"v\":2}"));
        assert_eq!(versions[0]["uploadedBy"], "alice-name");

        let (_, many) = call(
            &alice,
            "GET",
            "/notebooks/analytics/many.ipynb/versions",
            None,
            &[],
        )
        .await;
        assert_eq!(many.as_array().expect("array").len(), MAX_VERSIONS);

        let (status, answer) = call(
            &alice,
            "GET",
            "/notebooks/other/secret.ipynb/versions",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["error"], "you are not a member of the group other");

        // A notebook nobody published is an empty list, not a 404.
        let (status, answer) = call(
            &alice,
            "GET",
            "/notebooks/analytics/missing.ipynb/versions",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer, json!([]));
    }

    #[tokio::test]
    async fn s3_unconfigured_503() {
        let state = state(None);
        let app = app(state, Some(principal("alice", &["analytics"], false)));
        let expected = json!({"error": "notebook storage is not configured"});

        let (status, answer) = call(&app, "GET", "/notebooks", None, &[]).await;
        assert_eq!(
            (status, answer),
            (StatusCode::SERVICE_UNAVAILABLE, expected.clone())
        );
        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/x.ipynb",
            Some(&body(&notebook(vec![code_cell(json!(["x = 1\n"]))]))),
            &[],
        )
        .await;
        assert_eq!(
            (status, answer),
            (StatusCode::SERVICE_UNAVAILABLE, expected.clone())
        );
        let (status, answer) = call(
            &app,
            "GET",
            "/notebooks/analytics/x.ipynb/versions",
            None,
            &[],
        )
        .await;
        assert_eq!(
            (status, answer),
            (StatusCode::SERVICE_UNAVAILABLE, expected.clone())
        );
        let (status, answer) = call(
            &app,
            "GET",
            "/notebooks/analytics/x.ipynb/parameters?version=v1",
            None,
            &[],
        )
        .await;
        assert_eq!(
            (status, answer),
            (StatusCode::SERVICE_UNAVAILABLE, expected)
        );
    }

    #[tokio::test]
    async fn s3_down_502_not_500() {
        let state = state(Some(notebook_store("http://127.0.0.1:1")));
        let app = app(state, Some(principal("alice", &["analytics"], false)));

        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/x.ipynb",
            Some(&body(&notebook(vec![code_cell(json!(["x = 1\n"]))]))),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{answer}");
        assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(answer["error"], "the notebook store did not answer");

        let (status, answer) = call(&app, "GET", "/notebooks", None, &[]).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{answer}");
        assert_eq!(answer["error"], "the notebook store did not answer");
    }

    #[tokio::test]
    async fn rate_limited_429() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        {
            let store = state.notebooks.as_ref().expect("store");
            for _ in 0..PUBLISHES_PER_HOUR {
                assert!(store.take_publish_slot_at("alice", Instant::now()));
            }
        }
        let alice = app(
            Arc::clone(&state),
            Some(principal("alice", &["analytics"], false)),
        );
        let good = body(&notebook(vec![code_cell(json!(["x = 1\n"]))]));

        let (status, answer) = call(
            &alice,
            "PUT",
            "/notebooks/analytics/x.ipynb",
            Some(&good),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{answer}");
        assert_eq!(
            answer["error"],
            "too many notebooks published this hour; wait before publishing another"
        );
        assert!(s3.is_empty(), "the refused publish never reached S3");

        // The limit is per person: somebody else still publishes.
        let bob = app(state, Some(principal("bob", &["analytics"], false)));
        let (status, answer) = call(
            &bob,
            "PUT",
            "/notebooks/analytics/bob.ipynb",
            Some(&good),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }

    #[tokio::test]
    async fn audit_has_no_content() {
        let s3 = FakeS3::start().await;
        let state = state(Some(notebook_store(&s3.base)));
        let app = app(state, Some(principal("auditor", &["analytics"], false)));
        let canary = "CANARY-audit-8f21";
        let mut published = notebook(vec![code_cell(json!(["print(os.environ)\n"]))]);
        published["cells"][0]["outputs"][0]["text"] = json!([canary]);
        published["cells"][0]["source"] = json!(["print(os.environ)\n"]);
        let (status, answer) = call(
            &app,
            "PUT",
            "/notebooks/analytics/audited.ipynb",
            Some(&body(&published)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let version = answer["objectVersion"].as_str().expect("version");

        let records = crate::audit::notebook_test_sink::take_for("auditor");
        assert_eq!(records.len(), 1, "one publish, one audit line");
        let record = &records[0];
        assert_eq!(record.group, "analytics");
        assert_eq!(record.name, "audited.ipynb");
        assert_eq!(record.version, version);
        assert_eq!(record.size, answer["size"].as_u64().unwrap());
        let rendered = format!("{record:?}");
        assert!(
            !rendered.contains(canary),
            "no output content in the audit line"
        );
        assert!(
            !rendered.contains("os.environ"),
            "no source in the audit line"
        );
        assert!(
            !rendered.contains(answer["sha256"].as_str().unwrap()),
            "no content hash in the audit line"
        );
        assert!(
            !String::from_utf8(s3.object("analytics/audited.ipynb").body)
                .unwrap()
                .contains(canary)
        );
    }

    #[tokio::test]
    async fn bearer_token_with_audience_nova_accepted() {
        let s3 = FakeS3::start().await;
        s3.put_raw("analytics/from-notebook.ipynb", b"{}", "alice-name");
        let state = state(Some(notebook_store(&s3.base)));

        let config = OidcConfig {
            issuer: ISSUER.into(),
            client_id: CLIENT_ID.into(),
            client_secret: Secret::new("client-secret"),
            scopes: "openid profile email".into(),
            audiences: vec![CLIENT_ID.into()],
            public_url: "http://localhost:8080".into(),
            claims: ClaimConfig::default(),
            session_secret: Secret::new("x".repeat(48)),
        };
        let validator = TestBearer {
            jwk: test_jwk(),
            issuer: ISSUER.into(),
            audiences: vec![CLIENT_ID.into()],
        };
        let auth = AuthState::with_validator(
            config,
            Arc::new(Provider::new(reqwest::Client::new(), ISSUER.into())),
            Arc::new(validator),
        );
        let context = crate::auth::AuthContext {
            auth,
            app: state.clone(),
        };
        let app = Router::new()
            .merge(router())
            .route_layer(axum::middleware::from_fn_with_state(
                context,
                crate::auth::require_auth,
            ))
            .with_state(state);

        // The hub's access token: audience `jupyterhub` *and* `nova` through the
        // `aud-nova` mapper (stardelt-platform#290).
        let token = sign_token(json!(["jupyterhub", "nova"]));
        let (status, rows) = call(
            &app,
            "GET",
            "/notebooks",
            None,
            &[("authorization", &format!("Bearer {token}"))],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        assert_eq!(rows.as_array().expect("array").len(), 1);

        // A token without `nova` is refused by the same middleware.
        let foreign = sign_token(json!(["jupyterhub"]));
        let (status, answer) = call(
            &app,
            "GET",
            "/notebooks",
            None,
            &[("authorization", &format!("Bearer {foreign}"))],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{answer}");
        assert_eq!(answer["error"], "unauthenticated");
    }

    // -----------------------------------------------------------------------
    // Bearer test support: a real RS256 token verified with the test key
    // -----------------------------------------------------------------------

    fn test_jwk() -> Jwk {
        serde_json::from_value(json!({
            "kty": "RSA", "use": "sig", "kid": "k1",
            "n": TEST_KEY_N.trim(), "e": "AQAB"
        }))
        .expect("test JWK")
    }

    fn sign_token(audience: Value) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let claims = json!({
            "iss": ISSUER,
            "aud": audience,
            "sub": "hub-user",
            "exp": now + 300,
            "groups": ["analytics"],
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("k1".into());
        encode(&header, &claims, &EncodingKey::from_rsa_der(TEST_KEY_DER)).expect("sign token")
    }

    /// Verifies the token exactly as `auth::OidcBearer` does, with the configured
    /// audiences, and maps the claims to an `Identity`.
    struct TestBearer {
        jwk: Jwk,
        issuer: String,
        audiences: Vec<String>,
    }

    impl BearerValidator for TestBearer {
        fn validate<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Identity, AuthError>> {
            Box::pin(async move {
                let expect = Expect {
                    issuer: &self.issuer,
                    audiences: &self.audiences,
                    client_id: CLIENT_ID,
                    id_token: false,
                    nonce: None,
                };
                let claims =
                    crate::auth::verify_jwt(token, std::slice::from_ref(&self.jwk), &expect)?;
                let groups = claims
                    .get("groups")
                    .and_then(Value::as_array)
                    .map(|groups| {
                        groups
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Identity {
                    sub: claims["sub"].as_str().unwrap_or_default().into(),
                    display_name: "hub-user".into(),
                    email: None,
                    name: None,
                    groups,
                    roles: Vec::new(),
                    groups_overage: false,
                    is_admin: false,
                })
            })
        }
    }

    // -----------------------------------------------------------------------
    // Pure helpers
    // -----------------------------------------------------------------------

    #[test]
    fn name_rule_is_the_tickets_regex() {
        for good in [
            "a.ipynb",
            "My Notebook.ipynb",
            "report-2026_10.20.ipynb",
            &format!("{}.ipynb", "a".repeat(128)),
        ] {
            assert!(valid_name(good), "{good} should pass");
        }
        for bad in [
            "a.ipynb ",
            "a.ipnyb",
            ".ipynb",
            " a.ipynb",
            "a/b.ipynb",
            "a\\b.ipynb",
            "a\n.ipynb",
            &format!("{}.ipynb", "a".repeat(129)),
            "é.ipynb",
        ] {
            assert!(!valid_name(bad), "{bad} should fail");
        }
    }

    #[test]
    fn group_rule_keeps_the_key_inside_its_prefix() {
        assert!(valid_group("analytics"));
        assert!(valid_group("team.a_b-1"));
        assert!(!valid_group(""));
        assert!(!valid_group("/stardelt-admins"));
        assert!(!valid_group("team/a"));
    }

    #[test]
    fn sha256_matches_a_known_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // -----------------------------------------------------------------------
    // Header-signed ListObjectVersions (stardelt-nova#245)
    // -----------------------------------------------------------------------

    /// The expected header values were produced with botocore 1.43.116 in a throwaway
    /// venv, not by this code:
    ///
    /// ```text
    /// from botocore.auth import S3SigV4Auth
    /// from botocore.awsrequest import AWSRequest
    /// from botocore.credentials import Credentials
    /// auth = S3SigV4Auth(Credentials("AKIDEXAMPLE", "secretexample"), "s3", "us-east-1")
    /// request = AWSRequest(method="GET", url="http://seaweedfs-s3.stardelt.svc.cluster.local:8333/notebooks?versions=&prefix=analytics%2Fqa.ipynb&max-keys=1000")
    /// request.headers["x-amz-content-sha256"] = EMPTY_SHA256
    /// botocore.auth.get_current_datetime = lambda: 2026-10-21T01:24:28Z  # pinned
    /// auth.add_auth(request)
    /// ```
    ///
    /// which printed `x-amz-date: 20261021T012428Z`, the content hash below, and the
    /// `Authorization` line asserted here.
    #[test]
    fn sign_headers_matches_botocore() {
        let url = Url::parse(
            "http://seaweedfs-s3.stardelt.svc.cluster.local:8333/notebooks?versions=&prefix=analytics%2Fqa.ipynb&max-keys=1000",
        )
        .expect("S3 URL");
        let now = "2026-10-21T01:24:28Z"
            .parse::<jiff::Timestamp>()
            .expect("fixed time");
        let headers = sign_headers(
            "GET",
            &url,
            "us-east-1",
            "AKIDEXAMPLE",
            "secretexample",
            now,
        );
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("x-amz-date"), Some("20261021T012428Z"));
        assert_eq!(header("x-amz-content-sha256"), Some(EMPTY_SHA256));
        assert_eq!(
            header("authorization"),
            Some(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=e86c765e63de8b6e065949fcc19d121cbb730b043089840835b3098e56dd6083"
            )
        );
    }

    #[test]
    fn canonical_query_sorts_and_keeps_empty_value() {
        let url = Url::parse(
            "http://s3.local/notebooks?versions=&prefix=analytics%2Fqa.ipynb&max-keys=1000",
        )
        .expect("S3 URL");
        assert_eq!(
            canonical_query(&url),
            "max-keys=1000&prefix=analytics%2Fqa.ipynb&versions="
        );
        // A parameter that was written without `=` still signs with an empty value.
        let url = Url::parse("http://s3.local/notebooks?versions&prefix=a%20b").expect("S3 URL");
        assert_eq!(canonical_query(&url), "prefix=a%20b&versions=");
    }

    /// The fake refuses exactly what SeaweedFS refuses, so the test fails if the store
    /// ever goes back to a presigned `?versions` URL.
    #[tokio::test]
    async fn list_versions_requires_header_auth_in_the_fake_gateway() {
        let s3 = FakeS3::start().await;
        s3.put_raw("analytics/a.ipynb", b"{\"v\":1}", "alice-name");
        let http = reqwest::Client::new();

        // The shape rusty-s3's `sign` produced before #245: a query signature.
        let presigned = format!(
            "{}/notebooks?versions=&prefix=analytics%2F&max-keys=1000&X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=deadbeef",
            s3.base
        );
        let response = http.get(presigned).send().await.expect("presigned request");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // The shape Nova sends now: the same parameters, the signature in the headers.
        let response = http
            .get(format!(
                "{}/notebooks?versions=&prefix=analytics%2F&max-keys=1000",
                s3.base
            ))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=test-access-key/20261021/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=deadbeef",
            )
            .header("x-amz-date", "20261021T012428Z")
            .header("x-amz-content-sha256", EMPTY_SHA256)
            .send()
            .await
            .expect("header-signed request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.text().await.expect("listing body");
        assert!(body.contains("<Key>analytics/a.ipynb</Key>"), "{body}");
    }

    #[test]
    fn last_modified_rfc1123_becomes_rfc3339() {
        // What SeaweedFS writes into `Last-Modified` and its version listing.
        assert_eq!(
            rfc3339_utc("Wed, 21 Oct 2026 01:24:28 GMT"),
            "2026-10-21T01:24:28.000Z"
        );
        // What the fake and some gateways list, and what the row already answered.
        assert_eq!(
            rfc3339_utc("2026-10-20T12:00:01.000Z"),
            "2026-10-20T12:00:01.000Z"
        );
        // A shape this module does not know passes through: one odd row never fails the
        // whole listing.
        assert_eq!(rfc3339_utc("yesterday"), "yesterday");
    }

    /// A real S3 gateway, when one is handed over (the deployed stack uses SeaweedFS
    /// 4.25). CI has none, so the test prints "skipped" and passes — the same contract as
    /// the secret-store live test. `NOVA_TEST_S3_BUCKET` must exist and be versioned.
    #[tokio::test]
    #[ignore = "needs NOVA_TEST_S3_URL and a versioned NOVA_TEST_S3_BUCKET"]
    async fn live_gateway_round_trip() {
        let (Ok(url), Ok(bucket)) = (
            std::env::var("NOVA_TEST_S3_URL"),
            std::env::var("NOVA_TEST_S3_BUCKET"),
        ) else {
            eprintln!(
                "skipped: set NOVA_TEST_S3_URL and NOVA_TEST_S3_BUCKET (versioning on) to run this test"
            );
            return;
        };
        let store = NotebookStore::new(
            reqwest::Client::new(),
            NotebookConfig {
                endpoint: Url::parse(url.trim_end_matches('/')).expect("NOVA_TEST_S3_URL"),
                access_key: std::env::var("NOVA_TEST_S3_ACCESS_KEY")
                    .unwrap_or_else(|_| "any".into()),
                secret_key: Secret::new(
                    std::env::var("NOVA_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "any".into()),
                ),
                bucket,
            },
        )
        .expect("notebook store");
        let group = "stardelt-nova-test";
        let name = format!(
            "live-{}-{}.ipynb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis()
        );
        let state = state(Some(store));
        let store = state.notebooks.as_ref().expect("store");
        let app = app(Arc::clone(&state), Some(principal("live", &[group], false)));
        let uri = format!("/notebooks/{group}/{name}");
        let versions_uri = format!("{uri}/versions");
        let first = body(&notebook(vec![code_cell(json!(["x = 1\n"]))]));
        let second = body(&notebook(vec![code_cell(json!(["x = 2\n"]))]));

        let (status, answer) = call(&app, "PUT", &uri, Some(&first), &[]).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let v1 = answer["objectVersion"].as_str().expect("v1").to_string();
        let first_sha = answer["sha256"].as_str().expect("sha").to_string();

        // The exact version just written is byte-stable and carries the metadata.
        let head = store
            .head(&object_key(group, &name), None)
            .await
            .expect("head");
        assert_eq!(head.object_version, v1);
        assert_eq!(head.sha256, first_sha);
        assert_eq!(head.size, answer["size"].as_u64().unwrap());
        assert_eq!(head.uploaded_by, "live-name");

        let (status, answer) = call(&app, "PUT", &uri, Some(&second), &[]).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let v2 = answer["objectVersion"].as_str().expect("v2").to_string();
        assert_ne!(v1, v2, "each publish is a new immutable version");

        let old = store
            .head(&object_key(group, &name), Some(&v1))
            .await
            .expect("head v1");
        assert_eq!(old.sha256, first_sha, "the pinned version is unchanged");

        let (status, rows) = call(&app, "GET", "/notebooks", None, &[]).await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        let row = rows
            .as_array()
            .expect("array")
            .iter()
            .find(|row| row["name"] == name.as_str())
            .expect("the published notebook in the list");
        assert_eq!(
            row["objectVersion"],
            v2.as_str(),
            "the list carries the latest"
        );
        // SeaweedFS writes RFC 1123 (`Wed, 21 Oct 2026 01:24:28 GMT`); every row answers
        // RFC 3339 in UTC (#245).
        let uploaded_at = row["uploadedAt"].as_str().expect("uploadedAt");
        assert!(uploaded_at.ends_with('Z'), "RFC 3339 UTC: {uploaded_at}");
        eprintln!("live uploadedAt: {uploaded_at}");

        let (status, versions) = call(&app, "GET", &versions_uri, None, &[]).await;
        assert_eq!(status, StatusCode::OK, "{versions}");
        let versions = versions.as_array().expect("array");
        assert!(versions.len() >= 2, "both versions are listed");
        assert_eq!(versions[0]["objectVersion"], v2.as_str(), "newest first");
        assert_eq!(versions[1]["objectVersion"], v1.as_str());
        for version in versions {
            let uploaded_at = version["uploadedAt"].as_str().expect("uploadedAt");
            assert!(uploaded_at.ends_with('Z'), "RFC 3339 UTC: {uploaded_at}");
        }
        eprintln!("live notebook versions: {v1} {v2}");
    }

    #[test]
    fn notebook_config_is_all_four_or_none() {
        let config = |pairs: &[(&str, &str)]| {
            NotebookConfig::from_env_with(|key| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_string())
            })
        };
        assert!(config(&[]).unwrap().is_none(), "unset is off");
        // A chart that renders empty strings leaves the feature off.
        assert!(
            config(&[
                ("NOVA_S3_URL", ""),
                ("NOVA_S3_ACCESS_KEY", ""),
                ("NOVA_S3_SECRET_KEY", ""),
                ("NOVA_S3_NOTEBOOK_BUCKET", ""),
            ])
            .unwrap()
            .is_none()
        );

        let full = config(&[
            ("NOVA_S3_URL", "http://seaweedfs-s3:8333/"),
            ("NOVA_S3_ACCESS_KEY", "access"),
            ("NOVA_S3_SECRET_KEY", "secret"),
            ("NOVA_S3_NOTEBOOK_BUCKET", "notebooks"),
        ])
        .unwrap()
        .expect("a full configuration");
        assert_eq!(full.endpoint.as_str(), "http://seaweedfs-s3:8333/");
        assert_eq!(full.bucket, "notebooks");
        assert_eq!(full.secret_key.expose(), "secret");

        // A partial set refuses to start instead of answering 503 for a wired bucket.
        assert!(config(&[("NOVA_S3_URL", "http://seaweedfs-s3:8333")]).is_err());
        // The endpoint is an origin, not a path into a bucket.
        assert!(
            config(&[
                ("NOVA_S3_URL", "http://seaweedfs-s3:8333/s3"),
                ("NOVA_S3_ACCESS_KEY", "access"),
                ("NOVA_S3_SECRET_KEY", "secret"),
                ("NOVA_S3_NOTEBOOK_BUCKET", "notebooks"),
            ])
            .is_err()
        );
        assert!(
            config(&[
                ("NOVA_S3_URL", "not a url"),
                ("NOVA_S3_ACCESS_KEY", "access"),
                ("NOVA_S3_SECRET_KEY", "secret"),
                ("NOVA_S3_NOTEBOOK_BUCKET", "notebooks"),
            ])
            .is_err()
        );
    }
}
