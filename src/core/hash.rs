use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::registry::{Context, Registry};

const CONTRACT_SHA256: &str = "lcod://contract/core/hash/sha256@1";

pub fn register_hash(registry: &Registry) {
    registry.register(CONTRACT_SHA256, hash_sha256_contract);
}

fn hash_sha256_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let (buffer, bytes) = read_buffer(&input)?;
    let mut hasher = Sha256::new();
    hasher.update(&buffer);
    let digest = hasher.finalize();
    let hex = hex::encode(&digest);
    let base64 = base64::engine::general_purpose::STANDARD.encode(digest);
    Ok(json!({
        "hex": hex,
        "base64": base64,
        "bytes": bytes
    }))
}

fn read_buffer(input: &Value) -> Result<(Vec<u8>, usize)> {
    if let Some(data) = input.get("data").and_then(Value::as_str) {
        let encoding = input
            .get("encoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-8");
        let buffer = match encoding.to_lowercase().as_str() {
            "base64" => base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|err| anyhow!("invalid base64 payload: {err}"))?,
            "hex" => hex::decode(data).map_err(|err| anyhow!("invalid hex payload: {err}"))?,
            _ => data.as_bytes().to_vec(),
        };
        let len = buffer.len();
        return Ok((buffer, len));
    }

    if let Some(path) = input.get("path").and_then(Value::as_str) {
        let buf = fs::read(Path::new(path))
            .map_err(|err| anyhow!("unable to read file `{path}`: {err}"))?;
        let len = buf.len();
        return Ok((buf, len));
    }

    Err(anyhow!("missing `data` or `path` for hash input"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_data_utf8() {
        let input = json!({ "data": "hello", "encoding": "utf-8" });
        let (_buf, bytes) = read_buffer(&input).unwrap();
        assert_eq!(bytes, 5);
    }
}
