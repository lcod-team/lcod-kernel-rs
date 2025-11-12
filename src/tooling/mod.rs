use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use toml::Value as TomlValue;

use crate::compose::{parse_compose, run_compose};
use crate::registry::{ComponentMetadata, Context, Registry};

mod common;
mod logging;
mod registry_scope;
mod resolver;
mod script;

pub use logging::{
    log_kernel_debug, log_kernel_error, log_kernel_info, log_kernel_warn, set_kernel_log_threshold,
};

const CONTRACT_TEST_CHECKER: &str = "lcod://tooling/test_checker@1";
const CONTRACT_REGISTRY_NORMALIZE_SOURCE: &str =
    "lcod://contract/tooling/registry/normalize_source@1";
const CONTRACT_REGISTRY_NORMALIZE_SOURCES: &str =
    "lcod://contract/tooling/registry/normalize_sources@1";

pub fn register_tooling(registry: &Registry) {
    registry.register(CONTRACT_TEST_CHECKER, test_checker);
    script::register_script_contract(registry);
    registry_scope::register_registry_scope(registry);
    logging::register_logging(registry);
    register_std_helpers(registry);
    register_resolver_helpers(registry);
    register_std_helpers(registry);
}

fn register_std_helpers(registry: &Registry) {
    registry.register(
        "lcod://contract/tooling/value/is_defined@1",
        value_is_defined_helper,
    );
    registry.register(
        "lcod://tooling/value/is_defined@0.1.0",
        value_is_defined_helper,
    );
    registry.register(
        "lcod://tooling/value/is_string_nonempty@0.1.0",
        value_is_string_nonempty_helper,
    );
    registry.register(
        "lcod://contract/tooling/string/ensure_trailing_newline@1",
        string_ensure_trailing_newline_helper,
    );
    registry.register(
        "lcod://tooling/string/ensure_trailing_newline@0.1.0",
        string_ensure_trailing_newline_helper,
    );
    registry.register(
        "lcod://contract/tooling/array/compact@1",
        array_compact_helper,
    );
    registry.register("lcod://tooling/array/compact@0.1.0", array_compact_helper);
    registry.register(
        "lcod://contract/tooling/array/flatten@1",
        array_flatten_helper,
    );
    registry.register("lcod://tooling/array/flatten@0.1.0", array_flatten_helper);
    registry.register(
        "lcod://contract/tooling/array/find_duplicates@1",
        array_find_duplicates_helper,
    );
    registry.register(
        "lcod://tooling/array/find_duplicates@0.1.0",
        array_find_duplicates_helper,
    );
    registry.register(
        "lcod://contract/tooling/array/append@1",
        array_append_helper,
    );
    registry.register("lcod://tooling/array/append@0.1.0", array_append_helper);
    registry.register("lcod://contract/tooling/queue/bfs@1", queue_bfs_helper);
    registry.register(
        "lcod://contract/tooling/path/join_chain@1",
        path_join_chain_helper,
    );
    registry.register(
        "lcod://tooling/path/join_chain@0.1.0",
        path_join_chain_helper,
    );
    registry.register("lcod://contract/tooling/jsonl/read@1", jsonl_read_helper);
    registry.register(
        "lcod://contract/tooling/jsonl/read@1.0.0",
        jsonl_read_helper,
    );
    registry.register("lcod://tooling/jsonl/read@0.1.0", jsonl_read_helper);
    registry.register(
        "lcod://contract/tooling/fs/read_optional@1",
        fs_read_optional_helper,
    );
    registry.register(
        "lcod://contract/tooling/fs/write_if_changed@1",
        fs_write_if_changed_helper,
    );
    registry.register("lcod://tooling/object/clone@0.1.0", object_clone_helper);
    registry.register("lcod://tooling/object/set@0.1.0", object_set_helper);
    registry.register("lcod://tooling/object/has@0.1.0", object_has_helper);
    registry.register("lcod://tooling/object/entries@0.1.0", object_entries_helper);
    registry.register(
        "lcod://tooling/json/stable_stringify@0.1.0",
        json_stable_stringify_helper,
    );
    registry.register("lcod://tooling/hash/to_key@0.1.0", hash_to_key_helper);
    registry.register("lcod://tooling/queue/bfs@0.1.0", queue_bfs_helper);
    registry.register(
        CONTRACT_REGISTRY_NORMALIZE_SOURCE,
        registry_normalize_source,
    );
    registry.register(
        CONTRACT_REGISTRY_NORMALIZE_SOURCES,
        registry_normalize_sources,
    );
}

struct RegistryNormalizeResult {
    entry: Option<Value>,
    warnings: Vec<Value>,
}

fn registry_normalize_source(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let entry_value = input
        .get("entry")
        .cloned()
        .ok_or_else(|| anyhow!("`entry` is required"))?;
    let result = normalize_registry_entry(&entry_value);
    Ok(json!({
        "entry": result.entry.unwrap_or(Value::Null),
        "warnings": result.warnings
    }))
}

fn registry_normalize_sources(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let entries_value = input.get("entries").cloned().unwrap_or(Value::Null);
    let entries = entries_value.as_array().cloned().unwrap_or_default();
    let mut normalized_entries = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries {
        let result = normalize_registry_entry(&entry);
        warnings.extend(result.warnings);
        if let Some(value) = result.entry {
            normalized_entries.push(value);
        }
    }
    Ok(json!({
        "entries": normalized_entries,
        "warnings": warnings
    }))
}

fn normalize_registry_entry(entry_value: &Value) -> RegistryNormalizeResult {
    let Some(entry_map) = entry_value.as_object() else {
        return RegistryNormalizeResult {
            entry: None,
            warnings: Vec::new(),
        };
    };

    let mut warnings = Vec::new();
    let Some(registry_id) = read_trimmed_string(entry_map.get("id")) else {
        warnings.push(Value::String(
            "registry source entry is missing an id".to_owned(),
        ));
        return RegistryNormalizeResult {
            entry: None,
            warnings,
        };
    };

    let registry_type =
        read_trimmed_string(entry_map.get("type")).unwrap_or_else(|| "path".to_owned());
    let mut normalized = Map::new();
    normalized.insert("id".to_string(), Value::String(registry_id.clone()));
    normalized.insert("type".to_string(), Value::String(registry_type.clone()));

    if let Some(priority_value) = entry_map.get("priority").and_then(|v| v.as_f64()) {
        if priority_value.is_finite() {
            let truncated = priority_value.trunc() as i64;
            normalized.insert("priority".to_string(), Value::Number(truncated.into()));
        }
    }

    if let Some(defaults) = entry_map
        .get("defaults")
        .and_then(|v| v.as_object())
        .cloned()
    {
        normalized.insert("defaults".to_string(), Value::Object(defaults));
    }

    if let Some(registry_path) = read_trimmed_string(entry_map.get("registryPath")) {
        normalized.insert("registryPath".to_string(), Value::String(registry_path));
    }

    if let Some(packages_path) = read_trimmed_string(entry_map.get("packagesPath")) {
        normalized.insert("packagesPath".to_string(), Value::String(packages_path));
    }

    let normalized_entry = match registry_type.as_str() {
        "path" => {
            if let Some(path) = read_trimmed_string(entry_map.get("path")) {
                normalized.insert("path".to_string(), Value::String(path));
                Some(Value::Object(normalized))
            } else {
                warnings.push(Value::String(format!(
                    "registry source \"{}\" (type=path) is missing \"path\"",
                    registry_id
                )));
                None
            }
        }
        "jsonl" => {
            let path = read_trimmed_string(entry_map.get("path"));
            let inline_jsonl = read_trimmed_string(entry_map.get("jsonl"));
            if let Some(path) = path {
                normalized.insert("path".to_string(), Value::String(path));
                Some(Value::Object(normalized))
            } else if let Some(jsonl_text) = inline_jsonl {
                normalized.insert("jsonl".to_string(), Value::String(jsonl_text));
                Some(Value::Object(normalized))
            } else {
                warnings.push(Value::String(format!(
                    "registry source \"{}\" (type=jsonl) is missing \"path\" or inline \"jsonl\" content",
                    registry_id
                )));
                None
            }
        }
        "inline" => {
            let lines = entry_map
                .get("lines")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|value| value.as_object().cloned())
                        .map(Value::Object)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if lines.is_empty() {
                warnings.push(Value::String(format!(
                    "registry source \"{}\" (type=inline) is missing \"lines\" entries",
                    registry_id
                )));
                None
            } else {
                normalized.insert("lines".to_string(), Value::Array(lines));
                if let Some(jsonl_text) = read_trimmed_string(entry_map.get("jsonl")) {
                    normalized.insert("jsonl".to_string(), Value::String(jsonl_text));
                }
                Some(Value::Object(normalized))
            }
        }
        other => {
            warnings.push(Value::String(format!(
                "registry source \"{}\" has unsupported type \"{}\"",
                registry_id, other
            )));
            None
        }
    };

    RegistryNormalizeResult {
        entry: normalized_entry,
        warnings,
    }
}

