//! Connections: the rules behind Nova's Connections API (stardelt-nova#108; decisions D23 and D27).
//!
//! A connection is typed settings plus secret fields. The settings and the field *names* are
//! not secret; the values are, and live only in the secret store, written with the caller's
//! own token (`secret_store.rs`). Three scopes exist: a person's own (`personal`: only in the
//! store), an owner group's (`group`) and an administrator-managed one (`managed`, readable by
//! nobody); the last two are also a `Connection` resource that the operator syncs into a
//! Kubernetes Secret.
//!
//! This module holds the decisions as pure functions so they are tested without a cluster or a
//! store: validation, who may do what to which scope, what goes into the store and into the
//! resource, how a rotation merges, and what the API shows. It never sees a secret value except
//! to move it, and nothing here logs. The routes that call it live in `connection_routes.rs`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Identity;
use crate::secret_store::{SecretMeta, StoreScope, valid_connection_name};

/// Longest secret value Nova accepts, in bytes.
pub const MAX_VALUE_BYTES: usize = 4096;
/// Most secret fields and most settings of one connection.
pub const MAX_KEYS: usize = 16;
pub const MAX_SETTINGS: usize = 32;
/// The store keeps settings as custom metadata: a value is at most 512 bytes there.
pub const MAX_SETTING_BYTES: usize = 512;

/// The kinds of connection and what each needs. The operator's `Connection` resource enforces the
/// same table at admission (stardelt-operator `crd.rs`); a personal connection has no resource, so
/// Nova must enforce it itself.
pub const TYPES: [&str; 9] = [
    "postgresql",
    "mysql",
    "sqlserver",
    "oracle",
    "rest-api",
    "s3",
    "kafka",
    "sftp",
    "generic",
];

/// Words that mark a name as a secret. A setting with such a name is refused: it belongs in the
/// secret fields (the same list the operator uses).
const SECRET_WORDS: [&str; 6] = [
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Group,
    Managed,
}

/// `POST /api/connections` body. `secret` holds the values; they are written to the store and
/// never returned.
#[derive(Clone, Deserialize, utoipa::ToSchema)]
pub struct NewConnection {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub scope: Scope,
    #[serde(default, rename = "ownerGroup")]
    pub owner_group: Option<String>,
    #[serde(default, rename = "usableBy")]
    pub usable_by: Vec<String>,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// The secret field values. Write-only: Nova stores them and never reads them back.
    #[schema(write_only)]
    pub secret: BTreeMap<String, String>,
    #[serde(default, rename = "refreshInterval")]
    pub refresh_interval: Option<String>,
}

