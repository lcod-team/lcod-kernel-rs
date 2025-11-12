use serde_json::{json, Value};

use crate::registry::{Context, Registry};

const CONTRACT_KIND: &str = "lcod://contract/core/value/kind@1";
const CONTRACT_EQUALS: &str = "lcod://contract/core/value/equals@1";

pub fn register_value(registry: &Registry) {
    registry.register(CONTRACT_KIND, kind_contract);
    registry.register(CONTRACT_EQUALS, equals_contract);
}

fn kind_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> anyhow::Result<Value> {
    let value = input.get("value").cloned().unwrap_or(Value::Null);
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    Ok(json!({ "kind": kind }))
}

fn equals_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> anyhow::Result<Value> {
    let left = input.get("left").cloned().unwrap_or(Value::Null);
    let right = input.get("right").cloned().unwrap_or(Value::Null);
    Ok(json!({ "equal": left == right }))
}
