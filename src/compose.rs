use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry, SlotExecutor};
use crate::tooling::register_tooling;

const SPREAD_KEY: &str = "__lcod_spreads__";
const OPTIONAL_FLAG: &str = "__lcod_optional__";
const STATE_SENTINEL: &str = "__lcod_state__";
const RESULT_SENTINEL: &str = "__lcod_result__";

#[derive(Copy, Clone)]
enum MappingKind {
    Input,
    Output,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Step {
    pub call: String,
    #[serde(default, rename = "in")]
    pub inputs: Map<String, Value>,
    #[serde(default)]
    pub out: Map<String, Value>,
    #[serde(default, rename = "collectPath")]
    pub collect_path: Option<String>,
    #[serde(default)]
    pub children: Option<StepChildren>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepChildren {
    List(Vec<Step>),
    Map(HashMap<String, Vec<Step>>),
}

fn default_suffix_path(suffix: &str) -> String {
    let trimmed = suffix.trim_start_matches('.');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("$.{trimmed}")
    }
}

fn normalize_spread_source(raw: &Value, suffix: Option<&str>, kind: MappingKind) -> Value {
    match raw {
        Value::String(s) => {
            if s == "=" {
                if let Some(sfx) = suffix.filter(|s| !s.is_empty()) {
                    Value::String(default_suffix_path(sfx))
                } else {
                    match kind {
                        MappingKind::Input => Value::String(STATE_SENTINEL.to_string()),
                        MappingKind::Output => Value::String(RESULT_SENTINEL.to_string()),
                    }
                }
            } else {
                Value::String(s.clone())
            }
        }
        Value::Object(obj) => {
            if let Some(source) = obj.get("source").or_else(|| obj.get("path")) {
                return normalize_spread_source(source, suffix, kind);
            }
            if let Some(sfx) = suffix.filter(|s| !s.is_empty()) {
                Value::String(default_suffix_path(sfx))
            } else {
                match kind {
                    MappingKind::Input => Value::String(STATE_SENTINEL.to_string()),
                    MappingKind::Output => Value::String(RESULT_SENTINEL.to_string()),
                }
            }
        }
        _ => {
            if let Some(sfx) = suffix.filter(|s| !s.is_empty()) {
                Value::String(default_suffix_path(sfx))
            } else {
                match kind {
                    MappingKind::Input => Value::String(STATE_SENTINEL.to_string()),
                    MappingKind::Output => Value::String(RESULT_SENTINEL.to_string()),
                }
            }
        }
    }
}

fn normalize_spread_entries(raw: &Value, suffix: Option<&str>, kind: MappingKind) -> Vec<Value> {
    match raw {
        Value::Array(items) => items
            .iter()
            .flat_map(|entry| normalize_spread_entries(entry, suffix, kind))
            .collect(),
        Value::Object(obj) => {
            let mut descriptor = Map::new();
            let source_value = obj
                .get("source")
                .or_else(|| obj.get("path"))
                .map(|entry| normalize_spread_source(entry, suffix, kind))
                .unwrap_or_else(|| {
                    normalize_spread_source(&Value::String("=".to_string()), suffix, kind)
                });
            descriptor.insert(
                "source".to_string(),
                source_value,
            );
            if let Some(optional) = obj.get("optional").and_then(Value::as_bool) {
                descriptor.insert("optional".to_string(), Value::Bool(optional));
            }
            if let Some(pick) = obj.get("pick").and_then(Value::as_array) {
                let selections: Vec<Value> = pick
                    .iter()
                    .filter_map(|item| item.as_str().map(|s| Value::String(s.to_string())))
                    .collect();
                if !selections.is_empty() {
                    descriptor.insert("pick".to_string(), Value::Array(selections));
                }
            }
            vec![Value::Object(descriptor)]
        }
        Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => {
            let source = normalize_spread_source(raw, suffix, kind);
            vec![Value::Object(Map::from_iter([(
                "source".to_string(),
                source,
            )]))]
        }
    }
}

static NORMALIZER_STEPS: OnceLock<Vec<Step>> = OnceLock::new();
static NORMALIZER_REGISTRY: OnceLock<Registry> = OnceLock::new();

fn impl_set_passthrough(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    Ok(input)
}

fn resolve_spec_root() -> PathBuf {
    if let Ok(path) = env::var("LCOD_SPEC_PATH") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("lcod-spec")
}

fn normalizer_compose_path() -> PathBuf {
    resolve_spec_root()
        .join("tooling")
        .join("compose")
        .join("normalize")
        .join("compose.yaml")
}

fn load_normalizer_steps() -> Result<&'static Vec<Step>> {
    if let Some(steps) = NORMALIZER_STEPS.get() {
        return Ok(steps);
    }
    let compose_path = normalizer_compose_path();
    let text = fs::read_to_string(&compose_path).with_context(|| {
        format!(
            "unable to read compose normalizer: {}",
            compose_path.display()
        )
    })?;
    let doc: Value = serde_yaml::from_str(&text).with_context(|| {
        format!(
            "invalid YAML in compose normalizer: {}",
            compose_path.display()
        )
    })?;
    let compose_value = doc
        .get("compose")
        .cloned()
        .ok_or_else(|| anyhow!("compose section missing in {}", compose_path.display()))?;
    let steps: Vec<Step> = serde_json::from_value(compose_value).with_context(|| {
        format!(
            "compose normalizer has invalid structure: {}",
            compose_path.display()
        )
    })?;
    let _ = NORMALIZER_STEPS.set(steps);
    NORMALIZER_STEPS
        .get()
        .ok_or_else(|| anyhow!("normalizer steps not initialized"))
}

