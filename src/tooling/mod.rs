use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use serde_json::{json, Map, Value};

use crate::compose::{parse_compose, run_compose};
use crate::registry::{Context, Registry};

const CONTRACT_ID: &str = "lcod://tooling/test_checker@1";

pub fn register_tooling(registry: &Registry) {
    registry.register(CONTRACT_ID, test_checker);
}

fn load_compose_from_path(path: &Path) -> Result<Vec<crate::compose::Step>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("unable to read compose file: {}", path.display()))?;
    let doc: Value = serde_yaml::from_str(&content)
        .with_context(|| format!("invalid YAML compose: {}", path.display()))?;
    let compose_value = doc
        .get("compose")
        .cloned()
        .ok_or_else(|| anyhow!("compose root missing in {}", path.display()))?;
    parse_compose(&compose_value)
        .with_context(|| format!("invalid compose structure in {}", path.display()))
}

fn ensure_compose(input: &Value) -> Result<Vec<crate::compose::Step>> {
    if let Some(compose) = input.get("compose") {
        return parse_compose(compose).map_err(|err| anyhow!("invalid inline compose: {err}"));
    }
    if let Some(compose_ref) = input.get("composeRef") {
        let path_str = compose_ref
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("composeRef.path must be a string"))?;
        let resolved = PathBuf::from(path_str);
        return load_compose_from_path(&resolved);
    }
    Err(anyhow!("compose or composeRef.path must be provided"))
}

fn matches_expected(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e.iter().all(|(key, val)| {
            a.get(key)
                .map(|actual_val| matches_expected(actual_val, val))
                .unwrap_or(false)
        }),
        _ => false,
    }
}

fn simple_diff(actual: &Value, expected: &Value) -> Value {
    json!({
        "path": "$",
        "actual": actual,
        "expected": expected
    })
}

fn set_path_value(target: &mut Value, path: &str, new_value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        *target = new_value;
        return;
    }
    let mut cursor = target;
    for key in &parts[..parts.len() - 1] {
        if !cursor.is_object() {
            *cursor = Value::Object(Map::new());
        }
        let map = cursor.as_object_mut().expect("object expected");
        cursor = map.entry((*key).to_string()).or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(obj) = cursor.as_object_mut() {
        obj.insert(parts[parts.len() - 1].to_string(), new_value);
    }
}

fn decode_chunk(chunk: &str, encoding: &str) -> Result<Vec<u8>> {
    match encoding {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(chunk)
            .map_err(|err| anyhow!("invalid base64 chunk: {err}")),
        "hex" => hex::decode(chunk).map_err(|err| anyhow!("invalid hex chunk: {err}")),
        _ => Ok(chunk.as_bytes().to_vec()),
    }
}

fn register_streams(ctx: &mut Context, state: &mut Value, specs: &Value) -> Result<()> {
    let list = specs.as_array().ok_or_else(|| anyhow!("streams must be an array"))?;
    for spec in list {
        let target = spec
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("streams[].target must be a string"))?;
        let encoding = spec
            .get("encoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-8")
            .to_lowercase();
        let chunks = spec
            .get("chunks")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("streams[].chunks must be an array"))?;
        let mut decoded = Vec::new();
        for chunk in chunks {
            let chunk_str = chunk
                .as_str()
                .ok_or_else(|| anyhow!("streams[].chunks must contain strings"))?;
            decoded.push(decode_chunk(chunk_str, &encoding)?);
        }
        let handle = ctx.streams_mut().register_chunks(decoded, &encoding);
        set_path_value(state, target, handle);
    }
    Ok(())
}

fn test_checker(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let expected = input
        .get("expected")
        .cloned()
        .ok_or_else(|| anyhow!("expected output is required"))?;

    let compose_steps = ensure_compose(&input)?;

    let mut initial_state = input
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let fail_fast = input
        .get("failFast")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if let Some(stream_specs) = input.get("streams") {
        register_streams(ctx, &mut initial_state, stream_specs)?;
    }

    let start = Instant::now();
    let exec_result = run_compose(ctx, &compose_steps, initial_state);
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut report = Map::new();
    report.insert("expected".to_string(), expected.clone());
    report.insert(
        "durationMs".to_string(),
        Value::Number(serde_json::Number::from_f64(duration_ms).unwrap_or_else(|| 0.into())),
    );
    let mut messages = Vec::new();

    match exec_result {
        Ok(actual) => {
            let success = matches_expected(&actual, &expected);
            report.insert("success".to_string(), Value::Bool(success));
            report.insert("actual".to_string(), actual.clone());
            if !success {
                messages.push(Value::String(
                    "Actual output differs from expected output".to_string(),
                ));
                let diff = simple_diff(&actual, &expected);
                report.insert("diffs".to_string(), Value::Array(vec![diff]));
                if !fail_fast {
                    // Future: collect additional differences when available
                }
            }
        }
        Err(err) => {
            report.insert("success".to_string(), Value::Bool(false));
            report.insert(
                "actual".to_string(),
                json!({ "error": { "message": err.to_string() } }),
            );
            messages.push(Value::String(format!("Compose execution failed: {err}")));
        }
    }

    if !messages.is_empty() {
        report.insert("messages".to_string(), Value::Array(messages));
    }

    Ok(Value::Object(report))
}
