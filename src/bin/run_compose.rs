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
}

fn parse_args() -> Result<CliOptions> {
    let mut args = env::args().skip(1);
    let mut compose: Option<PathBuf> = None;
    let mut state: Option<PathBuf> = None;
    let mut serve = false;

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
    })
}

fn print_usage() {
    println!(
        "Usage: run_compose --compose path/to/compose.yaml [--state state.json] [--serve]\n\n\
         Options:\n  --compose, -c   Path to compose YAML/JSON file (required)\n  --state,   -s   Optional path to initial state JSON file\n  --serve         Keep HTTP hosts running until Ctrl+C\n  --help,    -h   Show this message"
    );
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
    let initial_state = load_state(options.state)?;

    let registry = Registry::new();
    register_flow(&registry);
    register_core(&registry);
    register_tooling(&registry);
    register_http_contracts(&registry);

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
