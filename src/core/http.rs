use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as AnyhowContext, Result};
use base64::Engine as _;
use curl::easy::{Easy, List};
use percent_encoding::{percent_encode, AsciiSet, CONTROLS};
use serde_json::{json, Map, Number, Value};

use crate::registry::{Context, Registry};

const CONTRACT_HTTP_REQUEST: &str = "lcod://contract/core/http/request@1";
const QUERY_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'#')
    .add(b'?')
    .add(b'=')
    .add(b'&');

pub fn register_http(registry: &Registry) {
    registry.register(CONTRACT_HTTP_REQUEST, http_request_contract);
}

fn http_request_contract(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let method = input
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    let url = input
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`url` is required"))?;

    let url = build_url(url, input.get("query"))?;

    let mut easy = Easy::new();
    easy.url(&url)?;
    easy.useragent("lcod-kernel-rs")?;
    if let Some(timeout) = input.get("timeoutMs").and_then(Value::as_u64) {
        easy.timeout(Duration::from_millis(timeout))?;
    }
    match input.get("followRedirects").and_then(Value::as_bool) {
        Some(true) | None => easy.follow_location(true)?,
        Some(false) => easy.follow_location(false)?,
    }
    easy.custom_request(&method)?;

    if let Some(headers) = input.get("headers").and_then(Value::as_object) {
        let list = build_header_list(headers)?;
        easy.http_headers(list)?;
    }

    let body_bytes = if let Some(body) = input.get("body") {
        let encoding = input
            .get("bodyEncoding")
            .and_then(Value::as_str)
            .unwrap_or("none");
        Some(encode_body(body, encoding)?)
    } else {
        None
    };

    if let Some(ref body) = body_bytes {
        easy.upload(true)?;
        let mut body_clone = body.clone();
        easy.read_function(move |buf| {
            let amt = std::cmp::min(buf.len(), body_clone.len());
            let data = body_clone[..amt].to_vec();
            body_clone.drain(..amt);
            buf[..amt].copy_from_slice(&data);
            Ok(amt)
        })?;
        if method == "POST" {
            easy.post(true)?;
            easy.post_field_size(body.len() as u64)?;
        }
    }

    let response_body: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let header_map: Arc<Mutex<BTreeMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(BTreeMap::new()));

    {
        let body_ref = Arc::clone(&response_body);
        easy.write_function(move |data| {
            if let Ok(mut lock) = body_ref.lock() {
                lock.extend_from_slice(data);
            }
            Ok(data.len())
        })?;
    }

    {
        let header_ref = Arc::clone(&header_map);
        easy.header_function(move |header| {
            if let Ok(text) = std::str::from_utf8(header) {
                if let Some((name, value)) = text.split_once(':') {
                    if let Ok(mut map) = header_ref.lock() {
                        let key = name.trim().to_ascii_lowercase();
                        let val = value.trim().to_string();
                        map.entry(key).or_default().push(val);
                    }
                }
            }
            true
        })?;
    }

    let start = Instant::now();
    easy.perform()
        .with_context(|| format!("HTTP request to {url} failed"))?;
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    let status = easy.response_code()? as u16;

    let header_map = header_map
        .lock()
        .map_err(|_| anyhow!("failed to acquire header map"))?
        .clone();
    let response_body = response_body
        .lock()
        .map_err(|_| anyhow!("failed to acquire response body"))?
        .clone();

    let mut output = Map::new();
    output.insert("status".to_string(), Value::Number(status.into()));
    output.insert(
        "headers".to_string(),
        Value::Object(map_to_value(header_map.clone())),
    );
    output.insert(
        "timings".to_string(),
        json!({
            "durationMs": Number::from_f64(duration_ms).unwrap_or_else(|| Number::from(0))
        }),
    );

    let response_mode = input
        .get("responseMode")
        .and_then(Value::as_str)
        .unwrap_or("buffer");

    if response_mode.eq_ignore_ascii_case("stream") {
        let chunks = chunk_bytes(&response_body);
        let handle = ctx.streams_mut().register_chunks(chunks, "base64");
        output.insert("stream".to_string(), handle);
        output.insert(
            "bodyEncoding".to_string(),
            Value::String("base64".to_string()),
        );
    } else {
        let (body_value, body_encoding) = encode_response_body(&header_map, &response_body);
        output.insert("body".to_string(), body_value);
        output.insert("bodyEncoding".to_string(), Value::String(body_encoding));
    }

    Ok(Value::Object(output))
}

fn build_url(base: &str, query: Option<&Value>) -> Result<String> {
    let mut url = base.to_string();
    if let Some(map) = query.and_then(Value::as_object) {
        let mut pairs = Vec::new();
        for (key, value) in map {
            match value {
                Value::Array(values) => {
                    for entry in values {
                        pairs.push((key.as_str(), value_to_string(entry)?));
                    }
                }
                _ => pairs.push((key.as_str(), value_to_string(value)?)),
            }
        }
        if !pairs.is_empty() {
            let mut query_string = String::new();
            for (idx, (key, value)) in pairs.into_iter().enumerate() {
                if idx > 0 {
                    query_string.push('&');
                }
                query_string.push_str(&encode_query_component(key));
                query_string.push('=');
                query_string.push_str(&encode_query_component(&value));
            }
            if url.contains('?') {
                url.push('&');
                url.push_str(&query_string);
            } else {
                url.push('?');
                url.push_str(&query_string);
            }
        }
    }
    Ok(url)
}

