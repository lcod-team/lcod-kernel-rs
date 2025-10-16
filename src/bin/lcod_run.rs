use std::env;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Parser};
use dirs::home_dir;
use flate2::read::GzDecoder;
use hex;
use lcod_kernel_rs::compose::{parse_compose, run_compose, Step};
use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::flow::register_flow;
use lcod_kernel_rs::http::register_http_contracts;
use lcod_kernel_rs::registry::Registry;
use lcod_kernel_rs::tooling::{register_resolver_axioms, register_tooling};
use serde_json::{json, Value};
use serde_yaml;
use sha2::{Digest, Sha256};
use tar::Archive;
use toml::Value as TomlValue;

mod embedded_runtime {
    #[allow(dead_code)]
    pub fn bundle_bytes() -> Option<&'static [u8]> {
        None
    }
}

#[derive(Parser, Debug)]
#[command(name = "lcod-run")]
#[command(about = "Execute an LCOD compose with minimal setup")]
struct CliOptions {
    /// Path to the compose file to execute (YAML/JSON)
    #[arg(long = "compose", short = 'c')]
    compose: PathBuf,

    /// JSON input payload file (use '-' for stdin)
    #[arg(long = "input", short = 'i')]
    input: Option<String>,

    /// Force lockfile resolution (not yet implemented)
    #[arg(long = "resolve", action = ArgAction::SetTrue)]
    resolve: bool,

    /// Output path for the generated lockfile
    #[arg(long = "lock")]
    lock_path: Option<PathBuf>,

    /// Override cache directory
    #[arg(long = "cache-dir")]
    cache_dir: Option<PathBuf>,

    /// Use global cache under ~/.lcod/cache
    #[arg(long = "global-cache", short = 'g', action = ArgAction::SetTrue)]
    global_cache: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let opts = CliOptions::parse();

    ensure_runtime_home()?;

    let compose_path = canonicalise_path(&opts.compose)
        .with_context(|| format!("Unable to read compose path {}", opts.compose.display()))?;
    if !compose_path.is_file() {
        return Err(anyhow!(
            "Compose path {} is not a regular file",
            compose_path.display()
        ));
    }

    let compose_dir = compose_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    if opts.resolve {
        eprintln!(
            "warning: --resolve not yet implemented, expecting lcp.lock to be present or unused"
        );
    }

    let lock_path = opts
        .lock_path
        .clone()
        .unwrap_or_else(|| compose_dir.join("lcp.lock"));
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Unable to create lockfile parent directory {}",
                parent.display()
            )
        })?;
    }

    let cache_dir = determine_cache_dir(&opts, &compose_dir)?;
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("Unable to create cache directory {}", cache_dir.display()))?;
    std::env::set_var("LCOD_CACHE_DIR", &cache_dir);

    let initial_state = load_input_state(opts.input)?;

    let compose_steps = load_compose(&compose_path)?;

    let registry = setup_registry();
    let mut ctx = registry.context();

    // ensure initial state is an object
    let state = match initial_state {
        Value::Object(map) => Value::Object(map),
        other => {
            eprintln!("warning: input payload is not an object; wrapping under {{\"input\": ...}}");
            json!({ "input": other })
        }
    };

    let result =
        run_compose(&mut ctx, &compose_steps, state).with_context(|| "Compose execution failed")?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn canonicalise_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .and_then(|joined| joined.canonicalize())
            .or_else(|_| Ok(path.to_path_buf()))
    }
}

fn determine_cache_dir(opts: &CliOptions, compose_dir: &Path) -> Result<PathBuf> {
    if let Some(explicit) = &opts.cache_dir {
        return canonicalise_path(explicit);
    }
    if opts.global_cache {
        let home = home_dir().ok_or_else(|| anyhow!("Unable to locate home directory"))?;
        let dir = home.join(".lcod").join("cache");
        return Ok(dir);
    }
    Ok(compose_dir.join(".lcod").join("cache"))
}

