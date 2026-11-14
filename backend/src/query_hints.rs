//! Deterministic hints for a stored query profile (stardelt-nova#788 S3a): what to change,
//! with the number that says why.
//!
//! The rules run at read time on the profile S1 stored plus the query's own timing, so they
//! never need a backfill and they never parse the plan text. An empty vector is the normal
//! answer — no evidence, no hint.
//!
//! One rule of the design's hint table is deliberately absent:
//!
//! - *No pruning* needs the table's total size, and the profile keeps per-**catalog** totals
//!   only (`CatalogStats`); its `inputs[].catalogName` is the connector's name (`iceberg`),
//!   not a warehouse. The skew rule reads the per-stage CPU spread `parse_profile` keeps in
//!   `stageStats`.

use serde::Serialize;

/// One suggestion for the person reading the profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hint {
    pub code: &'static str,
    pub title: String,
    pub evidence: String,
    pub docs_url: String,
}

/// The guide every hint links to; each hint appends its own `#anchor`.
pub const HINT_BASE: &str = "https://stardelt.io/docs/guides/query-log/";
/// A join that emits more than this many rows per input row is the suspect.
pub const JOIN_MULTIPLE: i64 = 10;
/// Below this many splits a table cannot be "many small files".
pub const SMALL_FILE_MIN_SPLITS: i64 = 100;
/// An average split smaller than this is a small file.
pub const SMALL_FILE_BYTES: i64 = 8 * 1024 * 1024;
/// A queue shorter than this is normal scheduling, not congestion.
pub const QUEUE_MIN_MS: i64 = 1000;
/// A stage whose slowest task took more than this multiple of its median is skewed.
pub const SKEW_MULTIPLE: i64 = 5;