fn encode_query_component(value: &str) -> String {
    percent_encode(value.as_bytes(), QUERY_ENCODE_SET).to_string()
}

fn build_header_list(headers: &Map<String, Value>) -> Result<List> {
    let mut list = List::new();
    for (key, value) in headers {
        match value {
            Value::Array(values) => {
                for entry in values {
                    list.append(&format!("{}: {}", key, value_to_string(entry)?))?;
                }
            }
            _ => list.append(&format!("{}: {}", key, value_to_string(value)?))?,
        }
    }
    Ok(list)
}

fn encode_body(body: &Value, encoding: &str) -> Result<Vec<u8>> {
    match encoding {
        "json" => {
            if let Value::String(raw) = body {
                let parsed: Value = serde_json::from_str(raw).map_err(|err| {
                    anyhow!("bodyEncoding=json but value is not valid JSON: {err}")
                })?;
                Ok(serde_json::to_vec(&parsed)?)
            } else {
                Ok(serde_json::to_vec(body)?)
            }
        }
        "base64" => {
            let raw = body
                .as_str()
                .ok_or_else(|| anyhow!("base64 body must be a string"))?;
            base64::engine::general_purpose::STANDARD
                .decode(raw)
                .map_err(|err| anyhow!("invalid base64 body: {err}"))
        }
        "form" => {
            let obj = body
                .as_object()
                .ok_or_else(|| anyhow!("form body must be an object"))?;
            let mut pairs = Vec::new();
            for (key, value) in obj {
                match value {
                    Value::Array(values) => {
                        for entry in values {
                            pairs.push((key.as_str(), value_to_string(entry)?));
                        }
                    }
                    _ => pairs.push((key.as_str(), value_to_string(value)?)),
                }
            }
            let mut encoded = String::new();
            for (idx, (key, value)) in pairs.into_iter().enumerate() {
                if idx > 0 {
                    encoded.push('&');
                }
                encoded.push_str(&encode_query_component(key));
                encoded.push('=');
                encoded.push_str(&encode_query_component(&value));
            }
            Ok(encoded.into_bytes())
        }
        _ => Ok(value_to_string(body)?.into_bytes()),
    }
}

fn value_to_string(value: &Value) -> Result<String> {
    Ok(match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other)
            .map_err(|err| anyhow!("unable to serialise value: {err}"))?,
    })
}

fn map_to_value(map: BTreeMap<String, Vec<String>>) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, values) in map {
        out.insert(
            key,
            Value::Array(values.into_iter().map(Value::String).collect()),
        );
    }
    out
}

fn encode_response_body(headers: &BTreeMap<String, Vec<String>>, bytes: &[u8]) -> (Value, String) {
    if let Some(values) = headers.get("content-type") {
        if let Some(ct) = values.first() {
            let lowered = ct.to_lowercase();
            if lowered.contains("application/json") {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    if let Ok(json) = serde_json::from_str::<Value>(text) {
                        return (json, "json".to_string());
                    }
                }
            }
            if lowered.contains("text/") || lowered.contains("application/xml") {
                if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                    return (Value::String(text), "utf-8".to_string());
                }
            }
        }
    }

    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        return (Value::String(text), "utf-8".to_string());
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    (Value::String(encoded), "base64".to_string())
}

fn chunk_bytes(bytes: &[u8]) -> Vec<Vec<u8>> {
    const CHUNK_SIZE: usize = 64 * 1024;
    if bytes.is_empty() {
        return vec![Vec::new()];
    }
    bytes
        .chunks(CHUNK_SIZE)
        .map(|chunk| chunk.to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn spawn_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{}", addr)
    }

    #[test]
    fn performs_buffered_request() {
        let response =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"message\":\"world\"}";
        let url = spawn_server(response) + "/hello";

        let registry = Registry::new();
        register_http(&registry);
        let mut ctx = registry.context();

        let input = json!({
            "method": "GET",
            "url": url,
            "headers": { "accept": "application/json" }
        });

        let result = http_request_contract(&mut ctx, input, None).unwrap();
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["bodyEncoding"], json!("json"));
        assert_eq!(result["body"]["message"], json!("world"));
    }

    #[test]
    fn streams_response_body() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nchunked payload";
        let url = spawn_server(response) + "/stream";

        let registry = Registry::new();
        register_http(&registry);
        let mut ctx = registry.context();

        let input = json!({
            "method": "GET",
            "url": url,
            "responseMode": "stream"
        });

        let result = http_request_contract(&mut ctx, input, None).unwrap();
        assert_eq!(result["status"], json!(200));
        let handle = result["stream"].clone();
        let chunk = ctx
            .streams_mut()
            .read(&handle, None, Some("utf-8"))
            .unwrap();
        assert_eq!(chunk["chunk"], json!("chunked payload"));
    }
}
