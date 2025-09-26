use anyhow::Result;
use lcod_kernel_rs::registry::{Context, Registry, SlotExecutor};
use lcod_kernel_rs::tooling::register_tooling;
use serde_json::{json, Value};

struct DummySlot;

impl SlotExecutor for DummySlot {
    fn run_slot(
        &mut self,
        _ctx: &mut Context,
        name: &str,
        local_state: Value,
        _slot_vars: Value,
    ) -> Result<Value> {
        Ok(json!({
            "slot": name,
            "state": local_state,
        }))
    }
}

#[test]
fn script_call_invokes_contracts() {
    let registry = Registry::new();
    register_tooling(&registry);

    registry.register(
        "lcod://impl/add@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let lhs = input.get("lhs").and_then(Value::as_i64).unwrap_or(0);
            let rhs = input.get("rhs").and_then(Value::as_i64).unwrap_or(0);
            Ok(json!({ "sum": lhs + rhs }))
        },
    );

    let mut ctx = registry.context();

    let request = json!({
        "source": "async ({ input }, api) => { api.log('received', input); const lhs = input?.input?.a ?? 0; const res = await api.call('lcod://impl/add@1', { lhs, rhs: 7 }); return { success: true, sum: res.sum }; }",
        "input": {
            "value": {
                "a": 5
            }
        },
        "bindings": {
            "input": {
                "path": "$.value"
            }
        }
    });

    let result = ctx
        .call("lcod://tooling/script@1", request, None)
        .expect("script execution");

    assert_eq!(result.get("success"), Some(&Value::Bool(true)));
    assert_eq!(
        result.get("sum"),
        Some(&Value::Number(serde_json::Number::from(12)))
    );
}

#[test]
fn script_run_slot_and_logs() {
    let registry = Registry::new();
    register_tooling(&registry);

    let mut ctx = registry.context();
    ctx.replace_run_slot_handler(Some(Box::new(DummySlot)));

    let request = json!({
        "source": "async (_scope, api) => { api.log('about to run slot'); const res = await api.runSlot('child', { value: 42 }); return { success: true, child: res }; }"
    });

    let result = ctx
        .call("lcod://tooling/script@1", request, None)
        .expect("script execution");

    assert_eq!(result.get("success"), Some(&Value::Bool(true)));
    assert_eq!(
        result
            .get("child")
            .and_then(|v| v.get("slot"))
            .and_then(Value::as_str),
        Some("child")
    );
    let messages = result
        .get("messages")
        .and_then(Value::as_array)
        .expect("script logs");
    assert!(messages.iter().any(|entry| entry == "about to run slot"));
}
