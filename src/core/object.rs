use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const CONTRACT_GET: &str = "lcod://contract/core/object/get@1";
const CONTRACT_SET: &str = "lcod://contract/core/object/set@1";
const CONTRACT_MERGE: &str = "lcod://contract/core/object/merge@1";
const CONTRACT_ENTRIES: &str = "lcod://contract/core/object/entries@1";
const AXIOM_GET: &str = "lcod://axiom/object/get@1";
const AXIOM_SET: &str = "lcod://axiom/object/set@1";
const AXIOM_MERGE: &str = "lcod://axiom/object/merge@1";

pub fn register_object(registry: &Registry) {
    registry.register(CONTRACT_GET, object_get_contract);
    registry.register(CONTRACT_SET, object_set_contract);
    registry.register(CONTRACT_MERGE, object_merge_contract);
    registry.register(CONTRACT_ENTRIES, object_entries_contract);
    registry.set_binding(AXIOM_GET, CONTRACT_GET);
    registry.set_binding(AXIOM_SET, CONTRACT_SET);
    registry.set_binding(AXIOM_MERGE, CONTRACT_MERGE);
}

#[derive(Clone, Debug)]
enum PathSegment {
    Key(String),
    Index(usize),
}

fn parse_path(path_value: &Value) -> Result<Vec<PathSegment>> {
    let array = path_value
        .as_array()
        .ok_or_else(|| anyhow!("`path` must be an array"))?;
    let mut segments = Vec::with_capacity(array.len());
    for segment in array {
        match segment {
            Value::String(s) => {
                if let Ok(index) = s.parse::<usize>() {
                    segments.push(PathSegment::Index(index));
                } else {
                    segments.push(PathSegment::Key(s.clone()));
                }
            }
            Value::Number(num) => {
                if let Some(index) = num.as_u64() {
                    segments.push(PathSegment::Index(index as usize));
                } else {
                    return Err(anyhow!("numeric path segment must be an unsigned integer"));
                }
            }
            _ => return Err(anyhow!("path segments must be strings or integers")),
        }
    }
    Ok(segments)
}

fn normalize_array_strategy(value: Option<&Value>) -> &'static str {
    match value.and_then(Value::as_str) {
        Some("concat") => "concat",
        _ => "replace",
    }
}

fn merge_objects(
    left: &Value,
    right: &Value,
    deep: bool,
    array_strategy: &'static str,
    conflicts: &mut Vec<String>,
    collect_conflicts: bool,
) -> Value {
    let mut result = left.as_object().cloned().unwrap_or_else(Map::new);
    if let Some(obj) = right.as_object() {
        for (key, right_value) in obj {
            if collect_conflicts {
                conflicts.push(key.clone());
            }
            let merged_value = if deep {
                match (result.get(key), right_value) {
                    (Some(Value::Object(lhs_obj)), Value::Object(rhs_obj)) => merge_objects(
                        &Value::Object(lhs_obj.clone()),
                        &Value::Object(rhs_obj.clone()),
                        true,
                        array_strategy,
                        conflicts,
                        false,
                    ),
                    (Some(Value::Array(lhs_arr)), Value::Array(rhs_arr)) => {
                        if array_strategy == "concat" {
                            let mut merged = lhs_arr.clone();
                            merged.extend(rhs_arr.iter().cloned());
                            Value::Array(merged)
                        } else {
                            Value::Array(rhs_arr.clone())
                        }
                    }
                    _ => right_value.clone(),
                }
            } else {
                right_value.clone()
            };
            result.insert(key.clone(), merged_value);
        }
    }
    Value::Object(result)
}