/// Prints the names of the secret fields, never a value: a request may end up in a log line
/// through an `{:?}` nobody thought about, and that must not leak a password.
impl std::fmt::Debug for NewConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewConnection")
            .field("name", &self.name)
            .field("type", &self.kind)
            .field("scope", &self.scope)
            .field("owner_group", &self.owner_group)
            .field("secret_keys", &self.secret.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// What a caller wants to do with a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Create,
    Rotate,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied(pub String);

fn secret_like(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_WORDS.iter().any(|w| lower.contains(w))
}

fn key_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 63
        && k.as_bytes()[0].is_ascii_alphabetic()
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn setting_name_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 63
        && k.as_bytes()[0].is_ascii_alphabetic()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// A group name: 1 to 64 ASCII letters, digits, dots, dashes or underscores, starting with
/// a letter or digit. Shared with the table-documentation owner field (`catalog_docs.rs`).
pub(crate) fn group_ok(g: &str) -> bool {
    !g.is_empty()
        && g.len() <= 64
        && g.as_bytes()[0].is_ascii_alphanumeric()
        && g.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `1m` to `24h`, written as digits and `m` or `h`.
fn refresh_ok(s: &str) -> bool {
    let (digits, minutes_per_unit) = if let Some(d) = s.strip_suffix('m') {
        (d, 1)
    } else if let Some(d) = s.strip_suffix('h') {
        (d, 60)
    } else {
        return false;
    };
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(n) = digits.parse::<u32>() else {
        return false;
    };
    (1..=1440).contains(&(n * minutes_per_unit))
}

/// Check a new connection. Returns the first problem as a sentence for the caller.
pub fn validate_new(req: &NewConnection) -> Result<(), String> {
    if !valid_connection_name(&req.name) {
        return Err(
            "name must be 1 to 40 lower-case letters, digits and dashes, starting with a letter"
                .into(),
        );
    }
    if !TYPES.contains(&req.kind.as_str()) {
        return Err(format!("type must be one of {}", TYPES.join(", ")));
    }
    match req.scope {
        Scope::Personal => {
            if req.owner_group.is_some() || !req.usable_by.is_empty() {
                return Err("a personal connection has no ownerGroup and no usableBy".into());
            }
        }
        Scope::Group => match req.owner_group.as_deref() {
            Some(g) if group_ok(g) && req.usable_by.is_empty() => {}
            _ => return Err("a group connection needs a valid ownerGroup and no usableBy".into()),
        },
        Scope::Managed => {
            if req.owner_group.is_some()
                || req.usable_by.is_empty()
                || req.usable_by.len() > 20
                || !req.usable_by.iter().all(|g| group_ok(g))
            {
                return Err(
                    "a managed connection needs usableBy (valid group names) and no ownerGroup"
                        .into(),
                );
            }
        }
    }
    if req.config.len() > MAX_SETTINGS {
        return Err(format!("at most {MAX_SETTINGS} settings"));
    }
    for (k, v) in &req.config {
        if !setting_name_ok(k) {
            return Err(format!("setting name `{k}` is not allowed"));
        }
        if secret_like(k) {
            return Err(format!(
                "setting `{k}` looks like a secret: settings go in config, secrets in secret"
            ));
        }
        if v.len() > MAX_SETTING_BYTES {
            return Err(format!(
                "setting `{k}` is longer than {MAX_SETTING_BYTES} bytes"
            ));
        }
    }
    if req.secret.is_empty() || req.secret.len() > MAX_KEYS {
        return Err(format!("secret needs 1 to {MAX_KEYS} fields"));
    }
    for (k, v) in &req.secret {
        if !key_ok(k) {
            return Err(format!(
                "secret field name `{k}` must be letters, digits and underscore, starting with a letter"
            ));
        }
        if v.is_empty() {
            return Err(format!("secret field `{k}` is empty"));
        }
        if v.len() > MAX_VALUE_BYTES {
            return Err(format!(
                "secret field `{k}` is longer than {MAX_VALUE_BYTES} bytes"
            ));
        }
    }
    if let Some(r) = req.refresh_interval.as_deref()
        && !refresh_ok(r)
    {
        return Err("refreshInterval must be between 1m and 24h, for example 15m".into());
    }
    type_rules(
        &req.kind,
        &req.config,
        req.secret.keys().map(String::as_str).collect(),
    )
}

/// What each type needs; the operator's admission rules, written once more.
fn type_rules(
    kind: &str,
    config: &BTreeMap<String, String>,
    keys: BTreeSet<&str>,
) -> Result<(), String> {
    let has_c = |k: &str| config.contains_key(k);
    let has_k = |k: &str| keys.contains(k);
    let ok = |cond: bool, what: &str| if cond { Ok(()) } else { Err(what.to_string()) };
    match kind {
        "postgresql" | "mysql" | "sqlserver" | "oracle" => ok(
            has_c("host") && has_k("user") && has_k("password"),
            "a database connection needs the setting host and the secret fields user and password",
        ),
        "rest-api" => ok(
            has_c("baseUrl") && (has_k("token") || (has_k("clientId") && has_k("clientSecret"))),
            "a rest-api connection needs the setting baseUrl and the secret field token, or clientId and clientSecret",
        ),
        "s3" => ok(
            has_c("bucket") && has_k("accessKeyId") && has_k("secretAccessKey"),
            "an s3 connection needs the setting bucket and the secret fields accessKeyId and secretAccessKey",
        ),
        "sftp" => ok(
            has_c("host") && has_k("user") && (has_k("password") || has_k("privateKey")),
            "an sftp connection needs the setting host and the secret fields user and password or privateKey",
        ),
        "kafka" => ok(
            has_c("bootstrapServers"),
            "a kafka connection needs the setting bootstrapServers",
        ),
        _ => Ok(()),
    }
}

/// Who may do what to which scope, and which part of the store that is (decision D27).
///
/// - `personal`: always the caller's own prefix, for every action.
/// - `group`: a **member** of the group for every action. An administrator who is not a member may
///   only **delete** — destroying a credential is the incident action D27 gives them (the store's
///   `connections-admin` policy allows `delete` on `metadata/connections/*`) — and may not create or
///   rotate one: no planting a value in another group's connections (the store refuses that too;
///   this answers earlier and with a sentence).
/// - `managed`: administrators only.
pub fn authorize(
    who: &Identity,
    scope: Scope,
    owner_group: Option<&str>,
    action: Action,
) -> Result<StoreScope, Denied> {
    match scope {
        Scope::Personal => Ok(StoreScope::Personal {
            sub: who.sub.clone(),
        }),
        Scope::Group => {
            let group = owner_group
                .ok_or_else(|| Denied("a group connection needs an ownerGroup".into()))?;
            let member = who.groups.iter().any(|g| g == group);
            if member || (who.is_admin && action == Action::Delete) {
                Ok(StoreScope::Group {
                    group: group.to_string(),
                })
            } else if who.is_admin {
                Err(Denied(format!(
                    "an administrator may delete a connection of the group `{group}` but not create or rotate one"
                )))
            } else {
                Err(Denied(format!(
                    "you are not a member of the group `{group}`"
                )))
            }
        }
        Scope::Managed => {
            if who.is_admin {
                Ok(StoreScope::Managed)
            } else {
                Err(Denied(
                    "only administrators manage managed connections".into(),
                ))
            }
        }
    }
}

/// The fields for the store and the settings kept beside them. The custom metadata holds the
/// type, the setting values and the **names** of the secret fields: enough to list a connection
/// without reading it (a managed one cannot be read at all).
pub fn store_payload(req: &NewConnection) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let fields = req.secret.clone();
    let mut meta = BTreeMap::new();
    meta.insert("type".to_string(), req.kind.clone());
    meta.insert(
        "keys".to_string(),
        req.secret.keys().cloned().collect::<Vec<_>>().join(","),
    );
    for (k, v) in &req.config {
        meta.insert(format!("config.{k}"), v.clone());
    }
    (fields, meta)
}

/// Merge a rotation into the existing fields. Personal and group connections may rotate some
/// fields (the rest is kept); a managed connection cannot be read, so every field must be sent.
/// A rotation never adds or drops a field.
pub fn merge_rotation(
    scope: Scope,
    existing: Option<&BTreeMap<String, String>>,
    keys: &BTreeSet<String>,
    patch: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if patch.is_empty() {
        return Err("send at least one field to rotate".into());
    }
    for (k, v) in patch {
        if !keys.contains(k) {
            return Err(format!("`{k}` is not a field of this connection"));
        }
        if v.is_empty() || v.len() > MAX_VALUE_BYTES {
            return Err(format!(
                "secret field `{k}` must be 1 to {MAX_VALUE_BYTES} bytes"
            ));
        }
    }
    match (scope, existing) {
        (Scope::Managed, _) => {
            if patch.len() != keys.len() {
                return Err(
                    "a managed connection cannot be read back, so a rotation sends every field"
                        .into(),
                );
            }
            Ok(patch.clone())
        }
        (_, Some(current)) => {
            let mut merged = current.clone();
            merged.extend(patch.iter().map(|(k, v)| (k.clone(), v.clone())));
            Ok(merged)
        }
        (_, None) => Err("the current value could not be read".into()),
    }
}

/// The `Connection` resource for a group or managed connection (personal ones have none). Nova
/// never sets `secret.remoteRef`: the operator derives the path, which is the point.
pub fn connection_resource(req: &NewConnection, namespace: &str) -> Option<Value> {
    let mut spec = json!({
        "scope": match req.scope { Scope::Group => "group", Scope::Managed => "managed", Scope::Personal => return None },
        "type": req.kind,
        "secret": { "keys": req.secret.keys().collect::<Vec<_>>() },
    });
    if let Some(g) = &req.owner_group {
        spec["ownerGroup"] = json!(g);
    }
    if !req.usable_by.is_empty() {
        spec["usableBy"] = json!(req.usable_by);
    }
    if !req.config.is_empty() {
        spec["config"] = json!(req.config);
    }
    if let Some(r) = &req.refresh_interval {
        spec["refreshInterval"] = json!(r);
    }
    Some(json!({
        "apiVersion": "stardelt.io/v1alpha1",
        "kind": "Connection",
        "metadata": {
            "name": req.name,
            "namespace": namespace,
            "labels": { "app.kubernetes.io/part-of": "stardelt", "app.kubernetes.io/managed-by": "stardelt-nova" },
        },
        "spec": spec,
    }))
}

/// The notice an administrator's break-glass read leaves on a group's `Connection` resource and
/// that the listing shows to the group. It never carries a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BreakGlassNotice {
    /// The administrator's display name.
    pub by: String,
    /// RFC 3339 UTC.
    pub at: String,
    pub reason: String,
}

