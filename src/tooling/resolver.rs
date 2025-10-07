use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use base64::Engine as _;
use hex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use toml::Value as TomlValue;
use url::Url;

use crate::core;
use crate::registry::{Context, Registry};

fn integrity_from_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let encoded = base64::engine::general_purpose::STANDARD.encode(digest);
    format!("sha256-{encoded}")
}

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

fn cache_key(parts: &[(&str, Option<&str>)]) -> String {
    let mut hasher = Sha256::new();
    for (label, value) in parts {
        hasher.update(label.as_bytes());
        hasher.update(&[0]);
        if let Some(val) = value {
            hasher.update(val.as_bytes());
        }
        hasher.update(&[0xff]);
    }
    hex::encode(hasher.finalize())
}

fn default_http_filename(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back().map(|s| s.to_string()))
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "artifact".to_string())
}

fn parse_requires(
    descriptor_text: &str,
    descriptor_path: &Path,
    dependency: &str,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    match TomlValue::from_str(descriptor_text) {
        Ok(toml) => toml
            .get("deps")
            .and_then(|deps| deps.get("requires"))
            .and_then(TomlValue::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(TomlValue::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Err(err) => {
            warnings.push(format!(
                "Failed to parse {} for {}: {err}",
                descriptor_path.display(),
                dependency
            ));
            Vec::new()
        }
    }
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

    registry.register("lcod://axiom/path/join@1", path_join_axiom);
    registry.register("lcod://axiom/toml/stringify@1", toml_stringify_axiom);
    registry.register("lcod://axiom/http/download@1", http_download_axiom);
    registry.register("lcod://impl/set@1", impl_set_axiom);
    registry.register(
        "lcod://contract/tooling/resolve-dependency@1",
        resolve_dependency_contract,
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

fn impl_set_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    Ok(input)
}

fn resolve_dependency_contract(
    ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let dependency = input
        .get("dependency")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("dependency must be a string"))?;
    let config = input
        .get("config")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let project_path = input
        .get("projectPath")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut stack = input
        .get("stack")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(Vec::new);
    let mut cache: HashMap<String, Value> = HashMap::new();
    let mut warnings = Vec::new();
    let resolved = resolve_dependency_recursive(
        ctx,
        dependency,
        &config,
        &project_path,
        &mut stack,
        &mut cache,
        &mut warnings,
    )?;
    Ok(json!({
        "resolved": resolved,
        "warnings": warnings
    }))
}

fn path_join_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let base = input.get("base").and_then(Value::as_str).unwrap_or("");
    let segment = input.get("segment").and_then(Value::as_str).unwrap_or("");

    let mut path = PathBuf::from(base);
    if Path::new(segment).is_absolute() {
        path = PathBuf::from(segment);
    } else {
        path.push(segment);
    }

    Ok(json!({ "path": path_to_string(&path)? }))
}

fn toml_stringify_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let value = input.get("value").cloned().unwrap_or(Value::Null);
    let toml_value = TomlValue::try_from(value)
        .map_err(|err| anyhow!("unable to convert value to TOML: {err}"))?;
    Ok(json!({ "text": toml_value.to_string() }))
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
    canonical
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow!("path contains invalid UTF-8"))
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

