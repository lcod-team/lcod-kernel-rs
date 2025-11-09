use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;

use lcod_kernel_rs::compose::{parse_compose, run_compose};
use lcod_kernel_rs::compose_contracts::register_compose_contracts;
use lcod_kernel_rs::flow::register_flow;
use lcod_kernel_rs::{CancelledError, Context as KernelContext, Registry};

#[test]
fn run_compose_aborts_when_flag_pre_set() {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);
    registry.register(
        "lcod://test/noop@1",
        |_ctx: &mut KernelContext, _input, _meta| Ok(json!(null)),
    );

    let steps = parse_compose(&json!([
        { "call": "lcod://test/noop@1" }
    ]))
    .expect("compose should parse");

    let token = Arc::new(AtomicBool::new(true));
    let mut ctx = registry.context_with_cancellation(token);

    let err = run_compose(&mut ctx, &steps, json!({})).expect_err("execution should be cancelled");
    assert!(err.is::<CancelledError>());
}

#[test]
fn flow_check_abort_stops_execution_when_cancelled() -> Result<()> {
    let registry = Registry::new();
    register_flow(&registry);
    register_compose_contracts(&registry);

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    registry.register(
        "lcod://test/should_not_run@1",
        move |_ctx: &mut KernelContext, _input, _meta| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!(null))
        },
    );

    let steps = parse_compose(&json!([
        { "call": "lcod://flow/check_abort@1" },
        { "call": "lcod://test/should_not_run@1" }
    ]))?;

    let token = Arc::new(AtomicBool::new(true));
    let mut ctx = registry.context_with_cancellation(token);
    let err = run_compose(&mut ctx, &steps, json!({}))
        .expect_err("execution should be cancelled immediately");
    assert!(err.is::<CancelledError>());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "subsequent steps must not run"
    );
    Ok(())
}

#[test]
fn cancellation_during_execution_prevents_follow_up_steps() -> Result<()> {
    let registry = Registry::new();
    register_flow(&registry);

    registry.register(
        "lcod://test/cancel@1",
        |ctx: &mut KernelContext, _input, _meta| {
            ctx.cancel();
            Ok(json!(null))
        },
    );

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    registry.register(
        "lcod://test/should_not_run@1",
        move |_ctx: &mut KernelContext, _input, _meta| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!(null))
        },
    );

    let steps = parse_compose(&json!([
        { "call": "lcod://test/cancel@1" },
        { "call": "lcod://test/should_not_run@1" }
    ]))?;

    let mut ctx = registry.context();
    let err = run_compose(&mut ctx, &steps, json!({}))
        .expect_err("execution should be cancelled mid-run");
    assert!(err.is::<CancelledError>());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "follow-up step must not execute"
    );
    Ok(())
}