fn read_trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
}

fn runtime_root() -> Option<PathBuf> {
    runtime_root_from_env().or_else(runtime_root_from_executable)
}

fn runtime_root_from_env() -> Option<PathBuf> {
    let home = env::var("LCOD_HOME").ok()?;
    let candidate = PathBuf::from(home);
    if is_runtime_dir(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

fn runtime_root_from_executable() -> Option<PathBuf> {
    let exe_path = env::current_exe().ok()?;
    let exe_dir = exe_path.parent()?;

    let candidate = exe_dir.join("runtime");
    if is_runtime_dir(&candidate) {
        return Some(candidate);
    }

    if let Ok(entries) = fs::read_dir(exe_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("lcod-runtime-") {
                continue;
            }
            if is_runtime_dir(&path) {
                return Some(path);
            }
        }
    }

    None
}

fn is_runtime_dir(path: &Path) -> bool {
    path.join("manifest.json").is_file() && path.join("tooling").is_dir()
}

fn runtime_resolver_root() -> Option<PathBuf> {
    runtime_root().and_then(|root| {
        let resolver = root.join("resolver");
        if resolver.join("workspace.lcp.toml").is_file() {
            Some(resolver)
        } else {
            None
        }
    })
}

fn resolve_spec_root() -> Option<PathBuf> {
    if let Ok(env_path) = env::var("SPEC_REPO_PATH") {
        let candidate = PathBuf::from(env_path);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    if let Some(root) = runtime_root() {
        return Some(root);
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut candidates = Vec::new();
    candidates.push(manifest_dir.join("..").join("lcod-spec"));
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("../lcod-spec"));
        candidates.push(cwd.join("../../lcod-spec"));
    }
    for candidate in candidates {
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

thread_local! {
    static SPEC_REGISTER_GUARD: Cell<bool> = Cell::new(false);
}

fn run_spec_register_components(
    registry: &Registry,
    spec_root_override: Option<&str>,
) -> Result<Value> {
    let skip = SPEC_REGISTER_GUARD.with(|flag| {
        if flag.get() {
            true
        } else {
            flag.set(true);
            false
        }
    });
    if skip {
        return Ok(Value::Null);
    }

    let result = (|| {
        let spec_root = if let Some(override_path) = spec_root_override {
            let candidate = PathBuf::from(override_path);
            if candidate.is_absolute() {
                candidate
            } else {
                env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(candidate)
            }
        } else {
            resolve_spec_root().ok_or_else(|| {
                anyhow!(
                    "[tooling/registry] Unable to locate lcod-spec repository; helpers not registered"
                )
            })?
        };
        let register_path = spec_root.join("tooling/resolver/register_components/compose.yaml");
        if !register_path.is_file() {
            return Err(anyhow!(
                "[tooling/registry] register_components compose missing: {}",
                register_path.display()
            ));
        }
        let steps = load_compose_from_path(&register_path)?;
        let mut ctx = registry.context();
        run_compose(
            &mut ctx,
            &steps,
            json!({ "specRoot": crate::core::path::path_to_string(&spec_root) }),
        )
    })();

    SPEC_REGISTER_GUARD.with(|flag| flag.set(false));
    result
}

fn register_resolver_helpers(registry: &Registry) {
    let dynamic_registry = registry.clone();
    registry.register(
        "lcod://tooling/resolver/register@1",
        move |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let components = input
                .get("components")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut warnings = Vec::new();
            let mut count = 0usize;

            for component in components {
                let Some(id_raw) = component.get("id").and_then(Value::as_str) else {
                    warnings.push("resolver/register: component missing id".to_string());
                    continue;
                };
                if !id_raw.starts_with("lcod://") {
                    warnings.push(format!(
                        "resolver/register: component id must be canonical, got {}",
                        id_raw
                    ));
                    continue;
                }

                let (compose_steps, compose_path_buf) = if let Some(inline) =
                    component.get("compose")
                {
                    let steps = parse_compose(inline).with_context(|| {
                        format!("resolver/register: invalid inline compose for {}", id_raw)
                    })?;
                    (steps, None)
                } else if let Some(path_str) = component.get("composePath").and_then(Value::as_str)
                {
                    let path = PathBuf::from(path_str);
                    let steps = load_compose_from_path(&path).with_context(|| {
                        format!(
                            "resolver/register: failed to load compose for {} from {}",
                            id_raw,
                            path.display()
                        )
                    })?;
                    (steps, Some(path))
                } else {
                    warnings.push(format!(
                        "resolver/register: component {} missing compose data",
                        id_raw
                    ));
                    continue;
                };

                let metadata = find_component_manifest_path(
                    &component,
                    compose_path_buf.as_ref().map(|path| path.as_path()),
                )
                .and_then(|manifest_path| load_component_metadata(&manifest_path));

                let steps_arc = Arc::new(compose_steps);
                let id_string = id_raw.to_string();
                let registry_clone = dynamic_registry.clone();
                let metadata_arc = metadata.map(Arc::new);
                registry_clone.register_with_metadata(
                    id_string.clone(),
                    move |ctx_inner: &mut Context,
                          input_inner: Value,
                          _meta_inner: Option<Value>| {
                        let steps = Arc::clone(&steps_arc);
                        run_compose(ctx_inner, &steps, input_inner)
                    },
                    metadata_arc.clone(),
                );
                count += 1;
            }

            let mut result = Map::new();
            result.insert("registered".to_string(), Value::from(count as u64));
            if !warnings.is_empty() {
                result.insert(
                    "warnings".to_string(),
                    Value::Array(warnings.into_iter().map(Value::from).collect()),
                );
            }
            Ok(Value::Object(result))
        },
    );

    if let Err(err) = run_spec_register_components(registry, None) {
        let _ = log_kernel_warn(
            None,
            "Failed to register resolver helpers from spec",
            Some(json!({ "error": err.to_string() })),
            Some(json!({ "module": "resolver-helpers" })),
        );
    }

    let cloned_registry = registry.clone();
    registry.register(
        "lcod://tooling/resolver/register_components@0.1.0",
        move |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let spec_root_override = input
                .get("specRoot")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            run_spec_register_components(&cloned_registry, spec_root_override.as_deref())
        },
    );

    let components_registry = registry.clone();
    registry.register(
        "lcod://tooling/resolver/register_components@0.1.0",
        move |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let spec_root = input.get("specRoot").and_then(Value::as_str);
            run_spec_register_components(&components_registry, spec_root)
        },
    );

    let defs = build_helper_definitions();
    for def in defs {
        let compose_path = Arc::new(def.compose_path);
        let context = Arc::new(def.context);
        let metadata = def.metadata.map(Arc::new);
        let ids: Vec<String> = std::iter::once(def.id.clone())
            .chain(def.aliases.into_iter())
            .collect();
        for id in ids {
            let id_arc = Arc::new(id.clone());
            let compose_path = Arc::clone(&compose_path);
            let context = Arc::clone(&context);
            let metadata_handle = metadata.clone();
            if id_arc.contains("testkit") {
                let meta_flag = metadata_handle.is_some();
                let source = compose_path.display().to_string();
                eprintln!(
                    "[registry] registering {} metadata_present={} source={}",
                    id_arc, meta_flag, source
                );
            }
            registry.register_with_metadata(
                id,
                move |ctx: &mut Context, input: Value, _meta: Option<Value>| {
                    let steps =
                        load_helper_compose(&compose_path, &context).with_context(|| {
                            format!("unable to load resolver helper {}", id_arc.as_ref())
                        })?;
                    run_compose(ctx, &steps, input)
                },
                metadata_handle.clone(),
            );
        }
    }
}

