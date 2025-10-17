use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const CONTRACT_GET: &str = "lcod://contract/core/object/get@1";
const CONTRACT_SET: &str = "lcod://contract/core/object/set@1";
const AXIOM_GET: &str = "lcod://axiom/object/get@1";
const AXIOM_SET: &str = "lcod://axiom/object/set@1";

pub fn register_object(registry: &Registry) {
    registry.register(CONTRACT_GET, object_get_contract);
    registry.register(CONTRACT_SET, object_set_contract);
    registry.set_binding(AXIOM_GET, CONTRACT_GET);
    registry.set_binding(AXIOM_SET, CONTRACT_SET);
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
}
