use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as AnyhowContext, Result};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use toml::Value as TomlValue;

use crate::core;
use crate::registry::{Context, Registry};

fn ensure_cache_dir(project_path: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(project_path.join(".lcod").join("cache"));
    if let Ok(env_cache) = env::var("LCOD_CACHE_DIR") {
        candidates.push(PathBuf::from(env_cache));
    }
    if let Ok(home) = env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".cache").join("lcod"));
    }

    for candidate in candidates {
        if candidate.as_os_str().is_empty() {
            continue;
        }
        if let Err(err) = fs::create_dir_all(&candidate) {
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                continue;
            }
        }
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    let fallback = project_path.join(".lcod").join("cache");
    fs::create_dir_all(&fallback)?;
    Ok(fallback)
}

pub fn register_resolver_axioms(registry: &Registry) {
    core::register_core(registry);

    alias_contract(
        registry,
        "lcod://contract/core/fs/read-file@1",
        "lcod://axiom/fs/read-file@1",
    );
    alias_contract(
        registry,
        "lcod://contract/core/fs/write-file@1",
        "lcod://axiom/fs/write-file@1",
    );
    alias_contract(
        registry,
        "lcod://contract/core/hash/sha256@1",
        "lcod://axiom/hash/sha256@1",
    );
    alias_contract(
        registry,
        "lcod://contract/core/http/request@1",
        "lcod://axiom/http/request@1",
    );
    alias_contract(
        registry,
        "lcod://contract/core/git/clone@1",
        "lcod://axiom/git/clone@1",
    );
    alias_contract(
        registry,
        "lcod://contract/core/parse/json@1",
        "lcod://axiom/json/parse@1",
    );
    alias_contract(
        registry,
        "lcod://contract/core/parse/toml@1",
        "lcod://axiom/toml/parse@1",
    );

    registry.register("lcod://axiom/toml/stringify@1", toml_stringify_axiom);
    registry.register("lcod://axiom/http/download@1", http_download_axiom);
    registry.register("lcod://tooling/resolver/cache-dir@1", cache_dir_axiom);
    registry.register("lcod://impl/set@1", impl_set_axiom);
    registry.register(
        "lcod://contract/tooling/resolve-dependency@1",
        resolve_dependency_contract,
    );
    registry.register(
        "lcod://contract/tooling/resolver/resolve_dependencies@1",
        resolve_dependencies_contract,
    );
}

fn alias_contract(registry: &Registry, contract_id: &'static str, alias_id: &'static str) {
    let contract = contract_id.to_string();
    registry.register(
        alias_id,
        move |ctx: &mut Context, input: Value, meta: Option<Value>| {
            ctx.call(&contract, input, meta)
        },
    );
}
fn cache_dir_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let project_path = input
        .get("projectPath")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cache = ensure_cache_dir(&project_path)?;
    Ok(json!({ "path": path_to_string(&cache)? }))
}

fn impl_set_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    Ok(input)
}

fn resolve_dependency_contract(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let dependency = input
        .get("dependency")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    Ok(json!({
        "resolved": {
            "id": dependency.clone(),
            "source": { "type": "registry", "reference": dependency },
            "dependencies": []
        },
        "warnings": [
            "contract/tooling/resolve-dependency@1 is deprecated; use the resolver compose pipeline instead."
        ]
    }))
}

fn resolve_dependencies_contract(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let project_root = value_as_path(&input, "projectPath")
        .unwrap_or_else(|_| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));

    let normalized_config = input
        .get("normalizedConfig")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let raw_config = input
        .get("config")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    let mut sources = collect_sources(normalized_config.get("sources"));
    if sources.is_empty() {
        sources = collect_sources(raw_config.get("sources"));
    }

    let root_descriptor = input
        .get("rootDescriptor")
        .cloned()
        .ok_or_else(|| anyhow!("resolve_dependencies contract requires rootDescriptor"))?;
    let root_descriptor_text = input
        .get("rootDescriptorText")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let root_id = root_descriptor
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| input.get("rootId").and_then(Value::as_str))
        .unwrap_or("lcod://root/unknown@0.0.0")
        .to_string();

    let mut visited = HashSet::new();
    let mut descriptor_cache = HashMap::new();

    let dependency_ids = parse_requires(&root_descriptor);
    let mut dependency_nodes = Vec::new();
    for dep_id in dependency_ids {
        let node = resolve_dependency_node(
            &dep_id,
            &project_root,
            &sources,
            &mut visited,
            &mut descriptor_cache,
        )?;
        dependency_nodes.push(node);
    }

    let warnings = collect_warning_buckets(&input);
    let warnings_value = Value::Array(warnings.iter().map(|w| Value::String(w.clone())).collect());
    let root_integrity = if root_descriptor_text.is_empty() {
        Value::Null
    } else {
        Value::String(compute_integrity(&root_descriptor_text))
    };

    let mut root_map = Map::new();
    root_map.insert("id".to_string(), Value::String(root_id.clone()));
    root_map.insert("requested".to_string(), Value::String(root_id.clone()));
    root_map.insert("resolved".to_string(), Value::String(root_id.clone()));
    root_map.insert(
        "source".to_string(),
        json!({
            "type": "path",
            "path": project_root.to_string_lossy()
        }),
    );
    root_map.insert("dependencies".to_string(), Value::Array(dependency_nodes));
    root_map.insert("integrity".to_string(), root_integrity);

    let resolver_result = json!({
        "root": Value::Object(root_map),
        "warnings": warnings_value.clone()
    });

    Ok(json!({
        "resolverResult": resolver_result,
        "warnings": warnings_value
    }))
}

