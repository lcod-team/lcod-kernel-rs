use std::env;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, bail, Context as _, Result};
use lcod_kernel_rs::tooling::register_resolver_axioms;
use lcod_kernel_rs::{
    register_compose_contracts, register_core, register_flow, register_tooling,
    Context as KernelContext, Registry,
};
use serde_json::{json, Value};

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Result<Self> {
        let previous = env::var(key).ok();
        env::set_var(key, value);
        Ok(Self { key, previous })
    }

    fn unset(key: &'static str) -> Result<Self> {
        let previous = env::var(key).ok();
        env::remove_var(key);
        Ok(Self { key, previous })
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}

#[test]
fn catalog_generation_via_runtime_bundle() -> Result<()> {
    let spec_root = match locate_repo("SPEC_REPO_PATH", "lcod-spec") {
        Ok(path) => path,
        Err(err) => {
            eprintln!("runtime bundle test skipped (spec repo missing): {err}");
            return Ok(());
        }
    };
    let resolver_root = match locate_repo("LCOD_RESOLVER_PATH", "lcod-resolver") {
        Ok(path) => path,
        Err(err) => {
            eprintln!("runtime bundle test skipped (resolver repo missing): {err}");
            return Ok(());
        }
    };

    let bundle_output = spec_root.join("dist").join("runtime");
    std::fs::create_dir_all(&bundle_output)?;

    let node_modules = spec_root.join("node_modules");
    if !node_modules.is_dir() {
        let status = Command::new("npm")
            .args(["install", "--loglevel", "error", "--no-fund", "--no-audit"])
            .current_dir(&spec_root)
            .status()
            .context("installing lcod-spec dependencies")?;
        if !status.success() {
            bail!("npm install failed with status {}", status);
        }
    }
    let status = Command::new("node")
        .arg("scripts/package-runtime.mjs")
        .arg("--output")
        .arg(&bundle_output)
        .arg("--label")
        .arg("test")
        .arg("--keep")
        .arg("--resolver")
        .arg(&resolver_root)
        .current_dir(&spec_root)
        .status()
        .context("running package-runtime script")?;
    if !status.success() {
        bail!("package-runtime script failed with status {}", status);
    }

    let runtime_root = bundle_output.join("lcod-runtime-test");
    if !runtime_root.is_dir() {
        bail!("runtime bundle missing at {}", runtime_root.display());
    }

    let runtime_str = runtime_root
        .to_str()
        .ok_or_else(|| anyhow!("runtime directory is not UTF-8: {}", runtime_root.display()))?
        .to_string();
    let resolver_runtime = runtime_root.join("resolver");
    let resolver_str = resolver_runtime
        .to_str()
        .ok_or_else(|| {
            anyhow!(
                "resolver runtime directory is not UTF-8: {}",
                resolver_runtime.display()
            )
        })?
        .to_string();

    let _lcod_home = EnvGuard::set("LCOD_HOME", &runtime_str)?;
    let _spec_guard = EnvGuard::set("SPEC_REPO_PATH", &runtime_str)?;
    let _resolver_path = EnvGuard::set("LCOD_RESOLVER_PATH", &resolver_str)?;
    let _resolver_components = EnvGuard::unset("LCOD_RESOLVER_COMPONENTS_PATH")?;

    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);
    register_core(&registry);
    register_tooling(&registry);
    register_resolver_axioms(&registry);

    let fixtures_root = runtime_root
        .join("tooling")
        .join("registry")
        .join("catalog")
        .join("test")
        .join("fixtures");
    assert!(fixtures_root.join("catalog.json").is_file());

    let mut ctx: KernelContext = registry.context();
    let output = ctx.call(
        "lcod://tooling/registry/catalog/generate@0.1.0",
        json!({
            "rootPath": fixtures_root
                .to_str()
                .ok_or_else(|| anyhow!("fixtures path not UTF-8"))?,
            "catalogPath": "catalog.json"
        }),
        None,
    )?;

    let packages_jsonl = output
        .get("packagesJsonl")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        packages_jsonl.contains("lcod://demo/catalog"),
        "catalog output should include demo/catalog component"
    );
    assert!(
        output.get("registryJson").is_some(),
        "registryJson should be present in output"
    );

    Ok(())
}

fn locate_repo(env_key: &str, fallback_dir: &str) -> Result<PathBuf> {
    if let Ok(env_path) = env::var(env_key) {
        let candidate = PathBuf::from(env_path);
        if candidate.is_dir() {
            return candidate
                .canonicalize()
                .with_context(|| format!("canonicalizing {}", candidate.display()));
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut candidates = Vec::new();
    candidates.push(manifest_dir.join("..").join(fallback_dir));
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("..").join(fallback_dir));
        candidates.push(cwd.join("..").join("..").join(fallback_dir));
    }
    for candidate in candidates {
        if candidate.is_dir() {
            return candidate
                .canonicalize()
                .with_context(|| format!("canonicalizing {}", candidate.display()));
        }
    }
    bail!(
        "unable to locate {} repository (set {} environment variable)",
        fallback_dir,
        env_key
    );
}
