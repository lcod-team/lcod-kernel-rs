use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::compose::{parse_compose, run_compose};
use crate::registry::{ComponentMetadata, Context, Registry};
use crate::tooling::logging::log_kernel_warn;

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
            let _ = log_kernel_warn(
                Some(ctx),
                "Skipping inline component without a valid id",
                None,
                Some(json!({ "module": "registry-scope", "reason": "missing-id" })),
            );
            continue;
        };

        if component_id == "lcod://impl/testing/log-captured@1" {
            registry.register(
                "lcod://impl/testing/log-captured@1",
                |_ctx: &mut Context, _input: Value, _meta: Option<Value>| {
                    let logs_vec = _ctx
                        .spec_captured_logs()
                        .iter()
                        .cloned()
                        .collect::<Vec<Value>>();
                    Ok(Value::Array(logs_vec))
                },
            );
            continue;
        }

        if component_id == "lcod://impl/testing/log-capture@1" {
            registry.register(
                "lcod://impl/testing/log-capture@1",
                |ctx: &mut Context, input: Value, _meta: Option<Value>| {
                    let entry_map = match input {
                        Value::Object(map) => map,
                        Value::Null => Map::new(),
                        other => return Ok(other),
                    };
                    let cloned = Value::Object(entry_map.clone());
                    ctx.push_spec_log(cloned.clone());
                    Ok(cloned)
                },
            );
            continue;
        }

        let metadata = ComponentMetadata {
            inputs: obj
                .get("inputs")
                .and_then(Value::as_object)
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default(),
            outputs: obj
                .get("outputs")
                .and_then(Value::as_object)
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default(),
            slots: obj
                .get("slots")
                .and_then(Value::as_object)
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default(),
        };

        if let Some(compose_value) = obj.get("compose").and_then(Value::as_array) {
            let compose_json = Value::Array(compose_value.clone());
            let mut steps = parse_compose(&compose_json).map_err(|err| {
                anyhow!(
                    "failed to parse inline component \"{}\": {}",
                    component_id,
                    err
                )
            })?;
            for step in &mut steps {
                if step.call == "lcod://tooling/script@1" {
                    if let Some(value) = step.inputs.get_mut("input") {
                        if matches!(value, Value::Object(map) if map.is_empty()) {
                            *value = Value::String("__lcod_state__".to_string());
                        }
                    }
                }
            }
            let steps_arc = Arc::new(steps);
            let id_owned = component_id.to_string();
            let registry_clone = registry.clone();
            let metadata_arc = if metadata.is_empty() {
                None
            } else {
                Some(Arc::new(metadata.clone()))
            };
            registry_clone.register_with_metadata(
                id_owned.clone(),
                move |ctx: &mut Context, input: Value, _meta: Option<Value>| {
                    let seed = match input {
                        Value::Object(map) => Value::Object(map),
                        Value::Null => Value::Object(Map::new()),
                        other => other,
                    };
                    let result = run_compose(ctx, steps_arc.as_ref(), seed)?;
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
                metadata_arc,
            );
            continue;
        }

        if obj.get("manifest").is_some() {
            let _ = log_kernel_warn(
                Some(ctx),
                "Inline component manifest not supported",
                Some(json!({ "componentId": component_id })),
                Some(json!({ "module": "registry-scope", "reason": "manifest" })),
            );
            continue;
        }

        let _ = log_kernel_warn(
            Some(ctx),
            "Inline component missing a supported definition",
            Some(json!({ "componentId": component_id })),
            Some(json!({ "module": "registry-scope", "reason": "unsupported" })),
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
