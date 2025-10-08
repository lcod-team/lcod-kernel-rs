use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use lcod_kernel_rs::compose::{parse_compose, run_compose, Step};
use lcod_kernel_rs::{
    register_core, register_flow, register_http_contracts, register_tooling,
    Context as KernelContext, Registry,
};
use serde_json::{Map, Value};
use serde_yaml;

struct CliOptions {
    compose: PathBuf,
    state: Option<PathBuf>,
    serve: bool,
    project: Option<PathBuf>,
    config: Option<PathBuf>,
    output: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
}

fn parse_args() -> Result<CliOptions> {
    let mut args = env::args().skip(1);
    let mut compose: Option<PathBuf> = None;
    let mut state: Option<PathBuf> = None;
    let mut serve = false;
    let mut project: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--compose" | "-c" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--compose requires a path"))?;
                compose = Some(PathBuf::from(value));
            }
            "--state" | "-s" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--state requires a path"))?;
                state = Some(PathBuf::from(value));
            }
            "--serve" => {
                serve = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--project" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--project requires a path"))?;
                project = Some(PathBuf::from(value));
            }
            "--config" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--config requires a path"))?;
                config = Some(PathBuf::from(value));
            }
            "--output" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--output requires a path"))?;
                output = Some(PathBuf::from(value));
            }
            "--cache-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--cache-dir requires a path"))?;
                cache_dir = Some(PathBuf::from(value));
            }
            other => {
                return Err(anyhow!("unknown argument: {other}"));
            }
        }
    }

    let compose = compose.ok_or_else(|| anyhow!("--compose is required"))?;

    Ok(CliOptions {
        compose,
        state,
        serve,
        project,
        config,
        output,
        cache_dir,
    })
}

fn print_usage() {
    println!(
        "Usage: run_compose --compose path/to/compose.yaml [--state state.json]\n            [--project path] [--config path] [--output path] [--cache-dir path] [--serve]\n\n\
         Options:\n  --compose, -c   Path to compose YAML/JSON file (required)\n  --state,   -s   Optional path to initial state JSON file\n  --project       Override projectPath for resolver composes\n  --config        Override configPath for resolver composes\n  --output        Override outputPath for resolver composes\n  --cache-dir     Override LCOD_CACHE_DIR before execution\n  --serve         Keep HTTP hosts running until Ctrl+C\n  --help,    -h   Show this message"
    );
}

fn resolve_path(base: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

fn load_compose(path: &Path) -> Result<Vec<Step>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("unable to read compose file: {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let value: Value = if ext == "yaml" || ext == "yml" {
        serde_yaml::from_str(&text)
            .with_context(|| format!("invalid YAML compose: {}", path.display()))?
    } else {
        serde_json::from_str(&text)
            .with_context(|| format!("invalid JSON compose: {}", path.display()))?
    };

    let compose_value = match &value {
        Value::Object(map) => map
            .get("compose")
            .cloned()
            .ok_or_else(|| anyhow!("compose root missing in {}", path.display()))?,
        Value::Array(_) => value.clone(),
        _ => return Err(anyhow!("compose file must contain a compose array")),
    };

    parse_compose(&compose_value)
        .with_context(|| format!("invalid compose structure in {}", path.display()))
}

fn load_state(path: Option<PathBuf>) -> Result<Value> {
    match path {
        None => Ok(Value::Object(Map::new())),
        Some(p) => {
            let text = fs::read_to_string(&p)
                .with_context(|| format!("unable to read state file: {}", p.display()))?;
            let value = serde_json::from_str(&text)
                .with_context(|| format!("invalid JSON state: {}", p.display()))?;
            Ok(value)
        }
    }
}

fn collect_http_host_metadata(value: &Value, hosts: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) => {
            if let (Some(Value::String(url)), Some(handle)) = (map.get("url"), map.get("handle")) {
                hosts.push((url.clone(), handle.clone()));
            }
            for child in map.values() {
                collect_http_host_metadata(child, hosts);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_http_host_metadata(item, hosts);
            }
        }
        _ => {}
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let options = parse_args()?;
    let compose_steps = load_compose(&options.compose)?;
    let mut initial_state = load_state(options.state.clone())?;

    let current_dir = env::current_dir()?;

    if !initial_state.is_object() {
        initial_state = Value::Object(Map::new());
    }

    let mut state_map = initial_state.as_object().cloned().unwrap_or_else(Map::new);

    if let Some(project_path) = options.project.as_ref() {
        let resolved = resolve_path(&current_dir, project_path);
        state_map.insert(
            "projectPath".to_string(),
            Value::String(resolved.to_string_lossy().into()),
        );
        if options.output.is_none() && !state_map.contains_key("outputPath") {
            let lock_path = resolved.join("lcp.lock");
            state_map.insert(
                "outputPath".to_string(),
                Value::String(lock_path.to_string_lossy().into()),
            );
        }
    }

    if let Some(config_path) = options.config.as_ref() {
        let resolved = resolve_path(&current_dir, config_path);
        state_map.insert(
            "configPath".to_string(),
            Value::String(resolved.to_string_lossy().into()),
        );
    }

    if let Some(output_path) = options.output.as_ref() {
        let resolved = resolve_path(&current_dir, output_path);
        state_map.insert(
            "outputPath".to_string(),
            Value::String(resolved.to_string_lossy().into()),
        );
    }

    if let Some(cache_dir) = options.cache_dir.as_ref() {
        let resolved = resolve_path(&current_dir, cache_dir);
        env::set_var("LCOD_CACHE_DIR", resolved);
    }

    let initial_state = Value::Object(state_map);

    let registry = Registry::new();
    register_flow(&registry);
    register_core(&registry);
    register_tooling(&registry);
    register_http_contracts(&registry);
    lcod_kernel_rs::tooling::register_resolver_axioms(&registry);

    let mut ctx: KernelContext = registry.context();
    let result = run_compose(&mut ctx, &compose_steps, initial_state)?;

    println!("{}", serde_json::to_string_pretty(&result)?);

    let mut hosts = Vec::new();
    collect_http_host_metadata(&result, &mut hosts);

    if options.serve && !hosts.is_empty() {
        println!(
            "Serving {} HTTP host(s). Press Ctrl+C to stop.",
            hosts.len()
        );
        for (url, _) in &hosts {
            println!("  - {}", url);
        }
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        ctrlc::set_handler(move || {
            flag.store(false, Ordering::SeqCst);
        })?;
        while running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(200));
        }
        ctx.stop_all_http_hosts();
    } else {
        ctx.stop_all_http_hosts();
    }

    Ok(())
}
