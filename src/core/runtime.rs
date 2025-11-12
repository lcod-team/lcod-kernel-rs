use std::env;

use anyhow::Result;
use dirs::home_dir;
use serde_json::{json, Value};

use crate::core::path::path_to_string;
use crate::registry::{Context, Registry};

const CONTRACT_RUNTIME_INFO: &str = "lcod://contract/core/runtime/info@1";

pub fn register_runtime(registry: &Registry) {
    registry.register(CONTRACT_RUNTIME_INFO, runtime_info_contract);
}

fn runtime_info_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let include_platform = input
        .get("includePlatform")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_pid = input
        .get("includePid")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let cwd = env::current_dir()?;
    let tmp_dir = env::temp_dir();
    let home = home_dir();

    let mut result = json!({
        "cwd": path_to_string(&cwd),
        "tmpDir": path_to_string(&tmp_dir),
        "homeDir": home.as_ref().map(|p| path_to_string(p))
    });

    if include_platform {
        result["platform"] = Value::String(env::consts::OS.to_string());
    }

    if include_pid {
        result["pid"] = Value::Number((std::process::id() as u64).into());
    }

    Ok(result)
}
