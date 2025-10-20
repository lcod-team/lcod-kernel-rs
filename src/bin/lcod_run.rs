use std::env;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Parser, ValueEnum};
use dirs::home_dir;
use flate2::read::GzDecoder;
use hex;
use humantime::format_duration;
use lcod_kernel_rs::compose::{parse_compose, run_compose, Step};
use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::flow::register_flow;
use lcod_kernel_rs::http::register_http_contracts;
use lcod_kernel_rs::registry::Registry;
use lcod_kernel_rs::tooling::{
    register_resolver_axioms, register_tooling, set_kernel_log_threshold,
};
use lcod_kernel_rs::CancelledError;
use lcod_kernel_rs::Context as KernelContext;
use serde_json::{json, Value};
use serde_yaml;
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::{Builder as TempDirBuilder, TempDir};
use toml::Value as TomlValue;
use url::Url;

mod embedded_runtime {
    include!(concat!(env!("OUT_DIR"), "/embedded_runtime.rs"));
}

struct ComposeHandle {
    path: PathBuf,
    temp_dir: Option<TempDir>,
}

impl ComposeHandle {
    fn local(path: PathBuf) -> Self {
        Self {
            path,
            temp_dir: None,
        }
    }

    fn temporary(path: PathBuf, temp_dir: TempDir) -> Self {
        Self {
            path,
            temp_dir: Some(temp_dir),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn is_temporary(&self) -> bool {
        self.temp_dir.is_some()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
            LogLevel::Fatal => "fatal",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "lcod-run",
    version,
    about = "Execute an LCOD compose with minimal setup"
)]
#[command(long_about = None)]
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

    /// Minimum kernel log level (trace|debug|info|warn|error|fatal)
    #[arg(long = "log-level", value_enum)]
    log_level: Option<LogLevel>,

    /// Abort execution after the given duration (e.g. "30s", "2m")
    #[arg(long = "timeout", value_parser = humantime::parse_duration, value_name = "DURATION")]
    timeout: Option<Duration>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err}");
        let mut sources = err.chain().skip(1);
        let mut idx = 0usize;
        while let Some(cause) = sources.next() {
            if idx == 0 {
                eprintln!("Caused by:");
            }
            idx += 1;
            eprintln!("  {idx}: {cause}");
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let opts = CliOptions::parse();

    let cancellation = Arc::new(AtomicBool::new(false));
    {
        let flag = cancellation.clone();
        ctrlc::set_handler(move || {
            if !flag.swap(true, Ordering::SeqCst) {
                eprintln!("Cancellation requested (Ctrl+C)");
            }
        })
        .context("Failed to install Ctrl+C handler")?;
    }

    if let Some(timeout) = opts.timeout {
        if timeout.is_zero() {
            cancellation.store(true, Ordering::SeqCst);
        } else {
            let flag = cancellation.clone();
            thread::spawn(move || {
                thread::sleep(timeout);
                if !flag.swap(true, Ordering::SeqCst) {
                    eprintln!("Execution timed out after {}", format_duration(timeout));
                }
            });
        }
    }

    if let Some(level) = opts.log_level {
        env::set_var("LCOD_LOG_LEVEL", level.as_str());
        set_kernel_log_threshold(level.as_str());
    }

    ensure_runtime_home()?;

    let compose_holder = acquire_compose(&opts.compose)?;
    let compose_path = compose_holder.path();

    let compose_dir = compose_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let default_lock = if compose_holder.is_temporary() {
        env::current_dir()
            .context("Unable to determine current directory for lockfile")?
            .join("lcp.lock")
    } else {
        compose_dir.join("lcp.lock")
    };
    let lock_path = opts.lock_path.clone().unwrap_or(default_lock);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Unable to create lockfile parent directory {}",
                parent.display()
            )
        })?;
    }

    let prefer_current_cache =
        compose_holder.is_temporary() && opts.cache_dir.is_none() && !opts.global_cache;
    let cache_dir = determine_cache_dir(&opts, &compose_dir, prefer_current_cache)?;
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("Unable to create cache directory {}", cache_dir.display()))?;
    env::set_var("LCOD_CACHE_DIR", &cache_dir);

    let registry = setup_registry();

    let has_manifest = compose_dir.join("lcp.toml").is_file();
    if opts.resolve && !has_manifest {
        eprintln!(
            "warning: --resolve requested but no lcp.toml found in {}; skipping resolver pipeline",
            compose_dir.display()
        );
    }
    let should_resolve = (opts.resolve || !lock_path.exists()) && has_manifest;
    if should_resolve {
        run_resolver_pipeline(&registry, &compose_dir, &lock_path)?;
    }

    let initial_state = load_input_state(opts.input)?;
    let compose_steps = load_compose(compose_path)?;

    let mut ctx = registry.context_with_cancellation(cancellation.clone());

    // ensure initial state is an object
    let state = match initial_state {
        Value::Object(map) => Value::Object(map),
        other => {
            eprintln!("warning: input payload is not an object; wrapping under {{\"input\": ...}}");
            json!({ "input": other })
        }
    };

    let result = match run_compose(&mut ctx, &compose_steps, state) {
        Ok(value) => value,
        Err(err) if err.is::<CancelledError>() => {
            eprintln!("Execution cancelled");
            std::process::exit(130);
        }
        Err(err) => return Err(err.context("Compose execution failed")),
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn acquire_compose(input: &Path) -> Result<ComposeHandle> {
    if let Some(url) = parse_remote_url(input) {
        return download_compose(&url);
    }
    let canonical = canonicalise_path(input)
        .with_context(|| format!("Unable to read compose path {}", input.display()))?;
    if !canonical.is_file() {
        return Err(anyhow!(
            "Compose path {} is not a regular file",
            canonical.display()
        ));
    }
    Ok(ComposeHandle::local(canonical))
}

fn parse_remote_url(input: &Path) -> Option<Url> {
    let raw = input.to_string_lossy();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        Url::parse(&raw).ok()
    } else {
        None
    }
}

