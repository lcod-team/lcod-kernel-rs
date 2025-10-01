use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::registry::{Context, Registry};

const CONTRACT_GIT_CLONE: &str = "lcod://contract/core/git/clone@1";

pub fn register_git(registry: &Registry) {
    registry.register(CONTRACT_GIT_CLONE, git_clone_contract);
}

fn git_clone_contract(_ctx: &mut Context, _input: Value, _meta: Option<Value>) -> Result<Value> {
    Err(anyhow!(
        "Git clone support is not yet implemented in the Rust substrate blueprint"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_clone_is_not_implemented_yet() {
        let registry = Registry::new();
        register_git(&registry);
        let mut ctx = registry.context();
        let err = git_clone_contract(&mut ctx, Value::Null, None).unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}
