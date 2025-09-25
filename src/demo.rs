use anyhow::anyhow;
use serde_json::{json, Value};

use crate::registry::Registry;

pub fn register_demo(registry: &mut Registry) {
    registry.register("lcod://core/localisation@1", |_input: Value| {
        Ok(json!({ "gps": { "lat": 48.8566, "lon": 2.3522 } }))
    });

    registry.register("lcod://core/extract_city@1", |input: Value| {
        let gps = input.get("gps").cloned().unwrap_or(Value::Null);
        if gps.is_null() {
            return Err(anyhow!("missing gps"));
        }
        Ok(json!({ "city": "Paris" }))
    });

    registry.register("lcod://core/weather@1", |input: Value| {
        let city = input.get("city").and_then(Value::as_str).unwrap_or("");
        if city.is_empty() {
            return Err(anyhow!("missing city"));
        }
        Ok(json!({ "tempC": 21 }))
    });
}
