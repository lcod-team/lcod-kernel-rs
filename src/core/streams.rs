use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::registry::{Context, Registry};

const CONTRACT_READ: &str = "lcod://contract/core/stream/read@1";
const CONTRACT_CLOSE: &str = "lcod://contract/core/stream/close@1";

pub fn register_streams(registry: &Registry) {
    registry.register(CONTRACT_READ, stream_read_contract);
    registry.register(CONTRACT_CLOSE, stream_close_contract);
}

fn stream_read_contract(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let stream = input
        .get("stream")
        .ok_or_else(|| anyhow!("stream handle required"))?;
    let max_bytes = input
        .get("maxBytes")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let decode = input.get("decode").and_then(Value::as_str);

    ctx.streams_mut().read(stream, max_bytes, decode)
}

fn stream_close_contract(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let stream = input
        .get("stream")
        .ok_or_else(|| anyhow!("stream handle required"))?;
    ctx.streams_mut().close(stream)
}
