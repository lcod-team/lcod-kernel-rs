use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use toml::Value as TomlValue;

use crate::core;
use crate::registry::{Context, Registry};

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
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let dependency = input
        .get("dependency")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("dependency must be a string"))?;
    let resolved = input
        .get("config")
        .and_then(|cfg| cfg.get("sources"))
        .and_then(|sources| sources.get(dependency))
        .cloned()
        .unwrap_or_else(|| json!({ "type": "path", "path": "." }));
    Ok(json!({
        "resolved": {
            "id": dependency,
            "source": resolved
        },
        "warnings": []
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
    let _ = input; // keep signature consistent with future implementation
    Err(anyhow!(
        "HTTP download axiom is not yet implemented in the Rust substrate blueprint"
    ))
}

fn path_to_string(path: &Path) -> Result<String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow!("path contains invalid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn http_download_returns_placeholder_error() {
        let registry = Registry::new();
        register_resolver_axioms(&registry);
        let mut ctx = registry.context();
        let input = json!({ "url": "https://example.com", "path": "/tmp/out" });
        let err = http_download_axiom(&mut ctx, input, None).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}
