use std::convert::TryFrom;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use git2::{build::RepoBuilder, FetchOptions, Oid, Repository};
use humantime::format_rfc3339;
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const CONTRACT_GIT_CLONE: &str = "lcod://contract/core/git/clone@1";

pub fn register_git(registry: &Registry) {
    registry.register(CONTRACT_GIT_CLONE, git_clone_contract);
}

fn git_clone_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let url = input
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("`url` is required"))?;

    if input.get("auth").is_some() {
        return Err(anyhow!(
            "auth-backed clones are not yet implemented in the Rust substrate"
        ));
    }

    let dest = input.get("dest").and_then(Value::as_str);
    let depth = input.get("depth").and_then(Value::as_u64);
    let requested_ref = input.get("ref").and_then(Value::as_str);
    let subdir = input.get("subdir").and_then(Value::as_str);

    let checkout_root = prepare_checkout_directory(dest)?;

    let mut fetch_options = FetchOptions::new();
    if let Some(depth) = depth {
        let depth = i32::try_from(depth).map_err(|_| anyhow!("depth is too large"))?;
        fetch_options.depth(depth);
    }

    let mut builder = RepoBuilder::new();
    builder.fetch_options(fetch_options);
    if let Some(reference) = requested_ref {
        if !looks_like_commit(reference) {
            builder.branch(reference);
        }
    }

    let repo = builder
        .clone(url, &checkout_root)
        .with_context(|| format!("failed to clone `{url}`"))?;

    let (commit, resolved_ref) = if let Some(reference) = requested_ref {
        checkout_reference(&repo, reference)?
    } else {
        resolve_head(&repo)?
    };

    let exposed_path = if let Some(subdir) = subdir {
        let candidate = checkout_root.join(subdir);
        if !candidate.exists() {
            return Err(anyhow!("subdir `{subdir}` does not exist in repository"));
        }
        candidate
    } else {
        checkout_root.clone()
    };

    let mut output = Map::new();
    output.insert(
        "path".to_string(),
        Value::String(path_to_string(&exposed_path)?),
    );
    output.insert("commit".to_string(), Value::String(commit.to_string()));
    if let Some(reference) = resolved_ref {
        output.insert("ref".to_string(), Value::String(reference));
    }
    if let Some(subdir) = subdir {
        output.insert("subdir".to_string(), Value::String(subdir.to_string()));
    }
    output.insert(
        "source".to_string(),
        json!({
            "url": url,
            "fetchedAt": format_rfc3339(SystemTime::now()).to_string()
        }),
    );

    Ok(Value::Object(output))
}

fn prepare_checkout_directory(dest: Option<&str>) -> Result<PathBuf> {
    let workspace = std::env::temp_dir().join("lcod-git");
    fs::create_dir_all(&workspace)
        .with_context(|| format!("unable to prepare workspace at {}", workspace.display()))?;
    let path = if let Some(dest) = dest {
        let target = workspace.join(dest);
        if target.exists() {
            fs::remove_dir_all(&target).with_context(|| {
                format!("unable to clear existing destination {}", target.display())
            })?;
        }
        target
    } else {
        let unique = format!(
            "clone-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        workspace.join(unique)
    };
    if !path.exists() {
        fs::create_dir_all(&path)
            .with_context(|| format!("unable to create destination {}", path.display()))?;
    }
    Ok(path)
}

fn checkout_reference(repo: &Repository, reference: &str) -> Result<(Oid, Option<String>)> {
    if looks_like_commit(reference) {
        let oid = Oid::from_str(reference)
            .map_err(|err| anyhow!("invalid commit `{reference}`: {err}"))?;
        let object = repo
            .find_object(oid, None)
            .with_context(|| format!("commit `{reference}` not found"))?;
        repo.checkout_tree(&object, None)?;
        repo.set_head_detached(oid)?;
        return Ok((oid, None));
    }

    for candidate in candidate_refs(reference) {
        if let Ok(git_ref) = repo.find_reference(&candidate) {
            let oid = git_ref
                .target()
                .ok_or_else(|| anyhow!("reference `{candidate}` has no target"))?;
            let name = git_ref.name().map(|s| s.to_string());
            repo.set_head(git_ref.name().unwrap())?;
            repo.checkout_head(None)?;
            return Ok((oid, name));
        }
    }

    Err(anyhow!(
        "unable to resolve ref `{reference}` after cloning repository"
    ))
}

fn resolve_head(repo: &Repository) -> Result<(Oid, Option<String>)> {
    let head = repo.head()?.resolve()?;
    let oid = head
        .target()
        .ok_or_else(|| anyhow!("repository HEAD has no target"))?;
    Ok((oid, head.name().map(|s| s.to_string())))
}

fn candidate_refs(reference: &str) -> Vec<String> {
    if reference.starts_with("refs/") {
        vec![reference.to_string()]
    } else {
        vec![
            format!("refs/heads/{reference}"),
            format!("refs/remotes/origin/{reference}"),
            format!("refs/tags/{reference}"),
        ]
    }
}

fn looks_like_commit(reference: &str) -> bool {
    reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
}

fn path_to_string(path: &Path) -> Result<String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(crate::core::path::path_to_string(&canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository as GitRepository, Signature};
    use serde_json::json;
    use url::Url;

    #[test]
    fn clones_local_repository() {
        let (_guard, repo_url, commit) = create_fixture_repo();

        let registry = Registry::new();
        register_git(&registry);
        let mut ctx = registry.context();

        let input = json!({
            "url": repo_url,
            "dest": "test-clone",
        });

        let result = git_clone_contract(&mut ctx, input, None).unwrap();
        let path = result["path"].as_str().unwrap();
        assert!(Path::new(path).exists());
        assert_eq!(result["commit"], json!(commit));
    }

    fn create_fixture_repo() -> (tempfile::TempDir, String, String) {
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("source");
        fs::create_dir_all(&repo_path).unwrap();
        let repo = GitRepository::init(&repo_path).unwrap();
        let mut index = repo.index().unwrap();
        let file_path = repo_path.join("README.md");
        fs::write(&file_path, b"hello world").unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Tester", "tester@example.com").unwrap();
        let commit_id = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        let repo_url = Url::from_directory_path(&repo_path)
            .expect("valid repository path")
            .to_string();
        (temp, repo_url, commit_id.to_string())
    }
}
