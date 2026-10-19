//! Turns the query engine's HTTP event-listener payloads into query-log records
//! (decision D25, stardelt-nova#116). Pure functions, no I/O.
//!
//! Field names and units were checked against real Trino 480 payloads on 2026-10-19:
//! a *created* event has no `statistics`; a *completed* event has `statistics`, an
//! optional `failureInfo` and `ioMetadata`; durations are seconds as floats; timestamps
//! are RFC 3339 strings. A completed event can be several hundred kilobytes (it carries
//! the query plan), so only the fields below are read and everything else is ignored.

use std::borrow::Cow;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Longest SQL text kept in a record, in bytes (cut on a character boundary).
pub const MAX_SQL_BYTES: usize = 100_000;

/// Where a query is in its life, as the query log shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryState {
    Queued,
    Finished,
    Failed,
    Canceled,
}

impl QueryState {
    /// The value stored in the `state` column and returned by the API.
    pub fn as_str(self) -> &'static str {
        match self {
            QueryState::Queued => "QUEUED",
            QueryState::Finished => "FINISHED",
            QueryState::Failed => "FAILED",
            QueryState::Canceled => "CANCELED",
        }
    }
}

/// Numbers of a finished query. Times are milliseconds, sizes are bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryStats {
    pub queued_ms: i64,
    pub planning_ms: i64,
    pub execution_ms: i64,
    pub wall_ms: i64,
    pub cpu_ms: i64,
    pub peak_memory_bytes: i64,
    /// Rows the engine processed after the connector read them (`processedInputRows`).
    pub input_rows: i64,
    /// Bytes read from storage (`physicalInputBytes`).
    pub input_bytes: i64,
    pub output_rows: i64,
    pub output_bytes: i64,
    pub written_rows: i64,
    pub written_bytes: i64,
}

/// One row of the query log, built from one event. A completed event overwrites the
/// row its created event made (same `query_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryLogRecord {
    pub query_id: String,
    pub state: QueryState,
    /// The engine user: the sign-in name of a person or of a service principal.
    pub principal: String,
    pub source: Option<String>,
    pub client_tags: Vec<String>,
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub query_type: Option<String>,
    /// The statement with catalog credentials hidden ([`redact_catalog_secrets`]), cut at
    /// [`MAX_SQL_BYTES`].
    pub sql_text: String,
    /// Lower-case hex SHA-256 of the complete statement after redaction, so the hash of a
    /// catalog statement cannot be used to guess its password.
    pub sql_hash: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    /// `None` for a created event.
    pub stats: Option<QueryStats>,
    /// The engine's error name, for example `TABLE_NOT_FOUND`.
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    /// `catalog.schema.table`
    pub tables_read: Vec<String>,
    pub tables_written: Vec<String>,
}