fn normalizer_registry() -> Result<Registry> {
    if let Some(registry) = NORMALIZER_REGISTRY.get() {
        return Ok(registry.clone());
    }
    let registry = Registry::new();
    register_tooling(&registry);
    registry.register("lcod://impl/set@1", impl_set_passthrough);
    let _ = NORMALIZER_REGISTRY.set(registry.clone());
    Ok(NORMALIZER_REGISTRY
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("normalizer registry not initialized"))?)
}

fn normalize_via_component(compose: &Value) -> Result<Value> {
    if !compose.is_array() {
        return Ok(compose.clone());
    }
    let steps = load_normalizer_steps()?;
    let registry = normalizer_registry()?;
    let mut ctx = registry.context();
    let initial_state = json!({ "compose": compose.clone() });
    let result = run_compose(&mut ctx, steps.as_slice(), initial_state)?;
    let normalized = result
        .as_object()
        .and_then(|map| map.get("compose"))
        .cloned()
        .ok_or_else(|| anyhow!("compose normalizer did not produce a compose output"))?;
    let arr = normalized
        .as_array()
        .ok_or_else(|| anyhow!("compose normalizer returned a non-array compose output"))?;
    let original_len = compose
        .as_array()
        .map(|items| items.len())
        .unwrap_or_default();
    if original_len > 0 && arr.is_empty() {
        return Err(anyhow!(
            "compose normalizer returned an empty compose for a non-empty input"
        ));
    }
    Ok(normalized)
}

