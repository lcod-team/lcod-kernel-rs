use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

const CONTRACT_LENGTH: &str = "lcod://contract/core/array/length@1";
const CONTRACT_PUSH: &str = "lcod://contract/core/array/push@1";
const CONTRACT_APPEND: &str = "lcod://contract/core/array/append@1";
const CONTRACT_SHIFT: &str = "lcod://contract/core/array/shift@1";
const AXIOM_LENGTH: &str = "lcod://axiom/array/length@1";
const AXIOM_PUSH: &str = "lcod://axiom/array/push@1";
const AXIOM_APPEND: &str = "lcod://axiom/array/append@1";
const AXIOM_SHIFT: &str = "lcod://axiom/array/shift@1";

pub fn register_array(registry: &Registry) {
    registry.register(CONTRACT_LENGTH, array_length_contract);
    registry.register(CONTRACT_PUSH, array_push_contract);
    registry.register(CONTRACT_APPEND, array_append_contract);
    registry.register(CONTRACT_SHIFT, array_shift_contract);
    registry.set_binding(AXIOM_LENGTH, CONTRACT_LENGTH);
    registry.set_binding(AXIOM_PUSH, CONTRACT_PUSH);
    registry.set_binding(AXIOM_APPEND, CONTRACT_APPEND);
    registry.set_binding(AXIOM_SHIFT, CONTRACT_SHIFT);
}

fn array_length_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let items = input
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("`items` must be an array"))?;
    Ok(json!({ "length": items.len() }))
}

fn array_push_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let value = input
        .get("value")
        .cloned()
        .ok_or_else(|| anyhow!("missing `value`"))?;
    let _clone = input.get("clone").and_then(Value::as_bool).unwrap_or(true);
    let mut items = input
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("`items` must be an array"))?
        .clone();

    items.push(value);
    let length = items.len();

    Ok(json!({
        "items": items,
        "length": length
    }))
}

fn array_append_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let mut array = input
        .get("array")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("`array` must be an array"))?
        .clone();

    let items = input.get("items").and_then(Value::as_array);
    let item = input.get("item");

    if items.is_none() && item.is_none() {
        return Err(anyhow!("`items` or `item` must be provided"));
    }

    if let Some(list) = items {
        array.extend(list.iter().cloned());
    }

    if let Some(single) = item {
        array.push(single.clone());
    }

    Ok(json!({
        "value": array.clone(),
        "length": array.len()
    }))
}

fn array_shift_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let items = input
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(Vec::new);

    if items.is_empty() {
        return Ok(json!({
            "head": Value::Null,
            "rest": Vec::<Value>::new()
        }));
    }

    let head = items[0].clone();
    let rest = items[1..].to_vec();

    Ok(json!({
        "head": head,
        "rest": rest
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    #[test]
    fn length_returns_size() {
        let registry = Registry::new();
        register_array(&registry);
        let mut ctx = registry.context();
        let res = array_length_contract(&mut ctx, json!({ "items": [1, 2, 3] }), None).unwrap();
        assert_eq!(res["length"].as_u64(), Some(3));
    }

    #[test]
    fn push_appends_value() {
        let registry = Registry::new();
        register_array(&registry);
        let mut ctx = registry.context();
        let res =
            array_push_contract(&mut ctx, json!({ "items": [1, 2], "value": 3 }), None).unwrap();
        assert_eq!(res["length"].as_u64(), Some(3));
        assert_eq!(res["items"], json!([1, 2, 3]));
    }

    #[test]
    fn append_concatenates_items() {
        let registry = Registry::new();
        register_array(&registry);
        let mut ctx = registry.context();
        let res = array_append_contract(
            &mut ctx,
            json!({
                "array": ["alpha", "beta"],
                "items": ["gamma"],
                "item": "delta"
            }),
            None,
        )
        .unwrap();
        assert_eq!(res["value"], json!(["alpha", "beta", "gamma", "delta"]));
        assert_eq!(res["length"].as_u64(), Some(4));
    }

    #[test]
    fn shift_returns_head_and_rest() {
        let registry = Registry::new();
        register_array(&registry);
        let mut ctx = registry.context();
        let res =
            array_shift_contract(&mut ctx, json!({ "items": [1, 2, 3] }), None).unwrap();
        assert_eq!(res["head"], json!(1));
        assert_eq!(res["rest"], json!([2, 3]));

        let empty = array_shift_contract(&mut ctx, json!({ "items": [] }), None).unwrap();
        assert_eq!(empty["head"], Value::Null);
        assert_eq!(empty["rest"], json!([]));
    }
}
