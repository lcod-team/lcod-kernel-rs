use std::cell::Cell;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use serde_json::{json, Map, Value};
use toml::Value as TomlValue;

use crate::compose::{parse_compose, run_compose};
use crate::registry::{Context, Registry};

mod common;
mod logging;
mod registry_scope;
mod resolver;
mod script;

const CONTRACT_TEST_CHECKER: &str = "lcod://tooling/test_checker@1";

pub fn register_tooling(registry: &Registry) {
    registry.register(CONTRACT_TEST_CHECKER, test_checker);
    script::register_script_contract(registry);
    registry_scope::register_registry_scope(registry);
    logging::register_logging(registry);
    register_resolver_helpers(registry);
}

fn runtime_root() -> Option<PathBuf> {
    if let Ok(home) = env::var("LCOD_HOME") {
        let candidate = PathBuf::from(home);
        if candidate.join("manifest.json").is_file() && candidate.join("tooling").is_dir() {
            return Some(candidate);
        }
    }
    None
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
    if let Some(root) = runtime_root() {
        return Some(root);
    }
    if let Ok(env_path) = env::var("SPEC_REPO_PATH") {
        let candidate = PathBuf::from(env_path);
        if candidate.is_dir() {
            return Some(candidate);
        }
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

fn run_spec_register_components(registry: &Registry, spec_root_override: Option<&str>) -> Result<Value> {
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
            json!({ "specRoot": spec_root.to_string_lossy() }),
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

                let steps = if let Some(inline) = component.get("compose") {
                    parse_compose(inline).with_context(|| {
                        format!(
                            "resolver/register: invalid inline compose for {}",
                            id_raw
                        )
                    })?
                } else if let Some(path_str) = component.get("composePath").and_then(Value::as_str)
                {
                    let path = PathBuf::from(path_str);
                    load_compose_from_path(&path).with_context(|| {
                        format!(
                            "resolver/register: failed to load compose for {} from {}",
                            id_raw,
                            path.display()
                        )
                    })?
                } else {
                    warnings.push(format!(
                        "resolver/register: component {} missing compose data",
                        id_raw
                    ));
                    continue;
                };

                let steps_arc = Arc::new(steps);
                let id_string = id_raw.to_string();
                let registry_clone = dynamic_registry.clone();
                registry_clone.register(
                    id_string.clone(),
                    move |ctx_inner: &mut Context, input_inner: Value, _meta_inner: Option<Value>| {
                        let steps = Arc::clone(&steps_arc);
                        run_compose(ctx_inner, &steps, input_inner)
                    },
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
        eprintln!("{err}");
    }

    let components_registry = registry.clone();
    registry.register(
        "lcod://tooling/resolver/register_components@0.1.0",
        move |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let spec_root = input
                .get("specRoot")
                .and_then(Value::as_str);
            run_spec_register_components(&components_registry, spec_root)
        },
    );

    let defs = build_helper_definitions();
    for def in defs {
        let compose_path = Arc::new(def.compose_path);
        let context = Arc::new(def.context);
        let ids: Vec<String> = std::iter::once(def.id.clone())
            .chain(def.aliases.into_iter())
            .collect();
        for id in ids {
            let id_arc = Arc::new(id.clone());
            let compose_path = Arc::clone(&compose_path);
            let context = Arc::clone(&context);
            registry.register(
                id,
                move |ctx: &mut Context, input: Value, _meta: Option<Value>| {
                    let steps =
                        load_helper_compose(&compose_path, &context).with_context(|| {
                            format!("unable to load resolver helper {}", id_arc.as_ref())
                        })?;
                    run_compose(ctx, &steps, input)
                },
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
}

#[derive(Debug)]
enum CandidateKind {
    Root,
    Components,
    Legacy,
}

#[derive(Debug)]
struct Candidate {
    kind: CandidateKind,
    path: PathBuf,
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
    collected
}

fn gather_candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    if let Some(runtime_resolver) = runtime_resolver_root() {
        out.push(Candidate {
            kind: CandidateKind::Root,
            path: runtime_resolver,
        });
    }
    if let Some(runtime) = runtime_root() {
        let tooling_resolver = runtime.join("tooling").join("resolver");
        if tooling_resolver.is_dir() {
            out.push(Candidate {
                kind: CandidateKind::Legacy,
                path: tooling_resolver,
            });
        }
        let tooling_registry = runtime.join("tooling").join("registry");
        if tooling_registry.is_dir() {
            out.push(Candidate {
                kind: CandidateKind::Legacy,
                path: tooling_registry,
            });
        }
    }
    if let Ok(path) = env::var("LCOD_RESOLVER_COMPONENTS_PATH") {
        out.push(Candidate {
            kind: CandidateKind::Components,
            path: PathBuf::from(path),
        });
    }
    if let Ok(path) = env::var("LCOD_RESOLVER_PATH") {
        out.push(Candidate {
            kind: CandidateKind::Root,
            path: PathBuf::from(path),
        });
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    out.push(Candidate {
        kind: CandidateKind::Root,
        path: manifest_dir.join("..").join("lcod-resolver"),
    });
    out
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
        CandidateKind::Components => load_legacy_component_definitions(&candidate.path),
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
        Err(_) => return Vec::new(),
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
        Err(_) => return Vec::new(),
    };
    let context = create_context(&manifest, workspace_aliases);
    let components = manifest
        .get("workspace")
        .and_then(TomlValue::as_table)
        .and_then(|w| w.get("components"))
        .and_then(TomlValue::as_array);
    let mut defs = Vec::new();
    if let Some(components) = components {
        for component in components {
            if let Some(table) = component.as_table() {
                let Some(id_raw) = table.get("id").and_then(TomlValue::as_str) else {
                    continue;
                };
                let Some(rel_path) = table.get("path").and_then(TomlValue::as_str) else {
                    continue;
                };
                let component_dir = pkg_dir.join(rel_path);
                let compose_path = component_dir.join("compose.yaml");
                if !compose_path.exists() {
                    continue;
                }
                let canonical_id = canonicalize_id(id_raw, &context);
                let mut aliases = Vec::new();
                let component_manifest_path = component_dir.join("lcp.toml");
                if let Ok(raw) = fs::read_to_string(&component_manifest_path) {
                    if let Ok(component_manifest) = raw.parse::<TomlValue>() {
                        if let Some(existing_id) =
                            component_manifest.get("id").and_then(TomlValue::as_str)
                        {
                            if existing_id != canonical_id {
                                aliases.push(existing_id.to_string());
                            }
                        }
                    }
                }
                defs.push(ResolverHelperDef {
                    id: canonical_id,
                    compose_path: compose_path.clone(),
                    context: context.clone(),
                    aliases,
                });
            }
        }
    }
    defs
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
        let Ok(manifest) = raw.parse::<TomlValue>() else {
            continue;
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
        defs.push(ResolverHelperDef {
            id: component_id.to_string(),
            compose_path: compose_path.clone(),
            context: HelperContext {
                base_path,
                version,
                alias_map: HashMap::new(),
            },
            aliases: Vec::new(),
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
    for (key, val) in map.iter_mut() {
        if key == "call" || key == "children" {
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