fn normalize_value(value: &Value, key: &str, kind: MappingKind, depth: usize) -> Value {
    match value {
        Value::String(s) if s == "=" && depth == 0 => match kind {
            MappingKind::Input => Value::String(format!("$.{key}")),
            MappingKind::Output => Value::String(key.to_string()),
        },
        Value::Object(map) => Value::Object(normalize_map(map, kind, depth + 1)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| match item {
                    Value::Object(map) => Value::Object(normalize_map(map, kind, depth + 1)),
                    _ => item.clone(),
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn normalize_map(map: &Map<String, Value>, kind: MappingKind, depth: usize) -> Map<String, Value> {
    let mut normalized = Map::new();
    let mut spreads = Vec::new();
    for (raw_key, value) in map {
        if depth == 0 && raw_key.starts_with("...") {
            let suffix = raw_key.trim_start_matches("...");
            spreads.extend(normalize_spread_entries(
                value,
                (!suffix.is_empty()).then_some(suffix),
                kind,
            ));
            continue;
        }
        let optional = depth == 0 && raw_key.ends_with('?');
        let key = if optional {
            raw_key.trim_end_matches('?')
        } else {
            raw_key.as_str()
        };
        let normalized_value = normalize_value(value, key, kind, depth);
        if optional {
            let mut wrapper = Map::new();
            wrapper.insert(OPTIONAL_FLAG.to_string(), Value::Bool(true));
            wrapper.insert("value".to_string(), normalized_value);
            normalized.insert(key.to_string(), Value::Object(wrapper));
        } else {
            normalized.insert(key.to_string(), normalized_value);
        }
    }
    if !spreads.is_empty() {
        normalized.insert(SPREAD_KEY.to_string(), Value::Array(spreads));
    }
    normalized
}

fn normalize_step(mut step: Step) -> Step {
    if !step.inputs.is_empty() {
        step.inputs = normalize_map(&step.inputs, MappingKind::Input, 0);
    }
    if !step.out.is_empty() {
        step.out = normalize_map(&step.out, MappingKind::Output, 0);
    }

    if let Some(children) = step.children.take() {
        step.children = Some(match children {
            StepChildren::List(list) => {
                StepChildren::List(list.into_iter().map(normalize_step).collect())
            }
            StepChildren::Map(map) => StepChildren::Map(
                map.into_iter()
                    .map(|(slot, steps)| (slot, steps.into_iter().map(normalize_step).collect()))
                    .collect(),
            ),
        });
    }

    step
}

fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for part in path.split('.') {
        match current {
            Value::Object(map) => current = map.get(part)?,
            Value::Array(arr) => {
                let idx: usize = part.parse().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn resolve_value(value: &Value, state: &Map<String, Value>, slot: &Map<String, Value>) -> Value {
    match value {
        Value::String(s) => {
            if s == STATE_SENTINEL {
                return Value::Object(state.clone());
            }
            if s == RESULT_SENTINEL {
                return Value::Null;
            }
            if let Some(stripped) = s.strip_prefix("$.") {
                let state_value = Value::Object(state.clone());
                return get_path(&state_value, stripped).cloned().unwrap_or(Value::Null);
            }
            if let Some(stripped) = s.strip_prefix("$slot.") {
                let slot_value = Value::Object(slot.clone());
                return get_path(&slot_value, stripped).cloned().unwrap_or(Value::Null);
            }
            value.clone()
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_value(item, state, slot))
                .collect(),
        ),
        Value::Object(map) => {
            let mut resolved = Map::new();
            for (key, val) in map {
                resolved.insert(key.clone(), resolve_value(val, state, slot));
            }
            Value::Object(resolved)
        }
        _ => value.clone(),
    }
}

fn unwrap_optional<'a>(value: &'a Value) -> (bool, &'a Value) {
    if let Some(obj) = value.as_object() {
        if obj
            .get(OPTIONAL_FLAG)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return (true, obj.get("value").unwrap_or(&Value::Null));
        }
    }
    (false, value)
}

fn is_path_like(value: &Value) -> bool {
    if let Some(s) = value.as_str() {
        return s.starts_with("$.") || s.starts_with("$slot.") || s == STATE_SENTINEL;
    }
    false
}

fn build_input(
    step: &Step,
    state: &Map<String, Value>,
    slot: &Map<String, Value>,
) -> Map<String, Value> {
    let mut map = Map::new();
    if let Some(spreads) = step
        .inputs
        .get(SPREAD_KEY)
        .and_then(Value::as_array)
    {
        for descriptor in spreads {
            let Some(obj) = descriptor.as_object() else { continue };
            let source_value = obj.get("source").unwrap_or(&Value::Null);
            let resolved = resolve_value(source_value, state, slot);
            let optional = obj
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let resolved_map = match resolved {
                Value::Object(map) => map,
                _ if optional => continue,
                _ => continue,
            };
            if let Some(pick) = obj.get("pick").and_then(Value::as_array) {
                for key in pick {
                    if let Some(name) = key.as_str() {
                        if let Some(val) = resolved_map.get(name) {
                            map.insert(name.to_string(), val.clone());
                        } else if !optional {
                            map.insert(name.to_string(), Value::Null);
                        }
                    }
                }
            } else {
                for (k, v) in resolved_map {
                    map.insert(k, v);
                }
            }
        }
    }
    for (key, value) in &step.inputs {
        if key == SPREAD_KEY {
            continue;
        }
        if key == "bindings" {
            map.insert(key.clone(), value.clone());
            continue;
        }
        let (optional, inner) = unwrap_optional(value);
        let resolved = resolve_value(inner, state, slot);
        if optional && is_path_like(inner) && resolved.is_null() {
            continue;
        }
        map.insert(key.clone(), resolved);
    }
    map
}

fn value_to_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn build_meta(step: &Step, slot: &Map<String, Value>) -> Option<Value> {
    let mut meta = Map::new();
    if let Some(children) = &step.children {
        if let Ok(serialized) = serde_json::to_value(children) {
            meta.insert("children".to_string(), serialized);
        }
    }
    if let Some(path) = &step.collect_path {
        meta.insert("collectPath".to_string(), Value::String(path.clone()));
    }
    meta.insert("slot".to_string(), Value::Object(slot.clone()));
    if meta.is_empty() {
        None
    } else {
        Some(Value::Object(meta))
    }
}

fn normalize_children(children: Option<&StepChildren>) -> HashMap<String, Vec<Step>> {
    match children {
        Some(StepChildren::List(list)) => {
            let mut map = HashMap::new();
            map.insert("children".to_string(), list.clone());
            map
        }
        Some(StepChildren::Map(map)) => map.clone(),
        None => HashMap::new(),
    }
}

struct ComposeSlotHandler {
    slots: HashMap<String, Vec<Step>>,
    parent_state: Map<String, Value>,
}

impl ComposeSlotHandler {
    fn new(children: Option<&StepChildren>, parent_state: &Map<String, Value>) -> Self {
        Self {
            slots: normalize_children(children),
            parent_state: parent_state.clone(),
        }
    }
}

impl SlotExecutor for ComposeSlotHandler {
    fn run_slot(
        &mut self,
        ctx: &mut Context,
        name: &str,
        local_state: Value,
        slot_vars: Value,
    ) -> Result<Value> {
        let local_map = if local_state.is_null() {
            self.parent_state.clone()
        } else {
            value_to_object(local_state)
        };
        let slot_map = value_to_object(slot_vars);
        let steps_opt = self.slots.get(name).or_else(|| self.slots.get("children"));
        let Some(steps) = steps_opt else {
            return Ok(Value::Object(local_map));
        };
        let result = run_steps(ctx, steps, local_map, &slot_map)?;
        Ok(Value::Object(result))
    }
}

fn apply_outputs(state: &mut Map<String, Value>, mappings: &Map<String, Value>, output: &Value) {
    if let Some(spreads) = mappings.get(SPREAD_KEY).and_then(Value::as_array) {
        if let Some(output_obj) = output.as_object() {
            let output_value = Value::Object(output_obj.clone());
            for descriptor in spreads {
                let Some(obj) = descriptor.as_object() else { continue };
                let source = obj.get("source").and_then(Value::as_str).unwrap_or("$");
                let optional = obj
                    .get("optional")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let payload = if source == "$" || source == RESULT_SENTINEL {
                    Some(output.clone())
                } else if let Some(stripped) = source.strip_prefix("$.") {
                    get_path(&output_value, stripped).cloned()
                } else {
                    Some(output.clone())
                };
                let Some(payload_value) = payload else {
                    if optional {
                        continue;
                    } else {
                        continue;
                    }
                };
                let payload_map = match payload_value {
                    Value::Object(map) => map,
                    _ if optional => continue,
                    _ => continue,
                };
                if let Some(pick) = obj.get("pick").and_then(Value::as_array) {
                    for key in pick {
                        if let Some(name) = key.as_str() {
                            if let Some(val) = payload_map.get(name) {
                                state.insert(name.to_string(), val.clone());
                            } else if !optional {
                                state.insert(name.to_string(), Value::Null);
                            }
                        }
                    }
                } else {
                    for (k, v) in payload_map {
                        state.insert(k, v);
                    }
                }
            }
        }
    }

    for (alias, key_value) in mappings {
        if alias == SPREAD_KEY {
            continue;
        }
        let (optional, inner) = unwrap_optional(key_value);
        let resolved = match inner {
            Value::String(s) if s == "$" => output.clone(),
            Value::String(s) => output.get(s).cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
        if optional && resolved.is_null() {
            continue;
        }
        state.insert(alias.clone(), resolved);
    }
}

fn run_steps(
    ctx: &mut Context,
    steps: &[Step],
    mut state: Map<String, Value>,
    slot: &Map<String, Value>,
) -> Result<Map<String, Value>> {
    for step in steps {
        let input_map = build_input(step, &state, slot);
        let input_value = Value::Object(input_map);
        let meta = build_meta(step, slot);

        let slot_handler: Box<dyn SlotExecutor + 'static> =
            Box::new(ComposeSlotHandler::new(step.children.as_ref(), &state));
        let previous = ctx.replace_run_slot_handler(Some(slot_handler));

        ctx.push_scope();
        let result = ctx.call(&step.call, input_value, meta);
        ctx.pop_scope();

        ctx.replace_run_slot_handler(previous);

        let output = result?;
        apply_outputs(&mut state, &step.out, &output);
    }

    Ok(state)
}

pub fn run_compose(ctx: &mut Context, steps: &[Step], initial_state: Value) -> Result<Value> {
    let state_map = initial_state.as_object().cloned().unwrap_or_default();
    let final_state = run_steps(ctx, steps, state_map, &Map::new())?;
    Ok(Value::Object(final_state))
}

pub fn parse_compose(value: &Value) -> Result<Vec<Step>> {
    if let Ok(normalized_value) = normalize_via_component(value) {
        if let Ok(steps) = serde_json::from_value::<Vec<Step>>(normalized_value.clone()) {
            return Ok(steps);
        }
    }
    let steps: Vec<Step> = serde_json::from_value(value.clone())?;
    Ok(steps.into_iter().map(normalize_step).collect())
}