#[derive(Clone)]
struct HelperContext {
    base_path: String,
    version: String,
    alias_map: HashMap<String, String>,
}

struct ResolverHelperDef {
    id: String,
    compose_path: PathBuf,
    context: HelperContext,
    aliases: Vec<String>,
    metadata: Option<ComponentMetadata>,
}

fn find_component_manifest_path(component: &Value, compose_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = component.get("lcpPath").and_then(Value::as_str) {
        return Some(resolve_manifest_path(path, compose_path));
    }
    if let Some(lcp_field) = component.get("lcp") {
        if let Some(path) = lcp_field.as_str() {
            return Some(resolve_manifest_path(path, compose_path));
        }
        if let Some(obj) = lcp_field.as_object() {
            if let Some(path) = obj.get("path").and_then(Value::as_str) {
                return Some(resolve_manifest_path(path, compose_path));
            }
        }
    }
    compose_path.and_then(|path| path.parent().map(|dir| dir.join("lcp.toml")))
}

fn resolve_manifest_path(path: &str, compose_path: Option<&Path>) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    if let Some(base) = compose_path.and_then(|cp| cp.parent()) {
        return base.join(path);
    }
    candidate
}

#[derive(Debug)]
enum CandidateKind {
    Root,
    Legacy,
}

#[derive(Debug)]
struct Candidate {
    kind: CandidateKind,
    path: PathBuf,
}

fn push_candidate_if_exists(
    seen: &mut HashSet<String>,
    out: &mut Vec<Candidate>,
    kind: CandidateKind,
    path: PathBuf,
) {
    if !path.exists() {
        return;
    }
    let key = format!("{:?}:{}", kind, path.display());
    if !seen.insert(key) {
        return;
    }
    out.push(Candidate { kind, path });
}

fn include_workspace_sources(seen: &mut HashSet<String>, out: &mut Vec<Candidate>, path: PathBuf) {
    if !path.exists() {
        return;
    }
    let mut handled = false;
    if path.join("workspace.lcp.toml").is_file() {
        push_candidate_if_exists(seen, out, CandidateKind::Root, path.clone());
        handled = true;
    }
    let components_dir = path.join("components");
    if components_dir.is_dir() {
        push_candidate_if_exists(seen, out, CandidateKind::Legacy, components_dir);
        handled = true;
    }
    let std_components = path.join("packages").join("std").join("components");
    if std_components.is_dir() {
        push_candidate_if_exists(seen, out, CandidateKind::Legacy, std_components);
        handled = true;
    }
    if !handled && path.is_dir() {
        push_candidate_if_exists(seen, out, CandidateKind::Legacy, path);
    }
}
fn build_helper_definitions() -> Vec<ResolverHelperDef> {
    let candidates = gather_candidates();
    let mut collected = Vec::new();
    for candidate in candidates {
        let defs = load_definitions_for_candidate(&candidate);
        if !defs.is_empty() {
            collected.extend(defs);
        }
    }
    append_spec_fallbacks(&mut collected);
    collected
}

fn gather_candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Some(runtime_resolver) = runtime_resolver_root() {
        push_candidate_if_exists(&mut seen, &mut out, CandidateKind::Root, runtime_resolver);
    }
    if let Some(runtime) = runtime_root() {
        let tooling_resolver = runtime.join("tooling").join("resolver");
        push_candidate_if_exists(&mut seen, &mut out, CandidateKind::Legacy, tooling_resolver);
        let tooling_registry = runtime.join("tooling").join("registry");
        push_candidate_if_exists(&mut seen, &mut out, CandidateKind::Legacy, tooling_registry);
    }

    if let Ok(paths) = env::var("LCOD_RESOLVER_COMPONENTS_PATH") {
        for entry in env::split_paths(&paths) {
            push_candidate_if_exists(&mut seen, &mut out, CandidateKind::Legacy, entry);
        }
    }
    if let Ok(paths) = env::var("LCOD_RESOLVER_PATH") {
        for entry in env::split_paths(&paths) {
            push_candidate_if_exists(&mut seen, &mut out, CandidateKind::Root, entry);
        }
    }
    if let Some(spec_root) = resolve_spec_root() {
        let tooling_root = spec_root.join("tooling");
        push_candidate_if_exists(
            &mut seen,
            &mut out,
            CandidateKind::Legacy,
            tooling_root.join("resolver"),
        );
        push_candidate_if_exists(
            &mut seen,
            &mut out,
            CandidateKind::Legacy,
            tooling_root.join("registry"),
        );
        push_candidate_if_exists(&mut seen, &mut out, CandidateKind::Legacy, tooling_root);
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    push_candidate_if_exists(
        &mut seen,
        &mut out,
        CandidateKind::Root,
        manifest_dir.join("..").join("lcod-resolver"),
    );

    for path in collect_workspace_paths() {
        include_workspace_sources(&mut seen, &mut out, path);
    }

    out
}

fn collect_workspace_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(cwd) = env::current_dir() {
        paths.push(cwd);
    }
    for var in [
        "LCOD_COMPONENTS_PATH",
        "LCOD_COMPONENTS_PATHS",
        "LCOD_WORKSPACE_PATHS",
    ] {
        if let Ok(raw) = env::var(var) {
            for entry in env::split_paths(&raw) {
                paths.push(entry);
            }
        }
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let default_components = manifest_dir
        .join("..")
        .join("lcod-components")
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.join("..").join("lcod-components"));
    paths.push(default_components);

    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }
        let normalized = path.canonicalize().unwrap_or(path);
        let key = normalized.to_string_lossy().to_string();
        if seen.insert(key) {
            unique.push(normalized);
        }
    }
    unique
}

