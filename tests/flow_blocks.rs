use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use lcod_kernel_rs::compose::{Step, StepChildren};
use lcod_kernel_rs::{register_flow, run_compose, Context, Registry};

fn create_registry() -> Registry {
    let registry = Registry::new();

    register_flow(&registry);

    registry.register(
        "lcod://impl/echo@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            Ok(json!({ "value": input.get("value").cloned().unwrap_or(Value::Null) }))
        },
    );

    registry.register(
        "lcod://impl/is_even@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let value = input
                .get("value")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("missing numeric value"))?;
            Ok(json!({ "isEven": value % 2 == 0 }))
        },
    );

    registry.register(
        "lcod://impl/gt@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let value = input
                .get("value")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("missing numeric value"))?;
            let limit = input
                .get("limit")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("missing limit"))?;
            Ok(json!({ "tooBig": value > limit }))
        },
    );

    registry.register(
        "lcod://impl/set@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            Ok(json!({ "value": input.get("value").cloned().unwrap_or(Value::Null) }))
        },
    );

    registry
}

fn simple_step(call: &str, inputs: Map<String, Value>, out: Map<String, Value>) -> Step {
    Step {
        call: call.to_string(),
        inputs,
        out,
        collect_path: None,
        children: None,
    }
}

#[test]
fn foreach_collects_body_output() -> Result<()> {
    let registry = create_registry();
    let mut ctx = registry.context();

    let mut body_inputs = Map::new();
    body_inputs.insert("value".to_string(), Value::String("$slot.item".to_string()));
    let mut body_out = Map::new();
    body_out.insert("val".to_string(), Value::String("value".to_string()));
    let body_step = simple_step("lcod://impl/echo@1", body_inputs, body_out);

    let mut children_map = std::collections::HashMap::new();
    children_map.insert("body".to_string(), vec![body_step]);

    let mut inputs = Map::new();
    inputs.insert("list".to_string(), json!([1, 2, 3]));

    let mut out_map = Map::new();
    out_map.insert("numbers".to_string(), Value::String("results".to_string()));

    let step = Step {
        call: "lcod://flow/foreach@1".to_string(),
        inputs,
        out: out_map,
        collect_path: Some("$.val".to_string()),
        children: Some(StepChildren::Map(children_map)),
    };

    let result = run_compose(&mut ctx, &[step], Value::Object(Map::new()))?;
    let numbers = result
        .get("numbers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap();
    assert_eq!(numbers, json!([1, 2, 3]).as_array().unwrap().clone());
    Ok(())
}

#[test]
fn foreach_handles_continue_and_break() -> Result<()> {
    let registry = create_registry();
    let mut ctx = registry.context();

    // Steps for body slot
    let mut steps = Vec::new();

    // is_even step
    let mut inputs = Map::new();
    inputs.insert("value".to_string(), Value::String("$slot.item".to_string()));
    let mut out = Map::new();
    out.insert("isEven".to_string(), Value::String("isEven".to_string()));
    steps.push(simple_step("lcod://impl/is_even@1", inputs, out));

    // if is even -> continue
    let mut cond_inputs = Map::new();
    cond_inputs.insert("cond".to_string(), Value::String("$.isEven".to_string()));
    let continue_step = simple_step("lcod://flow/continue@1", Map::new(), Map::new());
    let mut then_map = std::collections::HashMap::new();
    then_map.insert("then".to_string(), vec![continue_step]);
    steps.push(Step {
        call: "lcod://flow/if@1".to_string(),
        inputs: cond_inputs,
        out: Map::new(),
        collect_path: None,
        children: Some(StepChildren::Map(then_map)),
    });

    // greater than limit -> break
    let mut gt_inputs = Map::new();
    gt_inputs.insert("value".to_string(), Value::String("$slot.item".to_string()));
    gt_inputs.insert("limit".to_string(), Value::Number(7.into()));
    let mut gt_out = Map::new();
    gt_out.insert("tooBig".to_string(), Value::String("tooBig".to_string()));
    steps.push(simple_step("lcod://impl/gt@1", gt_inputs, gt_out));

    let mut cond_inputs = Map::new();
    cond_inputs.insert("cond".to_string(), Value::String("$.tooBig".to_string()));
    let break_step = simple_step("lcod://flow/break@1", Map::new(), Map::new());
    let mut then_map = std::collections::HashMap::new();
    then_map.insert("then".to_string(), vec![break_step]);
    steps.push(Step {
        call: "lcod://flow/if@1".to_string(),
        inputs: cond_inputs,
        out: Map::new(),
        collect_path: None,
        children: Some(StepChildren::Map(then_map)),
    });

    let mut echo_inputs = Map::new();
    echo_inputs.insert("value".to_string(), Value::String("$slot.item".to_string()));
    let mut echo_out = Map::new();
    echo_out.insert("val".to_string(), Value::String("value".to_string()));
    steps.push(simple_step("lcod://impl/echo@1", echo_inputs, echo_out));

    let mut children_map = std::collections::HashMap::new();
    children_map.insert("body".to_string(), steps);

    let mut foreach_inputs = Map::new();
    foreach_inputs.insert("list".to_string(), json!([1, 2, 3, 8, 9]));

    let mut foreach_out = Map::new();
    foreach_out.insert("numbers".to_string(), Value::String("results".to_string()));

    let foreach_step = Step {
        call: "lcod://flow/foreach@1".to_string(),
        inputs: foreach_inputs,
        out: foreach_out,
        collect_path: Some("$.val".to_string()),
        children: Some(StepChildren::Map(children_map)),
    };

    let result = run_compose(&mut ctx, &[foreach_step], Value::Object(Map::new()))?;
    let numbers = result
        .get("numbers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap();
    assert_eq!(numbers, json!([1, 3]).as_array().unwrap().clone());
    Ok(())
}
