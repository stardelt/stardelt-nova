//! Nova's public API description, generated from code (ticket #107).
//!
//! The `#[utoipa::path]` annotations live on the handlers (`main.rs` for
//! `/api/me`, `catalog.rs` for the catalog routes) and the `ToSchema` derives
//! on their request/response types; this module only collects them into one
//! document and serves it. Nothing here is hand-written YAML, so the
//! description cannot drift from the code — the `committed_document_is_fresh`
//! test below fails CI when the checked-in document is stale.
//!
//! Routes carry one of two tags: `public` (stable — tools and scripts may rely
//! on it) or `internal` (UI-only — may change without notice). Only `public`
//! is promised stable. `info.version` is Nova's own crate version and follows
//! semantic versioning for the `public` surface: a breaking change to a
//! `public` route bumps it.
//!
//! `GET /api/openapi.json` is served **unauthenticated** on purpose: the
//! document describes shapes, not data. It contains no user rows, no tokens,
//! no secrets and no upstream URLs — only paths, schema names and prose. The
//! `serves_the_document_unauthenticated` test pins that (status 200 with no
//! session, and no secret-shaped word in the JSON).
//!
//! Regenerate the checked-in document after changing an annotation or schema:
//!
//! ```bash
//! UPDATE_OPENAPI=1 cargo test -p stardelt-nova-backend openapi
//! ```

use axum::{http::header, response::IntoResponse};
use serde::Serialize;
use utoipa::OpenApi;

/// Every non-2xx answer under `/api/` is this envelope (see README "HTTP
/// conventions"), so every error response in the document points here.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    /// Human-readable sentence. Never contains tokens, URLs or stack traces.
    pub error: String,
}

