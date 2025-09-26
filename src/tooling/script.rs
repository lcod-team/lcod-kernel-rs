use anyhow::{anyhow, Result};
use quick_js::{Context as JsContext, JsValue};
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

use super::common;

const CONTRACT_ID: &str = "lcod://tooling/script@1";

pub(crate) fn register_script_contract(registry: &Registry) {
    registry.register(CONTRACT_ID, script_contract);
}

fn script_contract(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let language = input
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("javascript");
    if language != "javascript" {
        return Err(anyhow!("Unsupported scripting language: {language}"));
    }

    let source = input
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Script source must be provided"))?;
    if source.trim().is_empty() {
        return Err(anyhow!("Script source must be a non-empty string"));
    }

    let _timeout_ms = input
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(1000);

    let mut initial_state = input
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(stream_specs) = input.get("streams") {
        common::register_streams(ctx, &mut initial_state, stream_specs)?;
    }

    let bindings = build_bindings(&initial_state, input.get("bindings"));
    let meta = input
        .get("meta")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    let scope = json!({
        "input": bindings,
        "state": initial_state,
        "meta": meta
    });

    let evaluation = execute_script(source, &scope);

    match evaluation {
        Ok(result) => Ok(result),
        Err(err) => Ok(json!({
            "success": false,
            "messages": [err.to_string()]
        })),
    }
}

fn execute_script(source: &str, scope: &Value) -> Result<Value> {
    let context = JsContext::new().map_err(|err| anyhow!("unable to create JS context: {err}"))?;

    let scope_json = serde_json::to_string(scope)?;
    let scope_literal = serde_json::to_string(&scope_json)?;

    let wrapper = format!(
        r#"
        (function() {{
            const scope = JSON.parse({scope_literal});
            const api = {{
                call: () => {{ throw new Error('api.call not implemented yet'); }},
                runSlot: () => {{ throw new Error('api.runSlot not implemented yet'); }},
                log: () => undefined
            }};
            const userFn = ({source});
            return userFn(scope, api);
        }})()
        "#
    );

    let js_value = context
        .eval(&wrapper)
        .map_err(|err| anyhow!("script execution failed: {err}"))?;

    Ok(js_value_to_json(js_value))
}

fn js_value_to_json(value: JsValue) -> Value {
    match value {
        JsValue::Null => Value::Null,
        JsValue::Undefined => Value::Null,
        JsValue::Bool(b) => Value::Bool(b),
        JsValue::Int(n) => Value::Number(serde_json::Number::from(n)),
        JsValue::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        JsValue::String(s) => Value::String(s),
        JsValue::Array(items) => Value::Array(items.into_iter().map(js_value_to_json).collect()),
        JsValue::Object(entries) => {
            let mut map = Map::new();
            for (key, val) in entries {
                map.insert(key, js_value_to_json(val));
            }
            Value::Object(map)
        }
        #[cfg(feature = "chrono")]
        JsValue::Date(dt) => Value::String(dt.to_rfc3339()),
        #[cfg(feature = "bigint")]
        JsValue::BigInt(big) => Value::String(big.to_string()),
        JsValue::__NonExhaustive => Value::Null,
    }
}

fn build_bindings(state: &Value, bindings: Option<&Value>) -> Value {
    let mut out = Map::new();
    let Some(spec) = bindings.and_then(Value::as_object) else {
        return Value::Object(out);
    };

    for (name, descriptor) in spec {
        if let Some(desc_obj) = descriptor.as_object() {
            if let Some(literal) = desc_obj.get("value") {
                out.insert(name.clone(), literal.clone());
                continue;
            }
            if let Some(path) = desc_obj.get("path").and_then(Value::as_str) {
                if let Some(resolved) = resolve_path(state, path) {
                    out.insert(name.clone(), resolved.clone());
                    continue;
                }
                if let Some(default_value) = desc_obj.get("default") {
                    out.insert(name.clone(), default_value.clone());
                }
            }
        }
    }

    Value::Object(out)
}

fn resolve_path<'a>(state: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Some(state);
    }
    let normalized = if let Some(rest) = trimmed.strip_prefix("$.") {
        rest
    } else if trimmed == "$" {
        ""
    } else {
        trimmed
    };

    if normalized.is_empty() {
        return Some(state);
    }

    let mut cursor = state;
    for part in normalized.split('.') {
        if part.is_empty() {
            continue;
        }
        cursor = cursor.get(part)?;
    }
    Some(cursor)
}
