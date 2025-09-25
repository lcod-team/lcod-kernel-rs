use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::registry::{Context, SlotExecutor};

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
    if let Value::String(s) = value {
        if let Some(stripped) = s.strip_prefix("$.") {
            let state_value = Value::Object(state.clone());
            if let Some(v) = get_path(&state_value, stripped) {
                return v.clone();
            }
        }
        if let Some(stripped) = s.strip_prefix("$slot.") {
            let slot_value = Value::Object(slot.clone());
            if let Some(v) = get_path(&slot_value, stripped) {
                return v.clone();
            }
        }
    }
    value.clone()
}

fn build_input(
    step: &Step,
    state: &Map<String, Value>,
    slot: &Map<String, Value>,
) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in &step.inputs {
        map.insert(key.clone(), resolve_value(value, state, slot));
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
    for (alias, key_value) in mappings {
        let resolved = match key_value {
            Value::String(s) if s == "$" => output.clone(),
            Value::String(s) => output.get(s).cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
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
    let steps: Vec<Step> = serde_json::from_value(value.clone())?;
    Ok(steps)
}