fn collect_sources(value: Option<&Value>) -> HashMap<String, Value> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| {
                    if v.is_object() {
                        Some((k.clone(), v.clone()))
                    } else {
                        None
                    }
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default()
}

fn collect_warning_buckets(input: &Value) -> Vec<String> {
    let buckets = [
        "warnings",
        "loadWarnings",
        "indexWarnings",
        "registrationWarnings",
        "pointerWarnings",
    ];
    let mut collected = Vec::new();
    for key in buckets {
        if let Some(Value::Array(items)) = input.get(key) {
            for item in items {
                if let Some(text) = item.as_str() {
                    if !text.is_empty() {
                        collected.push(text.to_string());
                    }
                }
            }
        }
    }
    collected
}

fn resolve_dependency_node(
    dep_id: &str,
    project_root: &Path,
    sources: &HashMap<String, Value>,
    visited: &mut HashSet<String>,
    cache: &mut HashMap<String, (Value, String)>,
) -> Result<Value> {
    if !visited.insert(dep_id.to_string()) {
        return Err(anyhow!("dependency cycle detected for {dep_id}"));
    }

    let spec = sources
        .get(dep_id)
        .ok_or_else(|| anyhow!("no source specified for dependency {dep_id}"))?;
    let (descriptor, descriptor_text) =
        read_descriptor_for_spec(dep_id, spec, project_root, cache)?;
    let child_ids = parse_requires(&descriptor);
    let mut children = Vec::new();
    for child_id in child_ids {
        let node = resolve_dependency_node(&child_id, project_root, sources, visited, cache)?;
        children.push(node);
    }
    visited.remove(dep_id);

    let mut node = Map::new();
    node.insert("id".to_string(), Value::String(dep_id.to_string()));
    node.insert("requested".to_string(), Value::String(dep_id.to_string()));
    node.insert("resolved".to_string(), Value::String(dep_id.to_string()));
    node.insert(
        "source".to_string(),
        json!({
            "type": "registry",
            "reference": dep_id
        }),
    );
    node.insert("dependencies".to_string(), Value::Array(children));
    node.insert(
        "integrity".to_string(),
        Value::String(compute_integrity(&descriptor_text)),
    );
    Ok(Value::Object(node))
}

fn read_descriptor_for_spec(
    dep_id: &str,
    spec: &Value,
    project_root: &Path,
    cache: &mut HashMap<String, (Value, String)>,
) -> Result<(Value, String)> {
    let spec_obj = spec
        .as_object()
        .ok_or_else(|| anyhow!("source entry for {dep_id} must be an object"))?;
    let source_type = spec_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("path");

    let descriptor_dir = match source_type {
        "path" => {
            let rel = spec_obj
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("path source for {dep_id} missing `path`"))?;
            project_root.join(rel)
        }
        "git" => {
            let url = spec_obj
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("git source for {dep_id} missing `url`"))?;
            PathBuf::from(url)
        }
        other => {
            return Err(anyhow!(
                "unsupported source type `{}` for dependency {}",
                other,
                dep_id
            ))
        }
    };

    let descriptor_path = descriptor_dir.join("lcp.toml");
    let cache_key = descriptor_path
        .canonicalize()
        .unwrap_or(descriptor_path.clone())
        .to_string_lossy()
        .to_string();
    if let Some(entry) = cache.get(&cache_key) {
        return Ok(entry.clone());
    }

    let descriptor_text = fs::read_to_string(&descriptor_path).with_context(|| {
        format!(
            "reading descriptor for {} at {}",
            dep_id,
            descriptor_path.display()
        )
    })?;
    let toml_value: TomlValue = descriptor_text
        .parse()
        .with_context(|| format!("parsing descriptor for {}", dep_id))?;
    let descriptor_json =
        serde_json::to_value(toml_value).context("serializing descriptor to JSON")?;
    cache.insert(
        cache_key.clone(),
        (descriptor_json.clone(), descriptor_text.clone()),
    );
    Ok((descriptor_json, descriptor_text))
}