fn append_spec_fallbacks(collected: &mut Vec<ResolverHelperDef>) {
    let mut existing: HashSet<String> = collected.iter().map(|def| def.id.clone()).collect();
    let Some(spec_root) = resolve_spec_root() else {
        return;
    };

    let mut ensure_helper = |id: &str, rel_path: &[&str], base_path: &str| {
        let mut compose_path = spec_root.clone();
        for segment in rel_path {
            compose_path = compose_path.join(segment);
        }
        if !compose_path.is_file() {
            return;
        }
        if let Some(pos) = collected.iter().position(|def| def.id == id) {
            collected.remove(pos);
        }
        let manifest_path = compose_path.parent().map(|dir| dir.join("lcp.toml"));
        let metadata = manifest_path
            .as_ref()
            .and_then(|path| load_component_metadata(path));
        collected.push(ResolverHelperDef {
            id: id.to_string(),
            compose_path: compose_path.clone(),
            context: HelperContext {
                base_path: base_path.to_string(),
                version: "0.1.0".to_string(),
                alias_map: HashMap::new(),
            },
            aliases: Vec::new(),
            metadata,
        });
        existing.insert(id.to_string());
    };

    let definitions: &[(&str, &[&str], &str)] = &[
        (
            "lcod://tooling/value/default_object@0.1.0",
            &["tooling", "value", "default_object", "compose.yaml"],
            "tooling/value/default_object",
        ),
        (
            "lcod://tooling/value/default_array@0.1.0",
            &["tooling", "value", "default_array", "compose.yaml"],
            "tooling/value/default_array",
        ),
        (
            "lcod://tooling/value/is_object@0.1.0",
            &["tooling", "value", "is_object", "compose.yaml"],
            "tooling/value/is_object",
        ),
        (
            "lcod://tooling/value/is_array@0.1.0",
            &["tooling", "value", "is_array", "compose.yaml"],
            "tooling/value/is_array",
        ),
        (
            "lcod://tooling/value/is_string_nonempty@0.1.0",
            &["tooling", "value", "is_string_nonempty", "compose.yaml"],
            "tooling/value/is_string_nonempty",
        ),
        (
            "lcod://tooling/array/append@0.1.0",
            &["tooling", "array", "append", "compose.yaml"],
            "tooling/array/append",
        ),
        (
            "lcod://tooling/array/compact@0.1.0",
            &["tooling", "array", "compact", "compose.yaml"],
            "tooling/array/compact",
        ),
        (
            "lcod://tooling/array/concat@0.1.0",
            &["tooling", "array", "concat", "compose.yaml"],
            "tooling/array/concat",
        ),
        (
            "lcod://tooling/array/filter_objects@0.1.0",
            &["tooling", "array", "filter_objects", "compose.yaml"],
            "tooling/array/filter_objects",
        ),
        (
            "lcod://tooling/array/length@0.1.0",
            &["tooling", "array", "length", "compose.yaml"],
            "tooling/array/length",
        ),
        (
            "lcod://tooling/array/shift@0.1.0",
            &["tooling", "array", "shift", "compose.yaml"],
            "tooling/array/shift",
        ),
        (
            "lcod://tooling/fs/read_optional@0.1.0",
            &["tooling", "fs", "read_optional", "compose.yaml"],
            "tooling/fs/read_optional",
        ),
        (
            "lcod://tooling/json/decode_object@0.1.0",
            &["tooling", "json", "decode_object", "compose.yaml"],
            "tooling/json/decode_object",
        ),
        (
            "lcod://tooling/hash/sha256_base64@0.1.0",
            &["tooling", "hash", "sha256_base64", "compose.yaml"],
            "tooling/hash/sha256_base64",
        ),
        (
            "lcod://tooling/path/join_chain@0.1.0",
            &["tooling", "path", "join_chain", "compose.yaml"],
            "tooling/path/join_chain",
        ),
        (
            "lcod://tooling/path/dirname@0.1.0",
            &["tooling", "path", "dirname", "compose.yaml"],
            "tooling/path/dirname",
        ),
        (
            "lcod://tooling/path/is_absolute@0.1.0",
            &["tooling", "path", "is_absolute", "compose.yaml"],
            "tooling/path/is_absolute",
        ),
        (
            "lcod://tooling/path/to_file_url@0.1.0",
            &["tooling", "path", "to_file_url", "compose.yaml"],
            "tooling/path/to_file_url",
        ),
        (
            "lcod://core/array/append@0.1.0",
            &["core", "array", "append", "compose.yaml"],
            "core/array/append",
        ),
        (
            "lcod://core/json/decode@0.1.0",
            &["core", "json", "decode", "compose.yaml"],
            "core/json/decode",
        ),
        (
            "lcod://core/json/encode@0.1.0",
            &["core", "json", "encode", "compose.yaml"],
            "core/json/encode",
        ),
        (
            "lcod://core/object/merge@0.1.0",
            &["core", "object", "merge", "compose.yaml"],
            "core/object/merge",
        ),
        (
            "lcod://core/string/format@0.1.0",
            &["core", "string", "format", "compose.yaml"],
            "core/string/format",
        ),
        (
            "lcod://tooling/registry/source/load@0.1.0",
            &["tooling", "registry", "source", "compose.yaml"],
            "tooling/registry/source",
        ),
        (
            "lcod://tooling/registry/index@0.1.0",
            &["tooling", "registry", "index", "compose.yaml"],
            "tooling/registry/index",
        ),
        (
            "lcod://tooling/registry/select@0.1.0",
            &["tooling", "registry", "select", "compose.yaml"],
            "tooling/registry/select",
        ),
        (
            "lcod://tooling/registry/resolution@0.1.0",
            &["tooling", "registry", "resolution", "compose.yaml"],
            "tooling/registry/resolution",
        ),
        (
            "lcod://tooling/registry/catalog/generate@0.1.0",
            &["tooling", "registry", "catalog", "compose.yaml"],
            "tooling/registry/catalog",
        ),
        (
            "lcod://tooling/registry_sources/build_inline_entry@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "build_inline_entry",
                "compose.yaml",
            ],
            "tooling/registry_sources/build_inline_entry",
        ),
        (
            "lcod://tooling/registry_sources/collect_entries@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "collect_entries",
                "compose.yaml",
            ],
            "tooling/registry_sources/collect_entries",
        ),
        (
            "lcod://tooling/registry_sources/collect_queue@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "collect_queue",
                "compose.yaml",
            ],
            "tooling/registry_sources/collect_queue",
        ),
        (
            "lcod://tooling/registry_sources/load_config@0.1.0",
            &["tooling", "registry_sources", "load_config", "compose.yaml"],
            "tooling/registry_sources/load_config",
        ),
        (
            "lcod://tooling/registry_sources/merge_inline_entries@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "merge_inline_entries",
                "compose.yaml",
            ],
            "tooling/registry_sources/merge_inline_entries",
        ),
        (
            "lcod://tooling/registry_sources/normalize_pointer@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "normalize_pointer",
                "compose.yaml",
            ],
            "tooling/registry_sources/normalize_pointer",
        ),
        (
            "lcod://tooling/registry_sources/partition_normalized@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "partition_normalized",
                "compose.yaml",
            ],
            "tooling/registry_sources/partition_normalized",
        ),
        (
            "lcod://tooling/registry_sources/prepare_env@0.1.0",
            &["tooling", "registry_sources", "prepare_env", "compose.yaml"],
            "tooling/registry_sources/prepare_env",
        ),
        (
            "lcod://tooling/registry_sources/process_catalogue@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "process_catalogue",
                "compose.yaml",
            ],
            "tooling/registry_sources/process_catalogue",
        ),
        (
            "lcod://tooling/registry_sources/process_pointer@0.1.0",
            &[
                "tooling",
                "registry_sources",
                "process_pointer",
                "compose.yaml",
            ],
            "tooling/registry_sources/process_pointer",
        ),
        (
            "lcod://tooling/registry_sources/resolve@0.1.0",
            &["tooling", "registry_sources", "resolve", "compose.yaml"],
            "tooling/registry_sources/resolve",
        ),
        (
            "lcod://tooling/resolver/context/prepare@0.1.0",
            &["tooling", "resolver", "context", "compose.yaml"],
            "tooling/resolver/context",
        ),
        (
            "lcod://tooling/resolver/replace/apply@0.1.0",
            &["tooling", "resolver", "replace", "compose.yaml"],
            "tooling/resolver/replace",
        ),
        (
            "lcod://tooling/resolver/warnings/merge@0.1.0",
            &["tooling", "resolver", "warnings", "compose.yaml"],
            "tooling/resolver/warnings",
        ),
        (
            "lcod://tooling/resolver/register_components@0.1.0",
            &["tooling", "resolver", "register_components", "compose.yaml"],
            "tooling/resolver/register_components",
        ),
    ];

    for (id, rel_path, base_path) in definitions {
        ensure_helper(id, rel_path, base_path);
    }
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn value_is_defined_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let has_key = input
        .as_object()
        .map(|map| map.contains_key("value"))
        .unwrap_or(false);
    let is_defined = has_key && !matches!(input.get("value"), Some(Value::Null));
    Ok(json!({ "ok": is_defined }))
}

