//! Demo lakehouse. `NOVA_DEMO=1` serves an in-memory Iceberg-shaped catalog and a
//! tiny SQL responder, so the UI can be developed and demoed with no cluster.

use serde_json::{Value, json};

use crate::trino::{Column, QueryResult};

#[derive(Clone)]
pub struct DemoTable {
    pub namespace: String,
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Default)]
pub struct DemoState {
    pub tables: Vec<DemoTable>,
    /// The one catalog's administrator view after an edit through the dialog (#253).
    /// `None` is the seeded `warehouse` row; there is no cluster and no sync in demo mode.
    pub admin_catalog: Option<crate::admin_catalogs::AdminCatalog>,
    /// When the in-memory lakehouse last changed: the seed, or the last upload or
    /// transfer. The Overview reads it as "last change" (`catalog_metrics.rs`); there are
    /// no snapshots in memory, so there is no per-table commit time.
    pub last_write_ms: Option<i64>,
}

fn col(name: &str, ty: &str) -> Column {
    Column {
        name: name.into(),
        r#type: ty.into(),
    }
}

fn s(v: &str) -> Value {
    json!(v)
}
fn i(v: i64) -> Value {
    json!(v)
}
fn f(v: f64) -> Value {
    json!(v)
}

impl DemoState {
    pub fn seed() -> Self {
        let taxi_cols = vec![
            col("vendor_id", "integer"),
            col("pickup_at", "varchar"),
            col("dropoff_at", "varchar"),
            col("passenger_count", "integer"),
            col("trip_distance", "double"),
            col("fare_amount", "double"),
            col("tip_amount", "double"),
            col("total_amount", "double"),
            col("payment_type", "varchar"),
            col("pickup_borough", "varchar"),
        ];
        let taxi_rows = vec![
            vec![
                i(1),
                s("2026-10-01 08:12"),
                s("2026-10-01 08:31"),
                i(2),
                f(3.4),
                f(14.5),
                f(3.2),
                f(20.1),
                s("card"),
                s("Manhattan"),
            ],
            vec![
                i(1),
                s("2026-10-01 09:02"),
                s("2026-10-01 09:14"),
                i(1),
                f(1.2),
                f(7.0),
                f(1.5),
                f(9.8),
                s("card"),
                s("Manhattan"),
            ],
            vec![
                i(2),
                s("2026-10-01 09:30"),
                s("2026-10-01 10:05"),
                i(3),
                f(8.7),
                f(31.0),
                f(6.2),
                f(41.4),
                s("card"),
                s("Brooklyn"),
            ],
            vec![
                i(2),
                s("2026-10-01 10:11"),
                s("2026-10-01 10:26"),
                i(1),
                f(2.1),
                f(9.5),
                f(2.0),
                f(13.1),
                s("cash"),
                s("Queens"),
            ],
            vec![
                i(1),
                s("2026-10-01 11:40"),
                s("2026-10-01 11:58"),
                i(2),
                f(4.0),
                f(16.0),
                f(4.1),
                f(23.0),
                s("card"),
                s("Manhattan"),
            ],
            vec![
                i(2),
                s("2026-10-01 12:05"),
                s("2026-10-01 12:22"),
                i(4),
                f(5.6),
                f(21.0),
                f(5.0),
                f(29.5),
                s("card"),
                s("Bronx"),
            ],
            vec![
                i(1),
                s("2026-10-01 13:15"),
                s("2026-10-01 13:29"),
                i(1),
                f(1.8),
                f(8.0),
                f(1.8),
                f(11.2),
                s("card"),
                s("Manhattan"),
            ],
            vec![
                i(2),
                s("2026-10-01 14:02"),
                s("2026-10-01 14:44"),
                i(2),
                f(11.3),
                f(38.0),
                f(7.6),
                f(50.2),
                s("card"),
                s("Brooklyn"),
            ],
            vec![
                i(1),
                s("2026-10-01 15:20"),
                s("2026-10-01 15:37"),
                i(3),
                f(3.1),
                f(13.0),
                f(2.9),
                f(18.0),
                s("cash"),
                s("Manhattan"),
            ],
            vec![
                i(2),
                s("2026-10-01 16:05"),
                s("2026-10-01 16:19"),
                i(1),
                f(2.4),
                f(10.0),
                f(2.2),
                f(14.1),
                s("card"),
                s("Queens"),
            ],
            vec![
                i(1),
                s("2026-10-01 17:30"),
                s("2026-10-01 17:52"),
                i(2),
                f(5.0),
                f(19.5),
                f(4.4),
                f(26.8),
                s("card"),
                s("Manhattan"),
            ],
            vec![
                i(2),
                s("2026-10-01 18:10"),
                s("2026-10-01 18:27"),
                i(1),
                f(2.7),
                f(11.0),
                f(2.4),
                f(15.3),
                s("card"),
                s("Staten Island"),
            ],
        ];

        let by_borough_cols = vec![
            col("pickup_borough", "varchar"),
            col("trips", "bigint"),
            col("avg_fare", "double"),
            col("total_revenue", "double"),
        ];
        let by_borough_rows = vec![
            vec![s("Manhattan"), i(5), f(13.0), f(97.2)],
            vec![s("Brooklyn"), i(2), f(22.7), f(84.6)],
            vec![s("Queens"), i(2), f(9.75), f(27.2)],
            vec![s("Bronx"), i(1), f(21.0), f(29.5)],
            vec![s("Staten Island"), i(1), f(11.0), f(15.3)],
        ];

        let daily_cols = vec![
            col("day", "varchar"),
            col("trips", "bigint"),
            col("revenue", "double"),
            col("avg_tip", "double"),
        ];
        let daily_rows = vec![
            vec![s("2026-09-29"), i(118_402), f(2_410_882.10), f(3.71)],
            vec![s("2026-09-30"), i(121_750), f(2_502_144.55), f(3.82)],
            vec![s("2026-10-01"), i(119_338), f(2_468_901.42), f(3.77)],
        ];

        Self {
            tables: vec![
                DemoTable {
                    namespace: "nyc_taxi".into(),
                    name: "yellow_trips".into(),
                    columns: taxi_cols,
                    rows: taxi_rows,
                },
                DemoTable {
                    namespace: "analytics".into(),
                    name: "trips_by_borough".into(),
                    columns: by_borough_cols,
                    rows: by_borough_rows,
                },
                DemoTable {
                    namespace: "analytics".into(),
                    name: "daily_revenue".into(),
                    columns: daily_cols,
                    rows: daily_rows,
                },
            ],
            admin_catalog: None,
            last_write_ms: Some(crate::jobs::now_ms() as i64),
        }
    }

