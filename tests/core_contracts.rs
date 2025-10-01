use anyhow::Result;
use base64::Engine as _;
use serde_json::json;
use tempfile::tempdir;

use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::{Context, Registry};

fn registry_with_core() -> Registry {
    let registry = Registry::new();
    register_core(&registry);
    registry
}

fn context() -> Context {
    let registry = registry_with_core();
    registry.context()
}

#[test]
fn fs_write_and_read_roundtrip() -> Result<()> {
    let mut ctx = context();
    let dir = tempdir()?;
    let file_path = dir.path().join("sample.txt");
    let file_str = file_path.to_string_lossy().to_string();

    let write_res = ctx.call(
        "lcod://contract/core/fs/write-file@1",
        json!({
            "path": file_str,
            "data": "hello world",
            "encoding": "utf-8",
            "createParents": true
        }),
        None,
    )?;

    assert_eq!(
        write_res.get("bytesWritten").and_then(|v| v.as_u64()),
        Some(11)
    );

    let read_res = ctx.call(
        "lcod://contract/core/fs/read-file@1",
        json!({ "path": file_path, "encoding": "utf-8" }),
        None,
    )?;

    assert_eq!(read_res.get("data"), Some(&json!("hello world")));
    assert_eq!(read_res.get("encoding"), Some(&json!("utf-8")));

    Ok(())
}

#[test]
fn fs_write_base64_and_read_back() -> Result<()> {
    let mut ctx = context();
    let dir = tempdir()?;
    let file_path = dir.path().join("payload.bin");
    let file_str = file_path.to_string_lossy().to_string();

    let payload = base64::engine::general_purpose::STANDARD.encode(b"hello");

    ctx.call(
        "lcod://contract/core/fs/write-file@1",
        json!({
            "path": file_str,
            "data": payload,
            "encoding": "base64"
        }),
        None,
    )?;

    let read_res = ctx.call(
        "lcod://contract/core/fs/read-file@1",
        json!({ "path": file_path, "encoding": "base64" }),
        None,
    )?;

    assert_eq!(
        read_res.get("data"),
        Some(&json!(
            base64::engine::general_purpose::STANDARD.encode(b"hello")
        ))
    );

    Ok(())
}

#[test]
fn fs_list_directory_entries() -> Result<()> {
    let mut ctx = context();
    let dir = tempdir()?;
    let dir_path = dir.path();
    let file_a = dir_path.join("a.txt");
    let file_b = dir_path.join(".hidden.txt");
    std::fs::write(&file_a, "A")?;
    std::fs::write(&file_b, "B")?;

    let list_visible = ctx.call(
        "lcod://contract/core/fs/list-dir@1",
        json!({
            "path": dir_path,
            "includeHidden": false,
            "includeStats": false
        }),
        None,
    )?;

    let entries = list_visible
        .get("entries")
        .and_then(|v| v.as_array())
        .expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], json!("a.txt"));

    let list_all = ctx.call(
        "lcod://contract/core/fs/list-dir@1",
        json!({
            "path": dir_path,
            "includeHidden": true,
            "includeStats": true
        }),
        None,
    )?;

    let entries_all = list_all
        .get("entries")
        .and_then(|v| v.as_array())
        .expect("entries array");
    assert_eq!(entries_all.len(), 2);

    Ok(())
}

#[test]
fn hash_sha256_computes_digest() -> Result<()> {
    let mut ctx = context();
    let res = ctx.call(
        "lcod://contract/core/hash/sha256@1",
        json!({ "data": "hello world", "encoding": "utf-8" }),
        None,
    )?;

    assert_eq!(
        res.get("hex"),
        Some(&json!(
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        ))
    );
    assert_eq!(
        res.get("base64"),
        Some(&json!("uU0nuZNNPgilLlLX2n2r+sSE7+N6U4DukIj3rOLvzek="))
    );
    assert_eq!(res.get("bytes"), Some(&json!(11)));

    Ok(())
}

#[test]
fn parse_json_toml_and_csv() -> Result<()> {
    let mut ctx = context();

    let json_res = ctx.call(
        "lcod://contract/core/parse/json@1",
        json!({ "text": "{\"flag\":true,\"items\":[1,2,3]}" }),
        None,
    )?;
    assert_eq!(json_res["value"]["flag"], json!(true));
    assert_eq!(json_res["value"]["items"], json!([1, 2, 3]));

    let toml_res = ctx.call(
        "lcod://contract/core/parse/toml@1",
        json!({ "text": "title = \"demo\"\n[owner]\nname = \"Alice\"" }),
        None,
    )?;
    assert_eq!(toml_res["value"]["title"], json!("demo"));
    assert_eq!(toml_res["value"]["owner"]["name"], json!("Alice"));

    let csv_res = ctx.call(
        "lcod://contract/core/parse/csv@1",
        json!({ "text": "name,age\nBob,30\nAna,28", "header": true }),
        None,
    )?;
    let rows = csv_res["rows"].as_array().expect("csv rows array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], json!("Bob"));
    assert_eq!(rows[1]["age"], json!("28"));

    Ok(())
}
