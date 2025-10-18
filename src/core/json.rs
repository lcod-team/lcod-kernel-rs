use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::ser::{PrettyFormatter, Serializer};
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const CONTRACT_ENCODE: &str = "lcod://contract/core/json/encode@1";
const CONTRACT_DECODE: &str = "lcod://contract/core/json/decode@1";
const AXIOM_ENCODE: &str = "lcod://axiom/json/encode@1";
const AXIOM_DECODE: &str = "lcod://axiom/json/decode@1";

pub fn register_json(registry: &Registry) {
    registry.register(CONTRACT_ENCODE, json_encode_contract);
    registry.register(CONTRACT_DECODE, json_decode_contract);
    registry.set_binding(AXIOM_ENCODE, CONTRACT_ENCODE);
    registry.set_binding(AXIOM_DECODE, CONTRACT_DECODE);
}

fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut new_map = Map::with_capacity(entries.len());
            for (key, val) in entries {
                new_map.insert(key.clone(), sort_keys(val));
            }
            Value::Object(new_map)
        }
        Value::Array(list) => Value::Array(list.iter().map(sort_keys).collect()),
        _ => value.clone(),
    }
}

fn encode_with_indent(value: &Value, indent: usize) -> Result<String> {
    if indent == 0 {
        return Ok(serde_json::to_string(value)?);
    }
    let indent_bytes = vec![b' '; indent];
    let formatter = PrettyFormatter::with_indent(&indent_bytes);
    let mut buf = Vec::new();
    {
        let mut serializer = Serializer::with_formatter(&mut buf, formatter);
        value.serialize(&mut serializer)?;
    }
    Ok(String::from_utf8(buf)?)
}

fn escape_non_ascii(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch > '\u{7F}' {
            escaped.push_str(&format!("\\u{:04x}", ch as u32));
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

fn json_encode_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let mut value = input.get("value").cloned().unwrap_or(Value::Null);
    let sort_keys_flag = input
        .get("sortKeys")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ascii_only = input
        .get("asciiOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let space = input
        .get("space")
        .and_then(Value::as_u64)
        .map(|v| v.min(10) as usize)
        .unwrap_or(0);

    if sort_keys_flag {
        value = sort_keys(&value);
    }

    let mut text = encode_with_indent(&value, space)?;
    if ascii_only {
        text = escape_non_ascii(&text);
    }
    let bytes = text.as_bytes().len();

    Ok(json!({ "text": text, "bytes": bytes }))
}

fn json_decode_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let text = input
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`text` must be provided"))?;
    match serde_json::from_str::<Value>(text) {
        Ok(value) => Ok(json!({ "value": value, "bytes": text.as_bytes().len() })),
        Err(err) => {
            let mut error = json!({
                "code": "JSON_PARSE",
                "message": err.to_string()
            });
            if let Some(loc) = err.line().checked_sub(1) {
                error["line"] = json!(loc + 1);
            }
            error["column"] = json!(err.column());
            Ok(json!({ "error": error }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    #[test]
    fn encode_defaults_to_compact_string() {
        let registry = Registry::new();
        register_json(&registry);
        let mut ctx = registry.context();
        let res = json_encode_contract(
            &mut ctx,
            json!({ "value": { "b": 2, "a": 1 }, "sortKeys": true }),
            None,
        )
        .unwrap();
        assert_eq!(res["text"], json!("{\"a\":1,\"b\":2}"));
    }

    #[test]
    fn decode_returns_value() {
        let registry = Registry::new();
        register_json(&registry);
        let mut ctx = registry.context();
        let res =
            json_decode_contract(&mut ctx, json!({ "text": "{\"flag\":true}" }), None).unwrap();
        assert_eq!(res["value"], json!({ "flag": true }));
    }
}
