use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use base64::Engine as _;
use humantime::format_rfc3339;
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const CONTRACT_READ: &str = "lcod://contract/core/fs/read-file@1";
const CONTRACT_READ_ALT: &str = "lcod://contract/core/fs/read_file@1";
const CONTRACT_WRITE: &str = "lcod://contract/core/fs/write-file@1";
const CONTRACT_WRITE_ALT: &str = "lcod://contract/core/fs/write_file@1";
const CONTRACT_LIST: &str = "lcod://contract/core/fs/list-dir@1";
const CONTRACT_LIST_ALT: &str = "lcod://contract/core/fs/list_dir@1";

#[cfg(windows)]
fn from_unix_path(input: &str) -> PathBuf {
    if input.is_empty() {
        return PathBuf::new();
    }

    let mut rendered = input.replace('/', "\\");

    if rendered.starts_with("\\\\") {
        return PathBuf::from(rendered);
    }

    if rendered.starts_with("//?/") {
        rendered = rendered.replacen("//?/", "\\\\?\\", 1);
    } else if rendered.starts_with("//") {
        rendered = rendered.replacen("//", "\\\\", 1);
    }

    PathBuf::from(rendered)
}

#[cfg(not(windows))]
fn from_unix_path(input: &str) -> PathBuf {
    PathBuf::from(input)
}

fn to_unix_path(path: &Path) -> String {
    crate::core::path::path_to_string(path)
}

pub fn register_fs(registry: &Registry) {
    registry.register(CONTRACT_READ, read_file_contract);
    registry.register(CONTRACT_READ_ALT, read_file_contract);
    registry.register(CONTRACT_WRITE, write_file_contract);
    registry.register(CONTRACT_WRITE_ALT, write_file_contract);
    registry.register(CONTRACT_LIST, list_dir_contract);
    registry.register(CONTRACT_LIST_ALT, list_dir_contract);
}

fn value_as_str<'a>(value: &'a Value, key: &'static str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing or invalid `{key}`"))
}

fn optional_bool(value: &Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn optional_usize(value: &Value, key: &str) -> Option<usize> {
    value.get(key).and_then(Value::as_u64).map(|v| v as usize)
}

fn to_rfc3339(time: SystemTime) -> Result<String> {
    let formatted = format_rfc3339(time);
    Ok(formatted.to_string())
}

fn read_file_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let path_str = value_as_str(&input, "path")?;
    let encoding = input
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("utf-8");
    let path = from_unix_path(path_str);

    let metadata =
        fs::metadata(&path).with_context(|| format!("unable to stat file: {}", path.display()))?;
    let size = metadata.len();
    let mtime = metadata.modified().ok().and_then(|t| to_rfc3339(t).ok());

    let data_value = if encoding.eq_ignore_ascii_case("base64") {
        let raw =
            fs::read(&path).with_context(|| format!("unable to read file: {}", path.display()))?;
        Value::String(base64::engine::general_purpose::STANDARD.encode(raw))
    } else if encoding.eq_ignore_ascii_case("hex") {
        let raw =
            fs::read(&path).with_context(|| format!("unable to read file: {}", path.display()))?;
        Value::String(hex::encode(raw))
    } else {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("unable to read file: {}", path.display()))?;
        Value::String(content)
    };

    let mut map = Map::new();
    map.insert("data".to_string(), data_value);
    map.insert("encoding".to_string(), Value::String(encoding.to_string()));
    map.insert("size".to_string(), Value::Number(size.into()));
    if let Some(ts) = mtime {
        map.insert("mtime".to_string(), Value::String(ts));
    }

    Ok(Value::Object(map))
}

