use anyhow::Result;
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

pub fn register_state(registry: &Registry) {
    registry.register("lcod://axiom/state/raw_input@1", raw_input_axiom);
}

fn raw_input_axiom(ctx: &mut Context, _input: Value, _meta: Option<Value>) -> Result<Value> {
    let snapshot = ctx.current_raw_input().cloned().unwrap_or(Value::Null);
    Ok(json!({ "value": snapshot }))
}