/// `1_234_567` → `"1,234,567"`; `0` → `"0"`; a negative keeps its sign.
pub fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The deterministic hints for one stored profile. An empty vector is the normal answer.
pub fn hints(
    profile: &serde_json::Value,
    queued_ms: Option<i64>,
    execution_ms: Option<i64>,
) -> Vec<Hint> {
    let mut out = Vec::new();

    // 1. spill — the query ran out of memory.
    let spilled = profile["spilledBytes"].as_i64().unwrap_or(0);
    if spilled > 0 {
        out.push(Hint {
            code: "spill",
            title: "The query ran out of memory and spilled to disk — give it a larger warehouse, or read less data before the join".to_string(),
            evidence: format!("{} bytes spilled", commas(spilled)),
            docs_url: format!("{HINT_BASE}#spill"),
        });
    }

    // 2. join-explosion — a join emits far more rows than it reads.
    if let Some(operators) = profile["operators"].as_array() {
        for op in operators {
            let is_join = op["operatorType"]
                .as_str()
                .is_some_and(|t| t.ends_with("JoinOperator"));
            if !is_join {
                continue;
            }
            let input = op["inputPositions"].as_i64().unwrap_or(0);
            let output = op["outputPositions"].as_i64().unwrap_or(0);
            if input > 0 && output > JOIN_MULTIPLE * input {
                out.push(Hint {
                    code: "join-explosion",
                    title: "This join multiplies rows — check the join keys".to_string(),
                    evidence: format!(
                        "{} output rows from {} input rows ({}×)",
                        commas(output),
                        commas(input),
                        output / input
                    ),
                    docs_url: format!("{HINT_BASE}#join-explosion"),
                });
                break;
            }
        }
    }

    // 3. small-files — many splits, little data in each.
    let splits = profile["completedSplits"].as_i64().unwrap_or(0);
    if splits >= SMALL_FILE_MIN_SPLITS {
        let bytes: i64 = profile["inputs"]
            .as_array()
            .map(|inputs| {
                inputs
                    .iter()
                    .map(|i| i["physicalInputBytes"].as_i64().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        if bytes > 0 {
            let avg = bytes / splits;
            if avg < SMALL_FILE_BYTES {
                out.push(Hint {
                    code: "small-files",
                    title: "The table is stored in many small files — compact it with the engine's optimize procedure".to_string(),
                    evidence: format!("{} splits, {} bytes each on average", commas(splits), commas(avg)),
                    docs_url: format!("{HINT_BASE}#small-files"),
                });
            }
        }
    }

    // 4. queue-bound — it waited longer than it ran.
    match (queued_ms, execution_ms) {
        (Some(queued), Some(exec)) if queued > exec && queued >= QUEUE_MIN_MS => {
            out.push(Hint {
                code: "queue-bound",
                title: "The warehouse was full — lower its concurrency, or run this on a larger SqlWarehouse"
                    .to_string(),
                evidence: format!(
                    "{} ms in the queue, {} ms executing",
                    commas(queued),
                    commas(exec)
                ),
                docs_url: format!("{HINT_BASE}#queue-bound"),
            });
        }
        _ => {}
    }

    // 5. skew — one task of a stage did most of the work.
    let worst = profile["stageStats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|stage| {
            let tasks = stage["tasks"].as_i64().unwrap_or(0);
            let p50 = stage["cpuP50Ms"].as_i64().unwrap_or(0);
            let max = stage["cpuMaxMs"].as_i64().unwrap_or(0);
            (tasks >= 2 && p50 > 0 && max > SKEW_MULTIPLE * p50).then_some((stage, p50, max))
        })
        .max_by_key(|(_, p50, max)| max * 1_000 / p50);
    if let Some((stage, p50, max)) = worst {
        out.push(Hint {
            code: "skew",
            title: "One key holds most of the rows — salt the key or repartition before the join"
                .to_string(),
            evidence: format!(
                "stage {}: the slowest task took {} ms of CPU, the median {} ms ({}×)",
                stage["stageId"].as_i64().unwrap_or(0),
                commas(max),
                commas(p50),
                max / p50
            ),
            docs_url: format!("{HINT_BASE}#skew"),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spill_profile() -> serde_json::Value {
        serde_json::json!({"operators":[],"spilledBytes":2516582,"completedSplits":4,"inputs":[]})
    }

    fn plain_profile() -> serde_json::Value {
        serde_json::json!({"operators":[],"spilledBytes":0,"completedSplits":1,"inputs":[]})
    }

    #[test]
    fn commas_group_by_thousands() {
        assert_eq!(commas(1_234_567), "1,234,567");
        assert_eq!(commas(0), "0");
        assert_eq!(commas(12), "12");
        assert_eq!(commas(-1234), "-1,234");
    }

    /// The real Trino 480 event fires no rule: no spill, no row-multiplying join, and 22
    /// splits is far below the small-file floor.
    #[test]
    fn the_real_fixture_has_no_hint() {
        let p = crate::query_log_event::parse_profile(
            include_str!("testdata/query_log/completed_profile.json").as_bytes(),
        )
        .expect("a profile");
        assert_eq!(hints(&p, Some(12), Some(2300)), Vec::new());
    }

    #[test]
    fn spill_is_named_with_its_bytes() {
        assert_eq!(
            hints(&spill_profile(), None, None),
            vec![Hint {
                code: "spill",
                title: "The query ran out of memory and spilled to disk — give it a larger warehouse, or read less data before the join".to_string(),
                evidence: "2,516,582 bytes spilled".to_string(),
                docs_url: format!("{HINT_BASE}#spill"),
            }]
        );
    }

    #[test]
    fn a_multiplying_join_is_named_with_both_counts() {
        let p = serde_json::json!({"operators":[{"operatorType":"LookupJoinOperator","inputPositions":12000,"outputPositions":1200000}],"spilledBytes":0,"completedSplits":1,"inputs":[]});
        assert_eq!(
            hints(&p, None, None),
            vec![Hint {
                code: "join-explosion",
                title: "This join multiplies rows — check the join keys".to_string(),
                evidence: "1,200,000 output rows from 12,000 input rows (100×)".to_string(),
                docs_url: format!("{HINT_BASE}#join-explosion"),
            }]
        );
    }

    #[test]
    fn many_small_splits_are_named_with_the_average() {
        let p = serde_json::json!({"operators":[],"spilledBytes":0,"completedSplits":1000,"inputs":[{"physicalInputBytes":500000000},{"physicalInputBytes":null}]});
        assert_eq!(
            hints(&p, None, None),
            vec![Hint {
                code: "small-files",
                title: "The table is stored in many small files — compact it with the engine's optimize procedure".to_string(),
                evidence: "1,000 splits, 500,000 bytes each on average".to_string(),
                docs_url: format!("{HINT_BASE}#small-files"),
            }]
        );
    }

    #[test]
    fn waiting_longer_than_running_is_named() {
        assert_eq!(
            hints(&plain_profile(), Some(4200), Some(850)),
            vec![Hint {
                code: "queue-bound",
                title: "The warehouse was full — lower its concurrency, or run this on a larger SqlWarehouse"
                    .to_string(),
                evidence: "4,200 ms in the queue, 850 ms executing".to_string(),
                docs_url: format!("{HINT_BASE}#queue-bound"),
            }]
        );
        // Below the floor, or the queue is not the longer half: no hint.
        assert_eq!(hints(&plain_profile(), Some(900), Some(100)), Vec::new());
        assert_eq!(hints(&plain_profile(), Some(600), Some(600)), Vec::new());
    }

    #[test]
    fn a_skewed_stage_is_named_with_both_times() {
        let p = serde_json::json!({"operators":[],"spilledBytes":0,"completedSplits":1,"inputs":[],"stageStats":[{"stageId":3,"tasks":8,"cpuP50Ms":600,"cpuMaxMs":9000}]});
        assert_eq!(
            hints(&p, None, None),
            vec![Hint {
                code: "skew",
                title:
                    "One key holds most of the rows — salt the key or repartition before the join"
                        .to_string(),
                evidence: "stage 3: the slowest task took 9,000 ms of CPU, the median 600 ms (15×)"
                    .to_string(),
                docs_url: format!("{HINT_BASE}#skew"),
            }]
        );
    }

    #[test]
    fn a_stage_with_one_task_is_not_skewed() {
        let p = serde_json::json!({"operators":[],"spilledBytes":0,"completedSplits":1,"inputs":[],"stageStats":[{"stageId":3,"tasks":1,"cpuP50Ms":7,"cpuMaxMs":700}]});
        assert_eq!(hints(&p, None, None), Vec::new());
    }

    #[test]
    fn exactly_five_times_is_not_skewed() {
        let p = serde_json::json!({"operators":[],"spilledBytes":0,"completedSplits":1,"inputs":[],"stageStats":[{"stageId":3,"tasks":4,"cpuP50Ms":100,"cpuMaxMs":500}]});
        assert_eq!(hints(&p, None, None), Vec::new());
    }
}
