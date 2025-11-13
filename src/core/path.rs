use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

const AXIOM_PATH_JOIN: &str = "lcod://axiom/path/join@1";
const CONTRACT_DIRNAME: &str = "lcod://contract/core/path/dirname@1";
const CONTRACT_IS_ABSOLUTE: &str = "lcod://contract/core/path/is_absolute@1";

pub fn register_path(registry: &Registry) {
    registry.register(AXIOM_PATH_JOIN, path_join_axiom);
    registry.register(CONTRACT_DIRNAME, path_dirname_contract);
    registry.register(CONTRACT_IS_ABSOLUTE, path_is_absolute_contract);
}

fn path_join_axiom(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let base = input.get("base").and_then(Value::as_str).unwrap_or("");
    let mut path = PathBuf::from(base);

    match input.get("segment") {
        Some(Value::String(segment)) => {
            push_segment(&mut path, segment);
        }
        Some(Value::Array(segments)) => {
            for item in segments {
                if let Some(segment) = item.as_str() {
                    push_segment(&mut path, segment);
                } else if !item.is_null() {
                    push_segment(&mut path, &item.to_string());
                }
            }
        }
        Some(value) if !value.is_null() => {
            push_segment(&mut path, &value.to_string());
        }
        _ => {}
    }

    let normalized: PathBuf = path.components().collect();
    Ok(json!({ "path": path_to_string(&normalized) }))
}

fn path_dirname_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let raw = input.get("path").and_then(Value::as_str).unwrap_or("");
    let dirname = dirname_from(raw);
    Ok(json!({ "dirname": dirname }))
}

fn path_is_absolute_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let raw = input.get("path").and_then(Value::as_str).unwrap_or("");
    let absolute = Path::new(raw).is_absolute() || raw.starts_with("//") || raw.starts_with("\\\\");
    Ok(json!({ "absolute": absolute }))
}

fn push_segment(path: &mut PathBuf, segment: &str) {
    if segment.is_empty() {
        return;
    }
    if segment == "." {
        return;
    }
    if segment == ".." {
        path.pop();
        return;
    }
    if Path::new(segment).is_absolute() {
        *path = PathBuf::from(segment);
    } else {
        path.push(segment);
    }
}

pub fn path_to_string(path: &Path) -> String {
    let mut rendered = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        rendered = rendered.replace('\\', "/");
        if let Some(stripped) = rendered.strip_prefix("//?/") {
            rendered = stripped.to_string();
        }
    }
    while rendered.contains("/./") {
        rendered = rendered.replace("/./", "/");
    }
    if rendered.ends_with("/.") {
        rendered.truncate(rendered.len().saturating_sub(2));
    }
    while rendered.starts_with("./") && rendered.len() > 2 {
        rendered = rendered[2..].to_string();
    }
    if rendered == "./" {
        rendered = ".".to_string();
    }
    rendered
}

fn strip_trailing_separators(value: &str) -> String {
    let mut result = value.to_string();
    while result.len() > 1 && (result.ends_with('/') || result.ends_with('\\')) {
        result.pop();
    }
    result
}

fn dirname_from(value: &str) -> String {
    if value.is_empty() {
        return ".".to_string();
    }
    let trimmed = strip_trailing_separators(value);
    if trimmed.is_empty() {
        return ".".to_string();
    }
    if trimmed == "/" || trimmed == "\\" {
        return trimmed;
    }
    if let Some(pos) = trimmed.rfind(['/', '\\']) {
        if pos == 0 {
            let first = trimmed.chars().next().unwrap();
            if first == '/' || first == '\\' {
                return first.to_string();
            }
            return ".".to_string();
        }
        let candidate = &trimmed[..pos];
        if candidate.is_empty() {
            if trimmed.starts_with('/') || trimmed.starts_with('\\') {
                return trimmed.chars().next().unwrap().to_string();
            }
            return ".".to_string();
        }
        candidate.to_string()
    } else {
        ".".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn joins_relative_segments() {
        let mut ctx = Registry::new().context();
        let result = path_join_axiom(
            &mut ctx,
            json!({ "base": "/tmp/workspace", "segment": "foo/bar" }),
            None,
        )
        .unwrap();
        let path = result["path"].as_str().unwrap();
        assert!(path.ends_with("foo/bar"));
    }

    #[test]
    fn replaces_when_segment_absolute() {
        let mut ctx = Registry::new().context();
        let result = path_join_axiom(
            &mut ctx,
            json!({ "base": "/tmp/workspace", "segment": "/var/data" }),
            None,
        )
        .unwrap();
        let path = result["path"].as_str().unwrap();
        assert_eq!(path, "/var/data");
    }

    #[test]
    fn supports_segment_arrays() {
        let mut ctx = Registry::new().context();
        let result = path_join_axiom(
            &mut ctx,
            json!({
                "base": "/tmp",
                "segment": ["workspace", "nested", "file.toml"]
            }),
            None,
        )
        .unwrap();
        let path = result["path"].as_str().unwrap();
        assert!(path.ends_with("workspace/nested/file.toml"));
    }

    #[test]
    fn strips_leading_current_dir_segments() {
        let mut ctx = Registry::new().context();
        let result = path_join_axiom(
            &mut ctx,
            json!({ "base": ".", "segment": "fixtures/basic" }),
            None,
        )
        .unwrap();
        assert_eq!(result["path"], json!("fixtures/basic"));

        let cache =
            path_join_axiom(&mut ctx, json!({ "base": ".", "segment": ".cache" }), None).unwrap();
        assert_eq!(cache["path"], json!(".cache"));
    }

    #[test]
    fn dirname_for_absolute_paths() {
        let mut ctx = Registry::new().context();
        let result = path_dirname_contract(
            &mut ctx,
            json!({ "path": "/tmp/workspace/file.txt" }),
            None,
        )
        .unwrap();
        assert_eq!(result["dirname"], json!("/tmp/workspace"));

        let root = path_dirname_contract(&mut ctx, json!({ "path": "/etc/" }), None).unwrap();
        assert_eq!(root["dirname"], json!("/"));
    }

    #[test]
    fn dirname_for_relative_paths() {
        let mut ctx = Registry::new().context();
        let missing = path_dirname_contract(&mut ctx, json!({ "path": "README.md" }), None).unwrap();
        assert_eq!(missing["dirname"], json!("."));

        let nested =
            path_dirname_contract(&mut ctx, json!({ "path": "src/lib/mod.rs" }), None).unwrap();
        assert_eq!(nested["dirname"], json!("src/lib"));
    }

    #[test]
    fn is_absolute_reports_correctly() {
        let mut ctx = Registry::new().context();
        let abs = path_is_absolute_contract(&mut ctx, json!({ "path": "/tmp" }), None).unwrap();
        assert_eq!(abs["absolute"], json!(true));

        let rel = path_is_absolute_contract(&mut ctx, json!({ "path": "foo/bar" }), None).unwrap();
        assert_eq!(rel["absolute"], json!(false));
    }
}
