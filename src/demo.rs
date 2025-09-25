use anyhow::anyhow;
use serde_json::{json, Value};

use crate::registry::{Context, Registry};

pub fn register_demo(registry: &Registry) {
    registry.register(
        "lcod://core/localisation@1",
        |_ctx: &mut Context, _input: Value, _meta: Option<Value>| {
            Ok(json!({ "gps": { "lat": 48.8566, "lon": 2.3522 } }))
        },
    );

    registry.register(
        "lcod://core/extract_city@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let gps = input.get("gps").cloned().unwrap_or(Value::Null);
            if gps.is_null() {
                return Err(anyhow!("missing gps"));
            }
            Ok(json!({ "city": "Paris" }))
        },
    );

    registry.register(
        "lcod://core/weather@1",
        |_ctx: &mut Context, input: Value, _meta: Option<Value>| {
            let city = input.get("city").and_then(Value::as_str).unwrap_or("");
            if city.is_empty() {
                return Err(anyhow!("missing city"));
            }
            Ok(json!({ "tempC": 21 }))
        },
    );
}