    pub fn namespaces_json(&self) -> Value {
        let mut ns: Vec<String> = self.tables.iter().map(|t| t.namespace.clone()).collect();
        ns.sort();
        ns.dedup();
        json!({ "namespaces": ns.into_iter().map(|n| vec![n]).collect::<Vec<_>>() })
    }

    pub fn tables_json(&self, namespace: &str) -> Value {
        let ids: Vec<Value> = self
            .tables
            .iter()
            .filter(|t| t.namespace == namespace)
            .map(|t| json!({ "namespace": [t.namespace.clone()], "name": t.name, "kind": "table" }))
            .collect();
        json!({ "identifiers": ids })
    }

    pub fn table_metadata_json(&self, namespace: &str, table: &str) -> Option<Value> {
        let t = self.table(namespace, table)?;
        let fields: Vec<Value> = t
            .columns
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                json!({
                    "id": idx + 1,
                    "name": c.name,
                    "required": false,
                    "type": c.r#type,
                })
            })
            .collect();
        Some(json!({
            "metadata-location": format!("s3://lakehouse/warehouse/{namespace}/{table}/metadata/00001.metadata.json"),
            "metadata": {
                "table-uuid": format!("demo-{namespace}-{table}"),
                "location": format!("s3://lakehouse/warehouse/{namespace}/{table}"),
                "last-updated-ms": crate::jobs::now_ms(),
                "current-schema-id": 0,
                "schemas": [{ "schema-id": 0, "fields": fields }],
                "partition-specs": [{ "spec-id": 0, "fields": [] }],
                "properties": { "format-version": "2", "write.format.default": "parquet" }
            }
        }))
    }

    pub fn table(&self, namespace: &str, table: &str) -> Option<&DemoTable> {
        self.tables
            .iter()
            .find(|t| t.namespace == namespace && t.name == table)
    }

    /// The demo table's documentation, for `GET /api/catalog/tables/{ns}/{tbl}/documentation`
    /// (stardelt-nova#500). Demo mode is read-only: `canEdit` and `canRollback` are false.
    pub fn documentation(
        &self,
        namespace: &str,
        table: &str,
    ) -> Option<crate::catalog_docs::TableDocumentation> {
        let t = self.table(namespace, table)?;
        let (description, tags, owner, domain): (&str, Vec<&str>, &str, Option<&str>) =
            match (namespace, table) {
                ("nyc_taxi", "yellow_trips") => (
                    "NYC yellow-taxi trips, one row per completed trip.",
                    vec!["bronze", "taxi"],
                    "mobility-analytics",
                    Some("mobility"),
                ),
                ("analytics", "trips_by_borough") => (
                    "Trips by pickup borough, rebuilt from nyc_taxi.yellow_trips.",
                    vec!["gold", "taxi"],
                    "mobility-analytics",
                    Some("mobility"),
                ),
                ("analytics", "daily_revenue") => (
                    "Daily revenue and average fare, rebuilt from nyc_taxi.yellow_trips.",
                    vec!["gold", "finance"],
                    "retail-analytics",
                    Some("finance"),
                ),
                _ => ("", vec![], "", None),
            };
        Some(crate::catalog_docs::TableDocumentation {
            description: (!description.is_empty()).then(|| description.to_string()),
            tags: tags.into_iter().map(str::to_string).collect(),
            owner: (!owner.is_empty()).then(|| owner.to_string()),
            domain: domain.map(str::to_string),
            columns: t
                .columns
                .iter()
                .map(|c| crate::catalog_docs::ColumnDocumentation {
                    name: c.name.clone(),
                    r#type: c.r#type.clone(),
                    description: None,
                })
                .collect(),
            can_edit: false,
            edit_rule: "Demo mode keeps a read-only catalog.".to_string(),
            can_rollback: false,
            rollback_rule: "Demo mode keeps no snapshots, so there is nothing to roll back."
                .to_string(),
        })
    }

    fn table_mut(&mut self, namespace: &str, table: &str) -> Option<&mut DemoTable> {
        self.tables
            .iter_mut()
            .find(|t| t.namespace == namespace && t.name == table)
    }

    pub fn run_query(&self, sql: &str) -> Result<QueryResult, String> {
        let q = sql.trim().trim_end_matches(';').trim().to_lowercase();
        if q.is_empty() {
            return Err("empty query".into());
        }
        let query_id = Some(crate::jobs::new_id("demo"));
        if q.starts_with("select 1") {
            return Ok(QueryResult {
                columns: vec![Column {
                    name: "col1".into(),
                    r#type: "integer".into(),
                }],
                rows: vec![vec![json!(1)]],
                row_count: 1,
                query_id,
                stats: None,
                truncated: false,
            });
        }
        let (ns, tbl) = extract_ref(&q).ok_or_else(|| {
            "demo mode supports `SELECT * FROM <schema>.<table> [LIMIT n]` and `SELECT 1`"
                .to_string()
        })?;
        let t = self
            .table(&ns, &tbl)
            .ok_or_else(|| format!("table {ns}.{tbl} not found"))?;

        if q.contains("count(*)") {
            return Ok(QueryResult {
                columns: vec![Column {
                    name: "count".into(),
                    r#type: "bigint".into(),
                }],
                rows: vec![vec![json!(t.rows.len())]],
                row_count: 1,
                query_id,
                stats: None,
                truncated: false,
            });
        }

        let limit = extract_limit(&q).unwrap_or(100);
        let rows: Vec<Vec<Value>> = t.rows.iter().take(limit).cloned().collect();
        Ok(QueryResult {
            columns: t.columns.clone(),
            row_count: rows.len(),
            rows,
            query_id,
            stats: None,
            truncated: false,
        })
    }

    pub fn upload(
        &mut self,
        namespace: &str,
        table: &str,
        format: &str,
        content: &str,
    ) -> Result<usize, String> {
        self.upload_in_catalog("warehouse", namespace, table, format, content)
    }

    pub fn upload_in_catalog(
        &mut self,
        catalog: &str,
        namespace: &str,
        table: &str,
        format: &str,
        content: &str,
    ) -> Result<usize, String> {
        let _ = catalog;
        let (columns, rows) = if format.eq_ignore_ascii_case("json") {
            parse_json_table(content)?
        } else {
            parse_csv(content)?
        };
        if columns.is_empty() {
            return Err("no columns found".into());
        }
        let cols: Vec<Column> = columns
            .iter()
            .map(|c| Column {
                name: c.clone(),
                r#type: "varchar".into(),
            })
            .collect();
        let n = rows.len();
        match self.table_mut(namespace, table) {
            Some(t) => {
                t.columns = cols;
                t.rows = rows;
            }
            None => self.tables.push(DemoTable {
                namespace: namespace.to_string(),
                name: table.to_string(),
                columns: cols,
                rows,
            }),
        }
        self.mark_written();
        Ok(n)
    }

    pub fn transfer(&mut self, source: &str, target: &str, mode: &str) -> Result<usize, String> {
        self.transfer_in_catalogs("warehouse", source, "warehouse", target, mode)
    }

    pub fn transfer_in_catalogs(
        &mut self,
        _source_catalog: &str,
        source: &str,
        _target_catalog: &str,
        target: &str,
        mode: &str,
    ) -> Result<usize, String> {
        crate::data::check_transfer_mode(mode)?;
        let (sns, stbl) = parse_ref(source)?;
        let (tns, ttbl) = parse_ref(target)?;
        let src = self
            .table(&sns, &stbl)
            .cloned()
            .ok_or_else(|| format!("source {sns}.{stbl} not found"))?;
        let n = src.rows.len();
        match self.table_mut(&tns, &ttbl) {
            Some(t) => {
                t.columns = src.columns.clone();
                if mode.eq_ignore_ascii_case("append") {
                    t.rows.extend(src.rows.clone());
                } else {
                    t.rows = src.rows.clone();
                }
            }
            None => self.tables.push(DemoTable {
                namespace: tns,
                name: ttbl,
                columns: src.columns.clone(),
                rows: src.rows.clone(),
            }),
        }
        self.mark_written();
        Ok(n)
    }

    /// The demo lakehouse changed: remember when, for the Overview's "last change".
    fn mark_written(&mut self) {
        self.last_write_ms = Some(crate::jobs::now_ms() as i64);
    }
}