fn parse_requires(descriptor: &Value) -> Vec<String> {
    descriptor
        .get("deps")
        .and_then(|deps| deps.get("requires"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|value| value.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn compute_integrity(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("sha256-{}", hex::encode(hasher.finalize()))
}

fn value_as_path(value: &Value, key: &str) -> Result<PathBuf> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing `{}`", key))
}

fn toml_stringify_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let value = input.get("value").cloned().unwrap_or(Value::Null);
    let toml_value = TomlValue::try_from(value)
        .map_err(|err| anyhow!("unable to convert value to TOML: {err}"))?;
    let text = toml::to_string_pretty(&toml_value)
        .map_err(|err| anyhow!("unable to serialize TOML: {err}"))?;
    Ok(json!({ "text": text }))
}

fn http_download_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let url = input
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("url is required"))?;
    let path_str = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("path is required"))?;

    let mut request = Map::new();
    request.insert("url".to_string(), Value::String(url.to_string()));
    request.insert(
        "responseMode".to_string(),
        Value::String("stream".to_string()),
    );

    for key in [
        "method",
        "headers",
        "query",
        "timeoutMs",
        "followRedirects",
        "body",
        "bodyEncoding",
    ] {
        if let Some(value) = input.get(key) {
            request.insert(key.to_string(), value.clone());
        }
    }

    let response = _ctx.call(
        "lcod://contract/core/http/request@1",
        Value::Object(request.clone()),
        None,
    )?;

    let status = response
        .get("status")
        .and_then(Value::as_u64)
        .unwrap_or_default() as i64;

    let mut bytes = Vec::new();
    if let Some(stream) = response.get("stream") {
        loop {
            let chunk = _ctx.streams_mut().read(stream, None, None)?;
            if chunk.get("done").and_then(Value::as_bool).unwrap_or(false) {
                break;
            }
            if let Some(data) = chunk.get("chunk").and_then(Value::as_str) {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|err| anyhow!("invalid base64 chunk: {err}"))?;
                bytes.extend_from_slice(&decoded);
            }
        }
        let _ = _ctx.streams_mut().close(stream);
    } else if let Some(body) = response.get("body") {
        let encoding = response
            .get("bodyEncoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-8");
        bytes = decode_body(body, encoding)?;
    }

    let path = PathBuf::from(path_str);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("unable to create parent directory for {}", parent.display())
        })?;
    }
    fs::write(&path, &bytes)
        .with_context(|| format!("unable to write downloaded file to {}", path.display()))?;

    let headers = response
        .get("headers")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    Ok(json!({
        "status": status,
        "bytes": bytes.len(),
        "headers": headers
    }))
}

fn path_to_string(path: &Path) -> Result<String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(crate::core::path::path_to_string(&canonical))
}

fn decode_body(body: &Value, encoding: &str) -> Result<Vec<u8>> {
    match encoding {
        "base64" => {
            let text = body
                .as_str()
                .ok_or_else(|| anyhow!("base64 body must be a string"))?;
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|err| anyhow!("invalid base64 body: {err}"))
        }
        "json" => Ok(serde_json::to_vec(body)?),
        _ => {
            let text = match body {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                other => serde_json::to_string(other)?,
            };
            Ok(text.into_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
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
    fn http_download_writes_file() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nbinary-data";
        let base = spawn_server(response);

        let registry = Registry::new();
        register_resolver_axioms(&registry);
        let mut ctx = registry.context();
        let dest = tempfile::NamedTempFile::new().unwrap();
        let path = crate::core::path::path_to_string(dest.path());
        drop(dest);

        let input = json!({
            "url": format!("{}/file", base),
            "path": &path
        });

        let result = http_download_axiom(&mut ctx, input, None).unwrap();
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["bytes"], json!("binary-data".len()));
        assert_eq!(fs::read_to_string(&path).unwrap(), "binary-data");
    }
}
