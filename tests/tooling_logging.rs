use anyhow::Result;
use std::sync::{Arc, Mutex};

use lcod_kernel_rs::compose::{parse_compose, run_compose};
use lcod_kernel_rs::registry::{Context, Registry};
use lcod_kernel_rs::tooling::register_tooling;
use serde_json::{json, Map, Value};

const LOG_CONTRACT_ID: &str = "lcod://contract/tooling/log@1";
const KERNEL_HELPER_ID: &str = "lcod://kernel/log@1";

fn setup() -> (Registry, Context) {
    let registry = Registry::new();
    register_tooling(&registry);
    let ctx = registry.context();
    (registry, ctx)
}

#[test]
fn log_binding_reroutes_and_kernel_tags() -> Result<()> {
    let (registry, mut ctx) = setup();
    let captured: Arc<Mutex<Vec<Map<String, Value>>>> = Arc::new(Mutex::new(Vec::new()));
    let capture_clone = captured.clone();

    registry.register(
        "lcod://impl/testing/logger@1",
        move |_ctx: &mut Context, input: Value, _meta| {
            if let Value::Object(map) = input {
                capture_clone.lock().unwrap().push(map);
            }
            Ok(Value::Null)
        },
    );

    registry.set_binding(LOG_CONTRACT_ID, "lcod://impl/testing/logger@1");

    let payload = json!({
        "level": "debug",
        "message": "app log",
        "tags": { "feature": "alpha" }
    });
    registry.call(&mut ctx, LOG_CONTRACT_ID, payload, None)?;

    let kernel_payload = json!({
        "level": "warn",
        "message": "kernel log"
    });
    registry.call(&mut ctx, KERNEL_HELPER_ID, kernel_payload, None)?;

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0].get("message"),
        Some(&Value::String("app log".into()))
    );
    assert_eq!(
        captured[0].get("tags"),
        Some(&json!({ "feature": "alpha" }))
    );
    assert_eq!(
        captured[1].get("message"),
        Some(&Value::String("kernel log".into()))
    );
    let kernel_tags = captured[1].get("tags").and_then(|v| v.get("component"));
    assert_eq!(kernel_tags, Some(&Value::String("kernel".into())));

    Ok(())
}

#[test]
fn log_context_merges_and_restores_tags() -> Result<()> {
    let (registry, mut ctx) = setup();
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let capture_clone = captured.clone();

    registry.register(
        "lcod://impl/testing/logger@1",
        move |_ctx: &mut Context, input: Value, _meta| {
            if let Value::Object(map) = input {
                capture_clone.lock().unwrap().push(
                    map.get("tags")
                        .cloned()
                        .unwrap_or(Value::Object(Map::new())),
                );
            }
            Ok(Value::Null)
        },
    );

    registry.set_binding(LOG_CONTRACT_ID, "lcod://impl/testing/logger@1");

    let compose = json!({
        "compose": [
            {
                "call": "lcod://tooling/log.context@1",
                "in": { "tags": { "requestId": "abc" } },
                "children": [
                    {
                        "call": LOG_CONTRACT_ID,
                        "in": { "level": "info", "message": "first" }
                    },
                    {
                        "call": "lcod://tooling/log.context@1",
                        "in": { "tags": { "userId": "u1" } },
                        "children": [
                            {
                                "call": LOG_CONTRACT_ID,
                                "in": { "level": "info", "message": "nested" }
                            }
                        ]
                    }
                ]
            },
            { "call": LOG_CONTRACT_ID, "in": { "level": "info", "message": "after" } }
        ]
    });

    let steps = parse_compose(compose.get("compose").unwrap())?;
    run_compose(&mut ctx, &steps, Value::Null)?;

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0], json!({ "requestId": "abc" }));
    assert_eq!(captured[1], json!({ "requestId": "abc", "userId": "u1" }));
    assert_eq!(captured[2], Value::Object(Map::new()));

    Ok(())
}

#[test]
fn log_contract_handles_missing_binding() -> Result<()> {
    let (registry, mut ctx) = setup();
    let payload = json!({
        "level": "info",
        "message": "fallback"
    });
    registry.call(&mut ctx, LOG_CONTRACT_ID, payload, None)?;
    Ok(())
}
