use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::env;
use std::fs;
use std::path::PathBuf;

use lcod_kernel_rs::compose::{parse_compose, Step, StepChildren};
use lcod_kernel_rs::{register_demo_impls, register_flow, run_compose, Context, Registry};

fn create_registry() -> Registry {
    let registry = Registry::new();

    register_flow(&registry);
    register_demo_impls(&registry);

    register_stream_contracts(&registry);

    registry
}

fn register_stream_contracts(registry: &Registry) {
    registry.register(
        "lcod://contract/core/stream/read@1",
        |ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let stream = input
                .get("stream")
                .ok_or_else(|| anyhow!("stream handle required"))?;
            let max_bytes = input
                .get("maxBytes")
                .and_then(Value::as_u64)
                .map(|v| v as usize);
            let decode = input.get("decode").and_then(Value::as_str);
            ctx.streams_mut().read(stream, max_bytes, decode)
        },
    );

    registry.register(
        "lcod://contract/core/stream/close@1",
        |ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let stream = input
                .get("stream")
                .ok_or_else(|| anyhow!("stream handle required"))?;
            ctx.streams_mut().close(stream)
        },
    );
}

fn chunk_stream_handle(ctx: &mut Context) -> Value {
    let chunks = vec![b"12".to_vec(), b"34".to_vec(), b"56".to_vec()];
    ctx.streams_mut().register_chunks(chunks, "utf-8")
}

fn attempts_budget(count: usize) -> Value {
    json!((0..count).collect::<Vec<usize>>())
}

