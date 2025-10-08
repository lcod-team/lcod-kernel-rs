use lcod_kernel_rs::compose::{parse_compose, run_compose, Step};
use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::flow::register_flow;
use lcod_kernel_rs::registry::Registry;
use lcod_kernel_rs::tooling::{register_resolver_axioms, register_tooling};
use serde_json::json;
use std::env;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

fn resolver_compose_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = env::var("LCOD_RESOLVER_COMPOSE") {
        if !path.trim().is_empty() {
            candidates.push(PathBuf::from(path));
        }
    }
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

fn load_compose() -> Vec<Step> {
    let candidates = resolver_compose_candidates();
    for candidate in &candidates {
        match fs::read_to_string(candidate) {
            Ok(text) => {
                let yaml: serde_json::Value = serde_yaml::from_str(&text).expect("valid compose yaml");
                let steps_value = yaml.get("compose").cloned().expect("compose array present");
                return parse_compose(&steps_value).expect("parse compose steps");
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(err) => {
                panic!("failed to read {}: {}", candidate.display(), err);
            }
        }
    }
    panic!(
        "unable to locate resolver compose.yaml. checked: {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}
fn new_registry() -> Registry {
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

    let compose = load_compose();
    let output_path = project.join("lcp.lock");
    let state = json!({
        "projectPath": project,
        "configPath": config_path,
        "outputPath": output_path,
    });

    let result = run_compose(&mut ctx, &compose, state).expect("compose run");
    assert_eq!(result.get("warnings").unwrap().as_array().unwrap().len(), 0);
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
    assert_eq!(dep_entry["source"]["type"].as_str().unwrap(), "path");
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

    let compose = load_compose();
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
    assert_eq!(dep_entry["source"]["type"].as_str().unwrap(), "git");
    std::env::remove_var("LCOD_CACHE_DIR");
}