fn load_input_state(source: Option<String>) -> Result<Value> {
    let payload = match source {
        None => Value::Object(Default::default()),
        Some(path) if path == "-" => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .context("Failed to read JSON payload from stdin")?;
            if buffer.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&buffer).context("Invalid JSON payload read from stdin")?
            }
        }
        Some(path) => {
            let data =
                fs::read_to_string(&path).with_context(|| format!("Unable to read {path}"))?;
            serde_json::from_str(&data)
                .with_context(|| format!("Invalid JSON payload in {path}"))?
        }
    };
    Ok(payload)
}

fn setup_registry() -> Registry {
    let registry = Registry::new();
    register_flow(&registry);
    register_core(&registry);
    register_http_contracts(&registry);
    register_tooling(&registry);
    register_resolver_axioms(&registry);
    registry
}

fn ensure_runtime_home() -> Result<()> {
    if let Ok(existing) = env::var("LCOD_HOME") {
        let path = PathBuf::from(&existing);
        if runtime_manifest_present(&path) {
            return Ok(());
        }
    }

    if let Some(bytes) = embedded_runtime::bundle_bytes() {
        let install_path = install_embedded_runtime(bytes)?;
        env::set_var("LCOD_HOME", &install_path);
        return Ok(());
    }

    set_spec_repo_hint();
    Ok(())
}

fn runtime_manifest_present(path: &Path) -> bool {
    path.join("manifest.json").is_file()
}

fn install_embedded_runtime(bytes: &[u8]) -> Result<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    let bundle_id = format!("embedded-{}", hex::encode(hash));

    let home = home_dir().ok_or_else(|| anyhow!("Unable to locate user home directory"))?;
    let base = home.join(".lcod").join("runtime");
    let target = base.join(&bundle_id);
    if runtime_manifest_present(&target) {
        return Ok(target);
    }

    fs::create_dir_all(&base).with_context(|| {
        format!(
            "Unable to create runtime parent directory {}",
            base.display()
        )
    })?;

    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("Unable to clean runtime directory {}", target.display()))?;
    }

    fs::create_dir_all(&target)
        .with_context(|| format!("Unable to create runtime directory {}", target.display()))?;

    let cursor = Cursor::new(bytes);
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);
    if let Err(err) = archive.unpack(&target) {
        let _ = fs::remove_dir_all(&target);
        return Err(anyhow!("Failed to unpack embedded runtime bundle: {err}"));
    }

    Ok(target)
}

fn set_spec_repo_hint() {
    if env::var("SPEC_REPO_PATH").is_ok() {
        return;
    }
    if let Some(path) = candidate_spec_paths()
        .into_iter()
        .find(|candidate| candidate.is_dir())
    {
        env::set_var("SPEC_REPO_PATH", &path);
    }
}

fn candidate_spec_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe_path) = env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            candidates.push(dir.join("..").join("lcod-spec"));
            candidates.push(dir.join("../..").join("lcod-spec"));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("lcod-spec"));
        candidates.push(cwd.join("../lcod-spec"));
        candidates.push(cwd.join("../../lcod-spec"));
    }
    candidates
}

fn load_compose(path: &Path) -> Result<Vec<Step>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("unable to read compose file {}", path.display()))?;
    let raw: Value = if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "yaml" | "yml"))
        .unwrap_or(false)
    {
        serde_yaml::from_str(&text)
            .with_context(|| format!("invalid compose YAML {}", path.display()))?
    } else {
        serde_json::from_str(&text)
            .with_context(|| format!("invalid compose JSON {}", path.display()))?
    };

    let compose_value = match raw {
        Value::Object(mut map) => map
            .remove("compose")
            .ok_or_else(|| anyhow!("compose root missing in {}", path.display()))?,
        Value::Array(array) => Value::Array(array),
        _ => {
            return Err(anyhow!(
                "compose document must be an array or object with compose root"
            ))
        }
    };

    let mut canonical = compose_value;
    if let Some(context) = load_manifest_context(path.parent().unwrap_or(Path::new(".")))? {
        canonicalize_value(&mut canonical, &context);
    }

    parse_compose(&canonical)
        .with_context(|| format!("invalid compose structure in {}", path.display()))
}