fn value_is_string_nonempty_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let ok = input
        .get("value")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    Ok(json!({ "ok": ok }))
}

fn string_ensure_trailing_newline_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let mut text = input
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let newline = input
        .get("newline")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("\n");

    if newline.is_empty() || text.ends_with(newline) {
        return Ok(json!({ "text": text }));
    }

    text.push_str(newline);
    Ok(json!({ "text": text }))
}

fn array_compact_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let items = input
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let values: Vec<Value> = items
        .into_iter()
        .filter(|entry| !matches!(entry, Value::Null))
        .collect();
    Ok(json!({ "values": Value::Array(values) }))
}

fn array_flatten_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let items = input
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut values = Vec::new();
    for entry in items {
        match entry {
            Value::Array(inner) => {
                for nested in inner {
                    if !matches!(nested, Value::Null) {
                        values.push(nested);
                    }
                }
            }
            Value::Null => {}
            other => values.push(other),
        }
    }
    Ok(json!({ "values": Value::Array(values) }))
}

fn array_find_duplicates_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let items = input
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for entry in items {
        if let Value::String(text) = entry {
            if !seen.insert(text.clone()) {
                duplicates.insert(text);
            }
        }
    }
    let mut list: Vec<String> = duplicates.into_iter().collect();
    list.sort();
    let values: Vec<Value> = list.into_iter().map(Value::String).collect();
    Ok(json!({ "duplicates": Value::Array(values) }))
}

fn array_append_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let _clone = input.get("clone").and_then(Value::as_bool).unwrap_or(true);
    let mut result = input
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(values) = input.get("values").and_then(Value::as_array) {
        result.extend(values.iter().cloned());
    }
    if input
        .as_object()
        .map(|map| map.contains_key("value"))
        .unwrap_or(false)
    {
        result.push(input.get("value").cloned().unwrap_or(Value::Null));
    }
    let length = result.len();
    Ok(json!({
        "items": Value::Array(result),
        "length": length
    }))
}

fn path_join_chain_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let base = input
        .get("base")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(PathBuf::new);
    let segments = input
        .get("segments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut current = base;
    for segment in segments {
        if segment.is_null() {
            continue;
        }
        let segment_str = if let Some(s) = segment.as_str() {
            s.to_string()
        } else {
            segment.to_string()
        };
        if segment_str.is_empty() {
            continue;
        }
        if current.as_os_str().is_empty() {
            current = PathBuf::from(segment_str);
        } else {
            current = current.join(segment_str);
        }
    }

    let path_str = crate::core::path::path_to_string(&current);
    Ok(json!({ "path": path_str }))
}

fn fs_read_optional_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let encoding = non_empty_string(input.get("encoding")).unwrap_or_else(|| "utf-8".to_string());
    let path_value = non_empty_string(input.get("path"));
    let fallback = non_empty_string(input.get("fallback"));
    let warning_message = non_empty_string(input.get("warningMessage"));

    if path_value.is_none() {
        return Ok(json!({
            "text": fallback.map(Value::String).unwrap_or(Value::Null),
            "exists": false,
            "warning": warning_message.map(Value::String).unwrap_or(Value::Null)
        }));
    }

    let path_str = path_value.unwrap();
    match fs::read(&path_str) {
        Ok(bytes) => {
            let text = if encoding.eq_ignore_ascii_case("utf-8") {
                String::from_utf8(bytes).unwrap_or_else(|_| String::new())
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            Ok(json!({ "text": text, "exists": true, "warning": Value::Null }))
        }
        Err(err) => {
            if let Some(fallback_text) = fallback {
                return Ok(json!({
                    "text": fallback_text,
                    "exists": false,
                    "warning": warning_message.clone().map(Value::String).unwrap_or(Value::Null)
                }));
            }
            let warning = warning_message.unwrap_or_else(|| err.to_string());
            Ok(json!({
                "text": Value::Null,
                "exists": false,
                "warning": warning
            }))
        }
    }
}

fn fs_write_if_changed_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let path_str = input
        .get("path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("write_if_changed: path is required"))?;

    let encoding = non_empty_string(input.get("encoding")).unwrap_or_else(|| "utf-8".to_string());
    let content = match input.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };

    let previous = match fs::read(&path_str) {
        Ok(bytes) => {
            if encoding.eq_ignore_ascii_case("utf-8") {
                String::from_utf8(bytes).ok()
            } else {
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };

    if previous
        .as_ref()
        .map(|existing| existing == &content)
        .unwrap_or(false)
    {
        return Ok(json!({ "changed": false }));
    }

    fs::write(path_str, content.as_bytes())?;
    Ok(json!({ "changed": true }))
}

fn object_clone_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let clone_value = input
        .get("value")
        .and_then(|v| v.as_object())
        .map(|map| Value::Object(map.clone()))
        .unwrap_or_else(|| Value::Object(Map::new()));
    Ok(json!({ "clone": clone_value }))
}

fn object_set_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let mut target_map = input
        .get("target")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let previous = Value::Object(target_map.clone());

    let path = input
        .get("path")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if path.is_empty() {
        let replacement = input.get("value").cloned().unwrap_or(Value::Null);
        return Ok(json!({ "object": replacement, "previous": previous }));
    }

    let mut current = &mut target_map;
    for segment in path.iter().take(path.len().saturating_sub(1)) {
        let Some(key) = segment.as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let entry = current
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = entry.as_object_mut().expect("entry forced to object");
    }

    if let Some(last_key) = path
        .last()
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        current.insert(
            last_key.to_string(),
            input.get("value").cloned().unwrap_or(Value::Null),
        );
    }

    Ok(json!({
        "object": Value::Object(target_map),
        "previous": previous
    }))
}

fn object_has_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let path = input
        .get("path")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if path.is_empty() {
        return Ok(json!({ "hasKey": false, "value": Value::Null }));
    }

    let mut value_ref: Option<Value> = None;
    let mut current_obj_opt = input.get("target").and_then(Value::as_object).cloned();
    for (idx, segment) in path.iter().enumerate() {
        let Some(key) = segment.as_str().filter(|s| !s.is_empty()) else {
            return Ok(json!({ "hasKey": false, "value": Value::Null }));
        };
        let Some(current_obj) = current_obj_opt.as_ref() else {
            return Ok(json!({ "hasKey": false, "value": Value::Null }));
        };
        let Some(next_value) = current_obj.get(key) else {
            return Ok(json!({ "hasKey": false, "value": Value::Null }));
        };
        if idx == path.len() - 1 {
            value_ref = Some(next_value.clone());
        } else {
            current_obj_opt = next_value.as_object().cloned();
        }
    }

    Ok(json!({
        "hasKey": value_ref.is_some(),
        "value": value_ref.unwrap_or(Value::Null)
    }))
}

