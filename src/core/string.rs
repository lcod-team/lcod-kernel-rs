use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

const CONTRACT_FORMAT: &str = "lcod://contract/core/string/format@1";
const CONTRACT_SPLIT: &str = "lcod://contract/core/string/split@1";
const CONTRACT_TRIM: &str = "lcod://contract/core/string/trim@1";
const AXIOM_FORMAT: &str = "lcod://axiom/string/format@1";

pub fn register_string(registry: &Registry) {
    registry.register(CONTRACT_FORMAT, string_format_contract);
    registry.register(CONTRACT_SPLIT, string_split_contract);
    registry.register(CONTRACT_TRIM, string_trim_contract);
    registry.set_binding(AXIOM_FORMAT, CONTRACT_FORMAT);
}

#[derive(Debug)]
enum PlaceholderSegment {
    Key(String),
    Index(usize),
}

fn parse_segments(expression: &str) -> Result<Vec<PlaceholderSegment>> {
    let mut segments = Vec::new();
    let bytes = expression.as_bytes();
    let mut buffer = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                if !buffer.is_empty() {
                    segments.push(PlaceholderSegment::Key(buffer.clone()));
                    buffer.clear();
                }
                i += 1;
            }
            b'[' => {
                if !buffer.is_empty() {
                    segments.push(PlaceholderSegment::Key(buffer.clone()));
                    buffer.clear();
                }
                let close = expression[i + 1..]
                    .find(']')
                    .ok_or_else(|| anyhow!("unmatched '[' in placeholder"))?
                    + i
                    + 1;
                let token = expression[i + 1..close].trim();
                if token.is_empty() {
                    return Err(anyhow!("empty index in placeholder"));
                }
                if let Ok(index) = token.parse::<usize>() {
                    segments.push(PlaceholderSegment::Index(index));
                } else {
                    segments.push(PlaceholderSegment::Key(token.to_string()));
                }
                i = close + 1;
            }
            _ => {
                buffer.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    if !buffer.is_empty() {
        segments.push(PlaceholderSegment::Key(buffer));
    }
    Ok(segments)
}

fn resolve_token<'a>(root: &'a Value, token: &str) -> Option<&'a Value> {
    let segments = parse_segments(token).ok()?;
    let mut current = root;
    for segment in segments {
        match (segment, current) {
            (PlaceholderSegment::Key(key), Value::Object(map)) => current = map.get(&key)?,
            (PlaceholderSegment::Index(idx), Value::Array(vec)) => current = vec.get(idx)?,
            _ => return None,
        }
    }
    Some(current)
}

fn string_format_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let template = input
        .get("template")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`template` must be a string"))?;
    let values = input.get("values").cloned().unwrap_or(Value::Null);
    let fallback = input.get("fallback").and_then(Value::as_str).unwrap_or("");
    let missing_policy = input
        .get("missingPolicy")
        .and_then(Value::as_str)
        .unwrap_or("ignore");

    let mut output = String::with_capacity(template.len());
    let mut missing = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    output.push('{');
                    i += 2;
                    continue;
                }
                let close = template[i + 1..]
                    .find('}')
                    .ok_or_else(|| anyhow!("unmatched '{{' in template"))?
                    + i
                    + 1;
                let token = template[i + 1..close].trim();
                if token.is_empty() {
                    missing.push(String::new());
                    output.push_str(fallback);
                    i = close + 1;
                    continue;
                }
                if let Some(value) = resolve_token(&values, token) {
                    if value.is_null() {
                        output.push_str(fallback);
                    } else if let Some(str_value) = value.as_str() {
                        output.push_str(str_value);
                    } else {
                        output.push_str(&value.to_string());
                    }
                } else {
                    missing.push(token.to_string());
                    output.push_str(fallback);
                }
                i = close + 1;
            }
            b'}' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    output.push('}');
                    i += 2;
                } else {
                    output.push('}');
                    i += 1;
                }
            }
            byte => {
                output.push(byte as char);
                i += 1;
            }
        }
    }

    let mut result = json!({ "value": output });
    if !missing.is_empty() {
        if missing_policy == "error" {
            result.as_object_mut().unwrap().insert(
                "error".to_string(),
                json!({
                    "code": "MISSING_PLACEHOLDER",
                    "message": format!("Missing placeholders: {}", missing.join(", ")),
                    "missingKeys": missing
                }),
            );
        } else {
            result
                .as_object_mut()
                .unwrap()
                .insert("missing".to_string(), json!(missing));
        }
    }

    Ok(result)
}

fn string_split_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let text = input
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`text` must be a string"))?;
    let separator = input
        .get("separator")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`separator` must be a string"))?;
    if separator.is_empty() {
        return Err(anyhow!("`separator` must not be empty"));
    }
    let trim = input
        .get("trim")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let remove_empty = input
        .get("removeEmpty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize);

    let mut iterator: Vec<String> = text.split(separator).map(|s| s.to_string()).collect();
    if let Some(limit) = limit {
        if iterator.len() > limit {
            iterator.truncate(limit);
        }
    }

    let mut segments = Vec::new();
    for mut segment in iterator {
        if trim {
            segment = segment.trim().to_string();
        }
        if remove_empty && segment.is_empty() {
            continue;
        }
        segments.push(segment);
    }

    Ok(json!({ "segments": segments }))
}

fn string_trim_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let text = input
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`text` must be a string"))?;
    let mode = input
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("both");
    let trimmed = match mode {
        "start" => text.trim_start().to_string(),
        "end" => text.trim_end().to_string(),
        _ => text.trim().to_string(),
    };
    Ok(json!({ "value": trimmed }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    #[test]
    fn format_inserts_placeholders() {
        let registry = Registry::new();
        register_string(&registry);
        let mut ctx = registry.context();
        let res = string_format_contract(
            &mut ctx,
            json!({
                "template": "Hello {user.name}",
                "values": { "user": { "name": "Ada" } }
            }),
            None,
        )
        .unwrap();
        assert_eq!(res["value"], json!("Hello Ada"));
    }

    #[test]
    fn split_honours_trim_and_limit() {
        let registry = Registry::new();
        register_string(&registry);
        let mut ctx = registry.context();
        let res = string_split_contract(
            &mut ctx,
            json!({
                "text": "a, b, ,c",
                "separator": ",",
                "trim": true,
                "removeEmpty": true
            }),
            None,
        )
        .unwrap();
        assert_eq!(res["segments"], json!(["a", "b", "c"]));
    }

    #[test]
    fn trim_supports_modes() {
        let registry = Registry::new();
        register_string(&registry);
        let mut ctx = registry.context();
        let both = string_trim_contract(
            &mut ctx,
            json!({ "text": "  hello  " }),
            None,
        )
        .unwrap();
        assert_eq!(both["value"], json!("hello"));
        let start = string_trim_contract(
            &mut ctx,
            json!({ "text": "  hello  ", "mode": "start" }),
            None,
        )
        .unwrap();
        assert_eq!(start["value"], json!("hello  "));
    }
}
