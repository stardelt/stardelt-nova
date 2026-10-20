//! Table documentation: description, tags, owner, domain and column descriptions
//! (stardelt-nova#189, design ticket #104; what the catalog spike stardelt-platform#242
//! measured).
//!
//! Where the fields live, and why each goes the way it does:
//!
//! - **Table description** is the Iceberg property `comment`. `COMMENT ON TABLE` and the
//!   REST `set-properties` update write the same property, but Nova uses the REST path so
//!   one request can carry every property change at once.
//! - **Column description** is the field's `doc` in a **new schema**: only Trino's
//!   `COMMENT ON COLUMN` writes it, so this route runs that statement through
//!   [`crate::trino::execute_from`] as the caller (`source = nova-catalog`). Iceberg keeps
//!   every historical schema, so the read picks the schema whose `schema-id` equals
//!   `current-schema-id` — never `schemas[0]`.
//! - **Tags, owner and domain** are the custom properties `stardelt.tags` (sorted, comma
//!   separated), `stardelt.owner` and `stardelt.domain`. Trino cannot write custom
//!   properties, so they go through one Lakekeeper commit
//!   (`POST …/tables/<table>` with `set-properties` / `remove-properties`, empty
//!   `requirements`).
//!
//! ## The interim rule, and what it is not
//!
//! Lakekeeper runs with `authz.backend: allowall` and cannot refuse an edit. Until the
//! catalog enforces permissions, **Nova** applies [`may_edit`]: an administrator may edit;
//! a member of the current owner group may edit; a table without an owner may be edited by
//! anybody, but the first editor must set the owner to one of their own groups. Everybody
//! else is refused. This is enforced by Nova only: anyone who reaches Lakekeeper or Trino
//! directly can still change these properties. The read answer says which side of the rule
//! the caller is on (`canEdit` / `editRule`), and the OpenAPI description repeats the
//! caveat.
//!
//! ## One editor at a time
//!
//! Concurrent commits to one table deadlock in Lakekeeper 0.12.2 (40 simultaneous
//! commits: three succeeded, 24 answered 500 `Error starting transaction`, 13 answered 503
//! `deadlock detected`, none answered 409). Nova therefore serialises edits **per table**
//! with [`AppState::table_locks`] — every write holds the table's lock for the whole
//! operation, including the load that decides the rule — and retries a commit answered 500
//! or 503 up to three attempts in total (waits 200, 400 ms), then answers 502 `the catalog
//! is busy, try again`. The lock is in-process: it covers one Nova replica. Running more
//! than one replica needs a database lock, which is a later ticket — until then the
//! manifests run one.
//!
//! ## Limits and answers
//!
//! A description is trimmed and capped at 4,000 characters and must not contain a NUL; at
//! most 20 tags are allowed, each `^[a-z0-9][a-z0-9_-]{0,31}$` and unique; an owner is a
//! group name ([`crate::connections::group_ok`]); a domain is
//! `^[a-z0-9][a-z0-9_-]{0,39}$`. Validation happens before any request is sent. A key that
//! is absent from a write body changes nothing; `null` (or an empty tag list, or a blank
//! description) removes the property. A write with nothing to change sends no commit and
//! answers the current documentation. Demo mode answers 503.
//!
//! The route logic sits in plain `async fn`s that take their inputs, so the tests below run
//! them against a fake Lakekeeper and a recorded SQL closure without a cluster.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use crate::api_json::ApiJson;
use crate::auth::{Identity, Principal};
use crate::data::quote_ident;
use crate::{AppState, audit, catalog};

/// Most characters a description may hold, after trimming.
pub const MAX_DESCRIPTION_CHARS: usize = 4000;
/// Most tags one table may carry.
pub const MAX_TAGS: usize = 20;

/// One column of the current schema, as the documentation read reports it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ColumnDocumentation {
    /// Field name, exactly as the current schema spells it.
    pub name: String,
    /// The field's type as text; a complex type is its JSON text.
    pub r#type: String,
    /// The field's Iceberg `doc`, or `null`.
    pub description: Option<String>,
}

/// The documentation of one table, plus what the caller may do with it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TableDocumentation {
    /// The Iceberg `comment` property, or `null`.
    pub description: Option<String>,
    /// The `stardelt.tags` property, split on commas, trimmed and sorted.
    pub tags: Vec<String>,
    /// The `stardelt.owner` property: a group name, or `null`.
    pub owner: Option<String>,
    /// The `stardelt.domain` property, or `null`.
    pub domain: Option<String>,
    /// The fields of the **current** schema, in schema order.
    pub columns: Vec<ColumnDocumentation>,
    /// Whether the interim rule lets this caller edit (see the module docs).
    #[serde(rename = "canEdit")]
    #[schema(rename = "canEdit")]
    pub can_edit: bool,
    /// One sentence for a person: why `canEdit` has the value it has.
    #[serde(rename = "editRule")]
    #[schema(rename = "editRule")]
    pub edit_rule: String,
}

/// `PUT /api/catalog/tables/{ns}/{tbl}/documentation` body.
///
/// Every key is optional. Absent changes nothing; `null` removes the property; an empty tag
/// list and a blank description also remove. A field whose value equals what the table
/// already stores is not a change.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct DocumentationUpdate {
    /// New table description (`null` removes it).
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    /// New tag list (`null` or `[]` removes them).
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<Vec<String>>)]
    pub tags: Option<Option<Vec<String>>>,
    /// New owner group (`null` removes it).
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>)]
    pub owner: Option<Option<String>>,
    /// New domain (`null` removes it).
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>)]
    pub domain: Option<Option<String>>,
}

/// `PUT /api/catalog/tables/{ns}/{tbl}/columns/{column}/description` body.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ColumnDescriptionUpdate {
    /// New column description (`null` removes it).
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
}

/// Distinguish "key absent" (`None`, through `#[serde(default)]`) from "key present with
/// `null`" (`Some(None)`) — serde's plain `Option<Option<T>>` folds both into `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A refusal with one sentence and the status it answers.
#[derive(Debug)]
struct DocError {
    status: StatusCode,
    message: String,
}

impl DocError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    #[cfg(test)]
    fn status(&self) -> StatusCode {
        self.status
    }

    #[cfg(test)]
    fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for DocError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// A failed load/commit, mapped to the sentence the caller sees. Upstream failures are 502,
/// never 500 (rule U12 of `design/ui-conformance.md`); a missing table is 404.
#[derive(Debug)]
enum CatalogError {
    Unreachable,
    Malformed,
    NotFound,
    Busy,
    Refused(StatusCode),
    InvalidName(String),
}

fn catalog_fail(e: CatalogError) -> DocError {
    match e {
        CatalogError::Unreachable => {
            DocError::new(StatusCode::BAD_GATEWAY, "the catalog is not reachable")
        }
        CatalogError::Malformed => DocError::new(
            StatusCode::BAD_GATEWAY,
            "the catalog returned an unexpected response",
        ),
        CatalogError::NotFound => DocError::new(StatusCode::NOT_FOUND, "no such table"),
        CatalogError::Busy => {
            DocError::new(StatusCode::BAD_GATEWAY, "the catalog is busy, try again")
        }
        CatalogError::Refused(status) => DocError::new(
            StatusCode::BAD_GATEWAY,
            format!("the catalog answered {status}"),
        ),
        CatalogError::InvalidName(what) => {
            DocError::new(StatusCode::BAD_REQUEST, format!("invalid name '{what}'"))
        }
    }
}