fn object_get_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let object = input
        .get("object")
        .ok_or_else(|| anyhow!("`object` is required"))?;
    if !object.is_object() && !object.is_array() {
        return Err(anyhow!("`object` must be an object or array"));
    }
    let path_segments = parse_path(
        input
            .get("path")
            .ok_or_else(|| anyhow!("`path` is required"))?,
    )?;
    let default_value = input.get("default").cloned();
    let (value, found) = resolve_path(object, &path_segments);
    let result_value = if found {
        value.clone()
    } else {
        default_value.unwrap_or(Value::Null)
    };
    Ok(json!({ "value": result_value, "found": found }))
}

fn resolve_path<'a>(mut current: &'a Value, segments: &[PathSegment]) -> (&'a Value, bool) {
    if segments.is_empty() {
        return (current, true);
    }
    for segment in segments {
        match (segment, current) {
            (PathSegment::Key(key), Value::Object(map)) => {
                if let Some(next) = map.get(key) {
                    current = next;
                } else {
                    return (&Value::Null, false);
                }
            }
            (PathSegment::Index(index), Value::Array(vec)) => {
                if let Some(next) = vec.get(*index) {
                    current = next;
                } else {
                    return (&Value::Null, false);
                }
            }
            _ => return (&Value::Null, false),
        }
    }
    (current, true)
}

fn object_set_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let original = input
        .get("object")
        .ok_or_else(|| anyhow!("`object` is required"))?
        .clone();
    if !original.is_object() && !original.is_array() {
        return Err(anyhow!("`object` must be an object or array"));
    }
    let path = parse_path(
        input
            .get("path")
            .ok_or_else(|| anyhow!("`path` is required"))?,
    )?;
    if path.is_empty() {
        return Err(anyhow!("`path` must contain at least one segment"));
    }
    let value = input
        .get("value")
        .cloned()
        .ok_or_else(|| anyhow!("`value` is required"))?;
    let clone_flag = input.get("clone").and_then(Value::as_bool).unwrap_or(true);
    let create_missing = input
        .get("createMissing")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let existed = path_exists(&original, &path);
    let mut target = if clone_flag {
        original.clone()
    } else {
        original
    };
    set_path_recursive(&mut target, &path, value, create_missing)?;
    Ok(json!({ "object": target, "created": !existed }))
}

fn object_entries_contract(
    _ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let source = input
        .get("object")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut entries = Vec::new();
    if let Some(map) = source.as_object() {
        for (key, value) in map {
            entries.push(Value::Array(vec![
                Value::String(key.clone()),
                value.clone(),
            ]));
        }
    }
    Ok(json!({ "entries": Value::Array(entries) }))
}

fn path_exists(value: &Value, segments: &[PathSegment]) -> bool {
    let (_, found) = resolve_path(value, segments);
    found
}

fn set_path_recursive(
    target: &mut Value,
    path: &[PathSegment],
    value: Value,
    create_missing: bool,
) -> Result<()> {
    let (segment, rest) = path
        .split_first()
        .ok_or_else(|| anyhow!("path must not be empty"))?;
    ensure_container_for_segment(target, segment, create_missing)?;
    match (segment, target) {
        (PathSegment::Key(key), Value::Object(map)) => {
            if rest.is_empty() {
                map.insert(key.clone(), value);
                return Ok(());
            }
            let entry = map
                .entry(key.clone())
                .or_insert_with(|| initial_container(rest.first()));
            if !entry.is_object() && !entry.is_array() {
                if !create_missing {
                    return Err(anyhow!("cannot traverse segment `{key}`"));
                }
                *entry = initial_container(rest.first());
            }
            set_path_recursive(entry, rest, value, create_missing)
        }
        (PathSegment::Index(index), Value::Array(vec)) => {
            if *index >= vec.len() {
                if !create_missing {
                    return Err(anyhow!("missing array segment at index {index}"));
                }
                vec.resize(*index + 1, Value::Null);
            }
            if rest.is_empty() {
                vec[*index] = value;
                return Ok(());
            }
            if vec[*index].is_null() {
                vec[*index] = initial_container(rest.first());
            } else if !vec[*index].is_object() && !vec[*index].is_array() {
                if !create_missing {
                    return Err(anyhow!("cannot traverse array segment at index {index}"));
                }
                vec[*index] = initial_container(rest.first());
            }
            set_path_recursive(&mut vec[*index], rest, value, create_missing)
        }
        _ => Err(anyhow!("type mismatch while traversing object path")),
    }
}

