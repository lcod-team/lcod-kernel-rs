use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lcod_kernel_rs::compose::parse_compose;
use lcod_kernel_rs::{
    register_core, register_demo_impls, register_flow, register_tooling, run_compose,
    Context as KernelContext, Registry,
};
use serde_json::Value;

fn locate_spec_repo() -> Result<PathBuf> {
    if let Ok(path) = env::var("SPEC_REPO_PATH") {
        let candidate = PathBuf::from(&path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let candidates = [
        PathBuf::from("lcod-spec"),
        PathBuf::from("../lcod-spec"),
        PathBuf::from("../../lcod-spec"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("Unable to locate lcod-spec repository. Set SPEC_REPO_PATH");
}

fn load_compose(path: &Path) -> Result<Vec<lcod_kernel_rs::compose::Step>> {
    let yaml = fs::read_to_string(path)
        .with_context(|| format!("unable to read compose file: {}", path.display()))?;
    let value: Value = serde_yaml::from_str(&yaml)
        .with_context(|| format!("invalid compose YAML: {}", path.display()))?;
    let compose_value = value
        .get("compose")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("compose root missing in {}", path.display()))?;
    parse_compose(&compose_value)
        .with_context(|| format!("invalid compose structure in {}", path.display()))
}

fn run_test(compose_path: &Path) -> Result<bool> {
    let compose_path = compose_path.canonicalize()?;
    let compose_dir = compose_path.parent().unwrap_or(Path::new("."));
    let original = env::current_dir()?;
    env::set_current_dir(compose_dir)?;
    let compose = load_compose(&compose_path)?;

    let registry = Registry::new();
    register_flow(&registry);
    register_core(&registry);
    register_demo_impls(&registry);
    register_tooling(&registry);

    let mut ctx: KernelContext = registry.context();
    let result = run_compose(&mut ctx, &compose, Value::Object(Default::default()));
    env::set_current_dir(original)?;
    let result = result?;

    let report = result.get("report").cloned().unwrap_or(Value::Null);
    let success = report
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Ok(success)
}

fn main() -> Result<()> {
    let spec_root = locate_spec_repo()?;
    let tests_root = spec_root.join("examples/tests");
    let entries = fs::read_dir(&tests_root)
        .with_context(|| format!("unable to read tests directory: {}", tests_root.display()))?;

    let mut failures = 0usize;
    let mut total = 0usize;

    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let compose_path = entry.path().join("compose.yaml");
        if !compose_path.exists() {
            continue;
        }
        total += 1;
        match run_test(&compose_path) {
            Ok(true) => println!("✅ {}", entry.file_name().to_string_lossy()),
            Ok(false) => {
                failures += 1;
                println!("❌ {}", entry.file_name().to_string_lossy());
            }
            Err(err) => {
                failures += 1;
                println!("❌ {} — {}", entry.file_name().to_string_lossy(), err);
            }
        }
    }

    if total == 0 {
        println!("No spec tests discovered in {}", tests_root.display());
    }

    if failures == 0 {
        Ok(())
    } else {
        anyhow::bail!("{} spec test(s) failed", failures)
    }
}
