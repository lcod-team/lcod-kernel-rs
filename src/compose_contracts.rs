use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::compose::SlotNotFoundError;
use crate::registry::{Context, Registry};

pub fn register_compose_contracts(registry: &Registry) {
    registry.register("lcod://contract/compose/run_slot@1", compose_run_slot);
}

fn compose_run_slot(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let slot_name = input
        .get("slot")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("slot must be provided"))?;
    let state = input.get("state").cloned();
    let slot_vars = input.get("slotVars").cloned();
    let optional = input
        .get("optional")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    match ctx.run_slot(slot_name, state, slot_vars) {
        Ok(result) => Ok(json!({
            "ran": true,
            "result": result,
        })),
        Err(err) => Ok(Value::Object({
            if optional && err.downcast_ref::<SlotNotFoundError>().is_some() {
                return Ok(json!({
                    "ran": false,
                    "result": Value::Null,
                }));
            }
            let mut map = Map::new();
            map.insert("ran".to_string(), Value::Bool(true));
            map.insert(
                "error".to_string(),
                json!({
                    "message": err.to_string(),
                    "code": "slot_execution_failed",
                }),
            );
            map
        })),
    }
}
