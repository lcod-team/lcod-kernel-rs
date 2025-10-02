use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lcod_kernel_rs::compose::parse_compose;
use lcod_kernel_rs::{
    register_core, register_demo_impls, register_flow, register_tooling, run_compose,
    Context as KernelContext, Registry,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct TestOutcome {
    name: String,
    success: bool,
    report: Value,
    result: Value,
    error: Option<String>,
}

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

fn run_test(name: &str, compose_path: &Path) -> Result<TestOutcome> {
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

    Ok(TestOutcome {
        name: name.to_string(),
        success,
        report,
        result,
        error: None,
    })
}

fn load_manifest(spec_root: &Path, manifest: &str) -> Result<Vec<(String, PathBuf)>> {
    let manifest_path = if Path::new(manifest).is_absolute() {
        PathBuf::from(manifest)
    } else {
        spec_root.join(manifest)
    };
    let content = fs::read_to_string(&manifest_path)
        .with_context(|| format!("unable to read manifest: {}", manifest_path.display()))?;
    let entries: Value = serde_json::from_str(&content)
        .with_context(|| format!("invalid JSON manifest: {}", manifest_path.display()))?;
    let items = entries.as_array().ok_or_else(|| anyhow::anyhow!(
        "manifest must be an array"
    ))?;
    let mut list = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("manifest entry missing name"))?;
        let compose = item
            .get("compose")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("manifest entry missing compose"))?;
        let compose_path = if Path::new(compose).is_absolute() {
            PathBuf::from(compose)
        } else {
            spec_root.join(compose)
        };
        list.push((name.to_string(), compose_path));
    }
    Ok(list)
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let mut json_output = false;
    let mut manifest_path: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--manifest" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--manifest requires a path"))?;
                manifest_path = Some(value);
            }
            other => return Err(anyhow::anyhow!("Unknown argument: {other}")),
        }
    }

    let spec_root = locate_spec_repo()?;
    let tests_root = spec_root.join("tests/spec");
    let mut results: Vec<TestOutcome> = Vec::new();

    if let Some(manifest) = manifest_path {
        let entries = load_manifest(&spec_root, &manifest)?;
        for (name, compose_path) in entries {
            let outcome = match run_test(&name, &compose_path) {
                Ok(res) => res,
                Err(err) => TestOutcome {
                    name,
                    success: false,
                    report: Value::Null,
                    result: Value::Null,
                    error: Some(err.to_string()),
                },
            };
            results.push(outcome);
        }
    } else {
        let entries = fs::read_dir(&tests_root)
            .with_context(|| format!("unable to read tests directory: {}", tests_root.display()))?;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let compose_path = entry.path().join("compose.yaml");
            if !compose_path.exists() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let outcome = match run_test(&name, &compose_path) {
                Ok(res) => res,
                Err(err) => TestOutcome {
                    name,
                    success: false,
                    report: Value::Null,
                    result: Value::Null,
                    error: Some(err.to_string()),
                },
            };
            results.push(outcome);
        }
    }

    let failures = results.iter().filter(|res| !res.success).count();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&results)?);
        if failures == 0 {
            Ok(())
        } else {
            anyhow::bail!("{} spec test(s) failed", failures)
        }
    } else {
        if results.is_empty() {
            println!("No spec tests discovered in {}", tests_root.display());
        }
        for res in &results {
            if res.success {
                println!("✅ {}", res.name);
            } else if let Some(ref err) = res.error {
                println!("❌ {} — {}", res.name, err);
            } else {
                println!("❌ {}", res.name);
            }
        }
        if failures == 0 {
            Ok(())
        } else {
            anyhow::bail!("{} spec test(s) failed", failures)
        }
    }
}