/// The bits of a load-table answer this module reads. `columns` is the **current** schema's
/// field list — Lakekeeper answers every historical schema, so `schemas[0]` is not it.
struct Loaded {
    properties: serde_json::Map<String, Value>,
    columns: Vec<LoadedColumn>,
}

struct LoadedColumn {
    name: String,
    ty: String,
    doc: Option<String>,
}

/// Lakekeeper's catalog REST under one warehouse, with the caller's authorization (when the
/// platform forwards it). The route resolves the warehouse id; this only builds URLs.
struct Catalog<'a> {
    http: &'a reqwest::Client,
    /// `NOVA_LAKEKEEPER_URL/catalog/v1/<warehouse-id>`
    base: String,
    authorization: Option<&'a str>,
}

impl Catalog<'_> {
    fn table_url(&self, ns: &str, tbl: &str) -> Result<String, CatalogError> {
        let (Some(ns), Some(tbl)) = (catalog::path_segment(ns), catalog::path_segment(tbl)) else {
            return Err(CatalogError::InvalidName(format!("{ns}.{tbl}")));
        };
        Ok(format!("{}/namespaces/{ns}/tables/{tbl}", self.base))
    }

    /// `GET …/tables/<table>`: the properties and the current schema's columns.
    async fn load(&self, ns: &str, tbl: &str) -> Result<Loaded, CatalogError> {
        let url = self.table_url(ns, tbl)?;
        let mut request = self.http.get(&url);
        if let Some(header) = self.authorization {
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }
        let response = request.send().await.map_err(|e| {
            warn!(error = %e, "lakekeeper load-table failed");
            CatalogError::Unreachable
        })?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        match status.as_u16() {
            200 => parse_loaded(&body),
            404 => Err(CatalogError::NotFound),
            code => {
                warn!(status = code, "lakekeeper load-table refused");
                Err(CatalogError::Refused(status))
            }
        }
    }

    /// One Iceberg `CommitTable` carrying `set-properties` and/or `remove-properties`.
    ///
    /// Lakekeeper 0.12.2 answers 500/503 while another commit to the same table is in
    /// flight; those are retried (200, 400 ms) because they are contention, not a refusal.
    /// Three attempts in total, then [`CatalogError::Busy`]. The body's `Error ID` is
    /// logged when the catalog includes one — it is the only way to correlate the line with
    /// Lakekeeper's own log.
    async fn commit(&self, ns: &str, tbl: &str, body: &Value) -> Result<(), CatalogError> {
        let url = self.table_url(ns, tbl)?;
        const ATTEMPTS: usize = 3;
        let mut wait = std::time::Duration::from_millis(200);
        for attempt in 1..=ATTEMPTS {
            let mut request = self.http.post(&url).json(body);
            if let Some(header) = self.authorization {
                request = request.header(reqwest::header::AUTHORIZATION, header);
            }
            let response = request.send().await.map_err(|e| {
                warn!(attempt, error = %e, "lakekeeper commit failed");
                CatalogError::Unreachable
            })?;
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            let text = response.text().await.unwrap_or_default();
            let error_id = catalog_error_id(&text);
            match status.as_u16() {
                404 => return Err(CatalogError::NotFound),
                500 | 503 if attempt < ATTEMPTS => {
                    warn!(
                        attempt,
                        status = status.as_u16(),
                        error_id = error_id.as_deref().unwrap_or(""),
                        "catalog commit contended; retrying"
                    );
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                500 | 503 => {
                    warn!(
                        attempt,
                        status = status.as_u16(),
                        error_id = error_id.as_deref().unwrap_or(""),
                        "catalog commit still contended; giving up"
                    );
                    return Err(CatalogError::Busy);
                }
                _ => {
                    warn!(
                        attempt,
                        status = status.as_u16(),
                        error_id = error_id.as_deref().unwrap_or(""),
                        "catalog commit refused"
                    );
                    return Err(CatalogError::Refused(status));
                }
            }
        }
        Err(CatalogError::Busy)
    }
}

/// Lakekeeper's error id, when its body carries one. Accepts the JSON shapes seen in the
/// wild (`error.id`, `error.errorId`, `id`) and falls back to an `Error ID: <id>` in the
/// message. The text is never echoed to a client.
fn catalog_error_id(body: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        for pointer in ["/error/id", "/error/errorId", "/id", "/errorId"] {
            if let Some(id) = value.pointer(pointer).and_then(Value::as_str) {
                return Some(id.to_string());
            }
        }
    }
    let rest = body.split("Error ID").nth(1)?;
    let id: String = rest
        .trim_start_matches([':', ' ', '"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!id.is_empty()).then_some(id)
}

fn parse_loaded(body: &str) -> Result<Loaded, CatalogError> {
    let value: Value = serde_json::from_str(body).map_err(|_| CatalogError::Malformed)?;
    let metadata = value.get("metadata").ok_or(CatalogError::Malformed)?;
    let properties = match metadata.get("properties") {
        Some(Value::Object(map)) => map.clone(),
        None => serde_json::Map::new(),
        _ => return Err(CatalogError::Malformed),
    };
    let current = metadata
        .get("current-schema-id")
        .and_then(Value::as_i64)
        .ok_or(CatalogError::Malformed)?;
    let schema = metadata
        .get("schemas")
        .and_then(Value::as_array)
        .and_then(|schemas| {
            schemas
                .iter()
                .find(|s| s.get("schema-id").and_then(Value::as_i64) == Some(current))
        })
        .ok_or(CatalogError::Malformed)?;
    let fields = schema
        .get("fields")
        .and_then(Value::as_array)
        .ok_or(CatalogError::Malformed)?;
    let mut columns = Vec::with_capacity(fields.len());
    for field in fields {
        let name = field
            .get("name")
            .and_then(Value::as_str)
            .ok_or(CatalogError::Malformed)?;
        let ty = field.get("type").ok_or(CatalogError::Malformed)?;
        // A primitive type is a string in JSON; a struct/list/map is an object. The
        // documented answer is the type as text, so a complex type becomes its JSON text.
        let ty = match ty {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        columns.push(LoadedColumn {
            name: name.to_string(),
            ty,
            doc: field.get("doc").and_then(Value::as_str).map(str::to_string),
        });
    }
    Ok(Loaded {
        properties,
        columns,
    })
}

fn property(properties: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Tags are stored as one comma-separated property. Split, trim and drop empties on the way
/// out; the write side always stores the sorted canonical form.
fn split_tags(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Build the read answer from a load-table result. The rule is evaluated for `who`.
fn documentation(loaded: &Loaded, who: &Identity) -> TableDocumentation {
    let owner = property(&loaded.properties, "stardelt.owner");
    let can_edit = may_edit(who, owner.as_deref()).is_ok();
    TableDocumentation {
        description: property(&loaded.properties, "comment"),
        tags: property(&loaded.properties, "stardelt.tags")
            .map(|raw| split_tags(&raw))
            .unwrap_or_default(),
        owner: owner.clone(),
        domain: property(&loaded.properties, "stardelt.domain"),
        columns: loaded
            .columns
            .iter()
            .map(|c| ColumnDocumentation {
                name: c.name.clone(),
                r#type: c.ty.clone(),
                description: c.doc.clone(),
            })
            .collect(),
        can_edit,
        edit_rule: edit_rule(who, owner.as_deref()),
    }
}

/// Group names compare exactly, or with Keycloak's leading `/` (full group path) — the same
/// rule `auth::has_group` uses.
fn in_group(groups: &[String], wanted: &str) -> bool {
    groups
        .iter()
        .any(|g| g == wanted || g.strip_prefix('/') == Some(wanted))
}

/// The interim rule: who may edit this table's documentation.
///
/// Enforced by Nova only — the catalog does not enforce it yet, so anyone who reaches
/// Lakekeeper or Trino directly can still change these properties. A no-owner table may be
/// edited by anybody, but the **write** must then set `owner` to one of the caller's own
/// groups (see [`claim_allowed`]); this function only answers whether editing is allowed at
/// all.
pub fn may_edit(identity: &Identity, current_owner: Option<&str>) -> Result<(), String> {
    if identity.is_admin {
        return Ok(());
    }
    match current_owner {
        Some(owner) if in_group(&identity.groups, owner) => Ok(()),
        Some(owner) => Err(format!(
            "only members of the owner group {owner} and administrators may edit this"
        )),
        None => Ok(()),
    }
}

/// The sentence the read answer carries for `canEdit`. Success cases say why; a refusal
/// repeats the rule in a sentence fit for a person.
fn edit_rule(identity: &Identity, current_owner: Option<&str>) -> String {
    if identity.is_admin {
        return "You can edit because you are an administrator.".into();
    }
    match current_owner {
        Some(owner) if in_group(&identity.groups, owner) => {
            format!("You can edit because you belong to the owner group {owner}.")
        }
        Some(owner) => {
            format!("Only members of the owner group {owner} and administrators may edit this.")
        }
        None => {
            "You can edit, but you must set an owner group of your own: the table has no owner yet."
                .into()
        }
    }
}

/// The no-owner clause: a non-administrator may only edit an ownerless table while claiming
/// it for one of their own groups. Administrators may set any valid group name (or none).
fn claim_allowed(
    who: &Identity,
    current_owner: Option<&str>,
    requested_owner: &Option<Option<String>>,
) -> bool {
    if current_owner.is_some() || who.is_admin {
        return true;
    }
    match requested_owner {
        Some(Some(group)) => in_group(&who.groups, group),
        _ => false,
    }
}

/// A description as it is stored, or `None` when it should be removed. Trims, caps the
/// length and refuses a NUL.
fn checked_description(raw: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let text = raw.trim();
    if text.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        ));
    }
    if text.contains('\0') {
        return Err("description must not contain a NUL character".into());
    }
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text.to_string()))
}