/// The collected document: every path in this first slice, with the schemas
/// the `body = …` attributes point at.
#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Nova API",
        description = "The stardelt Nova gateway: catalog browse, queries and jobs over one identity. In `dev` mode (the default) every route answers without sign-in; in `oidc` mode every described route below requires a session or bearer token except this document itself. Only routes tagged `public` are promised stable."
    ),
    paths(
        crate::me,
        crate::health,
        crate::openapi::serve,
        crate::auth::config,
        crate::catalog::warehouse_info,
        crate::warehouses::list,
        crate::catalog::list_catalogs,
        crate::catalog_metrics::get,
        crate::catalog_metrics::refresh,
        crate::catalog::list_namespaces,
        crate::catalog::create_namespace,
        crate::catalog::list_tables,
        crate::catalog_search::search,
        crate::catalog::table_metadata,
        crate::catalog::table_history,
        crate::catalog::table_rollback,
        crate::catalog_docs::read,
        crate::catalog_docs::update,
        crate::catalog_docs::update_column,
        crate::connection_routes::list,
        crate::connection_routes::create,
        crate::connection_routes::rotate_personal,
        crate::connection_routes::rotate_group,
        crate::connection_routes::rotate_managed,
        crate::connection_routes::delete_personal,
        crate::connection_routes::delete_group,
        crate::connection_routes::delete_managed,
        crate::connection_routes::break_glass_group,
        crate::connection_routes::break_glass_personal,
        crate::connection_routes::break_glass_managed,
        crate::connection_routes::break_glass_log,
        crate::pipelines::list,
        crate::pipelines::principals,
        crate::pipelines::check,
        crate::pipelines::preview,
        crate::pipelines::validate,
        crate::pipelines::create,
        crate::pipelines::detail,
        crate::pipelines::update,
        crate::pipelines::remove,
        crate::pipelines::trigger_run,
        crate::pipelines::pause,
        crate::pipelines::resume,
        crate::pipelines::run,
        crate::pipelines::stop,
        crate::pipelines::logs,
        crate::trino::run_query,
        crate::saved_queries::list,
        crate::saved_queries::create,
        crate::saved_queries::remove,
        crate::query_log_api::list,
        crate::query_log_api::detail,
        crate::user_settings::list,
        crate::user_settings::put,
        crate::user_settings::remove,
        crate::notebooks::publish,
        crate::notebooks::remove,
        crate::notebooks::list,
        crate::notebooks::versions,
        crate::notebooks::parameters,
        crate::statements::submit,
        crate::statements::list,
        crate::statements::poll,
        crate::statements::chunk,
        crate::statements::cancel,
        crate::admin_catalogs::list,
        crate::admin_catalogs::update_access,
    ),
    components(schemas(
        crate::Me,
        crate::OidcMe,
        crate::Health,
        crate::statements::SubmitRequest,
        crate::statements::StatementList,
        crate::statements::StatementListItem,
        crate::statements::StatementView,
        crate::statements::StatementState,
        crate::statements::ChunkView,
        crate::auth::AuthConfig,
        crate::db::DbStatus,
        crate::catalog::WarehouseInfo,
        crate::warehouses::SqlWarehouse,
        crate::catalog::CatalogInfo,
        crate::catalog::CatalogsResponse,
        crate::catalog::RollbackRequest,
        crate::catalog_metrics::CatalogStats,
        crate::catalog_metrics::CatalogMetricsView,
        crate::catalog_metrics::MetricsTotals,
        crate::catalog_metrics::MetricsResponse,
        crate::catalog_metrics::RefreshAccepted,
        crate::catalog::NamespacesResponse,
        crate::catalog_search::SearchMatch,
        crate::catalog_search::SearchResult,
        crate::catalog::TablesResponse,
        crate::catalog::TableIdentifier,
        crate::catalog::TableMetadataResponse,
        crate::catalog::CreateNamespaceRequest,
        crate::catalog::CreateNamespaceResponse,
        crate::catalog_docs::TableDocumentation,
        crate::catalog_docs::ColumnDocumentation,
        crate::catalog_docs::DocumentationUpdate,
        crate::catalog_docs::ColumnDescriptionUpdate,
        crate::connections::NewConnection,
        crate::connections::ConnectionView,
        crate::connections::BreakGlassNotice,
        crate::connections::SyncView,
        crate::connections::Scope,
        crate::connection_routes::RotateRequest,
        crate::connection_routes::ConnectionsResponse,
        crate::connection_routes::BreakGlassRequest,
        crate::connection_routes::BreakGlassResponse,
        crate::connection_routes::BreakGlassLogResponse,
        crate::connection_routes::BreakGlassLogRow,
        crate::pipelines::PipelineView,
        crate::pipelines::PipelineListResponse,
        crate::pipelines::PipelineDetail,
        crate::pipelines::PrincipalsResponse,
        crate::pipelines::PrincipalView,
        crate::pipelines::CreatePipelineRequest,
        crate::pipelines::UpdatePipelineRequest,
        crate::pipelines::ScheduleRequest,
        crate::pipelines::PipelineOptions,
        crate::pipelines::PipelineOptionsView,
        crate::pipelines::CheckStatementRequest,
        crate::pipelines::CheckStatementResponse,
        crate::pipelines::PreviewScheduleRequest,
        crate::pipelines::PreviewScheduleResponse,
        crate::pipelines::ValidatePipelineResponse,
        crate::pipelines::ValidationIssue,
        crate::pipelines::RunView,
        crate::pipelines::RunResponse,
        crate::pipelines::TaskView,
        crate::pipelines::TaskSpecView,
        crate::pipelines::LogView,
        crate::pipelines::LogLine,
        crate::pipelines::RetriesView,
        crate::trino::QueryRequest,
        crate::trino::QueryResult,
        crate::trino::Column,
        crate::saved_queries::SavedQuery,
        crate::saved_queries::NewSavedQuery,
        crate::query_log_api::QueryLogPage,
        crate::query_log_api::QueryLogItem,
        crate::query_log_api::QueryLogDetail,
        crate::query_log_api::QueryTiming,
        crate::query_log_api::QueryData,
        crate::query_log_api::QueryFailure,
        crate::user_settings::StoredSetting,
        crate::notebooks::PublishedNotebook,
        crate::notebooks::DeletedNotebook,
        crate::notebooks::NotebookSummary,
        crate::notebooks::NotebookVersion,
        crate::notebooks::NotebookParameter,
        crate::notebooks::NotebookParameters,
        crate::admin_catalogs::AdminCatalog,
        crate::admin_catalogs::CatalogGrant,
        crate::admin_catalogs::CatalogCondition,
        crate::admin_catalogs::CatalogAccessUpdate,
        ErrorResponse,
    )),
    tags(
        (name = "public", description = "Stable surface for tools and scripts. Breaking changes bump `info.version`."),
        (name = "internal", description = "UI-only surface. May change or disappear without notice; do not build tools on it."),
    )
)]
pub struct ApiDoc;

