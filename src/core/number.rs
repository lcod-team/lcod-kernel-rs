use anyhow::{anyhow, Result};
use serde_json::{json, Number, Value};

use crate::registry::{Context, Registry};

const CONTRACT_TRUNC: &str = "lcod://contract/core/number/trunc@1";

pub fn register_number(registry: &Registry) {
    registry.register(CONTRACT_TRUNC, trunc_contract);
}

fn trunc_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let raw_value = input
        .get("value")
        .ok_or_else(|| anyhow!("`value` is required"))?
        .as_f64()
        .ok_or_else(|| anyhow!("`value` must be a finite number"))?;
    if !raw_value.is_finite() {
        return Err(anyhow!("`value` must be finite"));
    }
    let truncated = raw_value.trunc();
    let number = if truncated.fract() == 0.0 && truncated >= (i64::MIN as f64) && truncated <= (i64::MAX as f64) {
        Number::from(truncated as i64)
    } else {
        Number::from_f64(truncated).ok_or_else(|| anyhow!("invalid truncated value"))?
    };
    Ok(json!({ "value": Value::Number(number) }))
}
