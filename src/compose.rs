use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Number, Value};

use crate::registry::{Context, Registry, SlotExecutor};
use crate::tooling::{log_kernel_error, log_kernel_info, register_tooling};

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
    #[serde(default)]
    pub slots: Option<StepChildren>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepChildren {
    List(Vec<Step>),
    Map(HashMap<String, Vec<Step>>),
}

fn merge_map_with_fallback(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (key, source_value) in source {
        match target.get_mut(key) {
            Some(target_value) => {
                if let Value::Object(target_obj) = target_value {
                    if let Value::Object(source_obj) = source_value {
                        merge_map_with_fallback(target_obj, source_obj);
                        continue;
                    }
                }
                if target_value.is_null() && !source_value.is_null() {
                    *target_value = source_value.clone();
                }
            }
            None => {
                if !source_value.is_null() {
                    target.insert(key.clone(), source_value.clone());
                }
            }
        }
    }
}

fn merge_step_with_fallback(normalized: &mut Step, fallback: &Step) {
    merge_map_with_fallback(&mut normalized.inputs, &fallback.inputs);
    merge_map_with_fallback(&mut normalized.out, &fallback.out);

    match (&mut normalized.children, &fallback.children) {
        (Some(StepChildren::List(target_steps)), Some(StepChildren::List(source_steps))) => {
            for (target, source) in target_steps.iter_mut().zip(source_steps.iter()) {
                merge_step_with_fallback(target, source);
            }
        }
        (Some(StepChildren::Map(target_map)), Some(StepChildren::Map(source_map))) => {
            for (slot, target_steps) in target_map.iter_mut() {
                if let Some(source_steps) = source_map.get(slot) {
                    for (target, source) in target_steps.iter_mut().zip(source_steps.iter()) {
                        merge_step_with_fallback(target, source);
                    }
                }
            }
        }
        _ => {}
    }
    match (&mut normalized.slots, &fallback.slots) {
        (Some(StepChildren::List(target_steps)), Some(StepChildren::List(source_steps))) => {
            for (target, source) in target_steps.iter_mut().zip(source_steps.iter()) {
                merge_step_with_fallback(target, source);
            }
        }
        (Some(StepChildren::Map(target_map)), Some(StepChildren::Map(source_map))) => {
            for (slot, target_steps) in target_map.iter_mut() {
                if let Some(source_steps) = source_map.get(slot) {
                    for (target, source) in target_steps.iter_mut().zip(source_steps.iter()) {
                        merge_step_with_fallback(target, source);
                    }
                }
            }
        }
        _ => {}
    }
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
            descriptor.insert("source".to_string(), source_value);
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

fn normalizer_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = env::var("LCOD_SPEC_PATH") {
        if !path.is_empty() {
            paths.push(
                PathBuf::from(path)
                    .join("tooling")
                    .join("compose")
                    .join("normalize")
                    .join("compose.yaml"),
            );
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    paths.push(
        manifest_dir
            .join("..")
            .join("lcod-spec")
            .join("tooling")
            .join("compose")
            .join("normalize")
            .join("compose.yaml"),
    );
    paths.push(
        manifest_dir
            .join("resources")
            .join("normalize")
            .join("compose.yaml"),
    );
    paths
}

fn normalizer_compose_path() -> PathBuf {
    let candidates = normalizer_candidates();
    for candidate in &candidates {
        if candidate.is_file() {
            return candidate.clone();
        }
    }
    candidates
        .last()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("resources/normalize/compose.yaml"))
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

    if let Some(slots) = step.slots.take() {
        step.slots = Some(match slots {
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
                return get_path(&state_value, stripped)
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            if let Some(stripped) = s.strip_prefix("$slot.") {
                let slot_value = Value::Object(slot.clone());
                return get_path(&slot_value, stripped)
                    .cloned()
                    .unwrap_or(Value::Null);
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
    if let Some(spreads) = step.inputs.get(SPREAD_KEY).and_then(Value::as_array) {
        for descriptor in spreads {
            let Some(obj) = descriptor.as_object() else {
                continue;
            };
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
        if step.call == "lcod://tooling/test_checker@1" && key == "compose" {
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

fn build_meta(
    step: &Step,
    slot: &Map<String, Value>,
    slots_map: &HashMap<String, Vec<Step>>,
) -> Option<Value> {
    let mut meta = Map::new();
    let serialized_slots = serde_json::to_value(slots_map).ok();
    if let Some(value) = step
        .children
        .as_ref()
        .or(step.slots.as_ref())
        .and_then(|children| serde_json::to_value(children).ok())
    {
        meta.insert("children".to_string(), value);
    } else if let Some(value) = serialized_slots.clone() {
        if !value.is_null() {
            meta.insert("children".to_string(), value.clone());
        }
    }
    if let Some(value) = serialized_slots {
        if !value.is_null() {
            meta.insert("slots".to_string(), value);
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

fn merge_step_children(
    target: &mut HashMap<String, Vec<Step>>,
    source: &StepChildren,
    overwrite: bool,
) {
    match source {
        StepChildren::List(list) => {
            if overwrite || !target.contains_key("children") {
                target.insert("children".to_string(), list.clone());
            }
        }
        StepChildren::Map(map) => {
            for (key, steps) in map {
                if overwrite || !target.contains_key(key) {
                    target.insert(key.clone(), steps.clone());
                }
            }
        }
    }
}

fn normalize_children(
    children: Option<&StepChildren>,
    slots: Option<&StepChildren>,
) -> HashMap<String, Vec<Step>> {
    let mut map = HashMap::new();
    if let Some(child) = children {
        merge_step_children(&mut map, child, false);
    }
    if let Some(slot_map) = slots {
        merge_step_children(&mut map, slot_map, true);
    }
    if !map.contains_key("children") {
        if let Some(body) = map.get("body").cloned() {
            map.insert("children".to_string(), body);
        }
    }
    map
}

struct ComposeSlotHandler {
    slots: HashMap<String, Vec<Step>>,
    parent_state: Map<String, Value>,
}

impl ComposeSlotHandler {
    fn new(slots: HashMap<String, Vec<Step>>, parent_state: &Map<String, Value>) -> Self {
        Self {
            slots,
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
        let steps_opt = self
            .slots
            .get(name)
            .or_else(|| {
                if name == "children" {
                    self.slots.get("body")
                } else if name == "body" {
                    self.slots.get("children")
                } else {
                    None
                }
            })
            .or_else(|| self.slots.get("children"));
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
                let Some(obj) = descriptor.as_object() else {
                    continue;
                };
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

fn compose_step_tags(step: &Step) -> Value {
    let mut tags = Map::new();
    tags.insert(
        "logger".to_string(),
        Value::String("kernel.compose.step".to_string()),
    );
    tags.insert("componentId".to_string(), Value::String(step.call.clone()));
    Value::Object(tags)
}

fn compose_step_start_data(
    index: usize,
    collect_path: Option<&String>,
    input_keys: Option<&Vec<String>>,
    slot_keys: Option<&Vec<String>>,
    has_children: bool,
) -> Value {
    let mut data = Map::new();
    data.insert("phase".to_string(), Value::String("start".to_string()));
    data.insert(
        "stepIndex".to_string(),
        Value::Number(Number::from(index as u64)),
    );
    if let Some(path) = collect_path {
        data.insert("collectPath".to_string(), Value::String(path.clone()));
    }
    if let Some(keys) = input_keys {
        if !keys.is_empty() {
            data.insert(
                "inputKeys".to_string(),
                Value::Array(keys.iter().cloned().map(Value::String).collect()),
            );
        }
    }
    if let Some(keys) = slot_keys {
        if !keys.is_empty() {
            data.insert(
                "slotKeys".to_string(),
                Value::Array(keys.iter().cloned().map(Value::String).collect()),
            );
        }
    }
    if has_children {
        data.insert("hasChildren".to_string(), Value::Bool(true));
    }
    Value::Object(data)
}

fn value_type_label(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn compose_step_success_data(index: usize, duration_ms: f64, output: &Value) -> Value {
    let mut data = Map::new();
    data.insert("phase".to_string(), Value::String("success".to_string()));
    data.insert(
        "stepIndex".to_string(),
        Value::Number(Number::from(index as u64)),
    );
    if let Some(number) = Number::from_f64(duration_ms) {
        data.insert("durationMs".to_string(), Value::Number(number));
    }
    data.insert(
        "resultType".to_string(),
        Value::String(value_type_label(output).to_string()),
    );
    match output {
        Value::Object(map) => {
            if !map.is_empty() {
                let keys = map
                    .keys()
                    .cloned()
                    .map(Value::String)
                    .collect::<Vec<Value>>();
                data.insert("resultKeys".to_string(), Value::Array(keys));
            }
        }
        Value::Array(items) => {
            data.insert(
                "resultLength".to_string(),
                Value::Number(Number::from(items.len() as u64)),
            );
        }
        _ => {}
    }
    Value::Object(data)
}

fn compose_step_error_data(index: usize, duration_ms: f64, err: &anyhow::Error) -> Value {
    let mut data = Map::new();
    data.insert("phase".to_string(), Value::String("error".to_string()));
    data.insert(
        "stepIndex".to_string(),
        Value::Number(Number::from(index as u64)),
    );
    if let Some(number) = Number::from_f64(duration_ms) {
        data.insert("durationMs".to_string(), Value::Number(number));
    }

    let mut error_map = Map::new();
    error_map.insert("message".to_string(), Value::String(err.to_string()));
    if let Some(root) = err.source() {
        error_map.insert("rootCause".to_string(), Value::String(root.to_string()));
    }
    data.insert("error".to_string(), Value::Object(error_map));
    Value::Object(data)
}

fn log_step_info(ctx: &mut Context, step: &Step, payload: Value) {
    let tags = compose_step_tags(step);
    let _ = log_kernel_info(Some(ctx), "compose.step", Some(payload), Some(tags));
}

fn log_step_error(ctx: &mut Context, step: &Step, payload: Value) {
    let tags = compose_step_tags(step);
    let _ = log_kernel_error(Some(ctx), "compose.step", Some(payload), Some(tags));
}

fn run_steps(
    ctx: &mut Context,
    steps: &[Step],
    mut state: Map<String, Value>,
    slot: &Map<String, Value>,
) -> Result<Map<String, Value>> {
    for (index, step) in steps.iter().enumerate() {
        ctx.ensure_not_cancelled()?;
        if step.call == "lcod://tooling/script@1" {
            // no-op: retained escalation point for future diagnostics
        }
        let input_map = build_input(step, &state, slot);
        let input_value = Value::Object(input_map);
        let slot_map = normalize_children(step.children.as_ref(), step.slots.as_ref());
        let meta = build_meta(step, slot, &slot_map);

        let slot_handler: Box<dyn SlotExecutor + 'static> =
            Box::new(ComposeSlotHandler::new(slot_map.clone(), &state));
        let previous = ctx.replace_run_slot_handler(Some(slot_handler));

        let input_keys = match &input_value {
            Value::Object(map) => {
                let keys = map.keys().cloned().collect::<Vec<String>>();
                if keys.is_empty() {
                    None
                } else {
                    Some(keys)
                }
            }
            _ => None,
        };
        let slot_keys = if slot.is_empty() {
            None
        } else {
            let keys = slot.keys().cloned().collect::<Vec<String>>();
            if keys.is_empty() {
                None
            } else {
                Some(keys)
            }
        };
        let has_children = slot_map.values().any(|steps| !steps.is_empty());
        log_step_info(
            ctx,
            step,
            compose_step_start_data(
                index,
                step.collect_path.as_ref(),
                input_keys.as_ref(),
                slot_keys.as_ref(),
                has_children,
            ),
        );
        let started_at = Instant::now();

        ctx.push_scope();
        let result = ctx.call(&step.call, input_value, meta);
        ctx.pop_scope();

        ctx.replace_run_slot_handler(previous);

        let duration_ms = started_at.elapsed().as_secs_f64() * 1000.0;

        match result {
            Ok(output) => {
                apply_outputs(&mut state, &step.out, &output);
                log_step_info(
                    ctx,
                    step,
                    compose_step_success_data(index, duration_ms, &output),
                );
            }
            Err(err) => {
                log_step_error(ctx, step, compose_step_error_data(index, duration_ms, &err));
                return Err(err);
            }
        }
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
        if let Ok(mut steps) = serde_json::from_value::<Vec<Step>>(normalized_value.clone()) {
            if let Ok(fallback_steps) = serde_json::from_value::<Vec<Step>>(value.clone()) {
                for (normalized, fallback) in steps.iter_mut().zip(fallback_steps.iter()) {
                    merge_step_with_fallback(normalized, fallback);
                }
            }
            return Ok(steps);
        }
    }
    let steps: Vec<Step> = serde_json::from_value(value.clone())?;
    Ok(steps.into_iter().map(normalize_step).collect())
}