/// The served document. The version is stamped from the crate version at
/// compile time (see the module docs for the semver promise).
pub fn document() -> utoipa::openapi::OpenApi {
    use utoipa::openapi::security::{
        ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
    };
    let mut doc = ApiDoc::openapi();
    doc.info.version = env!("CARGO_PKG_VERSION").to_string();
    // How a client proves who it is (see auth.rs): either credential works on every
    // route below; generated clients, the CLI and the MCP server read this.
    let components = doc.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
        "bearer",
        SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("JWT")
                .description(Some(
                    "An access token from the platform's identity provider, sent in the `Authorization` header. In `dev` mode no credential is needed.",
                ))
                .build(),
        ),
    );
    components.add_security_scheme(
        "session",
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
            "nova_session",
            "The signed session cookie the browser sign-in sets. Same rules as the bearer token.",
        ))),
    );
    doc.security = Some(vec![
        SecurityRequirement::new("bearer", Vec::<String>::new()),
        SecurityRequirement::new("session", Vec::<String>::new()),
    ]);
    doc
}

/// The exact bytes served at `GET /api/openapi.json` and committed to
/// `openapi/nova.json` (plus one trailing newline in the file).
pub fn document_json() -> String {
    serde_json::to_string_pretty(&document()).expect("the OpenAPI document serializes")
}