/// The Pipelines page in demo mode (#500): two resources the API could have returned.
pub fn pipelines() -> crate::pipelines::PipelineListResponse {
    use crate::pipelines::{PipelineListResponse, PipelineView, RunView};
    let run =
        |id: &str, state: &str, started: &str, ended: Option<&str>, secs: Option<f64>| RunView {
            run_id: id.into(),
            state: state.into(),
            run_type: "scheduled".into(),
            logical_date: Some(started.into()),
            started_at: Some(started.into()),
            ended_at: ended.map(str::to_string),
            duration_seconds: secs,
            triggered_by: Some("scheduler".into()),
            note: None,
        };
    PipelineListResponse {
        pipelines: vec![
            PipelineView {
                name: "daily_revenue".into(),
                namespace: "stardelt".into(),
                owner_group: Some("analytics".into()),
                schedule: Some("0 6 * * * (UTC)".into()),
                paused: false,
                loaded: true,
                next_run: Some("2026-11-09T06:00:00Z".into()),
                state: "succeeded".into(),
                last_run: Some(run(
                    "scheduled__2026-11-08T06:00:00+00:00",
                    "success",
                    "2026-11-08T06:00:00Z",
                    Some("2026-11-08T06:02:11Z"),
                    Some(131.0),
                )),
                tasks: vec![],
                run_as: Some("pipeline-analytics".into()),
                concurrency: "Allow".into(),
                created_at: Some("2026-10-21T09:15:00Z".into()),
                // A sample that shows where a pipeline comes from (stardelt-nova#618): deployed
                // from the `shop-etl` bundle and not touched since.
                deployment: Some(crate::pipelines::DeploymentView {
                    bundle: "shop-etl".into(),
                    target: "prod".into(),
                    revision: Some("abc1234e5678".into()),
                    repository: Some("https://git.example/shop/pipelines".into()),
                    deployed_by: Some("ada".into()),
                    deployed_at: Some("2026-11-01T08:00:00Z".into()),
                    modified: None,
                }),
            },
            PipelineView {
                name: "ingest_orders".into(),
                namespace: "stardelt".into(),
                owner_group: Some("mobility".into()),
                schedule: Some("*/15 * * * * (UTC)".into()),
                paused: false,
                loaded: true,
                next_run: None,
                state: "running".into(),
                last_run: Some(run(
                    "scheduled__2026-11-08T09:00:00+00:00",
                    "running",
                    "2026-11-08T09:00:00Z",
                    None,
                    None,
                )),
                tasks: vec![],
                run_as: Some("pipeline-mobility".into()),
                concurrency: "Allow".into(),
                created_at: Some("2026-10-29T14:20:00Z".into()),
                // The same bundle, changed by hand after the deploy: the amber badge
                // (`deployment.modified`) needs a sample to show.
                deployment: Some(crate::pipelines::DeploymentView {
                    bundle: "shop-etl".into(),
                    target: "prod".into(),
                    revision: Some("abc1234e5678".into()),
                    repository: None,
                    deployed_by: Some("ada".into()),
                    deployed_at: Some("2026-11-01T08:00:00Z".into()),
                    modified: Some(true),
                }),
            },
        ],
    }
}