fn write_file_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let path_str = value_as_str(&input, "path")?;
    let encoding = input
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("utf-8");
    let data = input
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing or invalid `data`"))?;
    let append = optional_bool(&input, "append", false);
    let create_parents = optional_bool(&input, "createParents", false);

    let path = from_unix_path(path_str);
    if create_parents {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("unable to create parent directories for {}", path.display())
            })?;
        }
    }

    let mut file = if append {
        fs::OpenOptions::new().create(true).append(true).open(&path)
    } else {
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
    }
    .with_context(|| format!("unable to open file: {}", path.display()))?;

    let bytes = if encoding.eq_ignore_ascii_case("base64") {
        let buf = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|err| anyhow!("invalid base64 payload: {err}"))?;
        file.write_all(&buf)
            .with_context(|| format!("unable to write file: {}", path.display()))?;
        buf.len()
    } else if encoding.eq_ignore_ascii_case("hex") {
        let buf = hex::decode(data).map_err(|err| anyhow!("invalid hex payload: {err}"))?;
        file.write_all(&buf)
            .with_context(|| format!("unable to write file: {}", path.display()))?;
        buf.len()
    } else {
        file.write_all(data.as_bytes())
            .with_context(|| format!("unable to write file: {}", path.display()))?;
        data.len()
    };

    drop(file);

    let metadata =
        fs::metadata(&path).with_context(|| format!("unable to stat file: {}", path.display()))?;
    let mtime = metadata.modified().ok().and_then(|t| to_rfc3339(t).ok());

    let mut map = Map::new();
    map.insert("bytesWritten".to_string(), Value::Number(bytes.into()));
    if let Some(ts) = mtime {
        map.insert("mtime".to_string(), Value::String(ts));
    }

    Ok(Value::Object(map))
}

fn list_dir_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let path_str = value_as_str(&input, "path")?;
    let recursive = optional_bool(&input, "recursive", false);
    let include_hidden = optional_bool(&input, "includeHidden", false);
    let include_stats = optional_bool(&input, "includeStats", false);
    let max_depth = optional_usize(&input, "maxDepth").unwrap_or(usize::MAX);

    let root = from_unix_path(path_str);
    let mut entries = Vec::new();
    walk_dir(
        &root,
        &root,
        recursive,
        include_hidden,
        include_stats,
        max_depth,
        0,
        &mut entries,
    )?;

    Ok(json!({ "entries": entries }))
}

fn walk_dir(
    root: &Path,
    current: &Path,
    recursive: bool,
    include_hidden: bool,
    include_stats: bool,
    max_depth: usize,
    depth: usize,
    out: &mut Vec<Value>,
) -> Result<()> {
    let read_dir = fs::read_dir(current)
        .with_context(|| format!("unable to read directory: {}", current.display()))?;

    for entry_res in read_dir {
        let entry = entry_res
            .with_context(|| format!("unable to process directory entry: {}", current.display()))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("invalid UTF-8 entry name"))?;

        if !include_hidden && name.starts_with('.') {
            continue;
        }

        let file_type = entry
            .file_type()
            .with_context(|| format!("unable to get file type for {}", path.display()))?;

        let mut object = Map::new();
        object.insert("name".to_string(), Value::String(name.clone()));
        object.insert("path".to_string(), Value::String(to_unix_path(&path)));
        let entry_type = if file_type.is_dir() {
            "directory"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "file"
        };
        object.insert("type".to_string(), Value::String(entry_type.to_string()));

        if include_stats {
            if let Ok(metadata) = entry.metadata() {
                object.insert("size".to_string(), Value::Number(metadata.len().into()));
                if let Ok(modified) = metadata.modified() {
                    if let Ok(ts) = to_rfc3339(modified) {
                        object.insert("mtime".to_string(), Value::String(ts));
                    }
                }
            }
        }

        out.push(Value::Object(object));

        if recursive && depth < max_depth && file_type.is_dir() {
            walk_dir(
                root,
                &path,
                recursive,
                include_hidden,
                include_stats,
                max_depth,
                depth + 1,
                out,
            )?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_from_unix_preserves_unc_prefix() {
        let path = from_unix_path("//server/share/data.txt");
        assert_eq!(path.to_string_lossy(), r"\\server\share\data.txt");
    }

    #[cfg(windows)]
    #[test]
    fn windows_from_unix_handles_extended_prefix() {
        let path = from_unix_path("//?/C:/workspace/project");
        assert_eq!(path.to_string_lossy(), r"\\?\C:\workspace\project");
    }

    #[cfg(windows)]
    #[test]
    fn windows_to_unix_roundtrips_backslashes() {
        let original = std::path::PathBuf::from(r"C:\workspace\src\lib.rs");
        assert_eq!(to_unix_path(&original), "C:/workspace/src/lib.rs");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_from_unix_is_identity() {
        let path = from_unix_path("/tmp/data");
        assert_eq!(path.to_string_lossy(), "/tmp/data");
    }
}
