use anyhow::Result;
use lcod_kernel_rs::compose::{parse_compose, run_compose};
use lcod_kernel_rs::registry::{Context, Registry};
use lcod_kernel_rs::tooling::register_tooling;
use serde_json::json;

fn setup_registry() -> Registry {
    let registry = Registry::new();
    register_tooling(&registry);

    registry.register(
        "lcod://impl/demo/base@1",
        |_: &mut Context, _input, _meta| Ok(json!({ "result": "base" })),
    );
    registry.register(
        "lcod://impl/demo/scoped@1",
        |_: &mut Context, _input, _meta| Ok(json!({ "result": "scoped" })),
    );
    registry.register(
        "lcod://impl/demo/error@1",
        |_: &mut Context, _input, _meta| Err(anyhow::anyhow!("boom")),
    );
    registry.set_binding("lcod://contract/demo/value@1", "lcod://impl/demo/base@1");

    registry
}

#[test]
fn registry_scope_applies_temporary_bindings_and_restores() -> Result<()> {
    let registry = setup_registry();
    let mut ctx = registry.context();

    let compose = json!({
        "compose": [
            {
                "call": "lcod://tooling/registry/scope@1",
                "in": {
                    "bindings": {
                        "lcod://contract/demo/value@1": "lcod://impl/demo/scoped@1"
                    }
                },
                "children": [
                    {
                        "call": "lcod://contract/demo/value@1",
                        "out": { "scopedValue": "result" }
                    }
                ],
                "out": {
                    "scopedResult": "scopedValue"
                }
            },
            {
                "call": "lcod://contract/demo/value@1",
                "out": { "globalResult": "result" }
            }
        ]
    });

    let steps = parse_compose(compose.get("compose").unwrap())?;
    let result = run_compose(&mut ctx, &steps, serde_json::Value::Null)?;
    let obj = result.as_object().unwrap();
    assert_eq!(obj.get("scopedResult").unwrap().as_str().unwrap(), "scoped");
    assert_eq!(obj.get("globalResult").unwrap().as_str().unwrap(), "base");

    Ok(())
}

#[test]
fn registry_scope_restores_bindings_on_error() -> Result<()> {
    let registry = setup_registry();
    let mut ctx = registry.context();

    let failing_compose = json!({
        "compose": [
            {
                "call": "lcod://tooling/registry/scope@1",
                "in": {
                    "bindings": {
                        "lcod://contract/demo/value@1": "lcod://impl/demo/error@1"
                    }
                },
                "children": [
                    {
                        "call": "lcod://contract/demo/value@1"
                    }
                ]
            }
        ]
    });

    let failing_steps = parse_compose(failing_compose.get("compose").unwrap())?;
    assert!(run_compose(&mut ctx, &failing_steps, serde_json::Value::Null).is_err());

    let verify_compose = json!({
        "compose": [
            {
                "call": "lcod://contract/demo/value@1",
                "out": { "value": "result" }
            }
        ]
    });
    let verify_steps = parse_compose(verify_compose.get("compose").unwrap())?;
    let verify_result = run_compose(&mut ctx, &verify_steps, serde_json::Value::Null)?;
    let verify_obj = verify_result.as_object().unwrap();
    assert_eq!(verify_obj.get("value").unwrap().as_str().unwrap(), "base");

    Ok(())
}