/// `GET /api/pipelines/{namespace}/{name}` in demo mode (#500): one of the sample rows with
/// no runs. The page hides the write actions and the definition card, which have no demo
/// branch.
pub fn pipeline_detail(namespace: &str, name: &str) -> Option<crate::pipelines::PipelineDetail> {
    use crate::pipelines::{PipelineDetail, PipelineOptionsView};
    let pipeline = pipelines()
        .pipelines
        .into_iter()
        .find(|p| p.namespace == namespace && p.name == name)?;
    Some(PipelineDetail {
        pipeline,
        runs: vec![],
        params: std::collections::BTreeMap::new(),
        retries: None,
        options: PipelineOptionsView {
            retries: 1,
            backoff_minutes: 5,
            timeout_minutes: 120,
            concurrency: "Forbid".into(),
        },
        airflow_url: None,
    })
}

/// `GET /api/runs` in demo mode (#516): every run of the sample pipelines, newest first.
/// The samples carry one run each — their last — because their detail page has none.
pub fn runs() -> crate::pipelines::RunsListResponse {
    let mut runs = Vec::new();
    for pipeline in pipelines().pipelines {
        let kind = pipeline
            .tasks
            .first()
            .map(|task| task.kind.clone())
            .unwrap_or_else(|| "pipeline".into());
        if let Some(run) = pipeline.last_run.clone() {
            runs.push(crate::pipelines::RunsListRow {
                namespace: pipeline.namespace.clone(),
                name: pipeline.name.clone(),
                owner_group: pipeline.owner_group.clone(),
                kind,
                run,
            });
        }
    }
    runs.sort_by(|a, b| b.run.started_at.cmp(&a.run.started_at));
    crate::pipelines::RunsListResponse { runs }
}