fn object_entries_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let entries = input
        .get("value")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| Value::Array(vec![Value::String(key.clone()), value.clone()]))
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();
    Ok(json!({ "entries": Value::Array(entries) }))
}

fn json_stable_stringify_helper(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let value = input.get("value").cloned().unwrap_or(Value::Null);
    match stable_stringify(&value) {
        Ok(text) => Ok(json!({ "text": text, "warning": Value::Null })),
        Err(err) => Ok(json!({ "text": Value::Null, "warning": err.to_string() })),
    }
}

fn stable_stringify(value: &Value) -> Result<String> {
    let canonical = canonicalize(value);
    serde_json::to_string(&canonical).map_err(|err| anyhow!("unable to stringify value: {err}"))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut new_map = Map::new();
            for (key, val) in entries {
                new_map.insert(key.clone(), canonicalize(val));
            }
            Value::Object(new_map)
        }
        Value::Array(items) => {
            let canonical_items: Vec<Value> = items.iter().map(canonicalize).collect();
            Value::Array(canonical_items)
        }
        _ => value.clone(),
    }
}

fn jsonl_read_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    if let Some(url) = input.get("url").and_then(Value::as_str) {
        return Err(anyhow!("jsonl/read does not support url input yet: {url}"));
    }

    let encoding = input
        .get("encoding")
        .and_then(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| "utf-8".to_string());
    if encoding != "utf-8" && encoding != "utf8" {
        return Err(anyhow!(
            "jsonl/read only supports utf-8 encoding (got {encoding})"
        ));
    }

    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return Err(anyhow!("jsonl/read requires a `path`"));
    };

    let file =
        fs::File::open(path).with_context(|| format!("unable to open JSONL file: {path}"))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();

    for (index, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(line) => line,
            Err(err) => {
                warnings.push(Value::String(format!(
                    "failed to read JSONL line {} from {}: {}",
                    index + 1,
                    path,
                    err
                )));
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => entries.push(value),
            Err(err) => warnings.push(Value::String(format!(
                "invalid JSONL entry at {} line {}: {}",
                path,
                index + 1,
                err
            ))),
        }
    }

    Ok(json!({
        "entries": Value::Array(entries),
        "warnings": Value::Array(warnings)
    }))
}

fn hash_to_key_helper(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let text = input.get("text").and_then(Value::as_str).unwrap_or("");
    let prefix = input.get("prefix").and_then(Value::as_str).unwrap_or("");
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let hash = base64::engine::general_purpose::STANDARD.encode(digest);
    let key = if prefix.is_empty() {
        hash
    } else {
        format!("{prefix}{hash}")
    };
    Ok(json!({ "key": key }))
}

fn queue_bfs_helper(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let mut queue: VecDeque<Value> = input
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut visited_object = input
        .get("visited")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut visited: HashSet<String> = visited_object.keys().cloned().collect();

    let mut state = input
        .get("state")
        .and_then(Value::as_object)
        .cloned()
        .map(Value::Object)
        .unwrap_or_else(|| Value::Object(Map::new()));

    let context_value = input.get("context").cloned();

    let max_iterations = input
        .get("maxIterations")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(i64::MAX);

    let mut iterations: i64 = 0;
    let mut warnings: Vec<Value> = Vec::new();

    while let Some(item) = queue.pop_front() {
        let _ = ctx.ensure_not_cancelled();
        if iterations >= max_iterations {
            return Err(anyhow!(
                "queue/bfs exceeded maxIterations ({max_iterations})"
            ));
        }

        let mut slot_vars = serde_json::Map::new();
        slot_vars.insert("index".to_string(), Value::from(iterations));
        slot_vars.insert("remaining".to_string(), Value::from(queue.len()));
        slot_vars.insert("visitedCount".to_string(), Value::from(visited.len()));
        slot_vars.insert("item".to_string(), item.clone());
        slot_vars.insert("state".to_string(), state.clone());
        if let Some(ctx_val) = context_value.clone() {
            slot_vars.insert("context".to_string(), ctx_val);
        }
        let slot_vars = Value::Object(slot_vars);

        let mut slot_payload = Map::new();
        slot_payload.insert("item".to_string(), item.clone());
        slot_payload.insert("state".to_string(), state.clone());
        if let Some(ctx_val) = context_value.clone() {
            slot_payload.insert("context".to_string(), ctx_val);
        }

        let key_value = ctx.run_slot(
            "key",
            Some(Value::Object(slot_payload.clone())),
            Some(slot_vars.clone()),
        );

        let mut key = match key_value {
            Ok(val) => {
                if let Some(text) = val.as_str() {
                    Some(text.to_string())
                } else if let Some(obj) = val.as_object() {
                    obj.get("key")
                        .and_then(Value::as_str)
                        .map(|text| text.to_string())
                } else {
                    None
                }
            }
            Err(err) => {
                warnings.push(Value::String(format!("queue/bfs key slot failed: {err}")));
                None
            }
        };

        if key.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
            match serde_json::to_string(&item) {
                Ok(text) => key = Some(text),
                Err(err) => {
                    warnings.push(Value::String(format!(
                        "queue/bfs fallback key serialization failed: {err}"
                    )));
                    key = Some(format!("item:{iterations}"));
                }
            }
        }

        let key_str = key.unwrap();
        if visited.contains(&key_str) {
            iterations += 1;
            continue;
        }
        visited.insert(key_str.clone());
        visited_object.insert(key_str, Value::Bool(true));

        let process_input = Value::Object(slot_payload);
        let process_result = ctx.run_slot("process", Some(process_input), Some(slot_vars))?;

        if let Some(new_state) = process_result.get("state").and_then(Value::as_object) {
            state = Value::Object(new_state.clone());
        }

        if let Some(children) = process_result.get("children").and_then(Value::as_array) {
            for child in children {
                queue.push_back(child.clone());
            }
        }

        if let Some(extra_warnings) = process_result.get("warnings").and_then(Value::as_array) {
            for warning in extra_warnings {
                if let Some(text) = warning.as_str() {
                    warnings.push(Value::String(text.to_string()));
                }
            }
        }

        iterations += 1;
    }

    Ok(json!({
        "state": state,
        "visited": Value::Object(visited_object),
        "warnings": Value::Array(warnings),
        "iterations": iterations
    }))
}

fn load_definitions_for_candidate(candidate: &Candidate) -> Vec<ResolverHelperDef> {
    match candidate.kind {
        CandidateKind::Root => {
            let defs = load_workspace_definitions(&candidate.path);
            if !defs.is_empty() {
                defs
            } else {
                load_legacy_component_definitions(&candidate.path.join("components"))
            }
        }
        CandidateKind::Legacy => load_legacy_component_definitions(&candidate.path),
    }
}

