use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Result};
use quick_js::{Context as JsContext, JsValue};
use serde_json::{json, Map, Value};

use crate::registry::{Context, Registry};

use super::common;

const CONTRACT_ID: &str = "lcod://tooling/script@1";
const LOG_CONTRACT_ID: &str = "lcod://contract/tooling/log@1";

#[derive(Clone)]
struct ToolDef {
    source: String,
    timeout_ms: u64,
}

pub(crate) fn register_script_contract(registry: &Registry) {
    registry.register(CONTRACT_ID, script_contract);
}

fn script_contract(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let language = input
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("javascript");
    if language != "javascript" {
        return Err(anyhow!("Unsupported scripting language: {language}"));
    }

    let source = input
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Script source must be provided"))?;
    if source.trim().is_empty() {
        return Err(anyhow!("Script source must be a non-empty string"));
    }

    let timeout_ms = input
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(1000);

    let mut initial_state = input
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    let needs_fallback = matches!(&initial_state, Value::Object(map) if map.is_empty())
        && input.get("input").is_none();
    if needs_fallback {
        if let Value::Object(obj) = input.clone() {
            let mut fallback = Map::new();
            for (key, value) in obj {
                match key.as_str() {
                    "source" | "language" | "timeoutMs" | "tools" | "imports" | "bindings"
                    | "config" | "meta" | "streams" | "input" => continue,
                    _ => {
                        fallback.insert(key, value);
                    }
                }
            }
            if !fallback.is_empty() {
                initial_state = Value::Object(fallback);
            }
        }
    }
    if let Some(stream_specs) = input.get("streams") {
        common::register_streams(ctx, &mut initial_state, stream_specs)?;
    }

    let bindings = build_bindings(&initial_state, input.get("bindings"));
    let meta = input
        .get("meta")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let config = input
        .get("config")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let tools = build_tools(input.get("tools"), timeout_ms)?;
    let imports_map = build_imports(input.get("imports"));
    let imports = Arc::new(imports_map);

    let mut scope_map = Map::new();
    scope_map.insert("input".to_string(), bindings);
    scope_map.insert("state".to_string(), initial_state);
    scope_map.insert("meta".to_string(), meta);
    scope_map.insert("imports".to_string(), Value::Object(Map::new()));
    let scope = Value::Object(scope_map);

    let messages = Rc::new(Mutex::new(Vec::new()));
    let tools_rc = Arc::new(tools);
    let config_rc = Arc::new(config);

    let evaluation = execute_script(
        ctx,
        source,
        ScriptInvocation::Main { scope: &scope },
        timeout_ms,
        Rc::clone(&messages),
        tools_rc,
        config_rc,
        Arc::clone(&imports),
    );

    match evaluation {
        Ok(mut result) => {
            let logged_guard = messages.lock().unwrap();
            let logged = logged_guard.as_slice();
            if !logged.is_empty() {
                match &mut result {
                    Value::Object(map) => {
                        let entry = map
                            .entry("messages".to_string())
                            .or_insert_with(|| Value::Array(Vec::new()));
                        match entry {
                            Value::Array(existing) => {
                                existing.extend(logged.iter().cloned().map(Value::String));
                            }
                            other => {
                                let mut merged = vec![other.clone()];
                                merged.extend(logged.iter().cloned().map(Value::String));
                                *other = Value::Array(merged);
                            }
                        }
                    }
                    _ => {
                        result = json!({
                            "result": result,
                            "messages": logged.iter().cloned().collect::<Vec<_>>()
                        });
                    }
                }
            }
            Ok(result)
        }
        Err(err) => {
            let mut payload = Map::new();
            payload.insert("success".to_string(), Value::Bool(false));
            payload.insert(
                "messages".to_string(),
                Value::Array(vec![Value::String(err.to_string())]),
            );
            let log_entries = messages
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .map(Value::String)
                .collect::<Vec<_>>();
            if !log_entries.is_empty() {
                payload.insert("logs".to_string(), Value::Array(log_entries));
            }
            Ok(Value::Object(payload))
        }
    }
}