/// `GET /api/pipelines/principals` in demo mode: the two groups above, both able to schedule.
pub fn pipeline_principals() -> crate::pipelines::PrincipalsResponse {
    use crate::pipelines::{PrincipalView, PrincipalsResponse};
    PrincipalsResponse {
        groups: vec![
            PrincipalView {
                group: "analytics".into(),
                principal: Some("pipeline-analytics".into()),
                ready: true,
                message: None,
            },
            PrincipalView {
                group: "mobility".into(),
                principal: Some("pipeline-mobility".into()),
                ready: true,
                message: None,
            },
        ],
    }
}

/// `GET /api/service-principals` in demo mode: the two principals, ready.
pub fn service_principals() -> crate::pipelines::ServicePrincipalsResponse {
    use crate::pipelines::{ServicePrincipalView, ServicePrincipalsResponse};
    let principal = |name: &str, group: &str| ServicePrincipalView {
        name: name.into(),
        owner_group: group.into(),
        purpose: "pipeline".into(),
        audiences: vec!["lakekeeper".into()],
        ready: true,
        message: None,
    };
    ServicePrincipalsResponse {
        principals: vec![
            principal("pipeline-analytics", "analytics"),
            principal("pipeline-mobility", "mobility"),
        ],
    }
}

fn extract_ref(q: &str) -> Option<(String, String)> {
    let idx = q.find(" from ")?;
    let rest = &q[idx + 6..];
    let token = rest
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| c == ',' || c == ';');
    let parts: Vec<&str> = token.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        Some((
            parts[parts.len() - 2].to_string(),
            parts[parts.len() - 1].to_string(),
        ))
    } else {
        None
    }
}

fn extract_limit(q: &str) -> Option<usize> {
    let idx = q.rfind(" limit ")?;
    q[idx + 7..]
        .split_whitespace()
        .next()?
        .trim_end_matches(';')
        .parse()
        .ok()
}

/// Parse `ns.table` or `catalog.ns.table` into `(ns, table)`.
pub fn parse_ref(raw: &str) -> Result<(String, String), String> {
    let parts: Vec<&str> = raw.trim().split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return Err(format!("expected schema.table, got '{raw}'"));
    }
    Ok((
        parts[parts.len() - 2].to_string(),
        parts[parts.len() - 1].to_string(),
    ))
}

