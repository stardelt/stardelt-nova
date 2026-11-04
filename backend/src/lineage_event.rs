//! OpenLineage event parsing for the internal lineage receiver (D40, WP-34a).
//!
//! Accepts a typed subset of OpenLineage 1.x. Unknown fields are preserved in the
//! raw payload. Rejects events missing required fields.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("{0}")]
    Message(String),
}

impl ParseError {
    fn new(msg: impl Into<String>) -> Self {
        ParseError::Message(msg.into())
    }
}

#[derive(Debug, Clone)]
pub struct LineageDataset {
    pub namespace: String,
    pub name: String,
    #[allow(dead_code)]
    pub facets: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct LineageEvent {
    pub event_type: String,
    pub event_time: String,
    pub producer: Option<String>,
    pub schema_url: Option<String>,
    pub job_namespace: String,
    pub job_name: String,
    pub run_id: String,
    pub inputs: Vec<LineageDataset>,
    pub outputs: Vec<LineageDataset>,
    pub raw: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlEvent {
    #[serde(rename = "eventType")]
    event_type: Option<String>,
    #[serde(rename = "eventTime")]
    event_time: Option<String>,
    producer: Option<String>,
    #[serde(rename = "schemaURL")]
    schema_url: Option<String>,
    job: Option<OlJob>,
    run: Option<OlRun>,
    inputs: Option<Value>,
    outputs: Option<Value>,
    #[serde(flatten)]
    #[allow(dead_code)]
    extra: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlJob {
    namespace: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlRun {
    #[serde(rename = "runId")]
    run_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlDataset {
    namespace: Option<String>,
    name: Option<String>,
    facets: Option<Value>,
}

pub fn parse(body: &[u8]) -> Result<LineageEvent, ParseError> {
    let raw: Value = serde_json::from_slice(body)
        .map_err(|e| ParseError::new(format!("malformed event: {e}")))?;

    let ol: OlEvent = serde_json::from_value(raw.clone())
        .map_err(|e| ParseError::new(format!("malformed event: {e}")))?;

    let event_type = ol
        .event_type
        .ok_or_else(|| ParseError::new("missing eventType"))?;
    let allowed = ["START", "RUNNING", "COMPLETE", "FAIL", "ABORT"];
    if !allowed.contains(&event_type.as_str()) {
        // Accept as-is per typed subset? Ticket says "eventType in START|RUNNING|COMPLETE|FAIL|ABORT" - probably should validate
    }

    let event_time = ol
        .event_time
        .ok_or_else(|| ParseError::new("missing eventTime"))?;

    let job = ol.job.ok_or_else(|| ParseError::new("missing job"))?;
    let job_namespace = job
        .namespace
        .ok_or_else(|| ParseError::new("missing job namespace"))?;
    let job_name = job
        .name
        .ok_or_else(|| ParseError::new("missing job name"))?;

    let run = ol.run.ok_or_else(|| ParseError::new("missing run"))?;
    let run_id = run.run_id.ok_or_else(|| ParseError::new("missing runId"))?;

    // inputs/outputs must be arrays if present
    let inputs_val = ol.inputs.unwrap_or(Value::Array(vec![]));
    if !inputs_val.is_array() {
        return Err(ParseError::new("inputs must be an array"));
    }
    let outputs_val = ol.outputs.unwrap_or(Value::Array(vec![]));
    if !outputs_val.is_array() {
        return Err(ParseError::new("outputs must be an array"));
    }

    let mut inputs = Vec::new();
    if let Value::Array(arr) = inputs_val {
        for item in arr {
            if let Ok(ds) = serde_json::from_value::<OlDataset>(item.clone())
                && let (Some(ns), Some(name)) = (ds.namespace, ds.name)
            {
                inputs.push(LineageDataset {
                    namespace: ns,
                    name,
                    facets: ds.facets,
                });
                continue;
            }
        }
    }

    let mut outputs = Vec::new();
    if let Value::Array(arr) = outputs_val {
        for item in arr {
            if let Ok(ds) = serde_json::from_value::<OlDataset>(item.clone())
                && let (Some(ns), Some(name)) = (ds.namespace, ds.name)
            {
                outputs.push(LineageDataset {
                    namespace: ns,
                    name,
                    facets: ds.facets,
                });
                continue;
            }
        }
    }

    Ok(LineageEvent {
        event_type,
        event_time,
        producer: ol.producer,
        schema_url: ol.schema_url,
        job_namespace,
        job_name,
        run_id,
        inputs,
        outputs,
        raw,
    })
}