fn download_compose(url: &Url) -> Result<ComposeHandle> {
    let agent = ureq::Agent::new();
    let response = agent
        .get(url.as_str())
        .call()
        .map_err(|err| anyhow!("Failed to download compose from {}: {}", url, err))?;
    if response.status() >= 400 {
        return Err(anyhow!(
            "Download failed for {}: HTTP {}",
            url,
            response.status()
        ));
    }
    let mut reader = response.into_reader();
    let mut buffer = Vec::new();
    reader
        .read_to_end(&mut buffer)
        .with_context(|| format!("Unable to read response body from {}", url))?;

    let temp_dir = TempDirBuilder::new()
        .prefix("lcod-compose-")
        .tempdir()
        .context("Unable to create temporary directory for compose download")?;
    let filename = derive_remote_filename(url);
    let path = temp_dir.path().join(filename);
    fs::write(&path, &buffer)
        .with_context(|| format!("Unable to write downloaded compose to {}", path.display()))?;

    Ok(ComposeHandle::temporary(path, temp_dir))
}

fn derive_remote_filename(url: &Url) -> PathBuf {
    let path = Path::new(url.path());
    if let Some(name) = path.file_name().filter(|n| !n.is_empty()) {
        PathBuf::from(name)
    } else if url.path().ends_with(".json") {
        PathBuf::from("compose.json")
    } else {
        PathBuf::from("compose.yaml")
    }
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

fn determine_cache_dir(
    opts: &CliOptions,
    compose_dir: &Path,
    prefer_current: bool,
) -> Result<PathBuf> {
    if let Some(explicit) = &opts.cache_dir {
        return canonicalise_path(explicit);
    }
    if opts.global_cache {
        let home = home_dir().ok_or_else(|| anyhow!("Unable to locate home directory"))?;
        let dir = home.join(".lcod").join("cache");
        return Ok(dir);
    }
    if prefer_current {
        let cwd = env::current_dir().context("Unable to locate current directory for cache")?;
        return Ok(cwd.join(".lcod").join("cache"));
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
    register_builtin_echo(&registry);
    registry
}

fn register_builtin_echo(registry: &Registry) {
    registry.register("lcod://impl/echo@1", builtin_echo_contract);
}

fn builtin_echo_contract(
    _ctx: &mut KernelContext,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let value = input.get("value").cloned().unwrap_or(Value::Null);
    Ok(json!({ "val": value }))
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

    let mut final_path = target.clone();
    if !runtime_manifest_present(&final_path) {
        let mut subdirs = fs::read_dir(&target)
            .with_context(|| format!("Unable to inspect runtime directory {}", target.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
            .collect::<Vec<_>>();
        if subdirs.len() == 1 {
            let candidate = subdirs.pop().expect("length checked").path();
            if runtime_manifest_present(&candidate) {
                final_path = candidate;
            }
        }
    }

    if !runtime_manifest_present(&final_path) {
        let _ = fs::remove_dir_all(&target);
        return Err(anyhow!(
            "Embedded runtime bundle does not contain manifest.json"
        ));
    }

    Ok(final_path)
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

fn run_resolver_pipeline(registry: &Registry, project_path: &Path, lock_path: &Path) -> Result<()> {
    let compose_path = resolver_compose_path()?;
    let steps = load_compose(&compose_path)?;
    let mut ctx = registry.context();
    let state = json!({
        "projectPath": project_path.to_string_lossy(),
        "configPath": Value::Null,
        "outputPath": lock_path.to_string_lossy(),
    });

    let result = run_compose(&mut ctx, &steps, state)
        .with_context(|| "Resolver pipeline execution failed")?;

    if let Some(warnings) = result.get("warnings").and_then(Value::as_array) {
        if !warnings.is_empty() {
            eprintln!("resolver warnings:");
            for warning in warnings {
                if let Some(message) = warning.as_str() {
                    eprintln!("  - {}", message);
                } else {
                    eprintln!("  - {}", warning);
                }
            }
        }
    }

    if !lock_path.exists() {
        return Err(anyhow!(
            "Resolver pipeline did not produce lockfile at {}",
            lock_path.display()
        ));
    }

    Ok(())
}

fn resolver_compose_path() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(home) = env::var("LCOD_HOME") {
        candidates.push(
            PathBuf::from(&home)
                .join("resolver")
                .join("packages")
                .join("resolver")
                .join("compose.yaml"),
        );
    }
    if let Ok(path) = env::var("LCOD_RESOLVER_COMPOSE") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("LCOD_RESOLVER_PATH") {
        let base = PathBuf::from(&path);
        candidates.push(base.join("packages").join("resolver").join("compose.yaml"));
        candidates.push(PathBuf::from(path).join("compose.yaml"));
    }
    if let Ok(spec) = env::var("SPEC_REPO_PATH") {
        candidates.push(
            PathBuf::from(spec)
                .join("packages")
                .join("resolver")
                .join("compose.yaml"),
        );
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(anyhow!(
        "Unable to locate resolver compose; ensure LCOD_HOME or LCOD_RESOLVER_PATH is configured"
    ))
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