/// A tag: 1 to 32 lower-case ASCII letters, digits, `-` or `_`, starting with a letter or
/// digit — `^[a-z0-9][a-z0-9_-]{0,31}$`.
fn tag_ok(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    (1..=32).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

/// A domain: 1 to 40 lower-case ASCII letters, digits, `-` or `_`, starting with a letter or
/// digit — `^[a-z0-9][a-z0-9_-]{0,39}$`.
fn domain_ok(domain: &str) -> bool {
    let bytes = domain.as_bytes();
    (1..=40).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

/// A validated write: each `Some` is a key that was present. `Some(None)` removes, and for
/// tags an empty list is normalised to `None` (remove) before the diff.
struct Requested {
    description: Option<Option<String>>,
    tags: Option<Option<Vec<String>>>,
    owner: Option<Option<String>>,
    domain: Option<Option<String>>,
}

fn validate_update(req: DocumentationUpdate) -> Result<Requested, String> {
    let description = match req.description {
        None => None,
        Some(raw) => Some(checked_description(raw)?),
    };
    let tags = match req.tags {
        None => None,
        Some(None) => Some(None),
        Some(Some(tags)) => {
            if tags.len() > MAX_TAGS {
                return Err(format!("at most {MAX_TAGS} tags are allowed"));
            }
            if tags.is_empty() {
                Some(None)
            } else {
                let mut seen = BTreeSet::new();
                for tag in &tags {
                    if !tag_ok(tag) {
                        return Err(
                            "tags must be 1 to 32 lower-case letters, digits, dashes or \
                             underscores, starting with a letter or digit"
                                .into(),
                        );
                    }
                    if !seen.insert(tag.as_str()) {
                        return Err("tags must be unique".into());
                    }
                }
                let mut sorted = tags;
                sorted.sort();
                Some(Some(sorted))
            }
        }
    };
    let owner = match req.owner {
        None => None,
        Some(None) => Some(None),
        Some(Some(owner)) => {
            if !crate::connections::group_ok(&owner) {
                return Err(
                    "owner must be a group name: 1 to 64 letters, digits, dots, dashes or \
                     underscores, starting with a letter or digit"
                        .into(),
                );
            }
            Some(Some(owner))
        }
    };
    let domain = match req.domain {
        None => None,
        Some(None) => Some(None),
        Some(Some(domain)) => {
            if !domain_ok(&domain) {
                return Err(
                    "domain must be 1 to 40 lower-case letters, digits, dashes or underscores, \
                     starting with a letter or digit"
                        .into(),
                );
            }
            Some(Some(domain))
        }
    };
    Ok(Requested {
        description,
        tags,
        owner,
        domain,
    })
}

/// The per-table edit lock. Held for the whole write, including the load that decides the
/// rule and the statement a column write runs.
fn table_lock(state: &AppState, ns: &str, tbl: &str) -> Arc<tokio::sync::Mutex<()>> {
    let key = format!("{ns}.{tbl}");
    Arc::clone(
        state
            .table_locks
            .lock()
            .expect("table locks poisoned")
            .entry(key)
            .or_default(),
    )
}

/// `GET /api/catalog/tables/{ns}/{tbl}/documentation`.
async fn read_documentation(
    catalog: &Catalog<'_>,
    who: &Identity,
    ns: &str,
    tbl: &str,
) -> Result<TableDocumentation, DocError> {
    let loaded = catalog.load(ns, tbl).await.map_err(catalog_fail)?;
    Ok(documentation(&loaded, who))
}

/// `PUT /api/catalog/tables/{ns}/{tbl}/documentation`.
///
/// Validation runs before the lock and before any request. The lock covers the load that
/// decides the rule, the commit and the fresh load the answer is built from.
async fn write_documentation(
    state: &AppState,
    catalog: &Catalog<'_>,
    who: &Identity,
    ns: &str,
    tbl: &str,
    req: DocumentationUpdate,
) -> Result<TableDocumentation, DocError> {
    let requested = validate_update(req).map_err(|e| DocError::new(StatusCode::BAD_REQUEST, e))?;
    let lock = table_lock(state, ns, tbl);
    let _guard = lock.lock().await;

    let loaded = catalog.load(ns, tbl).await.map_err(catalog_fail)?;
    let current = documentation(&loaded, who);
    may_edit(who, current.owner.as_deref()).map_err(|e| DocError::new(StatusCode::FORBIDDEN, e))?;
    if !claim_allowed(who, current.owner.as_deref(), &requested.owner) {
        return Err(DocError::new(
            StatusCode::BAD_REQUEST,
            "choose an owner: one of your groups",
        ));
    }

    let mut set: BTreeMap<String, String> = BTreeMap::new();
    let mut remove: Vec<String> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();

    if let Some(next) = &requested.description
        && next.as_deref() != current.description.as_deref()
    {
        changed.push("description");
        match next {
            Some(text) => {
                set.insert("comment".into(), text.clone());
            }
            None => remove.push("comment".into()),
        }
    }
    if let Some(next) = &requested.tags {
        let next = next.clone().unwrap_or_default();
        if next != current.tags {
            changed.push("tags");
            if next.is_empty() {
                remove.push("stardelt.tags".into());
            } else {
                set.insert("stardelt.tags".into(), next.join(","));
            }
        }
    }
    if let Some(next) = &requested.owner
        && next.as_deref() != current.owner.as_deref()
    {
        changed.push("owner");
        match next {
            Some(owner) => {
                set.insert("stardelt.owner".into(), owner.clone());
            }
            None => remove.push("stardelt.owner".into()),
        }
    }
    if let Some(next) = &requested.domain
        && next.as_deref() != current.domain.as_deref()
    {
        changed.push("domain");
        match next {
            Some(domain) => {
                set.insert("stardelt.domain".into(), domain.clone());
            }
            None => remove.push("stardelt.domain".into()),
        }
    }

    if changed.is_empty() {
        return Ok(current);
    }

    let mut updates = Vec::new();
    if !set.is_empty() {
        updates.push(json!({ "action": "set-properties", "updates": set }));
    }
    if !remove.is_empty() {
        updates.push(json!({ "action": "remove-properties", "removals": remove }));
    }
    let body = json!({ "requirements": [], "updates": updates });
    catalog.commit(ns, tbl, &body).await.map_err(catalog_fail)?;

    audit::catalog_doc_event(who, &format!("{ns}.{tbl}"), &changed);
    let loaded = catalog.load(ns, tbl).await.map_err(catalog_fail)?;
    Ok(documentation(&loaded, who))
}

/// `PUT /api/catalog/tables/{ns}/{tbl}/columns/{column}/description`.
///
/// The column must be a field of the **current** schema, matched exactly; the name from the
/// metadata (never the path text) goes into the statement, through `quote_ident`. The
/// statement runs as the caller with the `nova-catalog` source, so the engine records the
/// person. The owner claim of [`claim_allowed`] needs an owner field the body does not have,
/// so a column write only asks [`may_edit`].
// Eight inputs is the seam the tests need: the table, the rule's identity, the statement
// and the injected runner. Grouping them would only move the arguments elsewhere.
#[allow(clippy::too_many_arguments)]
async fn write_column_description<F, Fut>(
    state: &AppState,
    catalog: &Catalog<'_>,
    who: &Identity,
    ns: &str,
    tbl: &str,
    column: &str,
    req: ColumnDescriptionUpdate,
    run_sql: F,
) -> Result<TableDocumentation, DocError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let requested = match req.description {
        None => None,
        Some(raw) => {
            Some(checked_description(raw).map_err(|e| DocError::new(StatusCode::BAD_REQUEST, e))?)
        }
    };
    let lock = table_lock(state, ns, tbl);
    let _guard = lock.lock().await;

    let loaded = catalog.load(ns, tbl).await.map_err(catalog_fail)?;
    let current = documentation(&loaded, who);
    may_edit(who, current.owner.as_deref()).map_err(|e| DocError::new(StatusCode::FORBIDDEN, e))?;

    let Some(field) = loaded.columns.iter().find(|c| c.name == column) else {
        return Err(DocError::new(StatusCode::NOT_FOUND, "no such column"));
    };

    let loaded = match requested {
        None => loaded,
        Some(description) => {
            let sql = comment_on_column_sql(ns, tbl, &field.name, description.as_deref());
            run_sql(sql)
                .await
                .map_err(|e| DocError::new(crate::trino::error_status(&e), e))?;
            let table = format!("{ns}.{tbl}");
            let changed = format!("column:{}", field.name);
            audit::catalog_doc_event(who, &table, &[changed.as_str()]);
            catalog.load(ns, tbl).await.map_err(catalog_fail)?
        }
    };
    Ok(documentation(&loaded, who))
}

/// The only escaping a Trino string literal needs at these sizes: `'` doubles, and nothing
/// else changes (backslash, `"`, newlines, `--`, `;` and emoji survive as written).
fn sql_literal(raw: &str) -> String {
    raw.replace('\'', "''")
}

/// `COMMENT ON COLUMN warehouse.<ns>.<tbl>.<column> IS '<text>'` (or `IS NULL`). Every
/// identifier goes through `quote_ident`, so a field named `a"b` or `x; DROP TABLE y` is one
/// quoted identifier, never SQL.
fn comment_on_column_sql(ns: &str, tbl: &str, column: &str, description: Option<&str>) -> String {
    let value = match description {
        Some(text) => format!("'{}'", sql_literal(text)),
        None => "NULL".to_string(),
    };
    format!(
        "COMMENT ON COLUMN warehouse.{}.{}.{} IS {value}",
        quote_ident(ns),
        quote_ident(tbl),
        quote_ident(column),
    )
}

/// The caller's identity: the principal in `oidc` mode, the static user as an administrator
/// in `dev` mode (the same convention as `apps.rs` and the break-glass log).
fn caller_identity(state: &AppState, caller: Option<&Principal>) -> Identity {
    match caller {
        Some(principal) => principal.identity.clone(),
        None => Identity {
            sub: state.dev_user.clone(),
            display_name: state.dev_user.clone(),
            email: None,
            name: None,
            groups: Vec::new(),
            roles: Vec::new(),
            groups_overage: false,
            is_admin: true,
        },
    }
}

fn demo_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "not available in demo mode" })),
    )
        .into_response()
}