fn resolve_dependency_recursive(
    ctx: &mut Context,
    dependency: &str,
    config: &Value,
    project_path: &Path,
    stack: &mut Vec<String>,
    cache: &mut HashMap<String, Value>,
    warnings: &mut Vec<String>,
) -> Result<Value> {
    if let Some(existing) = cache.get(dependency) {
        return Ok(existing.clone());
    }
    if stack.contains(&dependency.to_string()) {
        stack.push(dependency.to_string());
        let cycle = stack.join(" -> ");
        stack.pop();
        return Err(anyhow!("dependency cycle detected: {cycle}"));
    }

    stack.push(dependency.to_string());

    let sources = config
        .get("sources")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut dependencies_vec: Vec<Value> = Vec::new();
    let mut integrity: Option<String> = None;
    let resolved_source: Value;

    match sources.get(dependency) {
        None => {
            resolved_source = json!({ "type": "registry", "reference": dependency });
        }
        Some(spec) => {
            let spec_type = spec.get("type").and_then(Value::as_str);
            match spec_type {
                Some("path") => {
                    let path_str = spec.get("path").and_then(Value::as_str).unwrap_or(".");
                    let abs_path = if Path::new(path_str).is_absolute() {
                        PathBuf::from(path_str)
                    } else {
                        project_path.join(path_str)
                    };
                    let canonical = abs_path.canonicalize().unwrap_or_else(|_| abs_path.clone());
                    let descriptor_path = canonical.join("lcp.toml");
                    let descriptor_text = match fs::read_to_string(&descriptor_path) {
                        Ok(text) => Some(text),
                        Err(err) => {
                            warnings.push(format!(
                                "Failed to read {} for {}: {}",
                                descriptor_path.display(),
                                dependency,
                                err
                            ));
                            None
                        }
                    };
                    if let Some(text) = descriptor_text {
                        integrity = Some(integrity_from_text(&text));
                        let requires =
                            parse_requires(&text, &descriptor_path, dependency, warnings);
                        for child in requires {
                            let resolved_child = resolve_dependency_recursive(
                                ctx,
                                &child,
                                config,
                                project_path,
                                stack,
                                cache,
                                warnings,
                            )?;
                            dependencies_vec.push(resolved_child);
                        }
                    }
                    resolved_source = json!({
                        "type": "path",
                        "path": path_to_string(&canonical)?
                    });
                }
                Some("git") => {
                    let url = match spec.get("url").and_then(Value::as_str) {
                        Some(u) => u,
                        None => {
                            warnings.push(format!(
                                "Missing git url for {}; defaulting to registry reference",
                                dependency
                            ));
                            resolved_source =
                                json!({ "type": "registry", "reference": dependency });
                            stack.pop();
                            let resolved = json!({
                                "id": dependency,
                                "source": resolved_source,
                                "dependencies": dependencies_vec
                            });
                            cache.insert(dependency.to_string(), resolved.clone());
                            return Ok(resolved);
                        }
                    };
                    let ref_value = spec
                        .get("ref")
                        .and_then(Value::as_str)
                        .or_else(|| spec.get("rev").and_then(Value::as_str));
                    let subdir = spec.get("subdir").and_then(Value::as_str);
                    let mut clone_input = Map::new();
                    clone_input.insert("url".to_string(), Value::String(url.to_string()));
                    let dest_hint = format!(
                        "git/{}",
                        cache_key(&[
                            ("dependency", Some(dependency)),
                            ("url", Some(url)),
                            ("ref", ref_value),
                            ("subdir", subdir)
                        ])
                    );
                    clone_input.insert("dest".to_string(), Value::String(dest_hint));
                    if let Some(r) = ref_value {
                        clone_input.insert("ref".to_string(), Value::String(r.to_string()));
                    }
                    if let Some(depth) = spec.get("depth") {
                        clone_input.insert("depth".to_string(), depth.clone());
                    }
                    if let Some(sub) = subdir {
                        clone_input.insert("subdir".to_string(), Value::String(sub.to_string()));
                    }
                    if let Some(auth) = spec.get("auth") {
                        clone_input.insert("auth".to_string(), auth.clone());
                    }
                    let clone_result = match ctx.call(
                        "lcod://contract/core/git/clone@1",
                        Value::Object(clone_input),
                        None,
                    ) {
                        Ok(value) => value,
                        Err(err) => {
                            warnings.push(format!(
                                "Failed to clone {} for {}: {}",
                                url, dependency, err
                            ));
                            Value::Null
                        }
                    };
                    let descriptor_root = clone_result
                        .get("path")
                        .and_then(Value::as_str)
                        .map(PathBuf::from);
                    if let Some(root) = descriptor_root {
                        let descriptor_path = root.join("lcp.toml");
                        let descriptor_text = match fs::read_to_string(&descriptor_path) {
                            Ok(text) => Some(text),
                            Err(err) => {
                                warnings.push(format!(
                                    "Failed to read {} for {}: {}",
                                    descriptor_path.display(),
                                    dependency,
                                    err
                                ));
                                None
                            }
                        };
                        if let Some(text) = descriptor_text {
                            integrity = Some(integrity_from_text(&text));
                            let requires =
                                parse_requires(&text, &descriptor_path, dependency, warnings);
                            for child in requires {
                                let resolved_child = resolve_dependency_recursive(
                                    ctx,
                                    &child,
                                    config,
                                    project_path,
                                    stack,
                                    cache,
                                    warnings,
                                )?;
                                dependencies_vec.push(resolved_child);
                            }
                        }
                        let mut source_obj = Map::new();
                        source_obj.insert("type".to_string(), Value::String("git".to_string()));
                        source_obj.insert("url".to_string(), Value::String(url.to_string()));
                        source_obj
                            .insert("path".to_string(), Value::String(path_to_string(&root)?));
                        if let Some(commit) = clone_result.get("commit").and_then(Value::as_str) {
                            source_obj
                                .insert("commit".to_string(), Value::String(commit.to_string()));
                        }
                        if let Some(reference) = clone_result
                            .get("ref")
                            .and_then(Value::as_str)
                            .or(ref_value)
                        {
                            source_obj
                                .insert("ref".to_string(), Value::String(reference.to_string()));
                        }
                        if let Some(sub) = subdir {
                            source_obj.insert("subdir".to_string(), Value::String(sub.to_string()));
                        }
                        if let Some(fetched_at) = clone_result
                            .get("source")
                            .and_then(Value::as_object)
                            .and_then(|obj| obj.get("fetchedAt"))
                            .and_then(Value::as_str)
                        {
                            source_obj.insert(
                                "fetchedAt".to_string(),
                                Value::String(fetched_at.to_string()),
                            );
                        }
                        resolved_source = Value::Object(source_obj);
                    } else {
                        warnings.push(format!(
                            "Clone result for {} missing path; defaulting to registry reference",
                            dependency
                        ));
                        resolved_source = json!({ "type": "registry", "reference": dependency });
                    }
                }
                Some("http") => {
                    let url = match spec.get("url").and_then(Value::as_str) {
                        Some(u) => u,
                        None => {
                            warnings.push(format!(
                                "Missing http url for {}; defaulting to registry reference",
                                dependency
                            ));
                            resolved_source =
                                json!({ "type": "registry", "reference": dependency });
                            stack.pop();
                            let resolved = json!({
                                "id": dependency,
                                "source": resolved_source,
                                "dependencies": dependencies_vec
                            });
                            cache.insert(dependency.to_string(), resolved.clone());
                            return Ok(resolved);
                        }
                    };
                    let cache_root = ensure_cache_dir(project_path)?;
                    let cache_key = cache_key(&[
                        ("dependency", Some(dependency)),
                        ("url", Some(url)),
                        ("method", spec.get("method").and_then(Value::as_str)),
                    ]);
                    let target_dir = cache_root.join("http").join(cache_key);
                    fs::create_dir_all(&target_dir)?;
                    let filename = spec
                        .get("filename")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| default_http_filename(url));
                    let target_path = target_dir.join(filename);
                    let need_download = spec.get("force").and_then(Value::as_bool).unwrap_or(false)
                        || !target_path.exists();
                    if need_download {
                        let mut download_input = Map::new();
                        download_input.insert("url".to_string(), Value::String(url.to_string()));
                        download_input.insert(
                            "path".to_string(),
                            Value::String(path_to_string(&target_path)?),
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
                            if let Some(value) = spec.get(key) {
                                download_input.insert(key.to_string(), value.clone());
                            }
                        }
                        if let Err(err) = ctx.call(
                            "lcod://axiom/http/download@1",
                            Value::Object(download_input),
                            None,
                        ) {
                            warnings.push(format!(
                                "Failed to download {} for {}: {}",
                                url, dependency, err
                            ));
                        }
                    }
                    let descriptor_path = spec
                        .get("descriptorPath")
                        .and_then(Value::as_str)
                        .map(|rel| target_dir.join(rel))
                        .unwrap_or_else(|| target_path.clone());
                    let descriptor_text = match fs::read_to_string(&descriptor_path) {
                        Ok(text) => Some(text),
                        Err(err) => {
                            warnings.push(format!(
                                "Failed to read {} for {}: {}",
                                descriptor_path.display(),
                                dependency,
                                err
                            ));
                            None
                        }
                    };
                    if let Some(text) = descriptor_text {
                        integrity = Some(integrity_from_text(&text));
                        let requires =
                            parse_requires(&text, &descriptor_path, dependency, warnings);
                        for child in requires {
                            let resolved_child = resolve_dependency_recursive(
                                ctx,
                                &child,
                                config,
                                project_path,
                                stack,
                                cache,
                                warnings,
                            )?;
                            dependencies_vec.push(resolved_child);
                        }
                    }
                    resolved_source = json!({
                        "type": "http",
                        "url": url,
                        "path": path_to_string(&descriptor_path)?
                    });
                }
                Some(_) => {
                    resolved_source = spec.clone();
                }
                None => {
                    warnings.push(format!(
                        "Unknown source type for {}; defaulting to registry reference",
                        dependency
                    ));
                    resolved_source = json!({ "type": "registry", "reference": dependency });
                }
            }
        }
    }

    stack.pop();

    let mut resolved_obj = Map::new();
    resolved_obj.insert("id".to_string(), Value::String(dependency.to_string()));
    resolved_obj.insert("source".to_string(), resolved_source);
    resolved_obj.insert("dependencies".to_string(), Value::Array(dependencies_vec));
    if let Some(hash) = integrity {
        resolved_obj.insert("integrity".to_string(), Value::String(hash));
    }

    let resolved = Value::Object(resolved_obj);
    cache.insert(dependency.to_string(), resolved.clone());
    Ok(resolved)
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
    fn path_join_handles_relative_segments() {
        let registry = Registry::new();
        register_resolver_axioms(&registry);
        let mut ctx = registry.context();
        let input = json!({ "base": "/tmp/workspace", "segment": "foo/bar" });
        let result = path_join_axiom(&mut ctx, input, None).unwrap();
        let path = result["path"].as_str().unwrap();
        assert!(path.ends_with("foo/bar"));
    }

    #[test]
    fn http_download_writes_file() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nbinary-data";
        let base = spawn_server(response);

        let registry = Registry::new();
        register_resolver_axioms(&registry);
        let mut ctx = registry.context();
        let dest = tempfile::NamedTempFile::new().unwrap();
        let path = dest.path().to_string_lossy().to_string();
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
