use anyhow::Result;
use lcod_kernel_rs::compose::Step;
use lcod_kernel_rs::demo::register_demo;
use lcod_kernel_rs::{run_compose, Registry};
use serde_json::{json, Map, Value};

#[test]
fn run_demo_compose() -> Result<()> {
    let mut registry = Registry::new();
    register_demo(&mut registry);
    let steps = {
        let mut steps = Vec::new();

        let mut step1_out = Map::new();
        step1_out.insert("gps".to_string(), Value::String("gps".to_string()));
        steps.push(Step {
            call: "lcod://core/localisation@1".to_string(),
            inputs: Map::new(),
            out: step1_out,
        });

        let mut step2_in = Map::new();
        step2_in.insert("gps".to_string(), json!("$.gps"));
        let mut step2_out = Map::new();
        step2_out.insert("city".to_string(), Value::String("city".to_string()));
        steps.push(Step {
            call: "lcod://core/extract_city@1".to_string(),
            inputs: step2_in,
            out: step2_out,
        });

        let mut step3_in = Map::new();
        step3_in.insert("city".to_string(), json!("$.city"));
        let mut step3_out = Map::new();
        step3_out.insert("tempC".to_string(), Value::String("tempC".to_string()));
        steps.push(Step {
            call: "lcod://core/weather@1".to_string(),
            inputs: step3_in,
            out: step3_out,
        });

        steps
    };

    let result = run_compose(&registry, &steps, Value::Object(Map::new()))?;
    let temp = result.get("tempC").and_then(Value::as_i64).unwrap();
    assert_eq!(temp, 21);
    Ok(())
}
