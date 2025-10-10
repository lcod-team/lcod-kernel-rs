use std::io::{stderr, stdout, Write};

use anyhow::{anyhow, Result};
use humantime::format_rfc3339;
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const LOG_CONTRACT_ID: &str = "lcod://contract/tooling/log@1";
const KERNEL_HELPER_ID: &str = "lcod://kernel/log@1";
const LOG_CONTEXT_ID: &str = "lcod://tooling/log.context@1";

const ALLOWED_LEVELS: [&str; 6] = ["trace", "debug", "info", "warn", "error", "fatal"];

fn current_timestamp() -> String {
    let now = std::time::SystemTime::now();
    format_rfc3339(now).to_string()
}

fn stable_tags(value: Option<Value>) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    if let Some(Value::Object(obj)) = value {
        for (key, val) in obj {
            match val {
                Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                    out.insert(key, val);
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

fn scope_tags(ctx: &Context) -> Map<String, Value> {
    let mut merged = Map::new();
    for map in ctx.log_tag_stack() {
        for (k, v) in map {
            merged.insert(k.clone(), v.clone());
        }
    }
    merged
}

fn write_fallback(entry: &Map<String, Value>) {
    if let Ok(serialized) = serde_json::to_string(entry) {
        let level = entry
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        if matches!(level, "error" | "fatal") {
            let _ = writeln!(stderr(), "{}", serialized);
        } else {
            let _ = writeln!(stdout(), "{}", serialized);
        }
    }
}

fn emit_log(ctx: &mut Context, input: Value, kernel_tags: bool) -> Result<Value> {
    let mut payload = match input {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => return Err(anyhow!("log payload must be an object, got {other:?}")),
    };

    let level = payload
        .remove("level")
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .ok_or_else(|| anyhow!("log payload missing 'level'"))?;
    if !ALLOWED_LEVELS.contains(&level.as_str()) {
        return Err(anyhow!("unsupported log level: {level}"));
    }

    let message = payload
        .remove("message")
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .filter(|m| !m.is_empty())
        .ok_or_else(|| anyhow!("log payload missing 'message'"))?;

    let mut entry = Map::new();
    entry.insert("level".to_string(), Value::String(level.clone()));
    entry.insert("message".to_string(), Value::String(message));

    if let Some(data) = payload.remove("data") {
        if !matches!(data, Value::Object(_)) {
            return Err(anyhow!("log 'data' must be an object"));
        }
        entry.insert("data".to_string(), data);
    }

    if let Some(error) = payload.remove("error") {
        if !matches!(error, Value::Object(_)) {
            return Err(anyhow!("log 'error' must be an object"));
        }
        entry.insert("error".to_string(), error);
    }

    let mut tags = scope_tags(ctx);
    if kernel_tags {
        tags.insert("component".to_string(), Value::String("kernel".to_string()));
    }

    if let Some(payload_tags) = payload.remove("tags") {
        let extra = stable_tags(Some(payload_tags))?;
        for (k, v) in extra {
            tags.insert(k, v);
        }
    }

    if !tags.is_empty() {
        entry.insert("tags".to_string(), Value::Object(tags.clone()));
    }

    let timestamp = payload.remove("timestamp");
    match timestamp {
        Some(Value::String(ts)) => {
            entry.insert("timestamp".to_string(), Value::String(ts));
        }
        Some(_) => return Err(anyhow!("log 'timestamp' must be a string")),
        None => {
            entry.insert("timestamp".to_string(), Value::String(current_timestamp()));
        }
    }

    if let Some(target) = ctx.binding_for(LOG_CONTRACT_ID) {
        if target != LOG_CONTRACT_ID && target != KERNEL_HELPER_ID {
            let cloned = ctx.registry_clone();
            match cloned.call(ctx, &target, Value::Object(entry.clone()), None) {
                Ok(value) => return Ok(if value.is_null() { Value::Object(entry) } else { value }),
                Err(err) => {
                    let fallback = json!({
                        "level": "error",
                        "message": "log contract handler failed",
                        "data": { "error": err.to_string() },
                        "timestamp": current_timestamp(),
                        "tags": Value::Object(tags)
                    });
                    if let Value::Object(map) = fallback {
                        write_fallback(&map);
                    }
                }
            }
            return Ok(Value::Null);
        }
    }

    write_fallback(&entry);
    Ok(Value::Object(entry))
}

fn log_context(ctx: &mut Context, input: Value, meta: Option<Value>) -> Result<Value> {
    let map = match input {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        _ => return Err(anyhow!("log.context input must be an object")),
    };

    let tags = stable_tags(map.get("tags").cloned())?;
    let pushed = !tags.is_empty();
    if pushed {
        ctx.push_log_tags(tags);
    }

    let has_children = meta
        .as_ref()
        .and_then(|value| value.as_object())
        .and_then(|map| map.get("children"))
        .is_some();

    let result = if has_children {
        ctx.run_slot("children", None, None)?
    } else {
        Value::Object(Map::new())
    };

    if pushed {
        ctx.pop_log_tags();
    }
    Ok(result)
}

pub fn register_logging(registry: &Registry) {
    registry.register(LOG_CONTRACT_ID, log_contract_impl);
    registry.register(KERNEL_HELPER_ID, kernel_log_impl);
    registry.register(LOG_CONTEXT_ID, log_context);
}

fn log_contract_impl(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    emit_log(ctx, input, false)
}

fn kernel_log_impl(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    emit_log(ctx, input, true)
}
