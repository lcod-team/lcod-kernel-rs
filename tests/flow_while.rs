use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use lcod_kernel_rs::compose_contracts::register_compose_contracts;
use lcod_kernel_rs::flow::{flow_while, register_flow};
use lcod_kernel_rs::registry::{Context as KernelContext, Registry, SlotExecutor};
use lcod_kernel_rs::CancelledError;

struct TestWhileSlot {
    threshold: i64,
}

impl SlotExecutor for TestWhileSlot {
    fn run_slot(
        &mut self,
        _ctx: &mut KernelContext,
        name: &str,
        local_state: Value,
        _slot_vars: Value,
    ) -> Result<Value> {
        let state = local_state.as_object().cloned().unwrap_or_else(Map::new);
        let count = state
            .get("count")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        match name {
            "condition" => Ok(json!({ "continue": count < self.threshold })),
            "body" => Ok(json!({ "count": count + 1 })),
            "else" => Ok(Value::Null),
            other => Err(anyhow!("unexpected slot requested: {other}")),
        }
    }
}

struct CancellingSlot {
    threshold: i64,
    cancel_at: i64,
}

impl SlotExecutor for CancellingSlot {
    fn run_slot(
        &mut self,
        ctx: &mut KernelContext,
        name: &str,
        local_state: Value,
        _slot_vars: Value,
    ) -> Result<Value> {
        let state = local_state.as_object().cloned().unwrap_or_else(Map::new);
        let count = state
            .get("count")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        match name {
            "condition" => Ok(json!({ "continue": count < self.threshold })),
            "body" => {
                let next = count + 1;
                if next >= self.cancel_at {
                    ctx.cancel();
                }
                Ok(json!({ "count": next }))
            }
            "else" => Ok(Value::Null),
            other => Err(anyhow!("unexpected slot requested: {other}")),
        }
    }
}

struct ElseSlot;

impl SlotExecutor for ElseSlot {
    fn run_slot(
        &mut self,
        _ctx: &mut KernelContext,
        name: &str,
        _local_state: Value,
        _slot_vars: Value,
    ) -> Result<Value> {
        match name {
            "condition" => Ok(json!({ "continue": false })),
            "body" => Ok(Value::Null),
            "else" => Ok(json!({ "count": 42 })),
            other => Err(anyhow!("unexpected slot requested: {other}")),
        }
    }
}

#[test]
fn flow_while_runs_until_condition_false() -> Result<()> {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);

    let mut ctx = registry.context();
    ctx.replace_run_slot_handler(Some(Box::new(TestWhileSlot { threshold: 3 })));

    let input = json!({
        "state": { "count": 0 },
        "maxIterations": 10
    });
    let result = flow_while(&mut ctx, input, None)?;
    let result_map = result
        .as_object()
        .expect("flow/while should return an object");
    assert_eq!(
        result_map
            .get("iterations")
            .and_then(Value::as_i64)
            .expect("iterations must be an integer"),
        3
    );
    let state = result_map
        .get("state")
        .and_then(Value::as_object)
        .expect("state should be an object");
    assert_eq!(
        state
            .get("count")
            .and_then(Value::as_i64)
            .expect("count must be an integer"),
        3
    );
    Ok(())
}

#[test]
fn flow_while_honours_max_iterations() {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);

    let mut ctx = registry.context();
    ctx.replace_run_slot_handler(Some(Box::new(TestWhileSlot { threshold: 10 })));

    let input = json!({
        "state": { "count": 0 },
        "maxIterations": 2
    });
    let err = flow_while(&mut ctx, input, None).expect_err("should hit maxIterations");
    assert!(
        err.to_string().contains("maxIterations"),
        "error should mention maxIterations, got: {err}"
    );
}

#[test]
fn flow_while_aborts_when_context_cancelled() {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);

    let mut ctx = registry.context();
    ctx.replace_run_slot_handler(Some(Box::new(CancellingSlot {
        threshold: 10,
        cancel_at: 2,
    })));

    let input = json!({
        "state": { "count": 0 },
        "maxIterations": 10
    });
    let err = flow_while(&mut ctx, input, None).expect_err("cancellation should abort loop");
    assert!(
        err.is::<CancelledError>(),
        "expected CancelledError, got: {err}"
    );
}

#[test]
fn flow_while_invokes_else_when_no_iterations() -> Result<()> {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);

    let mut ctx = registry.context();
    ctx.replace_run_slot_handler(Some(Box::new(ElseSlot)));

    let input = json!({
        "state": { "count": 0 },
        "maxIterations": 5
    });
    let result = flow_while(&mut ctx, input, None)?;
    let result_map = result
        .as_object()
        .expect("flow/while should return an object");
    assert_eq!(
        result_map
            .get("iterations")
            .and_then(Value::as_i64)
            .expect("iterations must be an integer"),
        0
    );
    let state = result_map
        .get("state")
        .and_then(Value::as_object)
        .expect("state should be an object");
    assert_eq!(
        state
            .get("count")
            .and_then(Value::as_i64)
            .expect("count must be an integer"),
        42
    );
    Ok(())
}