fn load_workspace_definitions(root: &Path) -> Vec<ResolverHelperDef> {
    let workspace_path = root.join("workspace.lcp.toml");
    let Ok(raw) = fs::read_to_string(&workspace_path) else {
        return Vec::new();
    };
    let workspace_value: TomlValue = match raw.parse() {
        Ok(value) => value,
        Err(err) => {
            let _ = log_kernel_warn(
                None,
                "Failed to parse workspace manifest",
                Some(json!({
                    "path": workspace_path.display().to_string(),
                    "error": err.to_string()
                })),
                Some(json!({ "module": "resolver-helpers" })),
            );
            return Vec::new();
        }
    };
    let workspace_table = match workspace_value
        .get("workspace")
        .and_then(TomlValue::as_table)
    {
        Some(table) => table,
        None => return Vec::new(),
    };
    let mut alias_map = HashMap::new();
    if let Some(scope_aliases) = workspace_table
        .get("scopeAliases")
        .and_then(TomlValue::as_table)
    {
        for (key, value) in scope_aliases {
            if let Some(alias) = value.as_str() {
                alias_map.insert(key.to_string(), alias.to_string());
            }
        }
    }
    let packages = match workspace_table
        .get("packages")
        .and_then(TomlValue::as_array)
    {
        Some(list) => list,
        None => return Vec::new(),
    };
    let mut defs = Vec::new();
    for pkg in packages {
        if let Some(name) = pkg.as_str() {
            defs.extend(load_package_definitions(root, name, &alias_map));
        }
    }
    defs
}

fn load_package_definitions(
    root: &Path,
    package: &str,
    workspace_aliases: &HashMap<String, String>,
) -> Vec<ResolverHelperDef> {
    let pkg_dir = root.join("packages").join(package);
    let manifest_path = pkg_dir.join("lcp.toml");
    let Ok(raw) = fs::read_to_string(&manifest_path) else {
        return Vec::new();
    };
    let manifest: TomlValue = match raw.parse() {
        Ok(value) => value,
        Err(err) => {
            let _ = log_kernel_warn(
                None,
                "Failed to parse package manifest",
                Some(json!({
                    "path": manifest_path.display().to_string(),
                    "error": err.to_string()
                })),
                Some(json!({ "module": "resolver-helpers" })),
            );
            return Vec::new();
        }
    };
    let context = create_context(&manifest, workspace_aliases);
    let mut defs = Vec::new();
    let components_dir = manifest
        .get("workspace")
        .and_then(TomlValue::as_table)
        .and_then(|w| w.get("componentsDir"))
        .and_then(TomlValue::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if let Some(dir) = components_dir {
        defs.extend(load_package_components_from_dir(&pkg_dir, &dir, &context));
    } else {
        let _ = log_kernel_warn(
            None,
            "workspace.componentsDir is missing",
            Some(json!({
                "path": manifest_path.display().to_string(),
                "hint": "set workspace.componentsDir = \"components\""
            })),
            Some(json!({ "module": "resolver-helpers" })),
        );
    }
    defs
}

fn load_package_components_from_dir(
    pkg_dir: &Path,
    components_dir: &str,
    context: &HelperContext,
) -> Vec<ResolverHelperDef> {
    let resolved_dir = if Path::new(components_dir).is_absolute() {
        PathBuf::from(components_dir)
    } else {
        pkg_dir.join(components_dir)
    };
    if !resolved_dir.exists() {
        let _ = log_kernel_warn(
            None,
            "workspace.componentsDir does not exist",
            Some(json!({
                "path": resolved_dir.display().to_string()
            })),
            Some(json!({ "module": "resolver-helpers" })),
        );
        return Vec::new();
    }

    let component_dirs = collect_component_directories(&resolved_dir);
    let mut defs = Vec::new();
    for component_dir in component_dirs {
        let compose_path = component_dir.join("compose.yaml");
        if !compose_path.exists() {
            continue;
        }
        let manifest_path = component_dir.join("lcp.toml");
        let Ok(raw) = fs::read_to_string(&manifest_path) else {
            let _ = log_kernel_warn(
                None,
                "Failed to read component manifest",
                Some(json!({
                    "path": manifest_path.display().to_string()
                })),
                Some(json!({ "module": "resolver-helpers" })),
            );
            continue;
        };
        let manifest: TomlValue = match raw.parse::<TomlValue>() {
            Ok(value) => value,
            Err(err) => {
                let _ = log_kernel_warn(
                    None,
                    "Failed to parse component manifest",
                    Some(json!({
                        "path": manifest_path.display().to_string(),
                        "error": err.to_string()
                    })),
                    Some(json!({ "module": "resolver-helpers" })),
                );
                continue;
            }
        };
        let Some(component_id_raw) = manifest.get("id").and_then(TomlValue::as_str) else {
            let _ = log_kernel_warn(
                None,
                "Component manifest is missing id",
                Some(json!({
                    "path": manifest_path.display().to_string()
                })),
                Some(json!({ "module": "resolver-helpers" })),
            );
            continue;
        };
        let metadata = load_component_metadata(&manifest_path);
        let canonical_id = canonicalize_id(component_id_raw, context);
        let mut aliases = Vec::new();
        if canonical_id != component_id_raw {
            aliases.push(component_id_raw.to_string());
        }
        defs.push(ResolverHelperDef {
            id: canonical_id,
            compose_path: compose_path.clone(),
            context: context.clone(),
            aliases,
            metadata,
        });
    }
    defs
}

fn collect_component_directories(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut collected = Vec::new();

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        let mut has_manifest = false;
        let mut has_compose = false;
        let mut subdirs = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => {
                    subdirs.push(path);
                }
                Ok(_) | Err(_) => {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name == "lcp.toml" {
                            has_manifest = true;
                        } else if name == "compose.yaml" {
                            has_compose = true;
                        }
                    }
                }
            }
        }

        if has_manifest && has_compose {
            collected.push(dir.clone());
        }

        stack.extend(subdirs);
    }

    collected
}

fn load_component_metadata(manifest_path: &Path) -> Option<ComponentMetadata> {
    let raw = fs::read_to_string(manifest_path).ok()?;
    let value: TomlValue = raw.parse().ok()?;
    let inputs = extract_metadata_keys(value.get("inputs"));
    let outputs = extract_metadata_keys(value.get("outputs"));
    let slots = extract_metadata_keys(value.get("slots"));
    Some(ComponentMetadata {
        inputs,
        outputs,
        slots,
    })
}

fn extract_metadata_keys(section: Option<&TomlValue>) -> Vec<String> {
    section
        .and_then(TomlValue::as_table)
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default()
}

fn create_context(
    manifest: &TomlValue,
    workspace_aliases: &HashMap<String, String>,
) -> HelperContext {
    let mut alias_map = workspace_aliases.clone();
    if let Some(package_aliases) = manifest
        .get("workspace")
        .and_then(TomlValue::as_table)
        .and_then(|w| w.get("scopeAliases"))
        .and_then(TomlValue::as_table)
    {
        for (key, value) in package_aliases {
            if let Some(alias) = value.as_str() {
                alias_map.insert(key.to_string(), alias.to_string());
            }
        }
    }

    let manifest_id = manifest.get("id").and_then(TomlValue::as_str);
    let base_path = manifest_id
        .and_then(|id| extract_path_from_id(id).map(|s| s.to_string()))
        .unwrap_or_else(|| {
            let ns = manifest
                .get("namespace")
                .and_then(TomlValue::as_str)
                .unwrap_or("");
            let name = manifest
                .get("name")
                .and_then(TomlValue::as_str)
                .unwrap_or("");
            if ns.is_empty() {
                name.to_string()
            } else if name.is_empty() {
                ns.to_string()
            } else {
                format!("{}/{}", ns, name)
            }
        });

    let version = manifest
        .get("version")
        .and_then(TomlValue::as_str)
        .map(|s| s.to_string())
        .or_else(|| manifest_id.and_then(|id| extract_version_from_id(id).map(|v| v.to_string())))
        .unwrap_or_else(|| "0.0.0".to_string());

    HelperContext {
        base_path,
        version,
        alias_map,
    }
}

