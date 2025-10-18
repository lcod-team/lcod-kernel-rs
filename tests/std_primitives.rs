use anyhow::Result;
use lcod_kernel_rs::core::register_core;
use lcod_kernel_rs::{Context, Registry};
use serde_json::json;

fn context() -> Context {
    let registry = Registry::new();
    register_core(&registry);
    registry.context()
}

#[test]
fn std_primitives_match_spec() -> Result<()> {
    let mut ctx = context();
    let left = json!({ "a": 1, "nested": { "flag": true }, "arr": [1, 2] });
    let right =
        json!({ "b": 2, "nested": { "flag": false, "extra": "x" }, "arr": [3], "label": "y" });

    let shallow = ctx.call(
        "lcod://contract/core/object/merge@1",
        json!({ "left": left, "right": right }),
        None,
    )?;
    assert_eq!(
        shallow["value"],
        json!({
            "a": 1,
            "nested": { "flag": false, "extra": "x" },
            "arr": [3],
            "b": 2,
            "label": "y"
        })
    );
    assert_eq!(shallow["conflicts"], json!(["arr", "b", "label", "nested"]));

    let deep = ctx.call(
        "lcod://contract/core/object/merge@1",
        json!({
            "left": { "a": 1, "nested": { "flag": true }, "arr": [1, 2] },
            "right": right,
            "deep": true,
            "arrayStrategy": "concat"
        }),
        None,
    )?;
    assert_eq!(
        deep["value"],
        json!({
            "a": 1,
            "nested": { "flag": false, "extra": "x" },
            "arr": [1, 2, 3],
            "b": 2,
            "label": "y"
        })
    );
    assert_eq!(deep["conflicts"], json!(["arr", "b", "label", "nested"]));

    let append = ctx.call(
        "lcod://contract/core/array/append@1",
        json!({ "array": ["alpha", "beta"], "item": "gamma" }),
        None,
    )?;
    assert_eq!(append["value"], json!(["alpha", "beta", "gamma"]));
    assert_eq!(append["length"], json!(3));

    let formatted = ctx.call(
        "lcod://contract/core/string/format@1",
        json!({
            "template": "Hello {user.name}, you have {stats.count} messages",
            "values": { "user": { "name": "Ada" }, "stats": { "count": 3 } }
        }),
        None,
    )?;
    assert_eq!(formatted["value"], json!("Hello Ada, you have 3 messages"));

    let encoded = ctx.call(
        "lcod://contract/core/json/encode@1",
        json!({
            "value": {
                "greeting": "Hello Ada, you have 3 messages",
                "merged": deep["value"].clone()
            },
            "sortKeys": true
        }),
        None,
    )?;
    let text = encoded["text"].as_str().unwrap();
    assert!(text.contains("greeting"));

    let decoded = ctx.call(
        "lcod://contract/core/json/decode@1",
        json!({ "text": text }),
        None,
    )?;
    assert_eq!(
        decoded["value"],
        json!({
            "greeting": "Hello Ada, you have 3 messages",
            "merged": deep["value"].clone()
        })
    );

    Ok(())
}