/// The payload could not be used.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unusable query event: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    create_time: String,
    execution_start_time: Option<String>,
    end_time: Option<String>,
    context: Context,
    metadata: Metadata,
    statistics: Option<Statistics>,
    failure_info: Option<FailureInfo>,
    io_metadata: Option<IoMetadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Context {
    user: String,
    source: Option<String>,
    #[serde(default)]
    client_tags: Vec<String>,
    catalog: Option<String>,
    schema: Option<String>,
    query_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    query_id: String,
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Statistics {
    wall_time: f64,
    cpu_time: f64,
    queued_time: f64,
    planning_time: f64,
    execution_time: f64,
    peak_user_memory_bytes: i64,
    physical_input_bytes: i64,
    processed_input_rows: i64,
    output_bytes: i64,
    output_rows: i64,
    written_bytes: i64,
    written_rows: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FailureInfo {
    error_code: Option<ErrorCode>,
    failure_message: Option<String>,
}

#[derive(Deserialize)]
struct ErrorCode {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IoMetadata {
    #[serde(default)]
    inputs: Vec<TableRef>,
    output: Option<TableRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableRef {
    catalog_name: String,
    schema: String,
    table: String,
}

impl TableRef {
    fn dotted(&self) -> String {
        format!("{}.{}.{}", self.catalog_name, self.schema, self.table)
    }
}

fn ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// Cut `sql` to at most `max` bytes without splitting a character.
fn cut(sql: &str, max: usize) -> String {
    if sql.len() <= max {
        return sql.to_string();
    }
    let mut end = max;
    while !sql.is_char_boundary(end) {
        end -= 1;
    }
    sql[..end].to_string()
}

fn hex_sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Words that mark a catalog property as holding a credential (compared in lower case,
/// as a substring of the property name: `connection-password`, `s3.aws-secret-key`,
/// `iceberg.rest-catalog.oauth2.token`, `gcs.json-key`).
const SECRET_NAME_PARTS: [&str; 8] = [
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "key",
    "jaas",
];

/// What replaces the value of a credential property in the stored statement.
const REDACTED: &str = "'***'";

/// Hide the credentials a catalog statement carries.
///
/// The engine logs a complete `CREATE CATALOG` / `ALTER CATALOG` statement with its
/// properties, passwords included. Users never write these statements (catalogs come
/// from connections, D27); an administrator who does must not leave the password in a
/// log that others can read. In such a statement the string literal after
/// `<name> =` is replaced by `'***'` when the name contains one of
/// [`SECRET_NAME_PARTS`]; a literal that is only a reference (`'${ENV:DB_PASSWORD}'`)
/// stays. Every other statement is returned unchanged: a `WHERE password = 'x'` in a
/// query is data, not configuration. Comments are skipped, not scanned.
pub fn redact_catalog_secrets(sql: &str) -> Cow<'_, str> {
    // Tokens that matter: names (unquoted words and "quoted" identifiers, lower case),
    // single-quoted literals (byte range), and a lone `=`. Everything else is `Other`.
    enum Tok {
        Name(String),
        /// Start and end of the whole literal, and the end of its text (before a closing quote).
        Literal(usize, usize, usize),
        Equals,
        Other,
    }

    let bytes = sql.as_bytes();
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else if c == b'\'' || c == b'"' {
            let start = i;
            let mut closed = false;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == c {
                    if bytes.get(i + 1) == Some(&c) {
                        i += 2; // a doubled quote is an escaped quote
                        continue;
                    }
                    closed = true;
                    break;
                }
                i += 1;
            }
            // `inner_end` is where the text stops: before the closing quote, or at the end
            // of an unterminated one. Both are character boundaries.
            let inner_end = i.min(bytes.len());
            if closed {
                i += 1;
            }
            if c == b'\'' {
                toks.push(Tok::Literal(start, i, inner_end));
            } else {
                let name = sql[start + 1..inner_end].replace("\"\"", "\"");
                toks.push(Tok::Name(name.to_ascii_lowercase()));
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            toks.push(Tok::Name(sql[start..i].to_ascii_lowercase()));
        } else if matches!(c, b'<' | b'>' | b'!') && bytes.get(i + 1) == Some(&b'=') {
            toks.push(Tok::Other);
            i += 2;
        } else if c == b'=' {
            toks.push(Tok::Equals);
            i += 1;
        } else {
            toks.push(Tok::Other);
            i += 1;
        }
    }

    let is_catalog_ddl = matches!(
        (toks.first(), toks.get(1), toks.get(2)),
        (Some(Tok::Name(a)), Some(Tok::Name(b)), _)
            if (a == "create" || a == "alter") && b == "catalog"
    );
    if !is_catalog_ddl {
        return Cow::Borrowed(sql);
    }

    let mut hidden: Vec<(usize, usize)> = Vec::new();
    for w in toks.windows(3) {
        if let [Tok::Name(name), Tok::Equals, Tok::Literal(a, b, inner_end)] = w {
            let value = &sql[*a + 1..*inner_end];
            let is_reference = value.starts_with("${") && value.ends_with('}');
            if !is_reference && SECRET_NAME_PARTS.iter().any(|part| name.contains(part)) {
                hidden.push((*a, *b));
            }
        }
    }
    if hidden.is_empty() {
        return Cow::Borrowed(sql);
    }
    let mut out = String::with_capacity(sql.len());
    let mut at = 0;
    for (a, b) in hidden {
        out.push_str(&sql[at..a]);
        out.push_str(REDACTED);
        at = b;
    }
    out.push_str(&sql[at..]);
    Cow::Owned(out)
}

/// Parse the body of one `POST` from the engine's event listener.
pub fn parse(body: &[u8]) -> Result<QueryLogRecord, ParseError> {
    let event: Event = serde_json::from_slice(body).map_err(|e| ParseError(e.to_string()))?;
    if event.metadata.query_id.is_empty() || event.context.user.is_empty() {
        return Err(ParseError("empty query id or user".to_string()));
    }

    let error_name = event
        .failure_info
        .as_ref()
        .and_then(|f| f.error_code.as_ref())
        .map(|c| c.name.clone());
    let state = match (
        &event.statistics,
        &event.failure_info,
        error_name.as_deref(),
    ) {
        (None, _, _) => QueryState::Queued,
        (Some(_), Some(_), Some("USER_CANCELED")) => QueryState::Canceled,
        (Some(_), Some(_), _) => QueryState::Failed,
        (Some(_), None, _) => QueryState::Finished,
    };

    let stats = event.statistics.as_ref().map(|s| QueryStats {
        queued_ms: ms(s.queued_time),
        planning_ms: ms(s.planning_time),
        execution_ms: ms(s.execution_time),
        wall_ms: ms(s.wall_time),
        cpu_ms: ms(s.cpu_time),
        peak_memory_bytes: s.peak_user_memory_bytes,
        input_rows: s.processed_input_rows,
        input_bytes: s.physical_input_bytes,
        output_rows: s.output_rows,
        output_bytes: s.output_bytes,
        written_rows: s.written_rows,
        written_bytes: s.written_bytes,
    });

    let io = event.io_metadata.unwrap_or_default();
    let sql = redact_catalog_secrets(&event.metadata.query);
    Ok(QueryLogRecord {
        query_id: event.metadata.query_id,
        state,
        principal: event.context.user,
        source: event.context.source.filter(|s| !s.is_empty()),
        client_tags: event.context.client_tags,
        catalog: event.context.catalog,
        schema: event.context.schema,
        query_type: event.context.query_type,
        sql_hash: hex_sha256(&sql),
        sql_text: cut(&sql, MAX_SQL_BYTES),
        created_at: event.create_time,
        started_at: event.execution_start_time,
        ended_at: event.end_time,
        stats,
        error_code: error_name,
        error_message: event.failure_info.and_then(|f| f.failure_message),
        tables_read: io.inputs.iter().map(TableRef::dotted).collect(),
        tables_written: io.output.iter().map(TableRef::dotted).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATED: &str = include_str!("testdata/query_log/created.json");
    const SELECT: &str = include_str!("testdata/query_log/completed_select.json");
    const FAILED: &str = include_str!("testdata/query_log/completed_failed.json");
    const CANCELED: &str = include_str!("testdata/query_log/completed_canceled.json");
    const CTAS: &str = include_str!("testdata/query_log/completed_ctas.json");
    const CATALOG: &str = include_str!("testdata/query_log/completed_catalog.json");

    #[test]
    fn a_created_event_is_a_queued_row_without_numbers() {
        let r = parse(CREATED.as_bytes()).unwrap();
        assert_eq!(r.state, QueryState::Queued);
        assert_eq!(r.principal, "alice");
        assert_eq!(r.source.as_deref(), Some("nova-sql-editor"));
        assert_eq!(r.client_tags, ["nova", "warehouse-default"]);
        assert!(r.stats.is_none() && r.ended_at.is_none());
        assert!(r.tables_read.is_empty());
    }

    #[test]
    fn a_finished_select_carries_numbers_and_the_tables_it_read() {
        let r = parse(SELECT.as_bytes()).unwrap();
        assert_eq!(r.state, QueryState::Finished);
        assert_eq!(r.query_type.as_deref(), Some("SELECT"));
        assert_eq!(r.tables_read, ["tpch.tiny.nation"]);
        assert!(r.tables_written.is_empty());
        let s = r.stats.unwrap();
        assert_eq!(s.wall_ms, 2324);
        assert_eq!(s.cpu_ms, 1092);
        assert_eq!(s.input_rows, 25);
        assert_eq!(s.output_rows, 1);
        assert_eq!(s.peak_memory_bytes, 448);
        assert!(r.error_code.is_none());
    }

    #[test]
    fn a_failed_query_keeps_the_engine_error() {
        let r = parse(FAILED.as_bytes()).unwrap();
        assert_eq!(r.state, QueryState::Failed);
        assert_eq!(r.principal, "bob");
        assert_eq!(r.error_code.as_deref(), Some("TABLE_NOT_FOUND"));
        assert!(r.error_message.unwrap().contains("does not exist"));
    }

    #[test]
    fn a_user_cancel_is_canceled_not_failed() {
        let r = parse(CANCELED.as_bytes()).unwrap();
        assert_eq!(r.state, QueryState::Canceled);
        assert_eq!(r.error_code.as_deref(), Some("USER_CANCELED"));
    }

    #[test]
    fn a_create_table_as_select_reports_what_it_wrote() {
        let r = parse(CTAS.as_bytes()).unwrap();
        assert_eq!(r.tables_read, ["tpch.tiny.orders"]);
        assert_eq!(r.tables_written, ["memory.s1.t"]);
        let s = r.stats.unwrap();
        assert_eq!(s.written_rows, 15000);
        assert!(s.written_bytes > 0);
    }

    #[test]
    fn the_session_catalog_and_schema_are_kept() {
        let r = parse(CATALOG.as_bytes()).unwrap();
        assert_eq!(r.catalog.as_deref(), Some("tpch"));
        assert_eq!(r.schema.as_deref(), Some("tiny"));
    }

    #[test]
    fn the_hash_covers_the_whole_statement_and_the_text_is_cut_on_a_character() {
        let long = format!("select '{}'", "é".repeat(MAX_SQL_BYTES));
        let body = serde_json::json!({
            "createTime": "2026-10-19T11:33:25.058999857Z",
            "context": { "user": "alice" },
            "metadata": { "queryId": "q1", "query": long }
        })
        .to_string();
        let r = parse(body.as_bytes()).unwrap();
        assert!(r.sql_text.len() <= MAX_SQL_BYTES);
        assert!(
            long.starts_with(&r.sql_text),
            "the cut keeps a valid prefix"
        );
        assert_eq!(r.sql_hash, hex_sha256(&long));
        assert_eq!(r.sql_hash.len(), 64);
    }

    #[test]
    fn the_hash_is_a_lower_case_sha256() {
        assert_eq!(
            hex_sha256("select 1"),
            "822ae07d4783158bc1912bb623e5107cc9002d519e1143a9c200ed6ee18b6d0f"
        );
    }

    #[test]
    fn unusable_bodies_are_errors_not_panics() {
        assert!(parse(b"not json").is_err());
        assert!(parse(b"{}").is_err());
        let no_user =
            br#"{"createTime":"t","context":{"user":""},"metadata":{"queryId":"q","query":"x"}}"#;
        assert_eq!(
            parse(no_user),
            Err(ParseError("empty query id or user".to_string()))
        );
    }

    #[test]
    fn a_catalog_statement_loses_its_credentials_and_keeps_the_rest() {
        let sql = r#"CREATE CATALOG crm USING postgresql WITH ("connection-url" = 'jdbc:postgresql://db/crm', "connection-user" = 'etl', "connection-password" = 'p''w', "s3.aws-secret-key" = 'k')"#;
        let out = redact_catalog_secrets(sql);
        assert_eq!(
            out,
            r#"CREATE CATALOG crm USING postgresql WITH ("connection-url" = 'jdbc:postgresql://db/crm', "connection-user" = 'etl', "connection-password" = '***', "s3.aws-secret-key" = '***')"#
        );
    }

    #[test]
    fn alter_catalog_unquoted_names_and_leading_comments_are_covered() {
        let sql = "-- rotate\n  /* by hand */ alter Catalog crm SET PROPERTIES password = 'x', \"Oauth2.Token\" = 'abc'";
        let out = redact_catalog_secrets(sql);
        assert!(out.contains("password = '***'") && out.contains("\"Oauth2.Token\" = '***'"));
        assert!(out.starts_with("-- rotate\n  /* by hand */ alter Catalog"));
        assert!(!out.contains("'x'") && !out.contains("abc"));
    }

    #[test]
    fn a_reference_to_a_secret_is_not_a_secret() {
        let sql = r#"CREATE CATALOG crm USING postgresql WITH ("connection-password" = '${ENV:DB_PASSWORD}')"#;
        assert!(matches!(redact_catalog_secrets(sql), Cow::Borrowed(_)));
    }

    #[test]
    fn other_statements_are_never_changed() {
        for sql in [
            "SELECT * FROM users WHERE password = 'hunter2'",
            "SET SESSION crm.token = 'abc'",
            "CREATE TABLE t (a int) WITH (secret = 'x')",
            "SELECT 'CREATE CATALOG x WITH (password = ''y'')'",
        ] {
            assert!(
                matches!(redact_catalog_secrets(sql), Cow::Borrowed(s) if s == sql),
                "{sql}"
            );
        }
    }

    #[test]
    fn comparisons_and_unterminated_text_do_not_confuse_the_scanner() {
        let out = redact_catalog_secrets("CREATE CATALOG c USING x WITH (a >= 'k', token = 'v'");
        assert_eq!(
            out,
            "CREATE CATALOG c USING x WITH (a >= 'k', token = '***'"
        );
        let cut_short = redact_catalog_secrets("CREATE CATALOG c USING x WITH (token = 'v");
        assert_eq!(cut_short, "CREATE CATALOG c USING x WITH (token = '***'");
    }

    #[test]
    fn the_stored_text_and_the_hash_follow_the_redacted_statement() {
        let sql = r#"CREATE CATALOG crm USING postgresql WITH ("connection-password" = 'hunter2')"#;
        let body = serde_json::json!({
            "createTime": "2026-10-19T11:33:25.058999857Z",
            "context": { "user": "admin" },
            "metadata": { "queryId": "q2", "query": sql }
        })
        .to_string();
        let r = parse(body.as_bytes()).unwrap();
        assert!(!r.sql_text.contains("hunter2"));
        assert!(r.sql_text.contains("'***'"));
        assert_eq!(r.sql_hash, hex_sha256(&r.sql_text));
        assert_ne!(r.sql_hash, hex_sha256(sql));
    }

    #[test]
    fn no_prefix_of_a_statement_makes_the_scanner_panic() {
        let sql = "CREATE CATALOG c\u{e9} USING x WITH (\"pass\u{e9}word\" = 'v\u{e9}''\u{e9}', token = '\u{e9}') /* \u{e9} */ -- \u{e9}";
        for (end, _) in sql.char_indices().chain(std::iter::once((sql.len(), ' '))) {
            let _ = redact_catalog_secrets(&sql[..end]);
        }
        let out = redact_catalog_secrets("CREATE CATALOG c USING x WITH (token = '\u{e9}");
        assert_eq!(out, "CREATE CATALOG c USING x WITH (token = '***'");
    }
}