/// Resolve the warehouse id and build the catalog client for one request.
///
/// The catalog is called with Nova's own service token (`catalog_token.rs`), never the
/// caller's: per-user catalog rights do not exist yet, so one identity reaches Lakekeeper.
async fn catalog_or_error(state: &AppState) -> Result<(String, Option<String>), Box<Response>> {
    let id = match catalog::warehouse_id_or_error(state).await {
        Ok(id) => id,
        Err(response) => return Err(response),
    };
    let authorization = match catalog::lakekeeper_authorization(state).await {
        Ok(authorization) => authorization,
        Err(e) => {
            return Err(Box::new(
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            ));
        }
    };
    Ok((
        format!("{}/catalog/v1/{id}", state.lakekeeper_url),
        authorization,
    ))
}

#[utoipa::path(
    get,
    path = "/api/catalog/tables/{ns}/{tbl}/documentation",
    tag = "public",
    description = "A table's documentation: the Iceberg `comment` as `description`, the `stardelt.tags`/`stardelt.owner`/`stardelt.domain` custom properties, and the description and type of every column of the **current** schema. `canEdit`/`editRule` apply Nova's interim rule: until the catalog enforces permissions, Nova decides who may edit, and anyone who reaches Lakekeeper or Trino directly can still change these properties. Demo mode answers 503.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
    ),
    responses(
        (status = 200, description = "The table's documentation", body = TableDocumentation),
        (status = 400, description = "A name is not a single path segment", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such table", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog is not reachable or did not answer", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Demo mode", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn read(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    caller: Option<axum::Extension<Principal>>,
) -> Response {
    if s.demo {
        return demo_unavailable();
    }
    let caller = caller.as_ref().map(|c| &c.0);
    let who = caller_identity(&s, caller);
    let (base, authorization) = match catalog_or_error(&s).await {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let catalog = Catalog {
        http: &s.http,
        base,
        authorization: authorization.as_deref(),
    };
    match read_documentation(&catalog, &who, &ns, &tbl).await {
        Ok(doc) => Json(doc).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/api/catalog/tables/{ns}/{tbl}/documentation",
    tag = "public",
    description = "Set or remove a table's description, tags, owner and domain in one Lakekeeper commit. Every key is optional: absent changes nothing, `null` (or an empty tag list, or a blank description) removes the property, and a write with nothing to change sends no commit. Description at most 4,000 characters; at most 20 unique tags `^[a-z0-9][a-z0-9_-]{0,31}$`; owner a group name; domain `^[a-z0-9][a-z0-9_-]{0,39}$`. An ownerless table may be edited by anybody, but the request must then set `owner` to one of the caller's own groups. **The interim rule is enforced by Nova; the catalog does not enforce it yet** — anyone who reaches Lakekeeper or Trino directly can still change these properties. Edits of one table are serialised by Nova and a contended commit is retried; a catalog that stays busy answers 502 `the catalog is busy, try again`. Demo mode answers 503.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
    ),
    request_body = DocumentationUpdate,
    responses(
        (status = 200, description = "The documentation after the change, freshly loaded", body = TableDocumentation),
        (status = 400, description = "The body is invalid, or the table has no owner and this request does not claim it", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Only the owner group and administrators may edit", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such table", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog is not reachable, did not answer, or stayed busy", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Demo mode", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn update(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl)): Path<(String, String)>,
    caller: Option<axum::Extension<Principal>>,
    ApiJson(req): ApiJson<DocumentationUpdate>,
) -> Response {
    if s.demo {
        return demo_unavailable();
    }
    let caller = caller.as_ref().map(|c| &c.0);
    let who = caller_identity(&s, caller);
    let (base, authorization) = match catalog_or_error(&s).await {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let catalog = Catalog {
        http: &s.http,
        base,
        authorization: authorization.as_deref(),
    };
    match write_documentation(&s, &catalog, &who, &ns, &tbl, req).await {
        Ok(doc) => Json(doc).into_response(),
        Err(e) => e.into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/api/catalog/tables/{ns}/{tbl}/columns/{column}/description",
    tag = "public",
    description = "Set or remove one column's description. The column must be a field of the current schema, matched exactly. Nova runs `COMMENT ON COLUMN warehouse.<ns>.<tbl>.<column>` through Trino **as the caller** (source `nova-catalog`), because a column description lives in a new Iceberg schema and only the engine writes it. Same 4,000-character limit and null rule as the table description. The interim rule is enforced by Nova; the catalog does not enforce it yet. Demo mode answers 503.",
    params(
        ("ns" = String, Path, description = "Namespace (schema) name"),
        ("tbl" = String, Path, description = "Table name"),
        ("column" = String, Path, description = "Column name, exactly as the current schema spells it"),
    ),
    request_body = ColumnDescriptionUpdate,
    responses(
        (status = 200, description = "The table's documentation after the change, freshly loaded", body = TableDocumentation),
        (status = 400, description = "The body is invalid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "Only the owner group and administrators may edit", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such table or no such column in the current schema", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
        (status = 503, description = "Demo mode", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn update_column(
    State(s): State<Arc<AppState>>,
    Path((ns, tbl, column)): Path<(String, String, String)>,
    caller: Option<axum::Extension<Principal>>,
    ApiJson(req): ApiJson<ColumnDescriptionUpdate>,
) -> Response {
    if s.demo {
        return demo_unavailable();
    }
    let caller = caller.as_ref().map(|c| &c.0);
    let who = caller_identity(&s, caller);
    let (base, authorization) = match catalog_or_error(&s).await {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let catalog = Catalog {
        http: &s.http,
        base,
        authorization: authorization.as_deref(),
    };
    let trino_state = Arc::clone(&s);
    let result = write_column_description(
        &s,
        &catalog,
        &who,
        &ns,
        &tbl,
        &column,
        req,
        |sql| async move {
            crate::trino::execute_from(&trino_state, sql, caller, "nova-catalog")
                .await
                .map(|_| ())
        },
    )
    .await;
    match result {
        Ok(doc) => Json(doc).into_response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::{
        Router,
        extract::{Path, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::get,
    };
    use serde_json::{Value, json};
    use tokio::sync::OnceCell;

    use super::*;
    use crate::auth::Identity;

    // -----------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------

    fn who(sub: &str, groups: &[&str], admin: bool) -> Identity {
        Identity {
            sub: sub.into(),
            display_name: sub.into(),
            email: None,
            name: None,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            roles: Vec::new(),
            groups_overage: false,
            is_admin: admin,
        }
    }

    fn admin() -> Identity {
        who("admin", &[], true)
    }

    /// A load-table answer with two schemas. Schema 1 carries the `doc`s; the properties are
    /// the ones the read tests expect.
    fn load_fixture() -> Value {
        json!({
            "metadata-location": "s3://warehouse/sales/orders/metadata/00001.metadata.json",
            "metadata": {
                "format-version": 2,
                "current-schema-id": 1,
                "schemas": [
                    {
                        "type": "struct",
                        "schema-id": 0,
                        "fields": [
                            { "id": 1, "name": "id", "required": true, "type": "long" },
                            { "id": 2, "name": "amount", "required": false, "type": "double" },
                            { "id": 5, "name": "legacy", "required": false, "type": "string" }
                        ]
                    },
                    {
                        "type": "struct",
                        "schema-id": 1,
                        "fields": [
                            { "id": 1, "name": "id", "required": true, "type": "long",
                              "doc": "the identifier" },
                            { "id": 2, "name": "amount", "required": false, "type": "double",
                              "doc": "the amount" },
                            { "id": 3, "name": "a\"b", "required": false, "type": "string",
                              "doc": "quoted name" },
                            { "id": 4, "name": "x; DROP TABLE y", "required": false,
                              "type": { "type": "list", "element": "string" },
                              "doc": "not SQL" }
                        ]
                    }
                ],
                "properties": {
                    "comment": "orders, one row per order",
                    "stardelt.tags": "finance, pii , ",
                    "stardelt.owner": "data-eng",
                    "stardelt.domain": "sales"
                }
            }
        })
    }

    #[derive(Default)]
    struct FakeState {
        load: Value,
        load_status: Option<u16>,
        /// Statuses the next commits answer, in order; an empty list means 200.
        commit_statuses: Vec<u16>,
        commits: Vec<Value>,
        requests: usize,
        in_flight: usize,
        max_in_flight: usize,
        commit_delay: Option<Duration>,
    }

    #[derive(Clone, Default)]
    struct Fake {
        inner: Arc<Mutex<FakeState>>,
    }

    impl Fake {
        fn set_load(&self, load: Value) {
            self.inner.lock().unwrap().load = load;
        }

        fn fail_load(&self, status: u16) {
            self.inner.lock().unwrap().load_status = Some(status);
        }

        fn fail_commits(&self, statuses: &[u16]) {
            self.inner.lock().unwrap().commit_statuses = statuses.to_vec();
        }

        fn delay_commits(&self, delay: Duration) {
            self.inner.lock().unwrap().commit_delay = Some(delay);
        }

        fn snapshot(&self) -> (Vec<Value>, usize, usize) {
            let s = self.inner.lock().unwrap();
            (s.commits.clone(), s.requests, s.max_in_flight)
        }

        fn reset_stats(&self) {
            let mut s = self.inner.lock().unwrap();
            s.commits.clear();
            s.requests = 0;
            s.in_flight = 0;
            s.max_in_flight = 0;
        }
    }

    async fn load_handler(
        State(fake): State<Fake>,
        Path((_wh, _ns, _tbl)): Path<(String, String, String)>,
    ) -> Response {
        let (status, body) = {
            let mut s = fake.inner.lock().unwrap();
            s.requests += 1;
            (s.load_status.unwrap_or(200), s.load.clone())
        };
        match status {
            200 => Json(body).into_response(),
            404 => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": { "message": "no such table" } })),
            )
                .into_response(),
            other => (
                StatusCode::from_u16(other).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({ "error": { "message": "nope" } })),
            )
                .into_response(),
        }
    }

    async fn commit_handler(
        State(fake): State<Fake>,
        Path((_wh, _ns, _tbl)): Path<(String, String, String)>,
        Json(body): Json<Value>,
    ) -> Response {
        let (status, delay) = {
            let mut s = fake.inner.lock().unwrap();
            s.requests += 1;
            s.commits.push(body);
            s.in_flight += 1;
            s.max_in_flight = s.max_in_flight.max(s.in_flight);
            let status = if s.commit_statuses.is_empty() {
                200
            } else {
                s.commit_statuses.remove(0)
            };
            (status, s.commit_delay)
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut s = fake.inner.lock().unwrap();
            s.in_flight = s.in_flight.saturating_sub(1);
        }
        match status {
            200 => Json(json!({})).into_response(),
            404 => (StatusCode::NOT_FOUND, Json(json!({ "error": {} }))).into_response(),
            other => (
                StatusCode::from_u16(other).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({
                    "error": {
                        "message": "Error starting transaction",
                        "id": "err-42"
                    }
                })),
            )
                .into_response(),
        }
    }

    /// A fake Lakekeeper answering one table's GET and POST.
    async fn fake_catalog() -> (Fake, String) {
        let fake = Fake::default();
        fake.set_load(load_fixture());
        let app = Router::new()
            .route(
                "/catalog/v1/{wh}/namespaces/{ns}/tables/{tbl}",
                get(load_handler).post(commit_handler),
            )
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (fake, format!("http://{addr}"))
    }

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState {
            dev_user: "test".into(),
            trino_url: "http://127.0.0.1:1".into(),
            lakekeeper_url: "http://127.0.0.1:1".into(),
            // No catalog client in these tests: Lakekeeper is called as before (#222).
            catalog_token: None,
            superset_url: "http://127.0.0.1:1".into(),
            jupyterhub_url: "http://127.0.0.1:1".into(),
            warehouse_name: "warehouse".into(),
            warehouse_id: OnceCell::new(),
            http: reqwest::Client::new(),
            demo: false,
            forward_identity: false,
            jobs: Mutex::new(Vec::new()),
            demo_state: std::sync::RwLock::new(crate::demo::DemoState::default()),
            kube: None,
            workloads: crate::cluster::WorkloadCache::default(),
            admin_console_url: None,
            db: None,
            store: None,
            break_glass_reads: Mutex::new(std::collections::HashMap::new()),
            run_starts: std::sync::Mutex::new(std::collections::HashMap::new()),
            pipeline_creates: Mutex::new(std::collections::HashMap::new()),
            airflow: None,
            table_locks: Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn catalog<'a>(state: &'a AppState, addr: &str) -> Catalog<'a> {
        Catalog {
            http: &state.http,
            base: format!("{addr}/catalog/v1/wh"),
            authorization: None,
        }
    }

    fn update_body(
        description: Option<Option<&str>>,
        tags: Option<Option<&[&str]>>,
        owner: Option<Option<&str>>,
        domain: Option<Option<&str>>,
    ) -> DocumentationUpdate {
        DocumentationUpdate {
            description: description.map(|d| d.map(str::to_string)),
            tags: tags.map(|t| t.map(|list| list.iter().map(|s| s.to_string()).collect())),
            owner: owner.map(|o| o.map(str::to_string)),
            domain: domain.map(|d| d.map(str::to_string)),
        }
    }

    fn status_of(e: DocError) -> (StatusCode, String) {
        (e.status(), e.message().to_string())
    }

    // -----------------------------------------------------------------------
    // 1. Read: the current schema, not schemas[0]
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_picks_the_current_schema() {
        let (_fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let doc = read_documentation(&catalog, &member, "sales", "orders")
            .await
            .expect("the table loads");
        assert_eq!(doc.columns.len(), 4, "schema 1 is the current one");
        assert_eq!(
            doc.columns[0].description.as_deref(),
            Some("the identifier")
        );
        assert_eq!(doc.columns[1].description.as_deref(), Some("the amount"));
        assert_eq!(
            doc.columns[3].r#type, r#"{"element":"string","type":"list"}"#,
            "a complex type is its JSON text"
        );

        let mut older = load_fixture();
        older["metadata"]["current-schema-id"] = json!(0);
        _fake.set_load(older);
        let doc = read_documentation(&catalog, &member, "sales", "orders")
            .await
            .expect("the older schema also loads");
        assert_eq!(doc.columns.len(), 3, "schema 0 is current now");
        assert!(
            doc.columns.iter().all(|c| c.description.is_none()),
            "schema 0 has no doc fields"
        );
    }

    // -----------------------------------------------------------------------
    // 2. Read: properties and the rule sentence
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn read_splits_tags_and_reports_owner_domain_description() {
        let (_fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let doc = read_documentation(&catalog, &member, "sales", "orders")
            .await
            .expect("the table loads");
        assert_eq!(
            doc.description.as_deref(),
            Some("orders, one row per order")
        );
        assert_eq!(doc.tags, ["finance", "pii"]);
        assert_eq!(doc.owner.as_deref(), Some("data-eng"));
        assert_eq!(doc.domain.as_deref(), Some("sales"));
        assert!(doc.can_edit);
        assert_eq!(
            doc.edit_rule,
            "You can edit because you belong to the owner group data-eng."
        );
        let serialized = serde_json::to_value(&doc).expect("the answer serializes");
        assert_eq!(serialized["canEdit"], json!(true));
        assert_eq!(serialized["editRule"], json!(doc.edit_rule));
        assert_eq!(serialized["columns"][0]["name"], json!("id"));
        assert!(serialized["columns"][0]["description"].is_string());
    }

    // -----------------------------------------------------------------------
    // 3. Write: one commit with set and remove; a no-op sends nothing
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn write_sends_one_commit_with_set_and_remove() {
        let (fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let doc = write_documentation(
            &state,
            &catalog,
            &member,
            "sales",
            "orders",
            update_body(
                Some(Some("new description")),
                Some(Some(&["vip", "pii"])),
                None,
                Some(None),
            ),
        )
        .await
        .expect("the write succeeds");
        assert!(!doc.can_edit || doc.owner.as_deref() == Some("data-eng"));

        let (commits, _, _) = fake.snapshot();
        assert_eq!(commits.len(), 1, "one commit carries both actions");
        assert_eq!(commits[0]["requirements"], json!([]), "no requirements");
        assert_eq!(
            commits[0]["updates"],
            json!([
                {
                    "action": "set-properties",
                    "updates": {
                        "comment": "new description",
                        "stardelt.tags": "pii,vip"
                    }
                },
                {
                    "action": "remove-properties",
                    "removals": ["stardelt.domain"]
                }
            ]),
            "tags are sorted and joined; the domain is removed"
        );
    }

    #[tokio::test]
    async fn a_noop_write_sends_nothing() {
        let (fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let doc = write_documentation(
            &state,
            &catalog,
            &member,
            "sales",
            "orders",
            update_body(
                Some(Some("orders, one row per order")),
                Some(Some(&["pii", "finance"])),
                Some(Some("data-eng")),
                Some(Some("sales")),
            ),
        )
        .await
        .expect("the no-op succeeds");
        assert_eq!(doc.tags, ["finance", "pii"]);

        let (commits, requests, _) = fake.snapshot();
        assert!(commits.is_empty(), "nothing changed, so nothing was sent");
        assert_eq!(requests, 1, "only the load that decided the rule ran");

        // An empty body is a no-op too.
        let _ = write_documentation(
            &state,
            &catalog,
            &member,
            "sales",
            "orders",
            update_body(None, None, None, None),
        )
        .await
        .expect("an empty body is a no-op");
        assert!(fake.snapshot().0.is_empty());
    }

    // -----------------------------------------------------------------------
    // 4. Validation happens before any request
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn validation_refuses_before_any_request() {
        let (fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let long = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
        let many: Vec<String> = (0..=MAX_TAGS).map(|i| format!("tag{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let cases = [
            update_body(Some(Some(long.as_str())), None, None, None),
            update_body(None, Some(Some(&many)), None, None),
            update_body(None, Some(Some(&["Finance"])), None, None),
            update_body(None, Some(Some(&["pii", "pii"])), None, None),
            update_body(None, None, Some(Some("bad group!")), Some(Some("sales.eu"))),
            update_body(Some(Some("a\0b")), None, None, None),
        ];
        for req in cases {
            let (status, message) = status_of(
                write_documentation(&state, &catalog, &member, "sales", "orders", req)
                    .await
                    .expect_err("the body is invalid"),
            );
            assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
            assert!(!message.is_empty());
        }
        assert_eq!(
            fake.snapshot().1,
            0,
            "validation refused before the catalog saw a request"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Retries and the busy answer
    // -----------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn retries_500_and_503_then_succeeds() {
        let (fake, addr) = fake_catalog().await;
        fake.fail_commits(&[500, 503]);
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let started = tokio::time::Instant::now();
        let doc = write_documentation(
            &state,
            &catalog,
            &member,
            "sales",
            "orders",
            update_body(Some(Some("new")), None, None, None),
        )
        .await
        .expect("the third attempt succeeds");
        let elapsed = started.elapsed();
        assert_eq!(
            doc.description.as_deref(),
            Some("orders, one row per order")
        );
        assert_eq!(fake.snapshot().0.len(), 3, "two retries after the first");
        assert!(
            elapsed >= Duration::from_millis(600) && elapsed < Duration::from_millis(1000),
            "200 ms + 400 ms of waits, got {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_with_502_after_three_attempts() {
        let (fake, addr) = fake_catalog().await;
        fake.fail_commits(&[500, 503, 500, 503]);
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let started = tokio::time::Instant::now();
        let error = write_documentation(
            &state,
            &catalog,
            &member,
            "sales",
            "orders",
            update_body(Some(Some("new")), None, None, None),
        )
        .await
        .expect_err("three attempts are enough");
        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(error.message(), "the catalog is busy, try again");
        assert_eq!(fake.snapshot().0.len(), 3, "exactly three attempts");
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "the waits happened"
        );
    }

    #[tokio::test]
    async fn a_404_is_a_404() {
        let (fake, addr) = fake_catalog().await;
        fake.fail_load(404);
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);

        let error = read_documentation(&catalog, &member, "sales", "orders")
            .await
            .expect_err("no such table");
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.message(), "no such table");
    }

    // -----------------------------------------------------------------------
    // 6. Per-table serialisation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn edits_of_one_table_are_serialised() {
        let (fake, addr) = fake_catalog().await;
        fake.delay_commits(Duration::from_millis(100));
        let state = test_state();
        let catalog = catalog(&state, &addr);

        let a = who("ada", &["data-eng"], false);
        let b = who("bob", &["data-eng"], false);
        let first = update_body(Some(Some("first")), None, Some(Some("data-eng")), None);
        let second = update_body(Some(Some("second")), None, Some(Some("data-eng")), None);
        let (r1, r2) = tokio::join!(
            write_documentation(&state, &catalog, &a, "sales", "orders", first),
            write_documentation(&state, &catalog, &b, "sales", "orders", second),
        );
        r1.expect("the first write succeeds");
        r2.expect("the second write succeeds");
        let (commits, _, max) = fake.snapshot();
        assert_eq!(commits.len(), 2);
        assert_eq!(max, 1, "never two commits of one table in flight");

        // Two different tables are independent: their commits may overlap.
        fake.reset_stats();
        let (r1, r2) = tokio::join!(
            write_documentation(
                &state,
                &catalog,
                &a,
                "sales",
                "orders_a",
                update_body(Some(Some("a")), None, Some(Some("data-eng")), None)
            ),
            write_documentation(
                &state,
                &catalog,
                &b,
                "sales",
                "orders_b",
                update_body(Some(Some("b")), None, Some(Some("data-eng")), None)
            ),
        );
        r1.expect("the first table's write succeeds");
        r2.expect("the second table's write succeeds");
        let (commits, _, max) = fake.snapshot();
        assert_eq!(commits.len(), 2);
        assert_eq!(max, 2, "different tables may edit at the same time");
    }

    // -----------------------------------------------------------------------
    // 7. The SQL literal and the quoting of metadata names
    // -----------------------------------------------------------------------

    #[test]
    fn catalog_error_id_is_read_from_the_body() {
        assert_eq!(
            catalog_error_id(r#"{"error":{"message":"x","id":"abc-1"}}"#).as_deref(),
            Some("abc-1")
        );
        assert_eq!(
            catalog_error_id("deadlock detected (Error ID: 7f3a2)").as_deref(),
            Some("7f3a2")
        );
        assert_eq!(catalog_error_id("no id here"), None);
    }

    #[test]
    fn sql_literal_round_trips_the_nasty_text() {
        let text = "it's \"quoted\"\n-- not a comment🙂;";
        assert_eq!(
            sql_literal(text),
            "it''s \"quoted\"\n-- not a comment🙂;",
            "only the apostrophe doubles"
        );
        assert_eq!(
            comment_on_column_sql("a\"b", "x; DROP TABLE y", "amount", Some("it's")),
            "COMMENT ON COLUMN warehouse.\"a\"\"b\".\"x; DROP TABLE y\".\"amount\" IS 'it''s'"
        );
        assert_eq!(
            comment_on_column_sql("sales", "orders", "amount", None),
            "COMMENT ON COLUMN warehouse.\"sales\".\"orders\".\"amount\" IS NULL"
        );
    }

    #[tokio::test]
    async fn nasty_column_names_only_reach_sql_through_quote_ident() {
        let (_fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);
        let ran = Arc::new(Mutex::new(Vec::new()));

        for column in ["a\"b", "x; DROP TABLE y"] {
            let seen = Arc::clone(&ran);
            write_column_description(
                &state,
                &catalog,
                &member,
                "a\"b",
                "x; DROP TABLE y",
                column,
                ColumnDescriptionUpdate {
                    description: Some(Some("ok".into())),
                },
                move |sql| {
                    seen.lock().unwrap().push(sql);
                    async { Ok(()) }
                },
            )
            .await
            .expect("the column is in the current schema");
        }
        let ran = ran.lock().unwrap();
        assert_eq!(ran.len(), 2);
        assert!(
            ran[0].contains("\"a\"\"b\""),
            "the quote is doubled: {}",
            ran[0]
        );
        assert!(
            ran[1].contains("\"x; DROP TABLE y\""),
            "the semicolon stays inside one identifier: {}",
            ran[1]
        );
    }

    // -----------------------------------------------------------------------
    // 8. An unknown column is a 404 before any statement
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn unknown_column_is_404_and_runs_no_statement() {
        let (_fake, addr) = fake_catalog().await;
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let member = who("ada", &["data-eng"], false);
        let ran = Arc::new(Mutex::new(Vec::new()));

        for column in ["nope", "legacy"] {
            // `legacy` exists in schema 0 but the current schema is 1; `nope` nowhere.
            let seen = Arc::clone(&ran);
            let error = write_column_description(
                &state,
                &catalog,
                &member,
                "sales",
                "orders",
                column,
                ColumnDescriptionUpdate {
                    description: Some(Some("x".into())),
                },
                move |sql| {
                    seen.lock().unwrap().push(sql);
                    async { Ok(()) }
                },
            )
            .await
            .expect_err("not a current-schema column");
            assert_eq!(error.status(), StatusCode::NOT_FOUND);
            assert_eq!(error.message(), "no such column");
        }
        assert!(
            ran.lock().unwrap().is_empty(),
            "no COMMENT ON COLUMN for a column that is not in the current schema"
        );
    }

    // -----------------------------------------------------------------------
    // 9. The interim rule
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn interim_rule_table() {
        let owner = Some("data-eng");
        assert!(may_edit(&admin(), owner).is_ok(), "an administrator");
        assert!(
            may_edit(&who("ada", &["data-eng"], false), owner).is_ok(),
            "a member of the owner group"
        );
        assert!(
            may_edit(&who("ada", &["/data-eng"], false), owner).is_ok(),
            "Keycloak's full group path matches"
        );
        assert_eq!(
            may_edit(&who("bob", &["analytics"], false), owner).expect_err("not the owner group"),
            "only members of the owner group data-eng and administrators may edit this"
        );
        assert!(
            may_edit(&who("bob", &[], false), None).is_ok(),
            "anybody may edit an ownerless table"
        );

        // The write side of the no-owner clause.
        let (fake, addr) = fake_catalog().await;
        let mut no_owner = load_fixture();
        no_owner["metadata"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("stardelt.owner");
        fake.set_load(no_owner);
        let state = test_state();
        let catalog = catalog(&state, &addr);
        let bob = who("bob", &["analytics"], false);

        let error = write_documentation(
            &state,
            &catalog,
            &bob,
            "sales",
            "orders",
            update_body(None, None, Some(Some("data-eng")), None),
        )
        .await
        .expect_err("a non-member cannot claim the table for somebody else");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), "choose an owner: one of your groups");

        let error = write_documentation(
            &state,
            &catalog,
            &bob,
            "sales",
            "orders",
            update_body(Some(Some("new")), None, None, None),
        )
        .await
        .expect_err("an ownerless table must be claimed");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.message(), "choose an owner: one of your groups");
        assert!(fake.snapshot().0.is_empty(), "nothing was committed");

        let doc = write_documentation(
            &state,
            &catalog,
            &bob,
            "sales",
            "orders",
            update_body(None, None, Some(Some("analytics")), None),
        )
        .await
        .expect("the first editor claims the table for their own group");
        assert!(
            doc.owner.is_none(),
            "the fake catalog does not mutate its fixture"
        );

        let (commits, _, _) = fake.snapshot();
        assert_eq!(
            commits[0]["updates"][0]["updates"]["stardelt.owner"],
            json!("analytics")
        );

        // An administrator may choose any valid group name.
        let doc = write_documentation(
            &state,
            &catalog,
            &admin(),
            "sales",
            "orders",
            update_body(None, None, Some(Some("any-group")), None),
        )
        .await;
        assert!(doc.is_ok(), "an administrator claims any group");
    }

    // -----------------------------------------------------------------------
    // 10. canEdit agrees with the rule; the audit line names, never quotes
    // -----------------------------------------------------------------------

    #[test]
    fn can_edit_matches_the_rule() {
        let loaded = {
            let body = load_fixture().to_string();
            parse_loaded(&body).expect("the fixture parses")
        };
        let identities = [
            admin(),
            who("ada", &["data-eng"], false),
            who("bob", &["analytics"], false),
            who("nobody", &[], false),
        ];
        for owner in [Some("data-eng"), None] {
            for identity in &identities {
                let mut loaded = Loaded {
                    properties: loaded.properties.clone(),
                    columns: Vec::new(),
                };
                match owner {
                    Some(owner) => {
                        loaded
                            .properties
                            .insert("stardelt.owner".into(), json!(owner));
                    }
                    None => {
                        loaded.properties.remove("stardelt.owner");
                    }
                }
                let doc = documentation(&loaded, identity);
                assert_eq!(
                    doc.can_edit,
                    may_edit(identity, owner).is_ok(),
                    "canEdit must be the rule for {owner:?}"
                );
                assert!(!doc.edit_rule.is_empty());
            }
        }
    }

    #[derive(Default)]
    struct Captured(String);

    impl tracing::field::Visit for Captured {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }

    struct Capture(Arc<Mutex<String>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Captured::default();
            event.record(&mut fields);
            self.0.lock().unwrap().push_str(&fields.0);
        }
    }

    #[test]
    fn audit_names_not_text() {
        const CANARY: &str = "top-secret-description-text";
        let written = Arc::new(Mutex::new(String::new()));
        let subscriber =
            tracing_subscriber::layer::SubscriberExt::with(tracing_subscriber::registry(), {
                Capture(Arc::clone(&written))
            });
        tracing::subscriber::with_default(subscriber, || {
            audit::catalog_doc_event(
                &who("ada", &["data-eng"], false),
                "sales.orders",
                &["description", "column:id"],
            );
        });
        let line = written.lock().unwrap().clone();
        assert!(
            line.contains("description") && line.contains("column:id"),
            "the audit line names what changed: {line}"
        );
        assert!(
            !line.contains(CANARY),
            "the audit line never carries the text: {line}"
        );
        assert!(line.contains("sales.orders"), "the table is named: {line}");
    }
}