fn load_legacy_component_definitions(dir: &Path) -> Vec<ResolverHelperDef> {
    if !dir.exists() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut defs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let compose_path = path.join("compose.yaml");
        if !compose_path.exists() {
            continue;
        }
        let manifest_path = path.join("lcp.toml");
        let Ok(raw) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let manifest: TomlValue = match raw.parse::<TomlValue>() {
            Ok(value) => value,
            Err(err) => {
                let _ = log_kernel_warn(
                    None,
                    "Failed to parse legacy component manifest",
                    Some(json!({
                        "path": manifest_path.display().to_string(),
                        "error": err.to_string()
                    })),
                    Some(json!({ "module": "resolver-helpers" })),
                );
                continue;
            }
        };
        let Some(component_id) = manifest.get("id").and_then(TomlValue::as_str) else {
            continue;
        };
        let base_path = extract_path_from_id(component_id)
            .map(|p| {
                let mut base = p.to_string();
                if let Some(pos) = base.rfind('/') {
                    base.truncate(pos);
                } else {
                    base.clear();
                }
                base
            })
            .unwrap_or_default();
        let version = extract_version_from_id(component_id)
            .unwrap_or("0.0.0")
            .to_string();
        let metadata = load_component_metadata(&manifest_path);
        defs.push(ResolverHelperDef {
            id: component_id.to_string(),
            compose_path: compose_path.clone(),
            context: HelperContext {
                base_path,
                version,
                alias_map: HashMap::new(),
            },
            aliases: Vec::new(),
            metadata,
        });
    }
    defs
}

fn extract_path_from_id(id: &str) -> Option<&str> {
    if !id.starts_with("lcod://") {
        return None;
    }
    id.strip_prefix("lcod://")?.split('@').next()
}

fn extract_version_from_id(id: &str) -> Option<&str> {
    id.split('@').nth(1)
}

fn load_helper_compose(path: &Path, context: &HelperContext) -> Result<Vec<crate::compose::Step>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("unable to read compose file: {}", path.display()))?;
    let mut doc: Value = serde_yaml::from_str(&content)
        .with_context(|| format!("invalid YAML compose: {}", path.display()))?;
    let compose_value = doc
        .get_mut("compose")
        .ok_or_else(|| anyhow!("compose root missing in {}", path.display()))?;
    canonicalize_value(compose_value, context);
    parse_compose(compose_value)
        .with_context(|| format!("invalid compose structure in {}", path.display()))
}

fn canonicalize_value(value: &mut Value, context: &HelperContext) {
    match value {
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                canonicalize_value(item, context);
            }
        }
        Value::Object(map) => canonicalize_object(map, context),
        _ => {}
    }
}

fn canonicalize_object(map: &mut Map<String, Value>, context: &HelperContext) {
    if let Some(Value::String(call)) = map.get_mut("call") {
        let canonical = canonicalize_id(call, context);
        *call = canonical;
    }
    if let Some(children) = map.get_mut("children") {
        canonicalize_children(children, context);
    }
    if let Some(slots) = map.get_mut("slots") {
        canonicalize_children(slots, context);
    }
    for (key, val) in map.iter_mut() {
        if key == "call" || key == "children" || key == "slots" {
            continue;
        }
        canonicalize_value(val, context);
    }
}

fn canonicalize_children(value: &mut Value, context: &HelperContext) {
    match value {
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                canonicalize_value(item, context);
            }
        }
        Value::Object(map) => {
            for val in map.values_mut() {
                canonicalize_value(val, context);
            }
        }
        _ => {}
    }
}

fn canonicalize_id(raw: &str, context: &HelperContext) -> String {
    if raw.starts_with("lcod://") {
        return raw.to_string();
    }
    let trimmed = raw.trim_start_matches("./");
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return raw.to_string();
    }
    let alias = segments[0];
    let mapped = context
        .alias_map
        .get(alias)
        .map(|s| s.as_str())
        .unwrap_or(alias);
    let mut parts = Vec::new();
    if !context.base_path.is_empty() {
        parts.push(context.base_path.clone());
    }
    if !mapped.is_empty() {
        parts.push(mapped.to_string());
    }
    for seg in segments.iter().skip(1) {
        parts.push((*seg).to_string());
    }
    let full = parts.join("/");
    if full.is_empty() {
        return raw.to_string();
    }
    let version = if context.version.is_empty() {
        "0.0.0"
    } else {
        context.version.as_str()
    };
    format!("lcod://{}@{}", full, version)
}

pub use resolver::register_resolver_axioms;

fn load_compose_from_path(path: &Path) -> Result<Vec<crate::compose::Step>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("unable to read compose file: {}", path.display()))?;
    let doc: Value = serde_yaml::from_str(&content)
        .with_context(|| format!("invalid YAML compose: {}", path.display()))?;
    let compose_value = doc
        .get("compose")
        .cloned()
        .ok_or_else(|| anyhow!("compose root missing in {}", path.display()))?;
    parse_compose(&compose_value)
        .with_context(|| format!("invalid compose structure in {}", path.display()))
}

fn ensure_compose(input: &Value) -> Result<Vec<crate::compose::Step>> {
    if let Some(compose) = input.get("compose") {
        return parse_compose(compose).map_err(|err| anyhow!("invalid inline compose: {err}"));
    }
    if let Some(compose_ref) = input.get("composeRef") {
        let path_str = compose_ref
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("composeRef.path must be a string"))?;
        let resolved = PathBuf::from(path_str);
        return load_compose_from_path(&resolved);
    }
    Err(anyhow!("compose or composeRef.path must be provided"))
}

fn matches_expected(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e.iter().all(|(key, val)| {
            a.get(key)
                .map(|actual_val| matches_expected(actual_val, val))
                .unwrap_or(false)
        }),
        _ => false,
    }
}

fn simple_diff(actual: &Value, expected: &Value) -> Value {
    json!({
        "path": "$",
        "actual": actual,
        "expected": expected
    })
}

fn test_checker(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let expected = input
        .get("expected")
        .cloned()
        .ok_or_else(|| anyhow!("expected output is required"))?;

    let compose_steps = ensure_compose(&input)?;

    let mut initial_state = input
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let fail_fast = input
        .get("failFast")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if let Some(stream_specs) = input.get("streams") {
        common::register_streams(ctx, &mut initial_state, stream_specs)?;
    }

    let start = Instant::now();
    let exec_result = run_compose(ctx, &compose_steps, initial_state);
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut report = Map::new();
    report.insert("expected".to_string(), expected.clone());
    report.insert(
        "durationMs".to_string(),
        Value::Number(serde_json::Number::from_f64(duration_ms).unwrap_or_else(|| 0.into())),
    );
    let mut messages = Vec::new();

    match exec_result {
        Ok(actual) => {
            let success = matches_expected(&actual, &expected);
            report.insert("success".to_string(), Value::Bool(success));
            report.insert("actual".to_string(), actual.clone());
            if !success {
                messages.push(Value::String(
                    "Actual output differs from expected output".to_string(),
                ));
                let diff = simple_diff(&actual, &expected);
                report.insert("diffs".to_string(), Value::Array(vec![diff]));
                if !fail_fast {
                    // Future: collect additional differences when available
                }
            }
        }
        Err(err) => {
            report.insert("success".to_string(), Value::Bool(false));
            report.insert(
                "actual".to_string(),
                json!({ "error": { "message": err.to_string() } }),
            );
            messages.push(Value::String(format!("Compose execution failed: {err}")));
        }
    }

    if !messages.is_empty() {
        report.insert("messages".to_string(), Value::Array(messages));
    }

    Ok(Value::Object(report))
}
