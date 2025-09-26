use anyhow::anyhow;
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

pub fn register_demo_impls(registry: &Registry) {
    registry.register(
        "lcod://impl/echo@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            Ok(json!({ "val": input.get("value").cloned().unwrap_or(Value::Null) }))
        },
    );

    registry.register(
        "lcod://impl/is_even@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let value = input
                .get("value")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("missing numeric value"))?;
            Ok(json!({ "ok": value % 2 == 0 }))
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
            Ok(json!({ "ok": value > limit }))
        },
    );

    registry.register(
        "lcod://impl/set@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| Ok(input),
    );

    registry.register(
        "lcod://impl/fail@1",
        |_ctx: &mut Context, _input: Value, _meta: Option<Value>| Err(anyhow!("boom")),
    );

    registry.register(
        "lcod://impl/delay@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let ms = input.get("ms").and_then(Value::as_u64).unwrap_or(0);
            if ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            Ok(json!({ "value": input.get("value").cloned().unwrap_or(Value::Null) }))
        },
    );

    registry.register(
        "lcod://impl/cleanup@1",
        |_ctx: &mut Context, _input: Value, _meta: Option<Value>| Ok(json!({ "cleaned": true })),
    );
}
