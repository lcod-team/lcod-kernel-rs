use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

pub fn register_path(registry: &Registry) {
    registry.register("lcod://axiom/path/join@1", path_join_axiom);
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

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
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
}