enum ScriptInvocation<'a> {
    Main { scope: &'a Value },
    Tool { payload: &'a Value },
}

fn execute_script(
    ctx: &mut Context,
    source: &str,
    invocation: ScriptInvocation,
    timeout_ms: u64,
    messages: Rc<Mutex<Vec<String>>>,
    tools: Arc<HashMap<String, ToolDef>>,
    config: Arc<Value>,
    imports: Arc<HashMap<String, String>>,
) -> Result<Value> {
    let context = JsContext::new().map_err(|err| anyhow!("unable to create JS context: {err}"))?;

    let ctx_ptr_call = ctx as *mut Context as usize;

    context
        .add_callback(
            "__lcod_call",
            move |args: quick_js::Arguments| -> Result<JsValue, String> {
                let mut values = args.into_vec().into_iter();
                let id_value = values
                    .next()
                    .ok_or_else(|| "api.call requires an id".to_string())?;
                let id = id_value
                    .into_string()
                    .ok_or_else(|| "api.call id must be a string".to_string())?;
                let payload_js = values.next().unwrap_or(JsValue::Null);
                let payload = js_value_to_json(payload_js);
                let host_ctx = unsafe { &mut *(ctx_ptr_call as *mut Context) };
                host_ctx
                    .call(&id, payload, None)
                    .map(|value| json_to_js_value(&value))
                    .map_err(|err| err.to_string())
            },
        )
        .map_err(|err| anyhow!("failed to register api.call bridge: {err}"))?;

    let ctx_ptr_run = ctx as *mut Context as usize;
    context
        .add_callback(
            "__lcod_runSlot",
            move |args: quick_js::Arguments| -> Result<JsValue, String> {
                let mut values = args.into_vec().into_iter();
                let name_val = values
                    .next()
                    .ok_or_else(|| "api.runSlot requires a slot name".to_string())?;
                let name = name_val
                    .into_string()
                    .ok_or_else(|| "api.runSlot name must be a string".to_string())?;
                let state = values
                    .next()
                    .map(js_value_to_json)
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let slot_vars = values
                    .next()
                    .map(js_value_to_json)
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let host_ctx = unsafe { &mut *(ctx_ptr_run as *mut Context) };
                host_ctx
                    .run_slot(&name, Some(state), Some(slot_vars))
                    .map(|value| json_to_js_value(&value))
                    .map_err(|err| err.to_string())
            },
        )
        .map_err(|err| anyhow!("failed to register api.runSlot bridge: {err}"))?;

    let log_messages = Rc::clone(&messages);
    context
        .add_callback("__lcod_log", move |args: quick_js::Arguments| {
            let parts = args
                .into_vec()
                .into_iter()
                .map(format_js_value)
                .collect::<Vec<_>>();
            if let Ok(mut buffer) = log_messages.lock() {
                buffer.push(parts.join(" "));
            }
        })
        .map_err(|err| anyhow!("failed to register api.log bridge: {err}"))?;

    let console_messages = Rc::clone(&messages);
    let ctx_ptr_console = ctx as *mut Context as usize;
    context
        .add_callback(
            "__lcod_console",
            move |args: quick_js::Arguments| -> Result<JsValue, String> {
                let mut values = args.into_vec().into_iter();
                let method_val = values
                    .next()
                    .ok_or_else(|| "console handler missing method".to_string())?;
                let method = method_val
                    .into_string()
                    .unwrap_or_else(|| "log".to_string());
                let rendered = values.map(format_js_value).collect::<Vec<_>>();
                let joined = rendered.join(" ");
                let message = if joined.trim().is_empty() {
                    format!("[console.{method}]")
                } else {
                    joined
                };
                if let Ok(mut buffer) = console_messages.lock() {
                    buffer.push(message.clone());
                }
                let payload = json!({
                    "level": map_console_level(&method),
                    "message": message
                });
                let host_ctx = unsafe { &mut *(ctx_ptr_console as *mut Context) };
                if let Err(_err) = host_ctx.call(LOG_CONTRACT_ID, payload, None) {
                    // console.* must remain best-effort; swallow logging failures.
                }
                Ok(JsValue::Null)
            },
        )
        .map_err(|err| anyhow!("failed to register console bridge: {err}"))?;

    let config_for_callback = Arc::clone(&config);
    context
        .add_callback(
            "__lcod_config",
            move |args: quick_js::Arguments| -> Result<JsValue, String> {
                let mut values = args.into_vec().into_iter();
                let path_value = values.next();
                let fallback = values.next().map(js_value_to_json);
                if path_value.is_none()
                    || matches!(path_value, Some(JsValue::Null) | Some(JsValue::Undefined))
                {
                    return Ok(json_to_js_value(config_for_callback.as_ref()));
                }
                let path = path_value
                    .unwrap()
                    .into_string()
                    .ok_or_else(|| "api.config path must be a string".to_string())?;
                let normalized = normalize_config_path(&path);
                let resolved = normalized
                    .and_then(|p| resolve_path(config_for_callback.as_ref(), &p))
                    .cloned();
                match resolved {
                    Some(value) => Ok(json_to_js_value(&value)),
                    None => match fallback {
                        Some(value) => Ok(json_to_js_value(&value)),
                        None => Ok(JsValue::Undefined),
                    },
                }
            },
        )
        .map_err(|err| anyhow!("failed to register api.config bridge: {err}"))?;

    let tools_for_callback = Arc::clone(&tools);
    let config_for_tools = Arc::clone(&config);
    let imports_for_tools = Arc::clone(&imports);
    let messages_for_tools = Rc::clone(&messages);
    let ctx_ptr_tool = ctx as *mut Context as usize;
    context
        .add_callback(
            "__lcod_run",
            move |args: quick_js::Arguments| -> Result<JsValue, String> {
                let mut values = args.into_vec().into_iter();
                let name = values
                    .next()
                    .ok_or_else(|| "api.run requires a tool name".to_string())?
                    .into_string()
                    .ok_or_else(|| "api.run name must be a string".to_string())?;
                let payload = values
                    .next()
                    .map(js_value_to_json)
                    .unwrap_or_else(|| Value::Null);
                let options = values.next().map(js_value_to_json);
                let tool = tools_for_callback
                    .get(&name)
                    .ok_or_else(|| format!("Unknown tool: {name}"))?
                    .clone();
                let timeout_override = options
                    .as_ref()
                    .and_then(|opt| opt.get("timeoutMs"))
                    .and_then(Value::as_u64);
                let effective_timeout = timeout_override.unwrap_or(tool.timeout_ms);
                let host_ctx = unsafe { &mut *(ctx_ptr_tool as *mut Context) };
                execute_script(
                    host_ctx,
                    &tool.source,
                    ScriptInvocation::Tool { payload: &payload },
                    effective_timeout,
                    Rc::clone(&messages_for_tools),
                    Arc::clone(&tools_for_callback),
                    Arc::clone(&config_for_tools),
                    Arc::clone(&imports_for_tools),
                )
                .map(|value| json_to_js_value(&value))
                .map_err(|err| err.to_string())
            },
        )
        .map_err(|err| anyhow!("failed to register api.run bridge: {err}"))?;

    let imports_literal = serde_json::to_string(imports.as_ref())?;
    context
        .eval(&format!(
            "globalThis.__lcod_importTargets = Object.freeze({imports_literal});"
        ))
        .map_err(|err| anyhow!("failed to initialise script imports: {err}"))?;

    context
        .eval(
            r#"
            globalThis.__lcod_make_api = function () {
                return {
                    call: (id, args) => Promise.resolve(globalThis.__lcod_call(id, args ?? {})),
                    runSlot: (name, state, slotVars) => Promise.resolve(globalThis.__lcod_runSlot(name, state ?? {}, slotVars ?? {})),
                    log: (...values) => globalThis.__lcod_log(...values),
                    config: (path, fallback) => globalThis.__lcod_config(path, fallback),
                    run: (name, payload, options) => Promise.resolve(globalThis.__lcod_run(name, payload ?? {}, options ?? {}))
                };
            };

            globalThis.console = {
                log: (...args) => { globalThis.__lcod_console('log', ...args); },
                info: (...args) => { globalThis.__lcod_console('info', ...args); },
                warn: (...args) => { globalThis.__lcod_console('warn', ...args); },
                error: (...args) => { globalThis.__lcod_console('error', ...args); },
                debug: (...args) => { globalThis.__lcod_console('debug', ...args); },
                trace: (...args) => { globalThis.__lcod_console('trace', ...args); }
            };

            globalThis.__lcod_make_imports = function () {
                const targets = globalThis.__lcod_importTargets || {};
                const output = {};
                for (const key of Object.keys(targets)) {
                    const target = targets[key];
                    output[key] = (payload) => Promise.resolve(globalThis.__lcod_call(target, payload ?? {}));
                }
                return Object.freeze(output);
            };
        "#,
        )
        .map_err(|err| anyhow!("failed to initialise script API: {err}"))?;

    let argument_literal = match invocation {
        ScriptInvocation::Main { scope } => {
            let json = serde_json::to_string(scope)?;
            serde_json::to_string(&json)?
        }
        ScriptInvocation::Tool { payload } => {
            let json = serde_json::to_string(payload)?;
            serde_json::to_string(&json)?
        }
    };

    let mut wrapper = String::new();
    wrapper.push_str("(function() {\n");
    wrapper.push_str(&format!("  const arg0 = JSON.parse({argument_literal});\n"));
    wrapper.push_str("  const api = globalThis.__lcod_make_api();\n");
    wrapper.push_str("  const imports = globalThis.__lcod_make_imports();\n");
    wrapper.push_str("  if (arg0 && typeof arg0 === 'object') { arg0.imports = imports; }\n");
    wrapper.push_str(
        "  Object.defineProperty(api, 'imports', { value: imports, enumerable: true, writable: false });\n",
    );
    wrapper.push_str("  const userFn = (");
    wrapper.push_str(source);
    wrapper.push_str(");\n  const result = userFn(arg0, api);\n  if (result && typeof result.then === 'function') {\n    return result.then(value => value);\n  }\n  return result;\n})();");

    let start = Instant::now();
    let js_value = context
        .eval(&wrapper)
        .map_err(|err| anyhow!("script execution failed: {err}"))?;
    let elapsed = start.elapsed();
    if timeout_ms > 0 && elapsed.as_millis() as u64 > timeout_ms {
        return Err(anyhow!(
            "script exceeded timeout ({} ms > {} ms)",
            elapsed.as_millis(),
            timeout_ms
        ));
    }

    Ok(js_value_to_json(js_value))
}

fn js_value_to_json(value: JsValue) -> Value {
    match value {
        JsValue::Null => Value::Null,
        JsValue::Undefined => Value::Null,
        JsValue::Bool(b) => Value::Bool(b),
        JsValue::Int(n) => Value::Number(serde_json::Number::from(n)),
        JsValue::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        JsValue::String(s) => Value::String(s),
        JsValue::Array(items) => Value::Array(items.into_iter().map(js_value_to_json).collect()),
        JsValue::Object(entries) => {
            let mut map = Map::new();
            for (key, val) in entries {
                map.insert(key, js_value_to_json(val));
            }
            Value::Object(map)
        }
        JsValue::__NonExhaustive => Value::Null,
    }
}

fn json_to_js_value(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::Null,
        Value::Bool(b) => JsValue::Bool(*b),
        Value::Number(num) => {
            if let Some(int_val) = num.as_i64() {
                if let Ok(as_i32) = i32::try_from(int_val) {
                    return JsValue::Int(as_i32);
                }
                return JsValue::Float(int_val as f64);
            }
            if let Some(f) = num.as_f64() {
                return JsValue::Float(f);
            }
            JsValue::Null
        }
        Value::String(s) => JsValue::String(s.clone()),
        Value::Array(items) => JsValue::Array(items.iter().map(json_to_js_value).collect()),
        Value::Object(map) => {
            let mut entries: HashMap<String, JsValue> = HashMap::new();
            for (key, val) in map {
                entries.insert(key.clone(), json_to_js_value(val));
            }
            JsValue::Object(entries)
        }
    }
}

