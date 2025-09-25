use std::collections::HashMap;

use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Map, Value};

#[derive(Default)]
pub struct StreamManager {
    entries: HashMap<String, StreamEntry>,
    counter: u64,
}

struct StreamEntry {
    handle: Value,
    encoding: String,
    chunks: Vec<Vec<u8>>,
    pending: Vec<u8>,
    index: usize,
    done: bool,
    seq: u64,
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            counter: 0,
        }
    }

    pub fn register_chunks<I>(&mut self, chunks: I, encoding: &str) -> Value
    where
        I: IntoIterator<Item = Vec<u8>>,
    {
        self.counter += 1;
        let id = format!("stream-{}", self.counter);
        let handle = Value::Object({
            let mut map = Map::new();
            map.insert("id".to_string(), Value::String(id.clone()));
            map.insert("encoding".to_string(), Value::String(encoding.to_string()));
            map
        });
        let entry = StreamEntry {
            handle: handle.clone(),
            encoding: encoding.to_string(),
            chunks: chunks.into_iter().collect(),
            pending: Vec::new(),
            index: 0,
            done: false,
            seq: 0,
        };
        self.entries.insert(id, entry);
        handle
    }

    pub fn read(
        &mut self,
        stream: &Value,
        max_bytes: Option<usize>,
        decode: Option<&str>,
    ) -> Result<Value> {
        let id = extract_id(stream)?;
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| anyhow!("Unknown stream handle: {id}"))?;

        if entry.done && entry.pending.is_empty() {
            return Ok(json!({
                "done": true,
                "stream": entry.handle.clone()
            }));
        }

        let mut buffer = std::mem::take(&mut entry.pending);

        if let Some(limit) = max_bytes {
            while buffer.len() < limit && entry.index < entry.chunks.len() {
                let chunk = entry.chunks[entry.index].clone();
                entry.index += 1;
                buffer.extend_from_slice(&chunk);
            }
        } else {
            while entry.index < entry.chunks.len() {
                let chunk = entry.chunks[entry.index].clone();
                entry.index += 1;
                buffer.extend_from_slice(&chunk);
            }
        }

        if entry.index >= entry.chunks.len() {
            entry.done = true;
        }

        if buffer.is_empty() {
            return Ok(json!({
                "done": true,
                "stream": entry.handle.clone()
            }));
        }

        let mut carry = Vec::new();
        if let Some(limit) = max_bytes {
            if buffer.len() > limit {
                carry = buffer.split_off(limit);
            }
        }
        entry.pending = carry;

        let encoding = decode
            .map(|s| s.to_string())
            .unwrap_or_else(|| entry.encoding.clone());
        let output = match encoding.as_str() {
            "utf-8" | "utf8" => String::from_utf8(buffer.clone())?,
            "base64" => base64::engine::general_purpose::STANDARD.encode(&buffer),
            _ => base64::engine::general_purpose::STANDARD.encode(&buffer),
        };

        let seq = entry.seq;
        entry.seq += 1;

        Ok(json!({
            "done": false,
            "chunk": output,
            "encoding": if encoding == "utf8" { "utf-8" } else { encoding.as_str() },
            "bytes": buffer.len(),
            "seq": seq,
            "stream": entry.handle.clone()
        }))
    }

    pub fn close(&mut self, stream: &Value) -> Result<Value> {
        let id = extract_id(stream)?;
        self.entries
            .remove(&id)
            .ok_or_else(|| anyhow!("Unknown stream handle: {id}"))?;
        Ok(json!({ "released": true }))
    }

    pub fn contains_handle(&self, stream: &Value) -> bool {
        extract_id(stream)
            .ok()
            .map(|id| self.entries.contains_key(&id))
            .unwrap_or(false)
    }
}

fn extract_id(stream: &Value) -> Result<String> {
    let obj = stream
        .as_object()
        .ok_or_else(|| anyhow!("Invalid stream handle"))?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Invalid stream handle"))?;
    Ok(id.to_string())
}
