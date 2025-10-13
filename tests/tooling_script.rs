use anyhow::Result;
use lcod_kernel_rs::registry::{Context, Registry, SlotExecutor};
use lcod_kernel_rs::tooling::register_tooling;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

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

#[test]
fn script_run_tools_and_config() {
    let registry = Registry::new();
    register_tooling(&registry);
    registry.register(
        "lcod://impl/double@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let value = input.get("value").and_then(Value::as_i64).unwrap_or(0);
            Ok(json!({ "value": value * 2 }))
        },
    );

    let mut ctx = registry.context();

    let request = json!({
        "source": "async ({ input }, api) => { const doubled = await api.run('double', { value: input.value }); const guarded = await api.run('guard', doubled); const entire = api.config(); return { success: guarded.success && entire.feature.enabled, result: guarded.value, config: entire }; }",
        "bindings": {
            "value": { "value": 4 }
        },
        "config": {
            "feature": { "enabled": true },
            "thresholds": { "min": 3 }
        },
        "tools": [
            {
                "name": "double",
                "source": "async ({ value }, api) => { const res = await api.call('lcod://impl/double@1', { value }); api.log('tool.double', res.value); return { success: true, value: res.value }; }"
            },
            {
                "name": "guard",
                "source": "({ value }, api) => { const min = api.config('thresholds.min', 0); if (value < min) { return { success: false }; } return { success: true, value }; }"
            }
        ]
    });

    let result = ctx
        .call("lcod://tooling/script@1", request, None)
        .expect("script execution");

    assert_eq!(result.get("success"), Some(&Value::Bool(true)));
    assert_eq!(
        result.get("result"),
        Some(&Value::Number(serde_json::Number::from(8)))
    );
    let config = result
        .get("config")
        .and_then(Value::as_object)
        .expect("config object");
    assert_eq!(
        config
            .get("feature")
            .and_then(|v| v.get("enabled"))
            .and_then(Value::as_bool),
        Some(true)
    );
    let messages = result
        .get("messages")
        .and_then(Value::as_array)
        .expect("tool log messages");
    assert!(messages
        .iter()
        .any(|entry| entry.as_str().unwrap_or("").contains("tool.double")));
}

#[test]
fn script_imports_aliases() {
    let registry = Registry::new();
    register_tooling(&registry);
    registry.register(
        "lcod://impl/echo@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            Ok(json!({ "val": input.get("value").cloned().unwrap_or(Value::Null) }))
        },
    );

    let mut ctx = registry.context();
    let request = json!({
        "source": "async ({ imports, input }, api) => { const first = await imports.echo({ value: input.value }); const second = await api.call('lcod://impl/echo@1', { value: first.val * 2 }); return { result: second.val }; }",
        "input": { "value": 9 },
        "bindings": {
            "value": { "path": "$.value" }
        },
        "imports": {
            "echo": "lcod://impl/echo@1"
        }
    });

    let result = ctx
        .call("lcod://tooling/script@1", request, None)
        .expect("script execution");

    assert_eq!(
        result.get("result"),
        Some(&Value::Number(serde_json::Number::from(18)))
    );
}

#[test]
fn script_console_routes_to_logging_contract() {
    let registry = Registry::new();
    register_tooling(&registry);

    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture_clone = Arc::clone(&captured);
    registry.register(
        "lcod://impl/testing/log-capture@1",
        move |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            capture_clone
                .lock()
                .expect("capture mutex")
                .push(input.clone());
            Ok(input)
        },
    );
    registry.set_binding(
        "lcod://contract/tooling/log@1",
        "lcod://impl/testing/log-capture@1",
    );

    let mut ctx = registry.context();
    let request = json!({
        "source": "async () => { console.log('from script'); console.error('oops', { code: 500 }); return { ok: true }; }"
    });

    let result = ctx
        .call("lcod://tooling/script@1", request, None)
        .expect("script execution");

    assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
    let messages = result
        .get("messages")
        .and_then(Value::as_array)
        .expect("script messages");
    assert!(messages.iter().any(|entry| entry == "from script"));

    let captured = captured.lock().expect("capture guard");
    assert_eq!(captured.len(), 2);
    let first = captured[0].as_object().expect("first log payload");
    assert_eq!(first.get("level"), Some(&Value::String("info".into())));
    assert_eq!(
        first.get("message"),
        Some(&Value::String("from script".into()))
    );
    let second = captured[1].as_object().expect("second log payload");
    assert_eq!(second.get("level"), Some(&Value::String("error".into())));
    assert!(second
        .get("message")
        .and_then(Value::as_str)
        .map(|msg| msg.contains("oops"))
        .unwrap_or(false));
}
