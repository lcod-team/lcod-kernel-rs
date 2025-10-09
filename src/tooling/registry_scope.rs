use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::registry::{Context, Registry};

fn parse_bindings(value: Option<&Value>) -> Option<HashMap<String, String>> {
    let map_value = value?.as_object()?;
    let mut bindings = HashMap::new();
    for (contract, implementation) in map_value {
        if let Some(impl_id) = implementation.as_str() {
            bindings.insert(contract.clone(), impl_id.to_string());
        }
    }
    if bindings.is_empty() {
        None
    } else {
        Some(bindings)
    }
}

pub fn register_registry_scope(registry: &Registry) {
    registry.register(
        "lcod://tooling/registry/scope@1",
        |ctx: &mut Context, input: Value, meta: Option<Value>| -> Result<Value> {
            let bindings = parse_bindings(input.get("bindings"));

            if let Some(components) = input.get("components") {
                if components.is_array() && !components.as_array().unwrap().is_empty() {
                    eprintln!(
                        "tooling/registry/scope@1: `components` are not yet supported by the Rust kernel; ignoring."
                    );
                }
            }

            ctx.enter_registry_scope(bindings)?;

            let exec_result = (|| -> Result<Value> {
                let has_children = meta
                    .as_ref()
                    .and_then(|meta_map| meta_map.get("children"))
                    .is_some();
                if has_children {
                    ctx.run_slot("children", None, None)
                } else {
                    Ok(Value::Object(Map::new()))
                }
            })();

            let leave_result = ctx.leave_registry_scope();

            match (exec_result, leave_result) {
                (Ok(value), Ok(())) => Ok(value),
                (Ok(_), Err(err)) => Err(err),
                (Err(err), Ok(())) => Err(err),
                (Err(exec_err), Err(_leave_err)) => Err(exec_err),
            }
        },
    );
}
