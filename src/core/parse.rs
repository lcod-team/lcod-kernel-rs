use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};
use std::collections::HashMap;

use csv::Trim;
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

const CONTRACT_JSON: &str = "lcod://contract/core/parse/json@1";
const CONTRACT_TOML: &str = "lcod://contract/core/parse/toml@1";
const CONTRACT_CSV: &str = "lcod://contract/core/parse/csv@1";

pub fn register_parse(registry: &Registry) {
    registry.register(CONTRACT_JSON, parse_json_contract);
    registry.register(CONTRACT_TOML, parse_toml_contract);
    registry.register(CONTRACT_CSV, parse_csv_contract);
}

fn parse_json_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let (text, bytes) = read_text(&input)?;
    let value: Value =
        serde_json::from_str(&text).map_err(|err| anyhow!("JSON parse error: {err}"))?;
    Ok(json!({
        "value": value,
        "bytes": bytes,
        "validated": false
    }))
}

fn parse_toml_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let (text, bytes) = read_text(&input)?;
    let value: toml::Value = text
        .parse()
        .map_err(|err| anyhow!("TOML parse error: {err}"))?;
    let json_value = serde_json::to_value(value)?;
    Ok(json!({
        "value": json_value,
        "bytes": bytes
    }))
}

fn parse_csv_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let (text, bytes) = read_text(&input)?;
    let delimiter = input
        .get("delimiter")
        .and_then(Value::as_str)
        .and_then(|s| s.as_bytes().first().copied())
        .unwrap_or(b',');
    let quote = input
        .get("quote")
        .and_then(Value::as_str)
        .and_then(|s| s.as_bytes().first().copied())
        .unwrap_or(b'"');
    let trim = input.get("trim").and_then(Value::as_bool).unwrap_or(false);

    let header_value = input.get("header");
    let columns_from_header = header_value.and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect::<Vec<_>>()
    });
    let use_header = header_value.and_then(Value::as_bool).unwrap_or(false);

    let mut reader_builder = csv::ReaderBuilder::new();
    reader_builder
        .delimiter(delimiter)
        .quote(quote)
        .trim(if trim { Trim::All } else { Trim::None });

    let rows_value;
    let columns_value;

    if let Some(columns) = columns_from_header {
        reader_builder.has_headers(false);
        let mut reader = reader_builder.from_reader(text.as_bytes());
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record?;
            let mut object = Map::new();
            for (idx, column) in columns.iter().enumerate() {
                let value = record.get(idx).unwrap_or("");
                object.insert(column.clone(), Value::String(value.to_string()));
            }
            rows.push(Value::Object(object));
        }
        rows_value = Value::Array(rows);
        columns_value = Some(Value::Array(
            columns.into_iter().map(Value::String).collect(),
        ));
    } else if use_header {
        reader_builder.has_headers(true);
        let mut reader = reader_builder.from_reader(text.as_bytes());
        let mut rows = Vec::new();
        for result in reader.deserialize::<HashMap<String, String>>() {
            let map = result?;
            let object = map
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect::<Map<_, _>>();
            rows.push(Value::Object(object));
        }
        rows_value = Value::Array(rows);
        columns_value = None;
    } else {
        reader_builder.has_headers(false);
        let mut reader = reader_builder.from_reader(text.as_bytes());
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record?;
            let row = record
                .iter()
                .map(|s| Value::String(s.to_string()))
                .collect();
            rows.push(Value::Array(row));
        }
        rows_value = Value::Array(rows);
        columns_value = None;
    }

    let mut result = Map::new();
    result.insert("rows".to_string(), rows_value);
    if let Some(columns) = columns_value {
        result.insert("columns".to_string(), columns);
    }
    result.insert("bytes".to_string(), Value::Number(bytes.into()));

    Ok(Value::Object(result))
}

fn read_text(input: &Value) -> Result<(String, usize)> {
    if let Some(text) = input.get("text").and_then(Value::as_str) {
        let bytes = text.as_bytes().len();
        return Ok((text.to_string(), bytes));
    }
    if let Some(path) = input.get("path").and_then(Value::as_str) {
        let content = fs::read_to_string(Path::new(path))
            .map_err(|err| anyhow!("unable to read file `{path}`: {err}"))?;
        let bytes = content.as_bytes().len();
        return Ok((content, bytes));
    }
    Err(anyhow!("missing `text` or `path` for parse input"))
}