#[derive(Clone, Debug)]
struct ManifestContext {
    base_path: Option<String>,
    version: Option<String>,
    alias_map: serde_json::Map<String, Value>,
}

fn load_manifest_context(dir: &Path) -> Result<Option<ManifestContext>> {
    let manifest_path = dir.join("lcp.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("unable to read manifest {}", manifest_path.display()))?;
    let manifest: TomlValue = raw
        .parse()
        .with_context(|| format!("invalid manifest TOML {}", manifest_path.display()))?;

    let manifest_id = manifest.get("id").and_then(TomlValue::as_str);
    let base_path = manifest_id
        .and_then(|id| id.strip_prefix("lcod://"))
        .and_then(|rest| rest.split('@').next())
        .map(|s| s.to_string())
        .or_else(|| {
            let ns = manifest
                .get("namespace")
                .and_then(TomlValue::as_str)
                .unwrap_or("");
            let name = manifest
                .get("name")
                .and_then(TomlValue::as_str)
                .unwrap_or("");
            let joined = [ns, name]
                .iter()
                .filter(|segment| !segment.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("/");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        });

    let version = manifest
        .get("version")
        .and_then(TomlValue::as_str)
        .map(|s| s.to_string())
        .or_else(|| {
            manifest_id
                .and_then(|id| id.split('@').nth(1))
                .map(|s| s.to_string())
        });

    let alias_map = manifest
        .get("workspace")
        .and_then(TomlValue::as_table)
        .and_then(|table| table.get("scopeAliases"))
        .and_then(TomlValue::as_table)
        .map(|aliases| {
            aliases
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|alias| (key.clone(), alias.into())))
                .collect::<serde_json::Map<String, Value>>()
        })
        .unwrap_or_default();

    Ok(Some(ManifestContext {
        base_path,
        version,
        alias_map,
    }))
}

fn canonicalize_value(value: &mut Value, context: &ManifestContext) {
    match value {
        Value::Array(items) => {
            for item in items {
                canonicalize_value(item, context);
            }
        }
        Value::Object(map) => canonicalize_object(map, context),
        _ => {}
    }
}

fn canonicalize_object(map: &mut serde_json::Map<String, Value>, context: &ManifestContext) {
    if let Some(Value::String(call)) = map.get_mut("call") {
        if let Some(canonical) = canonicalize_id(call, context) {
            *call = canonical;
        }
    }
    if let Some(children) = map.get_mut("children") {
        canonicalize_value(children, context);
    }
    for (_key, value) in map.iter_mut() {
        canonicalize_value(value, context);
    }
}

fn canonicalize_id(raw: &str, context: &ManifestContext) -> Option<String> {
    if raw.starts_with("lcod://") {
        return Some(raw.to_string());
    }
    let trimmed = raw.trim_start_matches("./");
    if trimmed.is_empty() {
        return Some(raw.to_string());
    }
    let mut segments = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Some(raw.to_string());
    }
    let alias = segments.remove(0);
    let mapped = context
        .alias_map
        .get(alias)
        .and_then(Value::as_str)
        .unwrap_or(alias);

    let mut parts = Vec::new();
    if let Some(base) = &context.base_path {
        if !base.is_empty() {
            parts.push(base.clone());
        }
    }
    if !mapped.is_empty() {
        parts.push(mapped.to_string());
    }
    for segment in segments {
        parts.push(segment.to_string());
    }

    let version = context.version.as_deref().unwrap_or("0.0.0");
    let id = format!("lcod://{}@{}", parts.join("/"), version);
    Some(id)
}