fn format_js_value(value: JsValue) -> String {
    match value {
        JsValue::Undefined => "undefined".to_string(),
        JsValue::Null => "null".to_string(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Int(n) => n.to_string(),
        JsValue::Float(f) => {
            if f.fract() == 0.0 {
                format!("{:.0}", f)
            } else {
                f.to_string()
            }
        }
        JsValue::String(s) => s,
        JsValue::Array(items) => {
            let json = Value::Array(items.into_iter().map(js_value_to_json).collect::<Vec<_>>());
            serde_json::to_string(&json).unwrap_or_else(|_| "[object Array]".to_string())
        }
        JsValue::Object(map) => {
            let json = Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, js_value_to_json(v)))
                    .collect(),
            );
            serde_json::to_string(&json).unwrap_or_else(|_| "[object Object]".to_string())
        }
        #[cfg(feature = "chrono")]
        JsValue::Date(dt) => dt.to_rfc3339(),
        #[cfg(feature = "bigint")]
        JsValue::BigInt(big) => big.to_string(),
        JsValue::__NonExhaustive => "[unknown]".to_string(),
    }
}

fn map_console_level(method: &str) -> &'static str {
    match method {
        "error" => "error",
        "warn" => "warn",
        "debug" => "debug",
        "trace" => "trace",
        _ => "info",
    }
}

