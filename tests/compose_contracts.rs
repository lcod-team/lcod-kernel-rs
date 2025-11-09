use anyhow::Result;
use serde_json::{json, Value};

use lcod_kernel_rs::compose::{parse_compose, run_compose};
use lcod_kernel_rs::compose_contracts::register_compose_contracts;
use lcod_kernel_rs::flow::register_flow;
use lcod_kernel_rs::tooling::register_tooling;
use lcod_kernel_rs::{Context as KernelContext, Registry};

fn registry_with_defaults() -> Registry {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);
    register_tooling(&registry);
    registry.register("lcod://impl/set@1", impl_set_passthrough);
    registry
}

fn impl_set_passthrough(
    _ctx: &mut KernelContext,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    Ok(input)
}

#[test]
fn run_slot_executes_inline_slot() -> Result<()> {
    let registry = registry_with_defaults();
    let mut ctx: KernelContext = registry.context();
    let steps = parse_compose(&json!([
        {
            "call": "lcod://contract/compose/run_slot@1",
            "in": { "slot": "target" },
            "out": {
                "slotResult": "result",
                "slotRan": "ran"
            },
            "slots": {
                "target": [
                    {
                        "call": "lcod://impl/set@1",
                        "in": {
                            "result": {
                                "value": 42,
                                "warnings": [],
                                "error": null
                            }
                        },
                        "out": { "result": "result" }
                    }
                ]
            }
        }
    ]))?;

    let output = run_compose(&mut ctx, &steps, json!({}))?;
    println!("state after run_slot_executes_inline_slot: {:?}", output);
    assert_eq!(
        output
            .get("slotResult")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.get("value")),
        Some(&json!(42))
    );
    Ok(())
}

#[test]
fn run_slot_optional_skips_missing_slot() -> Result<()> {
    let registry = registry_with_defaults();
    let mut ctx = registry.context();
    let steps = parse_compose(&json!([
        {
            "call": "lcod://contract/compose/run_slot@1",
            "in": { "slot": "missing", "optional": true },
            "out": {
                "slotResult": "result",
                "slotRan": "ran"
            }
        }
    ]))?;

    let output = run_compose(&mut ctx, &steps, json!({}))?;
    assert_eq!(output.get("slotRan"), Some(&json!(false)));
    Ok(())
}