fn ensure_container_for_segment(
    target: &mut Value,
    segment: &PathSegment,
    create_missing: bool,
) -> Result<()> {
    let matches = match segment {
        PathSegment::Key(_) => target.is_object(),
        PathSegment::Index(_) => target.is_array(),
    };
    if matches {
        return Ok(());
    }
    if !create_missing {
        return Err(anyhow!("cannot traverse non-container value"));
    }
    *target = initial_container(Some(segment));
    Ok(())
}

fn initial_container(segment: Option<&PathSegment>) -> Value {
    match segment {
        Some(PathSegment::Index(_)) => Value::Array(Vec::new()),
        _ => Value::Object(Map::new()),
    }
}

fn object_merge_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let left = input
        .get("left")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let right = input
        .get("right")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    if !left.is_object() || !right.is_object() {
        return Err(anyhow!("`left` and `right` must be objects"));
    }

    let deep = input.get("deep").and_then(Value::as_bool).unwrap_or(false);
    let array_strategy = normalize_array_strategy(input.get("arrayStrategy"));

    let mut conflicts = Vec::new();
    let merged = merge_objects(&left, &right, deep, array_strategy, &mut conflicts, true);
    conflicts.sort();
    conflicts.dedup();

    let mut output = Map::new();
    output.insert("value".to_string(), merged);
    if !conflicts.is_empty() {
        output.insert(
            "conflicts".to_string(),
            Value::Array(conflicts.into_iter().map(Value::String).collect()),
        );
    }

    Ok(Value::Object(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    #[test]
    fn get_returns_value_and_flag() {
        let registry = Registry::new();
        register_object(&registry);
        let mut ctx = registry.context();
        let res = object_get_contract(
            &mut ctx,
            json!({ "object": { "foo": { "bar": 1 } }, "path": ["foo", "bar"] }),
            None,
        )
        .unwrap();
        assert_eq!(res["value"], json!(1));
        assert!(res["found"].as_bool().unwrap());
    }

    #[test]
    fn set_creates_intermediate_objects() {
        let registry = Registry::new();
        register_object(&registry);
        let mut ctx = registry.context();
        let res = object_set_contract(
            &mut ctx,
            json!({
                "object": {},
                "path": ["foo", "bar"],
                "value": 42,
                "createMissing": true
            }),
            None,
        )
        .unwrap();
        assert_eq!(res["object"], json!({ "foo": { "bar": 42 } }));
        assert!(res["created"].as_bool().unwrap());
    }

    #[test]
    fn merge_combines_objects() {
        let registry = Registry::new();
        register_object(&registry);
        let mut ctx = registry.context();
        let shallow = object_merge_contract(
            &mut ctx,
            json!({
                "left": { "a": 1, "nested": { "flag": true } },
                "right": { "b": 2, "nested": { "flag": false } }
            }),
            None,
        )
        .unwrap();
        assert_eq!(
            shallow["value"],
            json!({ "a": 1, "nested": { "flag": false }, "b": 2 })
        );
        assert_eq!(shallow["conflicts"], json!(["b", "nested"]));

        let deep = object_merge_contract(
            &mut ctx,
            json!({
                "left": { "a": 1, "nested": { "flag": true }, "arr": [1, 2] },
                "right": { "nested": { "flag": false, "extra": "x" }, "arr": [3] },
                "deep": true,
                "arrayStrategy": "concat"
            }),
            None,
        )
        .unwrap();
        assert_eq!(
            deep["value"],
            json!({
                "a": 1,
                "nested": { "flag": false, "extra": "x" },
                "arr": [1, 2, 3]
            })
        );
    }
}
