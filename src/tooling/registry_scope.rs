use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use crate::compose::{parse_compose, run_compose};
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

fn register_inline_components(ctx: &mut Context, value: Option<&Value>) -> Result<()> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Ok(());
    };
    if list.is_empty() {
        return Ok(());
    }

    let registry = ctx.registry_clone();

    for entry in list {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let component_id = obj
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(component_id) = component_id else {
            eprintln!("tooling/registry/scope@1: skipping inline component without a valid `id`.");
            continue;
        };

        if component_id == "lcod://impl/testing/log-captured@1" {
            registry.register(
                "lcod://impl/testing/log-captured@1",
                |_ctx: &mut Context, _input: Value, _meta: Option<Value>| {
                    let logs = _ctx
                        .spec_captured_logs()
                        .iter()
                        .cloned()
                        .collect::<Vec<Value>>();
                    Ok(Value::Array(logs))
                },
            );
            continue;
        }

        if let Some(compose_value) = obj.get("compose").and_then(Value::as_array) {
            let compose_json = Value::Array(compose_value.clone());
            let steps = parse_compose(&compose_json).map_err(|err| {
                anyhow!(
                    "failed to parse inline component \"{}\": {}",
                    component_id,
                    err
                )
            })?;
            let steps_arc = Arc::new(steps);
            let id_owned = component_id.to_string();
            let registry_clone = registry.clone();
            registry_clone.register(
                id_owned.clone(),
                move |ctx: &mut Context, input: Value, _meta: Option<Value>| {
                    let seed = match input {
                        Value::Object(map) => Value::Object(map),
                        Value::Null => Value::Object(Map::new()),
                        other => other,
                    };
                    let result = run_compose(ctx, steps_arc.as_ref(), seed)?;
                    if id_owned == "lcod://impl/testing/log-capture@1" {
                        if let Value::Object(map) = &result {
                            if let Some(entry) = map.get("entry") {
                                ctx.push_spec_log(entry.clone());
                            }
                        } else {
                            ctx.push_spec_log(result.clone());
                        }
                    }
                    if let Value::Object(map) = &result {
                        if let Some(entry) = map.get("entry") {
                            return Ok(entry.clone());
                        }
                        if let Some(logs) = map.get("logs") {
                            return Ok(logs.clone());
                        }
                    }
                    Ok(result)
                },
            );
            continue;
        }

        if obj.get("manifest").is_some() {
            eprintln!(
                "tooling/registry/scope@1: inline component \"{}\" with manifest is not yet supported; skipping.",
                component_id
            );
            continue;
        }

        eprintln!(
            "tooling/registry/scope@1: inline component \"{}\" missing a supported definition; skipping.",
            component_id
        );
    }

    Ok(())
}

pub fn register_registry_scope(registry: &Registry) {
    registry.register(
        "lcod://tooling/registry/scope@1",
        |ctx: &mut Context, input: Value, meta: Option<Value>| -> Result<Value> {
            let bindings = parse_bindings(input.get("bindings"));

            ctx.enter_registry_scope(bindings)?;

            let exec_result = (|| -> Result<Value> {
                register_inline_components(ctx, input.get("components"))?;

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