fn build_tools(spec: Option<&Value>, default_timeout: u64) -> Result<HashMap<String, ToolDef>> {
    let mut map = HashMap::new();
    let Some(array) = spec.and_then(Value::as_array) else {
        return Ok(map);
    };
    for item in array {
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow!("tool descriptors must be objects"))?;
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("tool entry missing name"))?;
        let source = obj
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("tool entry missing source"))?;
        let timeout_ms = obj
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(default_timeout);
        map.insert(
            name.to_string(),
            ToolDef {
                source: source.to_string(),
                timeout_ms,
            },
        );
    }
    Ok(map)
}

fn normalize_config_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("$") {
        if trimmed.starts_with("$.") {
            return Some(trimmed.to_string());
        }
        return Some(format!("$.{}", &trimmed[1..]));
    }
    Some(format!("$.{}", trimmed))
}

fn build_bindings(state: &Value, bindings: Option<&Value>) -> Value {
    let mut out = Map::new();
    let Some(spec) = bindings.and_then(Value::as_object) else {
        return Value::Object(out);
    };

    for (name, descriptor) in spec {
        if let Some(desc_obj) = descriptor.as_object() {
            if let Some(literal) = desc_obj.get("value") {
                out.insert(name.clone(), literal.clone());
                continue;
            }
            if let Some(path) = desc_obj.get("path").and_then(Value::as_str) {
                if let Some(resolved) = resolve_path(state, path) {
                    out.insert(name.clone(), resolved.clone());
                    continue;
                }
                if let Some(default_value) = desc_obj.get("default") {
                    out.insert(name.clone(), default_value.clone());
                }
            }
        }
    }

    Value::Object(out)
}

fn resolve_path<'a>(state: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Some(state);
    }
    let normalized = if let Some(rest) = trimmed.strip_prefix("$.") {
        rest
    } else if trimmed == "$" {
        ""
    } else {
        trimmed
    };

    if normalized.is_empty() {
        return Some(state);
    }

    let mut cursor = state;
    for part in normalized.split('.') {
        if part.is_empty() {
            continue;
        }
        cursor = cursor.get(part)?;
    }
    Some(cursor)
}

fn build_imports(spec: Option<&Value>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(object) = spec.and_then(Value::as_object) else {
        return map;
    };
    for (alias, target) in object {
        if let Some(id) = target.as_str() {
            if !alias.is_empty() && !id.is_empty() {
                map.insert(alias.clone(), id.to_string());
            }
        }
    }
    map
}
