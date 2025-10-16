use lcod_kernel_rs::compose::{parse_compose, run_compose, Step};
use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::flow::register_flow;
use lcod_kernel_rs::registry::Registry;
use lcod_kernel_rs::tooling::{register_resolver_axioms, register_tooling};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;
use tempfile::tempdir;

fn locate_spec_repo() -> Option<PathBuf> {
    if let Ok(env_path) = env::var("SPEC_REPO_PATH") {
        let candidate = PathBuf::from(&env_path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("../lcod-spec"),
        manifest_dir.join("../../lcod-spec"),
        manifest_dir.join("../../../lcod-spec"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn resolver_compose_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = env::var("LCOD_RESOLVER_COMPOSE") {
        if !path.trim().is_empty() {
            candidates.push(PathBuf::from(path));
        }
    }
    if let Ok(path) = env::var("SPEC_REPO_PATH") {
        if !path.trim().is_empty() {
            candidates.push(
                PathBuf::from(path)
                    .join("resources")
                    .join("compose")
                    .join("resolver")
                    .join("compose.yaml"),
            );
        }
    }
    if let Ok(path) = env::var("LCOD_SPEC_PATH") {
        if !path.trim().is_empty() {
            candidates.push(
                PathBuf::from(path)
                    .join("resources")
                    .join("compose")
                    .join("resolver")
                    .join("compose.yaml"),
            );
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("lcod-resolver")
            .join("packages")
            .join("resolver")
            .join("compose.yaml"),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("lcod-resolver")
            .join("compose.yaml"),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("lcod-spec")
            .join("examples")
            .join("tooling")
            .join("resolver")
            .join("compose.yaml"),
    );
    candidates
}

fn load_compose() -> Option<Vec<Step>> {
    let candidates = resolver_compose_candidates();
    for candidate in &candidates {
        match fs::read_to_string(candidate) {
            Ok(text) => {
                let yaml: serde_json::Value = match serde_yaml::from_str(&text) {
                    Ok(doc) => doc,
                    Err(err) => {
                        eprintln!("Failed to parse {}: {}", candidate.display(), err);
                        continue;
                    }
                };
                let Some(steps_value) = yaml.get("compose").cloned() else {
                    continue;
                };
                let mut canonical = steps_value;
                if let Some(context) = load_manifest_context(candidate) {
                    canonicalize_value(&mut canonical, &context);
                }
                match parse_compose(&canonical) {
                    Ok(steps) => return Some(steps),
                    Err(err) => {
                        eprintln!(
                            "Failed to normalize compose {}: {}",
                            candidate.display(),
                            err
                        );
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(err) => {
                eprintln!("Failed to read {}: {}", candidate.display(), err);
            }
        }
    }
    None
}

#[derive(Clone, Debug)]
struct ManifestContext {
    base_path: String,
    version: String,
    alias_map: HashMap<String, String>,
}

fn load_manifest_context(compose_path: &Path) -> Option<ManifestContext> {
    let manifest_path = compose_path.parent()?.join("lcp.toml");
    let manifest_text = fs::read_to_string(manifest_path).ok()?;
    let manifest: toml::Value = manifest_text.parse().ok()?;
    let id = manifest.get("id").and_then(|v| v.as_str());
    let base_path = if let Some(id) = id {
        id.strip_prefix("lcod://")?
            .split('@')
            .next()
            .map(|s| s.to_string())?
    } else {
        let ns = manifest
            .get("namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = manifest.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let joined = [ns, name]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/");
        if joined.is_empty() {
            return None;
        }
        joined
    };
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| id.and_then(|i| i.split('@').nth(1).map(|s| s.to_string())))
        .unwrap_or_else(|| "0.0.0".to_string());
    let alias_map = manifest
        .get("workspace")
        .and_then(|v| v.as_table())
        .and_then(|table| table.get("scopeAliases"))
        .and_then(|v| v.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|alias| (k.clone(), alias.to_string())))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    Some(ManifestContext {
        base_path,
        version,
        alias_map,
    })
}

fn canonicalize_value(value: &mut JsonValue, context: &ManifestContext) {
    match value {
        JsonValue::Array(arr) => {
            for item in arr.iter_mut() {
                canonicalize_value(item, context);
            }
        }
        JsonValue::Object(map) => canonicalize_object(map, context),
        _ => {}
    }
}

fn canonicalize_object(map: &mut JsonMap<String, JsonValue>, context: &ManifestContext) {
    if let Some(JsonValue::String(call)) = map.get_mut("call") {
        *call = canonicalize_id(call, context);
    }
    if let Some(children) = map.get_mut("children") {
        canonicalize_children(children, context);
    }
    if let Some(input) = map.get_mut("in") {
        canonicalize_value(input, context);
    }
    if let Some(output) = map.get_mut("out") {
        canonicalize_value(output, context);
    }
    if let Some(bindings) = map.get_mut("bindings") {
        canonicalize_value(bindings, context);
    }
    for (key, val) in map.iter_mut() {
        if matches!(
            key.as_str(),
            "call" | "children" | "in" | "out" | "bindings"
        ) {
            continue;
        }
        canonicalize_value(val, context);
    }
}

fn canonicalize_children(value: &mut JsonValue, context: &ManifestContext) {
    match value {
        JsonValue::Array(arr) => {
            for item in arr.iter_mut() {
                canonicalize_value(item, context);
            }
        }
        JsonValue::Object(map) => {
            for val in map.values_mut() {
                canonicalize_value(val, context);
            }
        }
        _ => {}
    }
}

fn canonicalize_id(raw: &str, context: &ManifestContext) -> String {
    if raw.starts_with("lcod://") {
        return raw.to_string();
    }
    let trimmed = raw.trim_start_matches("./");
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return raw.to_string();
    }
    let alias = segments[0];
    let mapped = context
        .alias_map
        .get(alias)
        .map(|s| s.as_str())
        .unwrap_or(alias);
    let mut parts = Vec::new();
    if !context.base_path.is_empty() {
        parts.push(context.base_path.clone());
    }
    if !mapped.is_empty() {
        parts.push(mapped.to_string());
    }
    for seg in segments.iter().skip(1) {
        parts.push((*seg).to_string());
    }
    if parts.is_empty() {
        return raw.to_string();
    }
    format!("lcod://{}@{}", parts.join("/"), context.version)
}
fn clear_resolver_env() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        env::remove_var("SPEC_REPO_PATH");
        env::remove_var("LCOD_RESOLVER_PATH");
        env::remove_var("LCOD_RESOLVER_COMPONENTS_PATH");
    });
}

fn new_registry() -> Registry {
    clear_resolver_env();
    let registry = Registry::new();
    register_core(&registry);
    register_flow(&registry);
    register_tooling(&registry);
    register_resolver_axioms(&registry);
    registry
}

#[test]
fn resolver_compose_handles_local_path_dependency() {
    let registry = new_registry();
    let mut ctx = registry.context();
    let temp = tempdir().unwrap();
    let project = temp.path();

    let dep_dir = project.join("components").join("dep");
    fs::create_dir_all(&dep_dir).unwrap();
    fs::write(
        dep_dir.join("lcp.toml"),
        "schemaVersion = \"1.0\"\nid = \"lcod://example/dep@0.1.0\"\n[deps]\nrequires = []\n",
    )
    .unwrap();

    fs::write(
        project.join("lcp.toml"),
        "schemaVersion = \"1.0\"\nid = \"lcod://example/app@0.1.0\"\n[deps]\nrequires = [\"lcod://example/dep@0.1.0\"]\n",
    )
    .unwrap();

    let config_path = project.join("resolve.config.json");
    fs::write(
        &config_path,
        serde_json::to_string_pretty(&json!({
            "sources": {
                "lcod://example/dep@0.1.0": { "type": "path", "path": "components/dep" }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let Some(compose) = load_compose() else {
        eprintln!("resolver compose unavailable; skipping test");
        return;
    };
    let output_path = project.join("lcp.lock");
    let state = json!({
        "projectPath": project,
        "configPath": config_path,
        "outputPath": output_path,
    });

    let result = run_compose(&mut ctx, &compose, state).expect("compose run");
    let warnings_len = result
        .get("warnings")
        .and_then(|value| value.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    assert_eq!(warnings_len, 0);
    let lock_raw = fs::read_to_string(&output_path).unwrap();
    let lock_doc: toml::Value = lock_raw.parse().unwrap();
    let components = lock_doc["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    let component = &components[0];
    assert_eq!(
        component["id"].as_str().unwrap(),
        "lcod://example/app@0.1.0"
    );
    let deps = component["dependencies"].as_array().unwrap();
    assert_eq!(deps.len(), 1);
    let dep_entry = &deps[0];
    assert_eq!(
        dep_entry["id"].as_str().unwrap(),
        "lcod://example/dep@0.1.0"
    );
    assert_eq!(
        dep_entry["source"]["type"].as_str().unwrap(),
        "registry"
    );
    assert_eq!(
        dep_entry["source"]["reference"].as_str().unwrap(),
        "lcod://example/dep@0.1.0"
    );
}

#[test]
fn resolver_compose_handles_git_dependency() {
    let registry = new_registry();
    let mut ctx = registry.context();
    let temp = tempdir().unwrap();
    let project = temp.path();
    let repo_dir = project.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join("lcp.toml"),
        "schemaVersion = \"1.0\"\nid = \"lcod://example/git@0.1.0\"\n[deps]\nrequires = []\n",
    )
    .unwrap();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "resolver@example.com"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Resolver Bot"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["add", "lcp.toml"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .env("GIT_AUTHOR_NAME", "Resolver Bot")
        .env("GIT_AUTHOR_EMAIL", "resolver@example.com")
        .env("GIT_COMMITTER_NAME", "Resolver Bot")
        .env("GIT_COMMITTER_EMAIL", "resolver@example.com")
        .current_dir(&repo_dir)
        .status()
        .unwrap();

    fs::write(
        project.join("lcp.toml"),
        "schemaVersion = \"1.0\"\nid = \"lcod://example/app@0.1.0\"\n[deps]\nrequires = [\"lcod://example/git@0.1.0\"]\n",
    )
    .unwrap();

    let config_path = project.join("resolve.config.json");
    fs::write(
        &config_path,
        serde_json::to_string_pretty(&json!({
            "sources": {
                "lcod://example/git@0.1.0": { "type": "git", "url": repo_dir }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    std::env::set_var("LCOD_CACHE_DIR", project.join("cache"));

    let Some(compose) = load_compose() else {
        eprintln!("resolver compose unavailable; skipping test");
        return;
    };
    let output_path = project.join("lcp.lock");
    let state = json!({
        "projectPath": project,
        "configPath": config_path,
        "outputPath": output_path,
    });

    run_compose(&mut ctx, &compose, state).expect("compose run");
    let lock_raw = fs::read_to_string(&output_path).unwrap();
    let lock_doc: toml::Value = lock_raw.parse().unwrap();
    let component = &lock_doc["components"].as_array().unwrap()[0];
    let dep_entry = component["dependencies"].as_array().unwrap()[0].clone();
    assert_eq!(
        dep_entry["source"]["type"].as_str().unwrap(),
        "registry"
    );
    assert_eq!(
        dep_entry["source"]["reference"].as_str().unwrap(),
        "lcod://example/git@0.1.0"
    );
    std::env::remove_var("LCOD_CACHE_DIR");
}

#[test]
fn load_sources_resolver_fixture_catalogues() {
    let Some(spec_root) = locate_spec_repo() else {
        eprintln!(
            "SPEC_REPO_PATH missing and lc0d-spec checkout not found; skipping load_sources_resolver_fixture_catalogues"
        );
        return;
    };
    let spec_root_string = spec_root.to_string_lossy().to_string();
    let fixture_root = spec_root
        .join("tests")
        .join("spec")
        .join("resolver_sources")
        .join("fixtures")
        .join("basic");
    if !fixture_root.exists() {
        eprintln!(
            "resolver_sources fixtures missing at {}; skipping",
            fixture_root.display()
        );
        return;
    }

    let cache_dir = tempdir().expect("create cache dir");
    let sources_path = fixture_root.join("sources.json");

    let registry = new_registry();
    {
        let mut init_ctx = registry.context();
        let _ = init_ctx.call(
            "lcod://tooling/resolver/register_components@0.1.0",
            json!({ "specRoot": spec_root_string }),
            None,
        );
    }
    let mut ctx = registry.context();
    let input = json!({
        "projectPath": fixture_root.to_string_lossy(),
        "cacheDir": cache_dir.path().to_string_lossy(),
        "sourcesPath": sources_path.to_string_lossy(),
        "resolverConfig": {}
    });

    let output = ctx
        .call(
            "lcod://tooling/resolver/internal/load-sources@0.1.0",
            input,
            None,
        )
        .expect("load-sources call");

    let registry_sources = output
        .get("registrySources")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();

    assert!(
        !registry_sources.is_empty(),
        "load-sources returned no registry sources: {}",
        serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string())
    );

    let mut fixture_core_found = false;
    let mut fixture_extra_found = false;
    for entry in &registry_sources {
        let Some(id) = entry.get("id").and_then(JsonValue::as_str) else { continue };
        match id {
            "fixture/core" => {
                fixture_core_found = true;
                assert_eq!(entry.get("priority").and_then(JsonValue::as_i64), Some(50));
                let lines = entry.get("lines").and_then(JsonValue::as_array).unwrap();
                assert!(lines.iter().any(|line| {
                    line.get("id")
                        .and_then(JsonValue::as_str)
                        == Some("lcod://fixture/core")
                }));
            }
            "fixture/extra" => {
                fixture_extra_found = true;
                assert_eq!(entry.get("priority").and_then(JsonValue::as_i64), Some(75));
                let lines = entry.get("lines").and_then(JsonValue::as_array).unwrap();
                assert!(lines.iter().any(|line| {
                    line.get("id")
                        .and_then(JsonValue::as_str)
                        == Some("lcod://fixture/extra")
                }));
            }
            _ => {}
        }
    }

    assert!(fixture_core_found, "fixture/core catalogue missing");
    assert!(fixture_extra_found, "fixture/extra catalogue missing");

    let warnings = output
        .get("warnings")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(warnings.is_empty(), "load-sources emitted warnings: {warnings:?}");
    let returned_sources_path = output
        .get("sourcesPath")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    assert!(
        returned_sources_path.ends_with("fixtures/basic/sources.json"),
        "unexpected sourcesPath: {returned_sources_path}"
    );
}
