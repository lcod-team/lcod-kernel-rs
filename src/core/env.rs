use std::env;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

const CONTRACT_ENV_GET: &str = "lcod://contract/core/env/get@1";

pub fn register_env(registry: &Registry) {
    registry.register(CONTRACT_ENV_GET, env_get_contract);
}

fn env_get_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("`name` is required"))?;
    let required = input
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expand = input
        .get("expand")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let default_value = input.get("default").and_then(|value| match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    });

    let env_value = env::var(name).ok();
    let exists = env_value.is_some();
    let mut final_value = env_value.or(default_value);

    if required && final_value.is_none() {
        return Err(anyhow!("environment variable `{name}` is not defined"));
    }

    if expand {
        if let Some(current) = final_value.clone() {
            final_value = Some(expand_placeholders(&current));
        }
    }

    Ok(json!({
        "exists": exists,
        "value": final_value
    }))
}

fn expand_placeholders(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && matches!(chars.peek(), Some('{')) {
            chars.next(); // consume '{'
            let mut token = String::new();
            while let Some(next) = chars.next() {
                if next == '}' {
                    break;
                }
                token.push(next);
            }
            if !token.is_empty() {
                if let Ok(replacement) = env::var(&token) {
                    result.push_str(&replacement);
                }
            }
            continue;
        }
        result.push(ch);
    }
    result
}