/// One row of `GET /api/connections`. Never carries a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ConnectionView {
    pub name: String,
    pub scope: Scope,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "ownerGroup", skip_serializing_if = "Option::is_none")]
    pub owner_group: Option<String>,
    #[serde(rename = "usableBy", skip_serializing_if = "Vec::is_empty")]
    pub usable_by: Vec<String>,
    pub config: BTreeMap<String, String>,
    /// The names of the secret fields.
    #[serde(rename = "secretKeys")]
    pub secret_keys: Vec<String>,
    #[serde(rename = "lastRotated", skip_serializing_if = "Option::is_none")]
    pub last_rotated: Option<String>,
    pub version: u64,
    /// `Ready` and its reason from the resource's status; absent for a personal connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncView>,
    /// The last break-glass read of a group connection; absent for personal and managed rows.
    #[serde(rename = "breakGlass", skip_serializing_if = "Option::is_none")]
    pub break_glass: Option<BreakGlassNotice>,
    /// Whether the caller belongs to the scope that owns this row: `true` for a personal or managed
    /// connection and for a group connection of one of the caller's groups; `false` for a group
    /// connection an administrator sees only because they are an administrator. `false` means
    /// read-only for this person — no create, no rotation — except delete and break-glass.
    #[serde(rename = "member")]
    pub member: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SyncView {
    pub ready: bool,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Build a view from what the store keeps beside a secret. `member` says whether the caller belongs
/// to the scope (a group row an administrator sees only as an administrator is `false`).
pub fn view_from_store(
    name: &str,
    scope: Scope,
    owner_group: Option<&str>,
    usable_by: Vec<String>,
    meta: &SecretMeta,
    sync: Option<SyncView>,
    member: bool,
) -> ConnectionView {
    let custom = &meta.custom_metadata;
    let config = custom
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("config.")
                .map(|c| (c.to_string(), v.clone()))
        })
        .collect();
    let secret_keys = custom
        .get("keys")
        .map(|k| {
            k.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ConnectionView {
        name: name.to_string(),
        scope,
        kind: custom.get("type").cloned().unwrap_or_default(),
        owner_group: owner_group.map(str::to_string),
        usable_by,
        config,
        secret_keys,
        last_rotated: meta.updated_time.clone(),
        version: meta.version,
        sync,
        break_glass: None,
        member,
    }
}

/// The break-glass notice from a `Connection` resource's annotation; an annotation that does not
/// parse is ignored (a half-written or foreign value must not break the listing).
pub fn break_glass_notice(resource: &Value) -> Option<BreakGlassNotice> {
    let text = resource["metadata"]["annotations"]["stardelt.io/break-glass"].as_str()?;
    serde_json::from_str(text).ok()
}

/// `Ready` and its reason from a `Connection` resource's status.
pub fn sync_from_resource(resource: &Value) -> Option<SyncView> {
    let cond = resource["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == "Ready")?;
    Some(SyncView {
        ready: cond["status"] == "True",
        reason: cond["reason"].as_str().unwrap_or("Unknown").to_string(),
        message: cond["message"]
            .as_str()
            .filter(|m| !m.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(sub: &str, groups: &[&str], admin: bool) -> Identity {
        Identity {
            sub: sub.into(),
            display_name: sub.into(),
            email: None,
            name: None,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            roles: vec![],
            groups_overage: false,
            is_admin: admin,
        }
    }

    fn crm() -> NewConnection {
        serde_json::from_value(json!({
            "name": "crm-db", "type": "sqlserver", "scope": "group", "ownerGroup": "analytics",
            "config": { "host": "crm.example.com", "port": "1433" },
            "secret": { "user": "etl", "password": "p1" }
        }))
        .unwrap()
    }

    fn with(patch: Value) -> NewConnection {
        let mut v = json!({
            "name": "crm-db", "type": "sqlserver", "scope": "group", "ownerGroup": "analytics",
            "config": { "host": "crm.example.com" },
            "secret": { "user": "etl", "password": "p1" }
        });
        for (k, val) in patch.as_object().unwrap() {
            if val.is_null() {
                v.as_object_mut().unwrap().remove(k);
            } else {
                v[k] = val.clone();
            }
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_new_connection_never_prints_its_values() {
        let req = with(json!({ "secret": { "user": "etl", "password": "canary-7f3a" } }));
        let printed = format!("{req:?}");
        assert!(
            !printed.contains("canary-7f3a"),
            "the secret value must not be printed"
        );
        assert!(!printed.contains("etl"), "no field value is printed");
        assert!(
            printed.contains("password") && printed.contains("user"),
            "the field names are fine: {printed}"
        );
        assert!(
            printed.contains("crm-db"),
            "what the request is about stays visible"
        );
    }

    #[test]
    fn the_three_scopes_validate() {
        assert!(validate_new(&crm()).is_ok());
        assert!(validate_new(&with(json!({ "scope": "personal", "ownerGroup": null }))).is_ok());
        let managed = with(
            json!({ "scope": "managed", "ownerGroup": null, "usableBy": ["analytics", "finance"] }),
        );
        assert!(validate_new(&managed).is_ok());
    }

    #[test]
    fn scope_rules_are_enforced() {
        let bad = |patch: Value| validate_new(&with(patch)).unwrap_err();
        assert!(bad(json!({ "ownerGroup": null })).contains("ownerGroup"));
        assert!(bad(json!({ "usableBy": ["finance"] })).contains("usableBy"));
        assert!(bad(json!({ "ownerGroup": "a/b" })).contains("ownerGroup"));
        assert!(bad(json!({ "scope": "personal" })).contains("personal"));
        assert!(bad(json!({ "scope": "managed", "ownerGroup": null })).contains("usableBy"));
        assert!(
            bad(json!({ "scope": "managed", "ownerGroup": null, "usableBy": ["../x"] }))
                .contains("usableBy")
        );
    }

    #[test]
    fn names_types_and_limits_are_checked() {
        let bad = |patch: Value| validate_new(&with(patch)).unwrap_err();
        assert!(bad(json!({ "name": "Crm" })).contains("name"));
        assert!(bad(json!({ "name": "a/b" })).contains("name"));
        assert!(bad(json!({ "type": "ftp" })).contains("type must be one of"));
        assert!(bad(json!({ "secret": {} })).contains("1 to 16"));
        assert!(bad(json!({ "secret": { "user": "u", "password": "" } })).contains("empty"));
        assert!(
            bad(json!({ "secret": { "user": "u", "password": "x".repeat(4097) } }))
                .contains("longer")
        );
        assert!(bad(json!({ "secret": { "user": "u", "pass word": "x" } })).contains("field name"));
        assert!(bad(json!({ "refreshInterval": "90s" })).contains("refreshInterval"));
        assert!(bad(json!({ "config": { "host": "h", "x": "y".repeat(513) } })).contains("longer"));
    }

    #[test]
    fn refresh_interval_edges() {
        for (s, ok) in [
            ("15m", true),
            ("1m", true),
            ("1440m", true),
            ("24h", true),
            ("1h", true),
            ("0024h", true),
            ("0m", false),
            ("1441m", false),
            ("25h", false),
            ("9999h", false),
            ("99999h", false),
            ("90s", false),
            ("", false),
            ("m", false),
            ("h", false),
            ("+5m", false),
            ("-5m", false),
            ("5 m", false),
            ("1m\n", false),
            // multi-byte characters: used to panic, must answer false
            ("1é", false),
            ("é", false),
            ("٣m", false),
        ] {
            assert_eq!(refresh_ok(s), ok, "{s:?}");
        }
    }

    #[test]
    fn a_secret_cannot_hide_in_the_settings() {
        for name in ["dbPassword", "API_TOKEN", "clientSecret", "credentials"] {
            let mut config = serde_json::Map::new();
            config.insert("host".into(), json!("h"));
            config.insert(name.into(), json!("x"));
            let err = validate_new(&with(json!({ "config": config }))).unwrap_err();
            assert!(err.contains("looks like a secret"), "{name}: {err}");
        }
    }

    #[test]
    fn each_type_needs_its_settings_and_fields() {
        let bad = |patch: Value| validate_new(&with(patch)).unwrap_err();
        assert!(bad(json!({ "config": { "port": "1" } })).contains("host"));
        assert!(bad(json!({ "secret": { "user": "u" } })).contains("user and password"));
        assert!(bad(json!({ "type": "rest-api", "config": { "baseUrl": "https://x" }, "secret": { "clientId": "a" } })).contains("rest-api"));
        assert!(bad(json!({ "type": "s3", "config": { "bucket": "b" }, "secret": { "accessKeyId": "a" } })).contains("s3"));
        assert!(
            bad(json!({ "type": "sftp", "config": { "host": "h" }, "secret": { "user": "u" } }))
                .contains("sftp")
        );
        assert!(
            bad(json!({ "type": "kafka", "config": {}, "secret": { "x": "y" } })).contains("kafka")
        );
        for ok in [
            json!({ "type": "rest-api", "config": { "baseUrl": "https://x" }, "secret": { "token": "t" } }),
            json!({ "type": "rest-api", "config": { "baseUrl": "https://x" }, "secret": { "clientId": "a", "clientSecret": "b" } }),
            json!({ "type": "s3", "config": { "bucket": "b" }, "secret": { "accessKeyId": "a", "secretAccessKey": "b" } }),
            json!({ "type": "sftp", "config": { "host": "h" }, "secret": { "user": "u", "privateKey": "k" } }),
            json!({ "type": "generic", "config": {}, "secret": { "anything": "x" } }),
        ] {
            assert!(validate_new(&with(ok)).is_ok());
        }
    }

    #[test]
    fn the_caller_gets_only_what_the_decision_gives_them() {
        let alice = who("sub-alice", &["analytics"], false);
        let admin = who("sub-admin", &["stardelt-admins"], true);
        // personal: always the caller's own prefix
        assert_eq!(
            authorize(&alice, Scope::Personal, None, Action::Create),
            Ok(StoreScope::Personal {
                sub: "sub-alice".into()
            })
        );
        // group: members may do everything; an administrator who is not a member only destroys
        assert_eq!(
            authorize(&alice, Scope::Group, Some("analytics"), Action::Rotate),
            Ok(StoreScope::Group {
                group: "analytics".into()
            })
        );
        assert!(authorize(&alice, Scope::Group, Some("finance"), Action::Create).is_err());
        assert_eq!(
            authorize(&admin, Scope::Group, Some("analytics"), Action::Delete),
            Ok(StoreScope::Group {
                group: "analytics".into()
            })
        );
        for action in [Action::Create, Action::Rotate] {
            let denied = authorize(&admin, Scope::Group, Some("analytics"), action).unwrap_err();
            assert!(
                denied.0.contains("not create or rotate"),
                "{action:?}: {}",
                denied.0
            );
        }
        assert!(authorize(&alice, Scope::Group, None, Action::Create).is_err());
        // managed: administrators only
        assert_eq!(
            authorize(&admin, Scope::Managed, None, Action::Create),
            Ok(StoreScope::Managed)
        );
        assert!(authorize(&alice, Scope::Managed, None, Action::Delete).is_err());
    }

    #[test]
    fn the_store_gets_the_values_and_the_metadata_only_the_names() {
        let (fields, meta) = store_payload(&crm());
        assert_eq!(fields["password"], "p1");
        assert_eq!(meta["type"], "sqlserver");
        assert_eq!(meta["keys"], "password,user");
        assert_eq!(meta["config.host"], "crm.example.com");
        assert!(
            meta.values().all(|v| v != "p1" && v != "etl"),
            "no value in the metadata"
        );
        assert!(!meta.contains_key("password"));
    }

    #[test]
    fn a_rotation_keeps_the_other_fields_except_for_managed_ones() {
        let keys: BTreeSet<String> = ["user", "password"].iter().map(|s| s.to_string()).collect();
        let existing: BTreeMap<String, String> = [("user", "etl"), ("password", "p1")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let patch: BTreeMap<String, String> = [("password".to_string(), "p2".to_string())].into();
        let merged = merge_rotation(Scope::Group, Some(&existing), &keys, &patch).unwrap();
        assert_eq!(
            (merged["user"].as_str(), merged["password"].as_str()),
            ("etl", "p2")
        );
        // a field the connection does not have
        let unknown: BTreeMap<String, String> = [("token".to_string(), "t".to_string())].into();
        assert!(
            merge_rotation(Scope::Group, Some(&existing), &keys, &unknown)
                .unwrap_err()
                .contains("not a field")
        );
        // nothing, or empty
        assert!(merge_rotation(Scope::Group, Some(&existing), &keys, &BTreeMap::new()).is_err());
        let empty: BTreeMap<String, String> = [("password".to_string(), String::new())].into();
        assert!(merge_rotation(Scope::Group, Some(&existing), &keys, &empty).is_err());
        // managed: every field, no reading
        assert!(
            merge_rotation(Scope::Managed, None, &keys, &patch)
                .unwrap_err()
                .contains("every field")
        );
        let all: BTreeMap<String, String> = [("user", "u2"), ("password", "p3")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            merge_rotation(Scope::Managed, None, &keys, &all).unwrap(),
            all
        );
    }

    #[test]
    fn the_resource_has_no_path_no_values_and_none_for_personal() {
        let cr = connection_resource(&crm(), "stardelt").unwrap();
        assert_eq!(cr["kind"], "Connection");
        assert_eq!(cr["metadata"]["namespace"], "stardelt");
        assert_eq!(cr["spec"]["scope"], "group");
        assert_eq!(cr["spec"]["ownerGroup"], "analytics");
        assert_eq!(cr["spec"]["secret"]["keys"], json!(["password", "user"]));
        assert!(cr["spec"]["secret"].get("remoteRef").is_none());
        let text = cr.to_string();
        assert!(
            !text.contains("p1") && !text.contains("etl"),
            "no secret value in the resource"
        );
        let managed =
            with(json!({ "scope": "managed", "ownerGroup": null, "usableBy": ["analytics"] }));
        let cr = connection_resource(&managed, "stardelt").unwrap();
        assert_eq!(cr["spec"]["usableBy"], json!(["analytics"]));
        assert!(cr["spec"].get("ownerGroup").is_none());
        assert!(
            connection_resource(
                &with(json!({ "scope": "personal", "ownerGroup": null })),
                "stardelt"
            )
            .is_none()
        );
    }

    #[test]
    fn a_view_shows_names_and_times_and_never_a_value() {
        let (_, custom) = store_payload(&crm());
        let meta = SecretMeta {
            version: 3,
            created_time: Some("2026-10-19T10:00:00Z".into()),
            updated_time: Some("2026-10-19T11:00:00Z".into()),
            custom_metadata: custom,
        };
        let sync = sync_from_resource(&json!({ "status": { "conditions": [
            { "type": "Ready", "status": "True", "reason": "Synced" }
        ]}}));
        let view = view_from_store(
            "crm-db",
            Scope::Group,
            Some("analytics"),
            vec![],
            &meta,
            sync,
            true,
        );
        assert_eq!(view.kind, "sqlserver");
        assert_eq!(view.secret_keys, ["password", "user"]);
        assert_eq!(view.config["host"], "crm.example.com");
        assert_eq!(view.version, 3);
        assert_eq!(view.sync.as_ref().unwrap().reason, "Synced");
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("p1") && !json.contains("etl"));
        assert!(json.contains("\"lastRotated\":\"2026-10-19T11:00:00Z\""));
        assert!(sync_from_resource(&json!({ "status": {} })).is_none());
    }

    #[test]
    fn member_is_true_for_personal_and_managed_rows() {
        let meta = SecretMeta {
            version: 1,
            created_time: None,
            updated_time: None,
            custom_metadata: BTreeMap::new(),
        };
        for (scope, group) in [
            (Scope::Personal, None),
            (Scope::Group, Some("analytics")),
            (Scope::Managed, None),
        ] {
            let view = view_from_store("crm-db", scope, group, vec![], &meta, None, true);
            assert!(view.member, "{scope:?}");
            let json = serde_json::to_value(&view).unwrap();
            assert_eq!(
                json["member"], true,
                "every row carries the field, named `member`: {json}"
            );
        }
        // a group row an administrator sees only as an administrator is not theirs
        let view = view_from_store(
            "crm-db",
            Scope::Group,
            Some("finance"),
            vec![],
            &meta,
            None,
            false,
        );
        assert!(!view.member);
        assert_eq!(serde_json::to_value(view).unwrap()["member"], false);
    }
}