fn spec_dir() -> Result<PathBuf> {
    if let Ok(env_path) = env::var("LCOD_SPEC_PATH") {
        let candidate = PathBuf::from(env_path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let candidates = [
        PathBuf::from("lcod-spec"),
        PathBuf::from("../lcod-spec"),
        PathBuf::from("../../lcod-spec"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("Unable to locate lcod-spec repository"))
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
    body_out.insert("val".to_string(), Value::String("val".to_string()));
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
    out.insert("isEven".to_string(), Value::String("ok".to_string()));
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
    gt_out.insert("tooBig".to_string(), Value::String("ok".to_string()));
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
    echo_out.insert("val".to_string(), Value::String("val".to_string()));
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

#[test]
fn foreach_executes_else_slot() -> Result<()> {
    let registry = create_registry();
    let mut ctx = registry.context();

    let mut body_inputs = Map::new();
    body_inputs.insert("value".to_string(), Value::String("$slot.item".to_string()));
    let mut body_out = Map::new();
    body_out.insert("val".to_string(), Value::String("val".to_string()));
    let body_step = simple_step("lcod://impl/echo@1", body_inputs, body_out);

    let mut else_inputs = Map::new();
    else_inputs.insert("value".to_string(), Value::String("empty".to_string()));
    let mut else_out = Map::new();
    else_out.insert("val".to_string(), Value::String("val".to_string()));
    let else_step = simple_step("lcod://impl/echo@1", else_inputs, else_out);

    let mut children_map = std::collections::HashMap::new();
    children_map.insert("body".to_string(), vec![body_step]);
    children_map.insert("else".to_string(), vec![else_step]);

    let mut inputs = Map::new();
    inputs.insert("list".to_string(), Value::String("$.numbers".to_string()));

    let mut out_map = Map::new();
    out_map.insert("results".to_string(), Value::String("results".to_string()));

    let step = Step {
        call: "lcod://flow/foreach@1".to_string(),
        inputs,
        out: out_map,
        collect_path: Some("$.val".to_string()),
        children: Some(StepChildren::Map(children_map)),
    };

    let initial_state = json!({ "numbers": [] });
    let result = run_compose(&mut ctx, &[step], initial_state)?;
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap();
    assert_eq!(results, json!(["empty"]).as_array().unwrap().clone());
    Ok(())
}

#[test]
fn foreach_consumes_stream_input() -> Result<()> {
    let registry = create_registry();
    let mut ctx = registry.context();

    let steps = parse_compose(&json!([
        {
            "call": "lcod://flow/foreach@1",
            "in": { "list": "$.attempts" },
            "children": {
                "body": [
                    {
                        "call": "lcod://contract/core/stream/read@1",
                        "in": {
                            "stream": "$.numbers.stream",
                            "decode": "utf-8",
                            "maxBytes": 2
                        },
                        "out": { "chunk": "$" }
                    },
                    {
                        "call": "lcod://flow/if@1",
                        "in": { "cond": "$.chunk.done" },
                        "children": {
                            "then": [
                                {
                                    "call": "lcod://contract/core/stream/close@1",
                                    "in": { "stream": "$.numbers.stream" }
                                },
                                { "call": "lcod://flow/break@1" }
                            ],
                            "else": [
                                {
                                    "call": "lcod://impl/echo@1",
                                    "in": { "value": "$.chunk.chunk" },
                                    "out": { "val": "val" }
                                }
                            ]
                        },
                        "out": { "val": "val" }
                    }
                ]
            },
            "collectPath": "$.val",
            "out": { "results": "results" }
        }
    ]))?;

    let handle = chunk_stream_handle(&mut ctx);
    let mut numbers_map = Map::new();
    numbers_map.insert("stream".to_string(), handle.clone());
    let mut state_map = Map::new();
    state_map.insert("numbers".to_string(), Value::Object(numbers_map));
    state_map.insert("attempts".to_string(), attempts_budget(10));
    let initial_state = Value::Object(state_map);

    let result = run_compose(&mut ctx, &steps, initial_state)?;
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap();
    assert_eq!(
        results,
        json!(["12", "34", "56"]).as_array().unwrap().clone()
    );
    assert!(!ctx.streams().contains_handle(&handle));
    assert!(ctx
        .streams_mut()
        .read(&handle, Some(1), Some("utf-8"))
        .is_err());
    Ok(())
}

#[test]
fn foreach_ctrl_demo_from_spec_yaml() -> Result<()> {
    let registry = create_registry();
    let mut ctx = registry.context();

    let spec_path = spec_dir()?.join("examples/flow/foreach_ctrl_demo/compose.yaml");
    let yaml_text = fs::read_to_string(spec_path)?;
    let doc: serde_json::Value = serde_yaml::from_str(&yaml_text)?;
    let compose_value = doc
        .get("compose")
        .cloned()
        .ok_or_else(|| anyhow!("compose root missing"))?;
    let steps = parse_compose(&compose_value)?;

    let initial_state = json!({ "numbers": [1, 2, 3, 8, 9] });
    let result = run_compose(&mut ctx, &steps, initial_state)?;
    let numbers = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("results missing"))?;

    assert_eq!(numbers, json!([1, 3]).as_array().unwrap().clone());
    Ok(())
}

#[test]
fn foreach_stream_demo_from_spec_yaml() -> Result<()> {
    let registry = create_registry();
    let mut ctx = registry.context();

    let spec_path = spec_dir()?.join("examples/flow/foreach_stream_demo/compose.yaml");
    let yaml_text = fs::read_to_string(spec_path)?;
    let doc: serde_json::Value = serde_yaml::from_str(&yaml_text)?;
    let compose_value = doc
        .get("compose")
        .cloned()
        .ok_or_else(|| anyhow!("compose root missing"))?;
    let steps = parse_compose(&compose_value)?;

    let handle = chunk_stream_handle(&mut ctx);
    let mut numbers_map = Map::new();
    numbers_map.insert("stream".to_string(), handle.clone());
    let mut state_map = Map::new();
    state_map.insert("numbers".to_string(), Value::Object(numbers_map));
    state_map.insert("attempts".to_string(), attempts_budget(10));
    let initial_state = Value::Object(state_map);

    let result = run_compose(&mut ctx, &steps, initial_state)?;
    let numbers = result
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("results missing"))?;

    assert_eq!(
        numbers,
        json!(["12", "34", "56"]).as_array().unwrap().clone()
    );
    assert!(!ctx.streams().contains_handle(&handle));
    Ok(())
}
