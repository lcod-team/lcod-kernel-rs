use std::fmt;

use crate::compose::SlotNotFoundError;
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

pub fn flow_check_abort(ctx: &mut Context, _input: Value, _meta: Option<Value>) -> Result<Value> {
    ctx.ensure_not_cancelled()?;
    Ok(Value::Object(Map::new()))
}

pub fn flow_if(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let cond = input.get("cond").map_or(false, |value| is_truthy(value));
    let slot_name = if cond { "then" } else { "else" };
    match ctx.run_slot(slot_name, None, None) {
        Ok(value) => Ok(value),
        Err(err) => match err.downcast::<SlotNotFoundError>() {
            Ok(_) => Ok(Value::Object(Map::new())),
            Err(err) => Err(err),
        },
    }
}

fn has_slot(meta: &Option<Value>, name: &str) -> bool {
    meta.as_ref()
        .and_then(Value::as_object)
        .and_then(|obj| obj.get("children"))
        .and_then(Value::as_object)
        .and_then(|children| children.get(name))
        .and_then(Value::as_array)
        .map(|arr| !arr.is_empty())
        .unwrap_or(false)
}

fn slot_vars(phase: &str, error: Option<&Value>) -> Value {
    let mut vars = Map::new();
    vars.insert("phase".to_string(), Value::String(phase.to_string()));
    if let Some(err) = error {
        vars.insert("error".to_string(), err.clone());
    }
    Value::Object(vars)
}

fn normalize_error_value(err: &anyhow::Error) -> Value {
    let mut map = Map::new();
    map.insert(
        "code".to_string(),
        Value::String("unexpected_error".to_string()),
    );
    map.insert("message".to_string(), Value::String(err.to_string()));
    Value::Object(map)
}

fn replace_state(target: &mut Map<String, Value>, value: Value, context: &str) -> Result<()> {
    match value {
        Value::Object(map) => {
            *target = map;
        }
        Value::Null => {}
        other => {
            return Err(anyhow!(
                "flow/try: {} slot must return an object or null, got {}",
                context,
                other
            ))
        }
    }
    Ok(())
}

fn merge_state(target: &mut Map<String, Value>, value: Value, context: &str) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, val) in map {
                target.insert(key, val);
            }
        }
        Value::Null => {}
        other => {
            return Err(anyhow!(
                "flow/try: {} slot must return an object or null, got {}",
                context,
                other
            ))
        }
    }
    Ok(())
}

