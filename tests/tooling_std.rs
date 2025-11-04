use std::collections::HashMap;

use anyhow::Result;
use lcod_kernel_rs::register_flow;
use lcod_kernel_rs::registry::{Context, Registry, SlotExecutor};
use lcod_kernel_rs::tooling::{register_resolver_axioms, register_tooling};
use serde_json::{json, Map, Value};

struct BfsSlot {
    graph: HashMap<String, Vec<String>>,
}

impl SlotExecutor for BfsSlot {
    fn run_slot(
        &mut self,
        _ctx: &mut Context,
        name: &str,
        local_state: Value,
        _slot_vars: Value,
    ) -> Result<Value> {
        let item = local_state.get("item").cloned().unwrap_or(Value::Null);
        match name {
            "key" => {
                let key = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Ok(Value::String(key))
            }
            "process" => {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut state_obj = local_state
                    .get("state")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_else(Map::new);
                let mut order = state_obj
                    .get("order")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                order.push(Value::String(id.clone()));
                state_obj.insert("order".to_string(), Value::Array(order));
                let children = self
                    .graph
                    .get(&id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|child| json!({ "id": child }))
                    .collect::<Vec<_>>();
                Ok(json!({
                    "state": Value::Object(state_obj),
                    "children": children
                }))
            }
            _ => Ok(Value::Null),
        }
    }
}

#[test]
fn object_clone_produces_deep_copy() {
    let registry = Registry::new();
    register_flow(&registry);
    register_resolver_axioms(&registry);
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/object/clone@0.1.0",
            json!({ "value": { "foo": 1, "bar": { "baz": 2 } } }),
            None,
        )
        .expect("object clone");
    let clone = result
        .get("clone")
        .and_then(Value::as_object)
        .expect("clone object");
    assert_eq!(clone.get("foo"), Some(&Value::from(1)));
}

#[test]
fn object_set_assigns_nested_path() {
    let registry = Registry::new();
    register_flow(&registry);
    register_resolver_axioms(&registry);
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/object/set@0.1.0",
            json!({
                "target": { "outer": { "inner": 1 } },
                "path": ["outer", "inner"],
                "value": 42
            }),
            None,
        )
        .expect("object set");
    assert_eq!(
        result
            .get("object")
            .and_then(Value::as_object)
            .and_then(|obj| obj.get("outer"))
            .and_then(Value::as_object)
            .and_then(|obj| obj.get("inner")),
        Some(&Value::from(42))
    );
    assert_eq!(
        result
            .get("previous")
            .and_then(Value::as_object)
            .and_then(|obj| obj.get("outer"))
            .and_then(Value::as_object)
            .and_then(|obj| obj.get("inner")),
        Some(&Value::from(1))
    );
}

#[test]
fn object_has_returns_value() {
    let registry = Registry::new();
    register_flow(&registry);
    register_resolver_axioms(&registry);
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/object/has@0.1.0",
            json!({
                "target": { "foo": { "bar": 7 } },
                "path": ["foo", "bar"]
            }),
            None,
        )
        .expect("object has");
    assert_eq!(result.get("hasKey"), Some(&Value::Bool(true)));
    assert_eq!(result.get("value"), Some(&Value::from(7)));
}

#[test]
fn json_stable_stringify_orders_keys() {
    let registry = Registry::new();
    register_flow(&registry);
    register_resolver_axioms(&registry);
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/json/stable_stringify@0.1.0",
            json!({
                "value": {
                    "b": { "d": 3, "c": 2 },
                    "a": 1
                }
            }),
            None,
        )
        .expect("stable stringify");
    assert_eq!(
        result.get("text"),
        Some(&Value::String(
            "{\"a\":1,\"b\":{\"c\":2,\"d\":3}}".to_string()
        ))
    );
}

#[test]
fn hash_to_key_applies_prefix() {
    let registry = Registry::new();
    register_flow(&registry);
    register_resolver_axioms(&registry);
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/hash/to_key@0.1.0",
            json!({ "text": "hello", "prefix": "id:" }),
            None,
        )
        .expect("hash to key");
    let key = result
        .get("key")
        .and_then(Value::as_str)
        .expect("key string");
    assert!(key.starts_with("id:"));
}

#[test]
fn queue_bfs_traverses_without_duplicates() {
    let registry = Registry::new();
    register_flow(&registry);
    register_resolver_axioms(&registry);
    register_tooling(&registry);

    let mut ctx = registry.context();
    let mut graph = HashMap::new();
    graph.insert("a".to_string(), vec!["b".to_string(), "c".to_string()]);
    graph.insert("b".to_string(), vec!["c".to_string()]);
    graph.insert("c".to_string(), vec![]);
    ctx.replace_run_slot_handler(Some(Box::new(BfsSlot { graph })));

    let result = ctx
        .call(
            "lcod://tooling/queue/bfs@0.1.0",
            json!({
                "items": [ { "id": "a" } ],
                "state": { "order": [] }
            }),
            None,
        )
        .expect("queue bfs");

    let order = result
        .get("state")
        .and_then(Value::as_object)
        .and_then(|obj| obj.get("order"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        order,
        vec![Value::from("a"), Value::from("b"), Value::from("c")]
    );
    let visited = result
        .get("visited")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    assert!(visited.contains_key("a"));
    assert!(visited.contains_key("b"));
    assert!(visited.contains_key("c"));
}

#[test]
fn jsonl_read_parses_entries() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let file_path = temp_dir.path().join("manifest.jsonl");
    let content = r#"{"type":"manifest","schema":"lcod-manifest/list@1"}
{"type":"component","id":"lcod://example/foo@0.1.0"}
{"type":"list","path":"nested.jsonl"}
"#;
    std::fs::write(&file_path, content).expect("write jsonl");

    let registry = Registry::new();
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/jsonl/read@0.1.0",
            json!({ "path": file_path.to_string_lossy() }),
            None,
        )
        .expect("jsonl read");

    let entries = result
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .expect("entries array");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1]["type"], Value::from("component"));
    assert_eq!(entries[2]["path"], Value::from("nested.jsonl"));
}

#[test]
fn jsonl_read_collects_warnings() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let file_path = temp_dir.path().join("manifest.jsonl");
    let content = r#"{"type":"manifest","schema":"lcod-manifest/list@1"}
not json
{"type":"component","id":"lcod://example/foo@0.1.0"}
"#;
    std::fs::write(&file_path, content).expect("write jsonl with invalid line");

    let registry = Registry::new();
    register_tooling(&registry);
    let mut ctx = registry.context();
    let result = ctx
        .call(
            "lcod://tooling/jsonl/read@0.1.0",
            json!({ "path": file_path.to_string_lossy() }),
            None,
        )
        .expect("jsonl read");

    let entries = result
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(entries.len(), 2);

    let warnings = result
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0]
        .as_str()
        .unwrap_or_default()
        .contains("invalid JSONL entry"));
}