pub fn parse_csv(content: &str) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
    let records = read_csv_records(content);
    // Drop blank lines: a single unquoted empty cell is not a record.
    let records: Vec<Vec<Cell>> = records
        .into_iter()
        .filter(|r| !(r.len() == 1 && !r[0].1 && r[0].0.trim().is_empty()))
        .collect();
    let header = records.first().ok_or("empty CSV")?;
    let columns: Vec<String> = header.iter().map(|(t, _)| t.trim().to_string()).collect();
    if columns.is_empty() || columns.iter().all(|c| c.is_empty()) {
        return Err("missing CSV header".into());
    }
    let want = columns.len();
    let mut rows = Vec::new();
    for (idx, rec) in records.iter().skip(1).enumerate() {
        let n = idx + 1;
        if rec.len() != want {
            return Err(format!(
                "row {n} has {} fields but the header has {want}",
                rec.len()
            ));
        }
        rows.push(
            rec.iter()
                .map(|(t, quoted)| {
                    let raw = if *quoted { t.as_str() } else { t.trim() };
                    coerce(raw)
                })
                .collect(),
        );
    }
    Ok((columns, rows))
}

pub fn parse_json_table(content: &str) -> Result<(Vec<String>, Vec<Vec<Value>>), String> {
    let value: Value = serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;
    let arr = value.as_array().ok_or("JSON must be an array of objects")?;
    let mut columns: Vec<String> = Vec::new();
    for row in arr {
        if let Some(obj) = row.as_object() {
            for key in obj.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return Err("no object keys found".into());
    }
    let rows = arr
        .iter()
        .map(|row| {
            let obj = row.as_object();
            columns
                .iter()
                .map(|c| obj.and_then(|o| o.get(c)).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    Ok((columns, rows))
}

type Cell = (String, bool);

fn read_csv_records(content: &str) -> Vec<Vec<Cell>> {
    let mut records: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut cur = String::new();
    let mut was_quoted = false;
    let mut in_quoted = false;
    let mut after_quote = false;
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    let mut field_has_content = false;
    while i < chars.len() {
        let ch = chars[i];
        if in_quoted {
            if ch == '"' {
                if i + 1 < chars.len() && chars[i + 1] == '"' {
                    cur.push('"');
                    i += 2;
                    field_has_content = true;
                    continue;
                } else {
                    in_quoted = false;
                    after_quote = true;
                    i += 1;
                    continue;
                }
            } else if ch == '\r' {
                if i + 1 < chars.len() && chars[i + 1] == '\n' {
                    cur.push('\n');
                    i += 2;
                } else {
                    cur.push('\n');
                    i += 1;
                }
                field_has_content = true;
                continue;
            } else {
                cur.push(ch);
                i += 1;
                field_has_content = true;
                continue;
            }
        }
        // Not in quotes.
        if !field_has_content && !was_quoted && cur.is_empty() && ch == '"' {
            in_quoted = true;
            was_quoted = true;
            after_quote = false;
            field_has_content = true;
            i += 1;
            continue;
        }
        if ch == ',' {
            row.push((std::mem::take(&mut cur), std::mem::take(&mut was_quoted)));
            field_has_content = false;
            after_quote = false;
            i += 1;
            continue;
        }
        if ch == '\r' || ch == '\n' {
            if ch == '\r' && i + 1 < chars.len() && chars[i + 1] == '\n' {
                i += 2;
            } else {
                i += 1;
            }
            row.push((std::mem::take(&mut cur), std::mem::take(&mut was_quoted)));
            field_has_content = false;
            after_quote = false;
            records.push(std::mem::take(&mut row));
            continue;
        }
        if after_quote && ch == '"' {
            // A quote after a closed quoted section is literal text.
            cur.push(ch);
            i += 1;
            field_has_content = true;
            continue;
        }
        cur.push(ch);
        i += 1;
        field_has_content = true;
    }
    // Flush the trailing field/record when the input does not end with a newline.
    if !cur.is_empty() || was_quoted || field_has_content || !row.is_empty() {
        row.push((cur, was_quoted));
        records.push(row);
    }
    records
}

fn is_integer_text(s: &str) -> bool {
    let t = s.strip_prefix('-').unwrap_or(s);
    if t.is_empty() {
        return false;
    }
    if t == "0" {
        return true;
    }
    if t.starts_with('0') {
        return false;
    }
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())
}

fn is_float_text(s: &str) -> bool {
    let t = s.strip_prefix('-').unwrap_or(s);
    if t.is_empty() {
        return false;
    }
    // Split off a trailing exponent: e/E + optional sign + digits.
    let (mantissa, exponent) = match t.find(['e', 'E']) {
        Some(pos) => {
            let (m, e) = t.split_at(pos);
            let exp = &e[1..];
            let exp = exp
                .strip_prefix('+')
                .unwrap_or_else(|| exp.strip_prefix('-').unwrap_or(exp));
            if m.is_empty() || exp.is_empty() || !exp.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            (m, true)
        }
        None => (t, false),
    };
    let _ = exponent;
    let dot = match mantissa.find('.') {
        Some(d) => d,
        None => return false,
    };
    if mantissa[dot + 1..].contains('.') {
        return false;
    }
    let (int_part, frac_part) = mantissa.split_at(dot);
    let frac_part = &frac_part[1..];
    if frac_part.is_empty() || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if int_part.is_empty() {
        return false;
    }
    if int_part == "0" {
        return true;
    }
    if int_part.starts_with('0') {
        return false;
    }
    int_part.bytes().all(|b| b.is_ascii_digit())
}

fn coerce(raw: &str) -> Value {
    if raw.is_empty() || raw.eq_ignore_ascii_case("null") {
        Value::Null
    } else if is_integer_text(raw) {
        match raw.parse::<i64>() {
            Ok(i) => json!(i),
            Err(_) => json!(raw),
        }
    } else if is_float_text(raw) {
        match raw.parse::<f64>() {
            Ok(f) if f.is_finite() => json!(f),
            _ => json!(raw),
        }
    } else {
        json!(raw)
    }
}

/// `GET /api/connections` in demo mode (#500): two rows, no values, exactly the shape the
/// real API answers with.
pub fn connections() -> crate::connection_routes::ConnectionsResponse {
    use crate::connections::{ConnectionView, Scope};
    let row = |name: &str, kind: &str, scope: Scope, group: Option<&str>, secret_keys: &[&str]| {
        ConnectionView {
            name: name.into(),
            scope,
            kind: kind.into(),
            owner_group: group.map(str::to_string),
            usable_by: vec![],
            config: std::collections::BTreeMap::from([
                ("host".to_string(), "demo.invalid".to_string()),
                ("port".to_string(), "5432".to_string()),
            ]),
            secret_keys: secret_keys.iter().map(|k| k.to_string()).collect(),
            last_rotated: Some("2026-10-30T08:00:00Z".into()),
            version: 1,
            sync: None,
            break_glass: None,
            member: true,
        }
    };
    crate::connection_routes::ConnectionsResponse {
        connections: vec![
            row(
                "orders-postgres",
                "postgres",
                Scope::Group,
                Some("mobility"),
                &["username", "password"],
            ),
            row(
                "my-bucket",
                "s3",
                Scope::Personal,
                None,
                &["accessKey", "secretKey"],
            ),
        ],
    }
}

/// `GET /api/streams` in demo mode (#500): two rows, exactly the shape the real API answers
/// with. Read-only; demo mode has no cluster, so the patch route stays 503 and the page hides
/// the action.
pub fn streams() -> crate::streams::StreamsResponse {
    use crate::streams::{StreamView, StreamsResponse};
    let row = |name: &str, phase: &str, topic: &str, table: &str| StreamView {
        namespace: "stardelt".into(),
        name: name.into(),
        owner_group: "mobility".into(),
        phase: phase.into(),
        reason: None,
        last_error: None,
        last_checkpoint_at: Some("2026-11-08T09:30:00Z".into()),
        attempts: None,
        last_submitted_at: None,
        stopped: false,
        source_connection: "mobility-events".into(),
        source_topic: topic.into(),
        sink_catalog: "warehouse".into(),
        sink_table: table.into(),
        sink_mode: "append".into(),
        run_as: Some("pipeline-mobility".into()),
    };
    StreamsResponse {
        streams: vec![
            row(
                "pegelonline-live",
                "Running",
                "pegelonline.measurements",
                "mobility.pegelonline",
            ),
            row("orders-cdc", "Pending", "orders.changes", "mobility.orders"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_returns_seeded_rows() {
        let demo = DemoState::seed();
        let r = demo
            .run_query("SELECT * FROM warehouse.nyc_taxi.yellow_trips LIMIT 5")
            .unwrap();
        assert_eq!(r.row_count, 5);
        assert_eq!(r.columns.len(), 10);
    }

    #[test]
    fn count_star_works() {
        let demo = DemoState::seed();
        let r = demo
            .run_query("SELECT count(*) FROM nyc_taxi.yellow_trips")
            .unwrap();
        assert_eq!(r.rows[0][0], json!(12));
    }

    #[test]
    fn upload_and_transfer() {
        let mut demo = DemoState::seed();
        let n = demo
            .upload(
                "nyc_taxi",
                "zones",
                "csv",
                "zone_id,zone\n1,Manhattan\n2,Brooklyn",
            )
            .unwrap();
        assert_eq!(n, 2);
        assert!(demo.table("nyc_taxi", "zones").is_some());
        let moved = demo
            .transfer("nyc_taxi.zones", "analytics.zones_copy", "overwrite")
            .unwrap();
        assert_eq!(moved, 2);
        assert!(demo.table("analytics", "zones_copy").is_some());
    }

    #[test]
    fn writes_mark_the_demo_lakehouse_changed() {
        let mut demo = DemoState::seed();
        assert!(demo.last_write_ms.is_some(), "the seed is a write");
        // A later write moves the stamp the Overview reads as "last change".
        demo.last_write_ms = Some(0);
        demo.upload("nyc_taxi", "zones", "csv", "zone_id,zone\n1,Manhattan")
            .unwrap();
        assert!(demo.last_write_ms.is_some_and(|ms| ms > 0));
        demo.last_write_ms = Some(0);
        demo.transfer("nyc_taxi.zones", "analytics.zones_copy", "overwrite")
            .unwrap();
        assert!(demo.last_write_ms.is_some_and(|ms| ms > 0));
    }

    #[test]
    fn pipeline_detail_is_one_of_the_samples() {
        let detail = pipeline_detail("stardelt", "daily_revenue").expect("a sample pipeline");
        assert_eq!(detail.pipeline.name, "daily_revenue");
        assert!(detail.runs.is_empty(), "the sample has no runs");
        assert!(pipeline_detail("stardelt", "nope").is_none());
        assert!(pipeline_detail("other", "daily_revenue").is_none());
    }

    #[test]
    fn ref_parsing_strips_catalog() {
        assert_eq!(
            parse_ref("warehouse.a.b").unwrap(),
            ("a".into(), "b".into())
        );
        assert_eq!(parse_ref("a.b").unwrap(), ("a".into(), "b".into()));
        assert!(parse_ref("b").is_err());
    }

    #[test]
    fn csv_escaped_quotes_survive() {
        let (cols, rows) = parse_csv("a,b\n1,\"say \"\"hi\"\"\"").unwrap();
        assert_eq!(cols, vec!["a", "b"]);
        assert_eq!(rows, vec![vec![json!(1), json!("say \"hi\"")]]);
    }

    #[test]
    fn csv_embedded_newline_stays_one_row() {
        let (cols, rows) = parse_csv("a,b\n1,\"line1\nline2\"\n2,x").unwrap();
        assert_eq!(cols, vec!["a", "b"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], json!("line1\nline2"));
    }

    #[test]
    fn csv_ragged_row_is_an_error_naming_the_row() {
        let err = parse_csv("a,b\n1,2\n3\n").unwrap_err();
        assert!(err.contains("row 2"), "{err}");
        assert!(err.contains("1") && err.contains("2"), "{err}");
    }

    #[test]
    fn leading_zero_stays_text() {
        let (_, rows) = parse_csv("zip\n00501").unwrap();
        assert_eq!(rows[0][0], json!("00501"));
        let (_, rows) = parse_csv("v\n007.5").unwrap();
        assert_eq!(rows[0][0], json!("007.5"));
        let (_, rows) = parse_csv("v\n0.5").unwrap();
        assert_eq!(rows[0][0], json!(0.5));
    }

    #[test]
    fn nan_and_inf_stay_text() {
        let (_, rows) = parse_csv("w\nnan\ninf\n").unwrap();
        assert_eq!(rows[0][0], json!("nan"));
        assert_eq!(rows[1][0], json!("inf"));
    }

    #[test]
    fn huge_integer_stays_text() {
        let (_, rows) = parse_csv("v\n9223372036854775808").unwrap();
        assert_eq!(rows[0][0], json!("9223372036854775808"));
    }
}
