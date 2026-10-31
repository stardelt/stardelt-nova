//! Catalog search, first slice (stardelt-nova#104): find tables by name.
//!
//!   GET /api/catalog/search?q=trips&catalog=bronze&limit=20
//!
//! A person or an agent that does not know where a table lives should not have to walk every
//! namespace by hand. This lists one catalog's namespaces and their tables (the same Lakekeeper
//! calls `namespaces_list` and `tables_list` make) and keeps the tables whose `namespace.table`
//! contains every word of `q`, in any case. It reads names only: comments, tags and owners need the
//! state-store index of the design and come in a later slice. A catalog the caller may not browse
//! is refused exactly as the other catalog routes refuse it.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use crate::catalog::{catalog_scope, lakekeeper_authorization, path_segment};
use crate::{AppState, auth};

/// The longest query.
const MAX_QUERY_CHARS: usize = 100;
/// Matches returned when `limit` is not given, and the most a request may ask for.
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
/// Namespaces read per request; past this the answer says `truncated`.
const MAX_NAMESPACES: usize = 100;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// Words to look for in `namespace.table`, separated by spaces; every word must be there. 1 to 100 characters.
    pub q: Option<String>,
    /// Catalog to search (`warehouse`, `finance`, …). Omitted, the default catalog is used.
    pub catalog: Option<String>,
    /// Most matches to return, 1 to 100; 20 when omitted.
    pub limit: Option<usize>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchMatch {
    pub catalog: String,
    pub namespace: String,
    pub table: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchResult {
    /// The words searched, trimmed.
    pub query: String,
    /// Matches in order of namespace, then table.
    pub matches: Vec<SearchMatch>,
    /// True when more tables match than were returned, or when the catalog has more namespaces than one
    /// request reads (100): the answer is then a prefix, so narrow the words or name the catalog.
    pub truncated: bool,
}

fn sentence(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// One Lakekeeper GET with Nova's own catalog token, as JSON.
// The `Err` is the ready-made refusal the handler returns, as in `catalog::catalog_scope`.
#[allow(clippy::result_large_err)]
async fn get_json(state: &AppState, path: &str) -> Result<Value, Response> {
    let mut request = state.http.get(format!("{}{}", state.lakekeeper_url, path));
    match lakekeeper_authorization(state).await {
        Ok(Some(header)) => request = request.header(reqwest::header::AUTHORIZATION, header),
        Ok(None) => {}
        Err(e) => {
            warn!("catalog service token unavailable: {e}");
            return Err(sentence(StatusCode::BAD_GATEWAY, &e.to_string()));
        }
    }
    let unreachable = || {
        sentence(
            StatusCode::BAD_GATEWAY,
            "the upstream service is not reachable",
        )
    };
    let response = request.send().await.map_err(|e| {
        warn!("lakekeeper search error: {e}");
        unreachable()
    })?;
    if !response.status().is_success() {
        return Err(sentence(
            StatusCode::BAD_GATEWAY,
            "the catalog answered with an error",
        ));
    }
    response.json::<Value>().await.map_err(|_| unreachable())
}

/// The single-level namespaces of an Iceberg `ListNamespaces` answer, sorted.
fn namespaces_of(answer: &Value) -> Vec<String> {
    let mut found: Vec<String> = answer["namespaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|levels| match levels.as_array()?.as_slice() {
            [only] => only.as_str().map(str::to_string),
            _ => None,
        })
        .collect();
    found.sort();
    found
}

/// The table names of an Iceberg `ListTables` answer, sorted.
fn tables_of(answer: &Value) -> Vec<String> {
    let mut found: Vec<String> = answer["identifiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id["name"].as_str().map(str::to_string))
        .collect();
    found.sort();
    found
}

fn matches_words(namespace: &str, table: &str, words: &[String]) -> bool {
    let name = format!("{namespace}.{table}").to_lowercase();
    words.iter().all(|word| name.contains(word.as_str()))
}

#[utoipa::path(
    get,
    operation_id = "catalog_search",
    path = "/api/catalog/search",
    tag = "public",
    description = "Find tables by name in one catalog. `q` is one or more words; a table matches when its `namespace.table` contains every word, in any case. The catalog's namespaces and tables are listed the way `namespaces_list` and `tables_list` list them, so the answer shows only what the caller may browse; only names are searched (single-level namespaces), not comments or tags. `?catalog=` picks the catalog; omitted, the default one is used. At most 100 namespaces are read per request: `truncated` says when the answer is a prefix.",
    params(SearchParams),
    responses(
        (status = 200, description = "The matching tables", body = SearchResult),
        (status = 400, description = "`q` is missing or longer than 100 characters, or the catalog name is invalid", body = crate::openapi::ErrorResponse),
        (status = 403, description = "The caller may not browse that catalog", body = crate::openapi::ErrorResponse),
        (status = 404, description = "No such catalog", body = crate::openapi::ErrorResponse),
        (status = 502, description = "The catalog or the engine is not reachable", body = crate::openapi::ErrorResponse),
    )
)]
pub async fn search(
    State(s): State<Arc<AppState>>,
    Query(params): Query<SearchParams>,
    caller: Option<axum::Extension<auth::Principal>>,
) -> Response {
    let query = params
        .q
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        return sentence(
            StatusCode::BAD_REQUEST,
            "q is required: one or more words, 1 to 100 characters",
        );
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

    let caller = caller.as_ref().map(|c| &c.0);
    let (catalog, id) = match catalog_scope(&s, caller, params.catalog.as_deref()).await {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let namespaces = if s.demo {
        let demo = s.demo_state.read().expect("demo state poisoned");
        namespaces_of(&demo.namespaces_json())
    } else {
        match get_json(&s, &format!("/catalog/v1/{id}/namespaces")).await {
            Ok(answer) => namespaces_of(&answer),
            Err(response) => return response,
        }
    };
    let mut truncated = namespaces.len() > MAX_NAMESPACES;

    let mut matches = Vec::new();
    'scan: for namespace in namespaces.iter().take(MAX_NAMESPACES) {
        let tables = if s.demo {
            let demo = s.demo_state.read().expect("demo state poisoned");
            tables_of(&demo.tables_json(namespace))
        } else {
            let Some(segment) = path_segment(namespace) else {
                continue;
            };
            match get_json(&s, &format!("/catalog/v1/{id}/namespaces/{segment}/tables")).await {
                Ok(answer) => tables_of(&answer),
                Err(response) => return response,
            }
        };
        for table in tables {
            if !matches_words(namespace, &table, &words) {
                continue;
            }
            if matches.len() == limit {
                truncated = true;
                break 'scan;
            }
            matches.push(SearchMatch {
                catalog: catalog.clone(),
                namespace: namespace.clone(),
                table,
            });
        }
    }

    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(SearchResult {
            query,
            matches,
            truncated,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::{Router, extract::Path};
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    /// A pretend Lakekeeper: namespaces and their tables.
    struct FakeCatalog {
        namespaces: Vec<Vec<String>>,
        tables: BTreeMap<String, Vec<String>>,
        fail: bool,
        asked: std::sync::Mutex<Vec<String>>,
    }

    async fn serve(namespaces: &[(&str, &[&str])], fail: bool) -> (String, Arc<FakeCatalog>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let fake = Arc::new(FakeCatalog {
            namespaces: namespaces
                .iter()
                .map(|(n, _)| vec![n.to_string()])
                .collect(),
            tables: namespaces
                .iter()
                .map(|(n, t)| (n.to_string(), t.iter().map(|x| x.to_string()).collect()))
                .collect(),
            fail,
            asked: std::sync::Mutex::new(Vec::new()),
        });
        let app =
            Router::new()
                .route(
                    "/catalog/v1/{id}/namespaces",
                    get(|State(f): State<Arc<FakeCatalog>>| async move {
                        f.asked.lock().unwrap().push("namespaces".into());
                        if f.fail {
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                        Json(json!({ "namespaces": f.namespaces })).into_response()
                    }),
                )
                .route(
                    "/catalog/v1/{id}/namespaces/{ns}/tables",
                    get(
                        |State(f): State<Arc<FakeCatalog>>,
                         Path((_, ns)): Path<(String, String)>| async move {
                            f.asked.lock().unwrap().push(ns.clone());
                            let ids: Vec<Value> = f
                                .tables
                                .get(&ns)
                                .into_iter()
                                .flatten()
                                .map(|t| json!({ "namespace": [ns.clone()], "name": t }))
                                .collect();
                            Json(json!({ "identifiers": ids }))
                        },
                    ),
                )
                .with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, fake)
    }

    fn app(lakekeeper: &str) -> Router {
        let state = crate::router_tests::test_state_with_lakekeeper(lakekeeper);
        {
            let mut ids = state.catalog_ids.write().unwrap();
            ids.insert("warehouse".into(), "w1".into());
            ids.insert("finance".into(), "w2".into());
        }
        Router::new()
            .route("/api/catalog/search", get(search))
            .with_state(state)
    }

    async fn call(app: &Router, uri: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    const LAYOUT: &[(&str, &[&str])] = &[
        ("demo", &["trips", "zones", "trip_fares"]),
        ("finance_raw", &["invoices", "Trips_by_Month"]),
        ("ops", &["tickets"]),
    ];

    #[tokio::test]
    async fn every_word_must_be_in_namespace_dot_table_in_any_case() {
        let (base, _) = serve(LAYOUT, false).await;
        let app = app(&base);
        let (status, body) = call(&app, "/api/catalog/search?q=TRIP").await;
        assert_eq!(status, StatusCode::OK);
        let found: Vec<String> = body["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                format!(
                    "{}.{}",
                    m["namespace"].as_str().unwrap(),
                    m["table"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                "demo.trip_fares",
                "demo.trips",
                "finance_raw.Trips_by_Month"
            ]
        );
        assert_eq!(
            (body["query"].clone(), body["truncated"].clone()),
            (json!("TRIP"), json!(false))
        );
        assert_eq!(body["matches"][0]["catalog"], "warehouse");
        let (_, both) = call(&app, "/api/catalog/search?q=finance%20trips").await;
        assert_eq!(both["matches"].as_array().unwrap().len(), 1);
        assert_eq!(both["matches"][0]["table"], "Trips_by_Month");
        let (_, none) = call(&app, "/api/catalog/search?q=nothing").await;
        assert_eq!(none["matches"], json!([]));
    }

    #[tokio::test]
    async fn the_limit_cuts_the_list_and_says_so() {
        let (base, _) = serve(LAYOUT, false).await;
        let app = app(&base);
        let (_, body) = call(&app, "/api/catalog/search?q=trip&limit=2").await;
        assert_eq!(body["matches"].as_array().unwrap().len(), 2);
        assert_eq!(body["truncated"], true);
        let (_, exact) = call(&app, "/api/catalog/search?q=trip&limit=3").await;
        assert_eq!(
            (
                exact["matches"].as_array().unwrap().len(),
                exact["truncated"].clone()
            ),
            (3, json!(false))
        );
    }

    #[tokio::test]
    async fn a_missing_or_too_long_query_is_a_400_and_asks_nothing() {
        let (base, fake) = serve(LAYOUT, false).await;
        let app = app(&base);
        for uri in [
            "/api/catalog/search",
            "/api/catalog/search?q=",
            "/api/catalog/search?q=%20%20",
        ] {
            assert_eq!(call(&app, uri).await.0, StatusCode::BAD_REQUEST, "{uri}");
        }
        let long = "x".repeat(MAX_QUERY_CHARS + 1);
        assert_eq!(
            call(&app, &format!("/api/catalog/search?q={long}")).await.0,
            StatusCode::BAD_REQUEST
        );
        assert!(fake.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_catalog_the_caller_may_not_browse_is_403_and_never_asked() {
        let (base, fake) = serve(LAYOUT, false).await;
        let app = app(&base);
        let (status, body) = call(&app, "/api/catalog/search?q=trip&catalog=finance").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(fake.asked.lock().unwrap().is_empty());
        assert_eq!(
            call(&app, "/api/catalog/search?q=trip&catalog=Bad_Name")
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn a_catalog_that_fails_is_a_502() {
        let (base, _) = serve(LAYOUT, true).await;
        let (status, body) = call(&app(&base), "/api/catalog/search?q=trip").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "the catalog answered with an error");
        let (status, _) = call(&app("http://127.0.0.1:1"), "/api/catalog/search?q=trip").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn only_a_hundred_namespaces_are_read_and_the_answer_says_it_is_a_prefix() {
        let names: Vec<String> = (0..MAX_NAMESPACES + 5)
            .map(|i| format!("ns{i:03}"))
            .collect();
        let layout: Vec<(&str, &[&str])> = names.iter().map(|n| (n.as_str(), &["t"][..])).collect();
        let (base, fake) = serve(&layout, false).await;
        let (_, body) = call(&app(&base), "/api/catalog/search?q=t&limit=100").await;
        assert_eq!(body["truncated"], true);
        assert_eq!(fake.asked.lock().unwrap().len(), 1 + MAX_NAMESPACES);
    }

    #[tokio::test]
    async fn a_namespace_with_levels_is_skipped_and_a_namespace_name_is_escaped_in_the_path() {
        let answer = json!({ "namespaces": [["a", "b"], ["ok"], ["with space"]] });
        assert_eq!(namespaces_of(&answer), ["ok", "with space"]);
        assert_eq!(path_segment("with space").as_deref(), Some("with%20space"));
        assert!(matches_words(
            "ok",
            "Trips",
            &["trip".to_string(), "ok".to_string()]
        ));
        assert!(!matches_words("ok", "Trips", &["zone".to_string()]));
    }
}
