use base64::Engine as _;
use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::tooling::register_resolver_axioms;
use lcod_kernel_rs::{Context, Registry};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::process::Command;

fn expected_integrity(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    format!(
        "sha256-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

#[test]
fn resolve_dependency_cycle_detected() {
    let registry = Registry::new();
    register_core(&registry);
    register_resolver_axioms(&registry);
    let mut ctx: Context = registry.context();

    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let comp_a = base.join("compA");
    let comp_b = base.join("compB");
    fs::create_dir_all(&comp_a).unwrap();
    fs::create_dir_all(&comp_b).unwrap();
    fs::write(
        comp_a.join("lcp.toml"),
        "schemaVersion = \"1.0\"\nid = \"lcod://example/a@0.1.0\"\nname = \"a\"\nnamespace = \"example\"\nversion = \"0.1.0\"\nkind = \"workflow\"\n\n[deps]\nrequires = [\"lcod://example/b@0.1.0\"]\n",
    )
    .unwrap();
    fs::write(
        comp_b.join("lcp.toml"),
        "schemaVersion = \"1.0\"\nid = \"lcod://example/b@0.1.0\"\nname = \"b\"\nnamespace = \"example\"\nversion = \"0.1.0\"\nkind = \"workflow\"\n\n[deps]\nrequires = [\"lcod://example/a@0.1.0\"]\n",
    )
    .unwrap();

    let config = json!({
        "sources": {
            "lcod://example/a@0.1.0": { "type": "path", "path": "compA" },
            "lcod://example/b@0.1.0": { "type": "path", "path": "compB" }
        }
    });

    let result = ctx.call(
        "lcod://contract/tooling/resolve-dependency@1",
        json!({
            "dependency": "lcod://example/a@0.1.0",
            "config": config,
            "projectPath": base.to_string_lossy(),
            "stack": []
        }),
        None,
    );

    assert!(result.is_err());
    let err = result.err().unwrap();
    let message = format!("{err}");
    assert!(message.contains("dependency cycle"));
}

#[test]
fn resolve_dependency_path_integrity() {
    let registry = Registry::new();
    register_core(&registry);
    register_resolver_axioms(&registry);
    let mut ctx: Context = registry.context();

    let temp = tempfile::tempdir().unwrap();
    let component_dir = temp.path().join("comp");
    fs::create_dir_all(&component_dir).unwrap();
    let descriptor =
        "schemaVersion = \"1.0\"\nid = \"lcod://example/comp@0.1.0\"\n[deps]\nrequires = []\n";
    fs::write(component_dir.join("lcp.toml"), descriptor).unwrap();

    let config = json!({
        "sources": {
            "lcod://example/comp@0.1.0": { "type": "path", "path": "comp" }
        }
    });

    let result = ctx
        .call(
            "lcod://contract/tooling/resolve-dependency@1",
            json!({
                "dependency": "lcod://example/comp@0.1.0",
                "config": config,
                "projectPath": temp.path().to_string_lossy(),
                "stack": []
            }),
            None,
        )
        .unwrap();
    let resolved = result.get("resolved").unwrap();

    assert_eq!(
        resolved.get("integrity").unwrap().as_str().unwrap(),
        expected_integrity(descriptor)
    );
    assert_eq!(
        resolved
            .get("source")
            .unwrap()
            .get("path")
            .unwrap()
            .as_str()
            .unwrap(),
        component_dir.canonicalize().unwrap().to_string_lossy()
    );
    assert!(resolved
        .get("dependencies")
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn resolve_dependency_git_integrity() {
    let registry = Registry::new();
    register_core(&registry);
    register_resolver_axioms(&registry);
    let mut ctx: Context = registry.context();

    let temp = tempfile::tempdir().unwrap();
    let repo_dir = temp.path().join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let descriptor =
        "schemaVersion = \"1.0\"\nid = \"lcod://example/git@0.1.0\"\n[deps]\nrequires = []\n";
    fs::write(repo_dir.join("lcp.toml"), descriptor).unwrap();

    Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "resolver@example.com"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Resolver Bot"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["add", "lcp.toml"])
        .current_dir(&repo_dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .env("GIT_AUTHOR_NAME", "Resolver Bot")
        .env("GIT_AUTHOR_EMAIL", "resolver@example.com")
        .env("GIT_COMMITTER_NAME", "Resolver Bot")
        .env("GIT_COMMITTER_EMAIL", "resolver@example.com")
        .current_dir(&repo_dir)
        .status()
        .unwrap();

    let config = json!({
        "sources": {
            "lcod://example/git@0.1.0": { "type": "git", "url": repo_dir.to_string_lossy() }
        }
    });

    env::set_var(
        "LCOD_CACHE_DIR",
        temp.path().join("cache").to_string_lossy().to_string(),
    );
    let result = ctx
        .call(
            "lcod://contract/tooling/resolve-dependency@1",
            json!({
                "dependency": "lcod://example/git@0.1.0",
                "config": config,
                "projectPath": temp.path().to_string_lossy(),
                "stack": []
            }),
            None,
        )
        .unwrap();
    env::remove_var("LCOD_CACHE_DIR");

    let resolved = result.get("resolved").unwrap();
    assert_eq!(resolved.get("source").unwrap().get("type").unwrap(), "git");
    assert!(resolved
        .get("integrity")
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with("sha256-"));
}
