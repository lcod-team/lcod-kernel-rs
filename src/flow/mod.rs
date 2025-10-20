use std::fmt;

use crate::registry::{Context, Registry};
use anyhow::{anyhow, Result};
use serde_json::{Map, Number, Value};

#[derive(Debug)]
pub struct FlowSignalError {
    name: &'static str,
}

impl FlowSignalError {
    fn new(name: &'static str) -> Self {
        Self { name }
    }

    pub fn is(&self, expected: &str) -> bool {
        self.name == expected
    }
}

impl fmt::Display for FlowSignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "flow signal: {}", self.name)
    }
}

impl std::error::Error for FlowSignalError {}

pub fn flow_break(_ctx: &mut Context, _input: Value, _meta: Option<Value>) -> Result<Value> {
    Err(FlowSignalError::new("break").into())
}

pub fn flow_continue(_ctx: &mut Context, _input: Value, _meta: Option<Value>) -> Result<Value> {
    Err(FlowSignalError::new("continue").into())
}

pub fn flow_if(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let cond = input.get("cond").map_or(false, |value| is_truthy(value));
    let slot_name = if cond { "then" } else { "else" };
    ctx.run_slot(slot_name, None, None)
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(num) => {
            if let Some(f) = num.as_f64() {
                f != 0.0 && !f.is_nan()
            } else if let Some(i) = num.as_i64() {
                i != 0
            } else if let Some(u) = num.as_u64() {
                u != 0
            } else {
                false
            }
        }
        Value::String(text) => !text.is_empty(),
        Value::Array(_) => true,
        Value::Object(_) => true,
    }
}

fn list_from_input(input: &Value) -> Result<Vec<Value>> {
    if let Some(arr) = input.get("list").and_then(Value::as_array) {
        return Ok(arr.clone());
    }
    if input.get("list").is_some() {
        return Err(anyhow!("flow/foreach: expected array for `list`"));
    }
    if let Some(stream_val) = input.get("stream") {
        if let Some(arr) = stream_val.as_array() {
            return Ok(arr.clone());
        }
        if stream_val.is_null() {
            return Ok(Vec::new());
        }
        return Err(anyhow!(
            "flow/foreach: stream must be an array in this runtime"
        ));
    }
    Ok(Vec::new())
}

fn get_by_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
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

fn collect_path_value(
    path: &str,
    iter_state: &Value,
    slot_vars: &Map<String, Value>,
) -> Option<Value> {
    let mut root = Map::new();
    root.insert("$".to_string(), iter_state.clone());
    root.insert("$slot".to_string(), Value::Object(slot_vars.clone()));
    get_by_path(&Value::Object(root), path).cloned()
}

pub fn flow_foreach(ctx: &mut Context, input: Value, meta: Option<Value>) -> Result<Value> {
    let items = list_from_input(&input)?;
    let collect_path = meta
        .as_ref()
        .and_then(|m| m.get("collectPath"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let mut results = Vec::new();

    if items.is_empty() {
        let mut slot_vars = Map::new();
        slot_vars.insert("item".to_string(), Value::Null);
        slot_vars.insert("index".to_string(), Value::Number(Number::from(-1)));
        let else_state = ctx.run_slot("else", None, Some(Value::Object(slot_vars.clone())));
        if let (Some(path), Ok(state)) = (collect_path.as_deref(), else_state) {
            if let Some(val) = collect_path_value(path, &state, &slot_vars) {
                results.push(val);
            }
        }
        let mut out = Map::new();
        out.insert("results".to_string(), Value::Array(results));
        return Ok(Value::Object(out));
    }

    for (index, item) in items.into_iter().enumerate() {
        ctx.ensure_not_cancelled()?;
        let mut slot_vars = Map::new();
        slot_vars.insert("item".to_string(), item.clone());
        slot_vars.insert(
            "index".to_string(),
            Value::Number(Number::from(index as i64)),
        );
        let slot_value = Value::Object(slot_vars.clone());
        match ctx.run_slot("body", None, Some(slot_value)) {
            Ok(iter_state) => {
                if let Some(path) = collect_path.as_deref() {
                    let val =
                        collect_path_value(path, &iter_state, &slot_vars).unwrap_or(Value::Null);
                    results.push(val);
                } else {
                    results.push(item);
                }
            }
            Err(err) => {
                if let Some(signal) = err.downcast_ref::<FlowSignalError>() {
                    if signal.is("continue") {
                        continue;
                    }
                    if signal.is("break") {
                        break;
                    }
                }
                return Err(err);
            }
        }
    }

    let mut out = Map::new();
    out.insert("results".to_string(), Value::Array(results));
    Ok(Value::Object(out))
}

pub fn register_flow(registry: &Registry) {
    registry.register("lcod://flow/break@1", flow_break);
    registry.register("lcod://flow/continue@1", flow_continue);
    registry.register("lcod://flow/if@1", flow_if);
    registry.register("lcod://flow/foreach@1", flow_foreach);
}