pub fn flow_try(ctx: &mut Context, _input: Value, meta: Option<Value>) -> Result<Value> {
    let mut result_state = Map::new();
    let mut pending_error: Option<anyhow::Error> = None;
    let mut pending_error_value: Option<Value> = None;

    match ctx.run_slot("children", None, Some(slot_vars("try", None))) {
        Ok(value) => replace_state(&mut result_state, value, "try")?,
        Err(err) => match err.downcast::<FlowSignalError>() {
            Ok(signal) => return Err(signal.into()),
            Err(err) => {
                let normalized = normalize_error_value(&err);
                pending_error_value = Some(normalized.clone());
                pending_error = Some(err);
            }
        },
    }

    if pending_error.is_some() && has_slot(&meta, "catch") {
        let error_value = pending_error_value.clone();
        match ctx.run_slot(
            "catch",
            None,
            Some(slot_vars("catch", error_value.as_ref())),
        ) {
            Ok(value) => {
                replace_state(&mut result_state, value, "catch")?;
                pending_error = None;
                pending_error_value = None;
            }
            Err(err) => match err.downcast::<FlowSignalError>() {
                Ok(signal) => return Err(signal.into()),
                Err(err) => {
                    let normalized = normalize_error_value(&err);
                    pending_error_value = Some(normalized.clone());
                    pending_error = Some(err);
                }
            },
        }
    }

    if has_slot(&meta, "finally") {
        let final_value = ctx.run_slot(
            "finally",
            None,
            Some(slot_vars("finally", pending_error_value.as_ref())),
        )?;
        merge_state(&mut result_state, final_value, "finally")?;
    }

    if let Some(err) = pending_error {
        return Err(err);
    }

    Ok(Value::Object(result_state))
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

fn interpret_condition(value: &Value) -> Result<(bool, Option<Map<String, Value>>)> {
    let state_override = value
        .as_object()
        .and_then(|map| map.get("state"))
        .and_then(Value::as_object)
        .cloned();

    if let Some(map) = value.as_object() {
        if let Some(cond_value) = map
            .get("continue")
            .or_else(|| map.get("cond"))
            .or_else(|| map.get("value"))
        {
            return Ok((is_truthy(cond_value), state_override));
        }
        if map.is_empty() {
            return Ok((false, state_override));
        }
        return Err(anyhow!(
            "flow/while: condition slot must return a boolean or an object with `continue`, `cond`, or `value`"
        ));
    }

    Ok((is_truthy(value), state_override))
}

pub fn flow_while(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let mut state = match input.get("state") {
        Some(Value::Object(map)) => map.clone(),
        Some(Value::Null) | None => Map::new(),
        Some(other) => {
            return Err(anyhow!(
                "flow/while: `state` must be an object, got {}",
                other
            ))
        }
    };

    let max_iterations = match input.get("maxIterations") {
        Some(Value::Number(num)) => {
            if let Some(lim) = num.as_u64() {
                if lim == 0 {
                    None
                } else {
                    Some(lim)
                }
            } else {
                return Err(anyhow!(
                    "flow/while: `maxIterations` must be a non-negative integer"
                ));
            }
        }
        Some(Value::Null) | None => None,
        Some(other) => {
            return Err(anyhow!(
                "flow/while: `maxIterations` must be a non-negative integer, got {}",
                other
            ))
        }
    };

    let mut iterations: u64 = 0;

    loop {
        ctx.ensure_not_cancelled()?;
        if let Some(limit) = max_iterations {
            if iterations >= limit {
                return Err(anyhow!(
                    "flow/while: exceeded maxIterations limit ({})",
                    limit
                ));
            }
        }

        let mut slot_vars = Map::new();
        slot_vars.insert(
            "index".to_string(),
            Value::Number(Number::from(iterations as i64)),
        );
        slot_vars.insert("state".to_string(), Value::Object(state.clone()));

        let condition_output = ctx.run_slot(
            "condition",
            Some(Value::Object(state.clone())),
            Some(Value::Object(slot_vars.clone())),
        )?;
        let (should_continue, state_override) = interpret_condition(&condition_output)?;
        if let Some(new_state) = state_override {
            state = new_state;
        }
        if !should_continue {
            break;
        }

        let body_result = match ctx.run_slot(
            "body",
            Some(Value::Object(state.clone())),
            Some(Value::Object(slot_vars.clone())),
        ) {
            Ok(value) => value,
            Err(err) => {
                if let Some(signal) = err.downcast_ref::<FlowSignalError>() {
                    iterations += 1;
                    if signal.is("continue") {
                        continue;
                    }
                    if signal.is("break") {
                        break;
                    }
                }
                return Err(err);
            }
        };

        if let Value::Object(map) = body_result {
            state = map;
        } else if !body_result.is_null() {
            return Err(anyhow!(
                "flow/while: body slot must return an object or null, got {}",
                body_result
            ));
        }

        iterations += 1;
    }

    if iterations == 0 {
        let mut else_slot_vars = Map::new();
        else_slot_vars.insert("index".to_string(), Value::Number(Number::from(-1)));
        else_slot_vars.insert("state".to_string(), Value::Object(state.clone()));
        let else_value = ctx.run_slot(
            "else",
            Some(Value::Object(state.clone())),
            Some(Value::Object(else_slot_vars)),
        )?;
        if let Value::Object(map) = else_value {
            state = map;
        } else if !else_value.is_null() {
            return Err(anyhow!(
                "flow/while: else slot must return an object or null, got {}",
                else_value
            ));
        }
    }

    let mut output = Map::new();
    output.insert("state".to_string(), Value::Object(state));
    output.insert(
        "iterations".to_string(),
        Value::Number(Number::from(iterations as i64)),
    );
    Ok(Value::Object(output))
}

pub fn register_flow(registry: &Registry) {
    registry.register("lcod://flow/break@1", flow_break);
    registry.register("lcod://flow/continue@1", flow_continue);
    registry.register("lcod://flow/try@1", flow_try);
    registry.register("lcod://flow/if@1", flow_if);
    registry.register("lcod://flow/foreach@1", flow_foreach);
    registry.register("lcod://flow/check_abort@1", flow_check_abort);
    registry.register("lcod://flow/while@1", flow_while);
}
