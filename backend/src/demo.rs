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
            .map(|t| json!({ "namespace": [t.namespace.clone()], "name": t.name }))
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
        })
    }

    pub fn upload(
        &mut self,
        namespace: &str,
        table: &str,
        format: &str,
        content: &str,
    ) -> Result<usize, String> {
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
        Ok(n)
    }

    pub fn transfer(&mut self, source: &str, target: &str, mode: &str) -> Result<usize, String> {
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
        Ok(n)
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
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().ok_or("empty CSV")?;
    let columns: Vec<String> = split_csv(header)
        .into_iter()
        .map(|s| s.trim().to_string())
        .collect();
    if columns.is_empty() {
        return Err("missing CSV header".into());
    }
    let mut rows = Vec::new();
    for line in lines {
        let cells = split_csv(line);
        if cells.len() != columns.len() {
            continue;
        }
        rows.push(cells.into_iter().map(|c| coerce(c.trim())).collect());
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

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn coerce(raw: &str) -> Value {
    if raw.is_empty() || raw.eq_ignore_ascii_case("null") {
        Value::Null
    } else if let Ok(i) = raw.parse::<i64>() {
        json!(i)
    } else if let Ok(f) = raw.parse::<f64>() {
        json!(f)
    } else {
        json!(raw)
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
    fn ref_parsing_strips_catalog() {
        assert_eq!(
            parse_ref("warehouse.a.b").unwrap(),
            ("a".into(), "b".into())
        );
        assert_eq!(parse_ref("a.b").unwrap(), ("a".into(), "b".into()));
        assert!(parse_ref("b").is_err());
    }
}
