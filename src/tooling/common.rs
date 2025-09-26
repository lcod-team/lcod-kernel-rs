use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{Map, Value};

use crate::registry::Context;

pub(crate) fn set_path_value(target: &mut Value, path: &str, new_value: Value) {
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
        cursor = map
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(obj) = cursor.as_object_mut() {
        obj.insert(parts[parts.len() - 1].to_string(), new_value);
    }
}

pub(crate) fn decode_chunk(chunk: &str, encoding: &str) -> Result<Vec<u8>> {
    match encoding {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(chunk)
            .map_err(|err| anyhow!("invalid base64 chunk: {err}")),
        "hex" => hex::decode(chunk).map_err(|err| anyhow!("invalid hex chunk: {err}")),
        _ => Ok(chunk.as_bytes().to_vec()),
    }
}

pub(crate) fn register_streams(ctx: &mut Context, state: &mut Value, specs: &Value) -> Result<()> {
    let list = specs
        .as_array()
        .ok_or_else(|| anyhow!("streams must be an array"))?;
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
