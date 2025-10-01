use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::registry::{Context, Registry};

const CONTRACT_HTTP_REQUEST: &str = "lcod://contract/core/http/request@1";

pub fn register_http(registry: &Registry) {
    registry.register(CONTRACT_HTTP_REQUEST, http_request_contract);
}

fn http_request_contract(_ctx: &mut Context, _input: Value, _meta: Option<Value>) -> Result<Value> {
    Err(anyhow!(
        "HTTP client support is not yet implemented in the Rust substrate blueprint"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_contract_is_not_implemented_yet() {
        let registry = Registry::new();
        register_http(&registry);
        let mut ctx = registry.context();
        let err = http_request_contract(&mut ctx, Value::Null, None).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}