/// Serve the document. Deliberately outside sign-in (wired next to
/// `/api/health` in `main`): it carries shapes, not data.
#[utoipa::path(
    get,
    operation_id = "openapi_document",
    path = "/api/openapi.json",
    tag = "public",
    description = "This document. It answers without sign-in and carries shapes, not data.",
    responses(
        (status = 200, description = "The OpenAPI 3.1 document", body = Object),
    )
)]
pub async fn serve() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        document_json(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path of the committed document, relative to the crate root.
    fn committed_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../openapi/nova.json")
    }

    /// The CI check: the committed document must equal what the annotations
    /// generate. Regenerate with `UPDATE_OPENAPI=1 cargo test -p
    /// stardelt-nova-backend openapi` and commit the result alongside the
    /// annotation change.
    #[test]
    fn committed_document_is_fresh() {
        let path = committed_path();
        let generated = format!("{}\n", document_json());
        if std::env::var("UPDATE_OPENAPI").is_ok() {
            std::fs::create_dir_all(path.parent().expect("openapi dir")).expect("openapi dir");
            std::fs::write(&path, &generated).expect("write the regenerated document");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing {} — run UPDATE_OPENAPI=1 cargo test -p stardelt-nova-backend openapi",
                path.display()
            )
        });
        assert_eq!(
            committed, generated,
            "openapi/nova.json is stale — run UPDATE_OPENAPI=1 cargo test -p stardelt-nova-backend openapi and commit the result"
        );
    }

    /// Routes that are served but not in the document yet. This list only
    /// shrinks: document a route (add its `#[utoipa::path]`) and delete its
    /// line here. Adding a route without either fails the test below.
    const UNDOCUMENTED: &[&str] = &[
        "/api/apps",
        "/api/apps/links",
        "/api/apps/{id}",
        "/api/embeds",
        "/api/ingest/upload",
        "/api/jobs",
        "/api/transfer",
        "/api/trino/cluster",
        "/api/workloads",
    ];

    /// The `.route("<path>"` literals inside the router function that starts
    /// at `marker` in `src`, each prefixed with `/api` (where every router is
    /// mounted). The function body ends at the first closing brace in column 0.
    fn routes_in(src: &str, marker: &str) -> Vec<String> {
        let start = src
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} not found"));
        let body = &src[start..];
        let body = &body[..body.find("\n}\n").expect("end of the router function")];
        let mut paths = Vec::new();
        let mut rest = body;
        while let Some(i) = rest.find(".route(") {
            rest = rest[i + ".route(".len()..].trim_start();
            if let Some(quoted) = rest.strip_prefix('"')
                && let Some(end) = quoted.find('"')
            {
                paths.push(format!("/api{}", &quoted[..end]));
            }
        }
        paths
    }

    /// Every route Nova serves under `/api` must be in the OpenAPI document or
    /// in `UNDOCUMENTED`, and `UNDOCUMENTED` may not list a documented or
    /// vanished route. One contract is what the SDKs, the CLI and the MCP
    /// server are generated from (stardelt-nova#229).
    #[test]
    fn every_api_route_is_documented_or_listed() {
        let main_rs = include_str!("main.rs");
        let mut served = Vec::new();
        for marker in ["fn public_router()", "fn api_router()", "fn admin_router()"] {
            served.extend(routes_in(main_rs, marker));
        }
        served.extend(routes_in(include_str!("pipelines.rs"), "fn router()"));
        served.extend(routes_in(include_str!("notebooks.rs"), "fn router()"));
        served.extend(routes_in(include_str!("statements.rs"), "fn router()"));
        assert!(
            served.len() > 40,
            "the scan found only {} routes",
            served.len()
        );

        let value = serde_json::to_value(document()).expect("the document is JSON");
        let documented: Vec<String> = value["paths"]
            .as_object()
            .expect("paths is an object")
            .keys()
            .cloned()
            .collect();

        let mut missing: Vec<&String> = served
            .iter()
            .filter(|p| !documented.contains(p) && !UNDOCUMENTED.contains(&p.as_str()))
            .collect();
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "served but neither documented nor listed in UNDOCUMENTED: {missing:#?}"
        );

        let stale: Vec<&&str> = UNDOCUMENTED
            .iter()
            .filter(|p| documented.iter().any(|d| d == **p) || !served.iter().any(|s| s == **p))
            .collect();
        assert!(
            stale.is_empty(),
            "UNDOCUMENTED lists a route that is now documented or no longer served: {stale:#?}"
        );
    }

    /// A client can tell from the document how to authenticate: two schemes
    /// (bearer token, session cookie) and a global requirement naming both.
    #[test]
    fn security_schemes_are_documented() {
        let value = serde_json::to_value(document()).expect("the document is JSON");
        let schemes = &value["components"]["securitySchemes"];
        assert_eq!(schemes["bearer"]["type"], "http");
        assert_eq!(schemes["bearer"]["scheme"], "bearer");
        assert_eq!(schemes["session"]["type"], "apiKey");
        assert_eq!(schemes["session"]["in"], "cookie");
        assert_eq!(schemes["session"]["name"], "nova_session");
        let required = value["security"]
            .as_array()
            .expect("a global security list");
        assert_eq!(required.len(), 2, "bearer or session");
    }

    /// A generated client names its methods after the `operationId`, so two
    /// operations may not share one. A handler whose function name is generic
    /// (`list`, `create`, `update`, `remove`) sets `operation_id = "<resource>_<verb>"`
    /// in its `#[utoipa::path]`.
    #[test]
    fn operation_ids_are_unique() {
        let value = serde_json::to_value(document()).expect("the document is JSON");
        let mut seen = std::collections::BTreeMap::new();
        let mut clashes = Vec::new();
        for (path, item) in value["paths"].as_object().expect("paths is an object") {
            for (method, op) in item.as_object().expect("a path item is an object") {
                let Some(id) = op["operationId"].as_str() else {
                    continue;
                };
                if let Some(first) = seen.insert(id.to_string(), format!("{method} {path}")) {
                    clashes.push(format!("{id}: {first} and {method} {path}"));
                }
            }
        }
        assert!(clashes.is_empty(), "duplicate operationIds: {clashes:#?}");
    }

    /// The SDKs, the CLI and the MCP server name their methods and tools after the
    /// operation ids, so an id must read as `<resource>_<action>` (`pipelines_get`,
    /// `table_history`), never as a bare verb (`get`, `run`, `logs`) that says nothing
    /// outside its file. `me` and `health` are the two single-word ids.
    #[test]
    fn operation_ids_are_resource_first_snake_case() {
        const BARE_VERBS: &[&str] = &[
            "list", "get", "read", "create", "update", "delete", "remove", "run", "stop", "rotate",
            "break", "trigger", "publish", "check", "preview", "pause", "resume", "refresh",
        ];
        let value = serde_json::to_value(document()).expect("the document is JSON");
        let mut bad = Vec::new();
        for (path, item) in value["paths"].as_object().expect("paths is an object") {
            for (method, op) in item.as_object().expect("a path item is an object") {
                let Some(id) = op["operationId"].as_str() else {
                    continue;
                };
                let words: Vec<&str> = id.split('_').collect();
                let snake = words.iter().all(|w| {
                    !w.is_empty()
                        && w.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                });
                let ok = id == "me"
                    || id == "health"
                    || (snake && words.len() >= 2 && !BARE_VERBS.contains(&words[0]));
                if !ok {
                    bad.push(format!("{id} ({method} {path})"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "operation ids must be <resource>_<action> in snake case: {bad:#?}"
        );
    }

    /// A generated client builds the URL from the declared path parameters, so the
    /// `{name}` parts of a path and its `in: path` parameters must be the same set:
    /// a query parameter documented as a path one becomes a required argument.
    #[test]
    fn path_parameters_match_the_path_template() {
        let value = serde_json::to_value(document()).expect("the document is JSON");
        let mut bad = Vec::new();
        for (path, item) in value["paths"].as_object().expect("paths is an object") {
            let mut in_path: Vec<&str> = path
                .split('/')
                .filter_map(|part| part.strip_prefix('{')?.strip_suffix('}'))
                .collect();
            in_path.sort_unstable();
            for (method, op) in item.as_object().expect("a path item is an object") {
                let mut declared: Vec<&str> = op["parameters"]
                    .as_array()
                    .map(|all| all.iter().filter(|p| p["in"] == "path"))
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["name"].as_str())
                    .collect();
                declared.sort_unstable();
                if declared != in_path {
                    bad.push(format!(
                        "{method} {path}: declared {declared:?}, template {in_path:?}"
                    ));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "path parameters differ from the path: {bad:#?}"
        );
    }

    /// A setting's value is any JSON (a string, a number, a list), so the schema
    /// must not say `object`: a generated client would refuse the rest.
    #[test]
    fn a_setting_value_is_any_json() {
        let value = serde_json::to_value(document()).expect("the document is JSON");
        let body = &value["paths"]["/api/settings/{key}"]["put"]["requestBody"]["content"]["application/json"]
            ["schema"];
        assert!(body.get("type").is_none(), "the PUT body is typed: {body}");
        let stored = &value["components"]["schemas"]["StoredSetting"]["properties"]["value"];
        assert!(
            stored.get("type").is_none(),
            "the stored value is typed: {stored}"
        );
    }

    /// A cell of a query result is any JSON scalar, array or object, so the row
    /// schema must not say `object`: a generated client would refuse the numbers.
    #[test]
    fn a_result_cell_is_any_json() {
        let value = serde_json::to_value(document()).expect("the document is JSON");
        let cell =
            &value["components"]["schemas"]["QueryResult"]["properties"]["rows"]["items"]["items"];
        assert!(cell.get("type").is_none(), "a result cell is typed: {cell}");
    }

    /// The document endpoint answers without a session and carries no data:
    /// no tokens, no cookies, no secrets — only paths, schemas and prose.
    #[test]
    fn document_carries_no_secrets() {
        let json = document_json();
        for word in ["access_token", "refresh_token", "id_token", "Secret"] {
            assert!(
                !json.contains(word),
                "the public document must not mention {word:?}"
            );
        }
        let value: serde_json::Value = serde_json::from_str(&json).expect("the document is JSON");
        assert_eq!(
            value["info"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "the version field follows the crate version"
        );
        for path in [
            "/api/me",
            "/api/catalog/warehouse",
            "/api/catalog/namespaces",
            "/api/catalog/namespaces/{ns}/tables",
            "/api/catalog/tables/{ns}/{tbl}",
        ] {
            assert!(
                value["paths"].get(path).is_some(),
                "the document describes {path}"
            );
        }
    }
}
