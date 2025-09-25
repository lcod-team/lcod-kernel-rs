use anyhow::Result;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::registry::Registry;

#[derive(Debug, Deserialize)]
pub struct Step {
    pub call: String,
    #[serde(default, rename = "in")]
    pub inputs: Map<String, Value>,
    #[serde(default)]
    pub out: Map<String, Value>,
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

fn resolve_value(value: &Value, state: &Value) -> Value {
    if let Value::String(s) = value {
        if let Some(stripped) = s.strip_prefix("$.") {
            if let Some(v) = get_path(state, stripped) {
                return v.clone();
            }
        }
    }
    value.clone()
}

fn build_input(step: &Step, state: &Value) -> Value {
    let mut map = Map::new();
    for (key, value) in &step.inputs {
        map.insert(key.clone(), resolve_value(value, state));
    }
    Value::Object(map)
}

pub fn run_compose(registry: &Registry, steps: &[Step], initial_state: Value) -> Result<Value> {
    let mut state_map = initial_state.as_object().cloned().unwrap_or_else(Map::new);
    for step in steps {
        let current_state = Value::Object(state_map.clone());
        let input = build_input(step, &current_state);
        let output = registry.call(&step.call, input)?;
        for (alias, key_value) in &step.out {
            let resolved = match key_value {
                Value::String(s) if s == "$" => output.clone(),
                Value::String(s) => output.get(s).cloned().unwrap_or(Value::Null),
                other => other.clone(),
            };
            state_map.insert(alias.clone(), resolved);
        }
    }
    Ok(Value::Object(state_map))
}

pub fn parse_compose(value: &Value) -> Result<Vec<Step>> {
    let steps: Vec<Step> = serde_json::from_value(value.clone())?;
    Ok(steps)
}
