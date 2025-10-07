pub mod manager;

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;

use anyhow::{anyhow, Context as AnyhowContext, Result};
use base64::Engine;
use manager::HttpHostControl;
use serde_json::{json, Map, Value};
use tiny_http::{Header, Response, StatusCode};
use url::Url;

use crate::compose::{parse_compose, run_compose};
use crate::registry::{Context, Registry};

const CONTRACT_API_ROUTE: &str = "lcod://http/api_route@0.1.0";
const CONTRACT_PROJECT_HTTP_APP: &str = "lcod://project/http_app@0.1.0";
const CONTRACT_ENV_HTTP_HOST: &str = "lcod://env/http_host@0.1.0";
const CONTRACT_ENV_HTTP_HOST_STOP: &str = "lcod://env/http_host/stop@0.1.0";

#[derive(Clone)]
struct RouteEntry {
    project: Value,
    route: Value,
    handler: Value,
}

pub fn register_http_contracts(registry: &Registry) {
    registry.register(CONTRACT_API_ROUTE, api_route_contract);
    registry.register(CONTRACT_PROJECT_HTTP_APP, project_http_app_contract);
    registry.register(CONTRACT_ENV_HTTP_HOST, env_http_host_contract);
    registry.register(CONTRACT_ENV_HTTP_HOST_STOP, env_http_host_stop_contract);
}

fn normalize_segment(segment: &str) -> Option<String> {
    let trimmed = segment.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return None;
    }
    let stripped = trimmed.trim_matches('/');
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

fn join_paths(parts: &[&str]) -> String {
    let mut segments = Vec::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if let Some(norm) = normalize_segment(part) {
            segments.push(norm);
        }
    }
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

fn collect_slot_results(state: &Value, key: &str) -> Vec<Value> {
    match state {
        Value::Null => Vec::new(),
        Value::Array(items) => items.clone(),
        Value::Object(map) => {
            let mut out = Vec::new();
            if let Some(value) = map.get(key) {
                out.push(value.clone());
            }
            let plural = format!("{key}s");
            if let Some(Value::Array(items)) = map.get(&plural) {
                out.extend(items.iter().cloned());
            }
            out
        }
        other => vec![other.clone()],
    }
}

fn ensure_array(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other.clone()],
    }
}

fn api_route_contract(_ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let sequence_id = input
        .get("sequenceId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("sequenceId is required for http/api_route"))?;
    let method = input
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    let path = input.get("path").and_then(Value::as_str).unwrap_or("/");
    let joined = join_paths(&["/", path]);
    let mut route = Map::new();
    route.insert("method".to_string(), Value::String(method));
    route.insert("path".to_string(), Value::String(joined));
    route.insert(
        "sequenceId".to_string(),
        Value::String(sequence_id.to_string()),
    );
    if let Some(desc) = input.get("description") {
        route.insert("description".to_string(), desc.clone());
    }
    if let Some(middlewares) = input.get("middlewares") {
        route.insert("middlewares".to_string(), middlewares.clone());
    }
    Ok(Value::Object({
        let mut map = Map::new();
        map.insert("route".to_string(), Value::Object(route));
        map
    }))
}

fn project_http_app_contract(
    ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("project/http_app requires name"))?;
    let base_path = input.get("basePath").and_then(Value::as_str).unwrap_or("/");
    let project_meta = json!({
        "name": name,
        "basePath": join_paths(&["/", base_path]),
        "metadata": input.get("metadata").cloned().unwrap_or(Value::Object(Map::new()))
    });

    let seq_state = ctx.run_slot(
        "sequences",
        Some(json!({ "project": project_meta.clone() })),
        Some(json!({ "project": project_meta.clone() })),
    )?;
    let mut sequences = collect_slot_results(&seq_state, "sequence");
    if let Value::Object(map) = seq_state {
        if let Some(Value::Array(items)) = map.get("sequences") {
            sequences = items.clone();
        }
    }

    let api_state = ctx.run_slot(
        "apis",
        Some(json!({ "project": project_meta.clone(), "sequences": sequences.clone() })),
        Some(json!({ "project": project_meta.clone(), "sequences": sequences.clone() })),
    )?;
    let mut routes = collect_slot_results(&api_state, "route");
    if let Value::Object(map) = api_state {
        if let Some(Value::Array(items)) = map.get("routes") {
            routes = items.clone();
        }
    }

    Ok(json!({
        "project": project_meta,
        "routes": routes,
        "sequences": sequences
    }))
}

fn env_http_host_stop_contract(
    ctx: &mut Context,
    input: Value,
    _meta: Option<Value>,
) -> Result<Value> {
    let handle = input
        .get("handle")
        .cloned()
        .ok_or_else(|| anyhow!("handle is required to stop http host"))?;
    ctx.stop_http_host(&handle)
}

fn env_http_host_contract(ctx: &mut Context, input: Value, _meta: Option<Value>) -> Result<Value> {
    let host = input
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("0.0.0.0")
        .to_string();
    let port_value = input.get("port").and_then(Value::as_u64).unwrap_or(0);
    if port_value > u16::MAX as u64 {
        return Err(anyhow!("env/http_host requires a valid port"));
    }
    let port = port_value as u16;
    let base_path = input.get("basePath").and_then(Value::as_str).unwrap_or("");
    let normalized_base = join_paths(&["/", base_path]);
    let metadata = input
        .get("metadata")
        .cloned()
        .unwrap_or(Value::Object(Map::new()));

    let host_descriptor = json!({
        "host": host,
        "port": port,
        "basePath": normalized_base,
        "metadata": metadata.clone()
    });

    let project_state = ctx.run_slot(
        "projects",
        Some(Value::Object(Map::new())),
        Some(json!({ "host": host_descriptor.clone() })),
    )?;
    let mut raw_projects = collect_slot_results(&project_state, "project");
    if raw_projects.is_empty() && !project_state.is_null() {
        raw_projects.push(project_state.clone());
    }

    let mut route_handlers: HashMap<String, RouteEntry> = HashMap::new();
    let mut output_routes = Vec::new();
    let mut project_summaries = Vec::new();

    for entry in raw_projects {
        let project = entry
            .get("project")
            .cloned()
            .unwrap_or_else(|| entry.clone());
        let routes = ensure_array(entry.get("routes"));
        let sequences = ensure_array(entry.get("sequences"));
        let project_base = project
            .get("basePath")
            .and_then(Value::as_str)
            .unwrap_or("/");
        let project_base_joined = join_paths(&[&normalized_base, project_base]);

        let mut seq_map = HashMap::new();
        for seq in sequences {
            if let Some(id) = seq.get("id").and_then(Value::as_str) {
                seq_map.insert(id.to_string(), seq);
            }
        }

        let summary = json!({
            "name": project.get("name").cloned().unwrap_or(Value::Null),
            "basePath": project.get("basePath").cloned().unwrap_or(Value::Null),
            "metadata": project.get("metadata").cloned().unwrap_or(Value::Null)
        });
        project_summaries.push(summary);

        for route in routes {
            let sequence_id = route
                .get("sequenceId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("route descriptor missing sequenceId"))?;
            let method = route
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_uppercase();
            let method_display = method.clone();
            let route_path = route.get("path").and_then(Value::as_str).unwrap_or("/");
            let full_path = join_paths(&[&project_base_joined, route_path]);
            let key = format!("{method} {}", full_path);
            let sequence = seq_map
                .get(sequence_id)
                .cloned()
                .ok_or_else(|| anyhow!("sequence not found for route {sequence_id}"))?;
            let handler = sequence
                .get("handler")
                .cloned()
                .ok_or_else(|| anyhow!("sequence {sequence_id} missing handler"))?;
            if route_handlers.contains_key(&key) {
                return Err(anyhow!("duplicate route registered: {key}"));
            }
            route_handlers.insert(
                key.clone(),
                RouteEntry {
                    project: project.clone(),
                    route: route.clone(),
                    handler,
                },
            );
            let handler_id = sequence
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(sequence_id)
                .to_string();
            let project_name = project.get("name").cloned().unwrap_or(Value::Null);
            output_routes.push(json!({
                "method": method_display,
                "path": full_path,
                "handlerId": handler_id,
                "sequenceId": sequence_id,
                "project": project_name
            }));
        }
    }

    let listener = TcpListener::bind((
        host_descriptor
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("0.0.0.0"),
        port,
    ))
    .with_context(|| "unable to bind HTTP host socket")?;
    let local_addr = listener.local_addr()?;
    let actual_port = local_addr.port();
    let server = Arc::new(
        tiny_http::Server::from_listener(listener, None)
            .map_err(|err| anyhow!("failed to create HTTP server: {err}"))?,
    );
    let running = Arc::new(AtomicBool::new(true));

    let registry_clone = ctx.registry_clone();
    let route_map_arc = Arc::new(route_handlers);
    let host_info_value = json!({
        "name": host_descriptor
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("0.0.0.0"),
        "port": actual_port,
        "basePath": normalized_base.clone()
    });

    let server_for_thread: Arc<tiny_http::Server> = Arc::clone(&server);
    let running_flag = Arc::clone(&running);
    let route_map_thread = Arc::clone(&route_map_arc);

    let thread_handle = thread::spawn(move || {
        let mut incoming = server_for_thread.incoming_requests();
        while running_flag.load(Ordering::SeqCst) {
            match incoming.next() {
                Some(request) => {
                    if let Err(err) = handle_http_request(
                        &registry_clone,
                        route_map_thread.as_ref(),
                        &host_info_value,
                        request,
                    ) {
                        eprintln!("http_host handler error: {err}");
                    }
                }
                None => {
                    if !running_flag.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    });

    let control = HttpHostControl::new(server, running, thread_handle);
    let handle_value = ctx.register_http_host(control);

    let public_host = match host_descriptor
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("0.0.0.0")
    {
        "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    };
    let base_url = if normalized_base != "/" {
        format!(
            "http://{}:{}/{}",
            public_host,
            actual_port,
            normalized_base.trim_start_matches('/')
        )
    } else {
        format!("http://{}:{}", public_host, actual_port)
    };

    let mut result = Map::new();
    result.insert("url".to_string(), Value::String(base_url));
    result.insert("routes".to_string(), Value::Array(output_routes));
    result.insert("projects".to_string(), Value::Array(project_summaries));
    result.insert("handle".to_string(), handle_value);
    result.insert(
        "basePath".to_string(),
        Value::String(normalized_base.clone()),
    );
    Ok(Value::Object(result))
}

fn handle_http_request(
    registry: &Registry,
    routes: &HashMap<String, RouteEntry>,
    host_info: &Value,
    mut request: tiny_http::Request,
) -> Result<()> {
    let method = request.method().as_str().to_uppercase();
    let url_path = request.url().to_string();
    let (path_part, query_part) = match url_path.split_once('?') {
        Some((path, query)) => (path.to_string(), Some(query.to_string())),
        None => (url_path.clone(), None),
    };
    let normalized_path = join_paths(&["/", &path_part]);
    let key = format!("{method} {normalized_path}");
    let Some(entry) = routes.get(&key) else {
        let response = Response::from_string("{\"error\":\"Not found\"}")
            .with_status_code(StatusCode(404))
            .with_header(
                Header::from_bytes("content-type", "application/json")
                    .unwrap_or_else(|_| Header::from_bytes("content-type", "text/plain").unwrap()),
            );
        let _ = request.respond(response);
        return Ok(());
    };

    let mut body_bytes = Vec::new();
    request
        .as_reader()
        .read_to_end(&mut body_bytes)
        .with_context(|| "failed to read request body")?;
    let content_type = request
        .headers()
        .iter()
        .find_map(|h| {
            let name = h.field.to_string();
            if name.eq_ignore_ascii_case("content-type") {
                Some(h.value.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "".to_string());

    let full_url = format!("http://local.host{}", request.url());
    let parsed_url = Url::parse(&full_url)?;
    let mut query_map = Map::new();
    for (key, value) in parsed_url.query_pairs() {
        let entry = query_map.entry(key.into_owned()).or_insert(Value::Null);
        match entry {
            Value::Null => *entry = Value::String(value.into_owned()),
            Value::String(existing) => {
                let mut arr = Vec::new();
                arr.push(Value::String(existing.clone()));
                arr.push(Value::String(value.into_owned()));
                *entry = Value::Array(arr);
            }
            Value::Array(arr) => arr.push(Value::String(value.into_owned())),
            _ => {}
        }
    }

    let mut headers_map = Map::new();
    for header in request.headers() {
        let name = header.field.to_string().to_ascii_lowercase();
        let value = header.value.to_string();
        let entry = headers_map.entry(name).or_insert(Value::Null);
        match entry {
            Value::Null => *entry = Value::String(value),
            Value::String(existing) => {
                let mut arr = Vec::new();
                arr.push(Value::String(existing.clone()));
                arr.push(Value::String(value));
                *entry = Value::Array(arr);
            }
            Value::Array(arr) => arr.push(Value::String(value)),
            _ => {}
        }
    }

    let body_value = if body_bytes.is_empty() {
        Value::Null
    } else if content_type.to_lowercase().starts_with("application/json") {
        serde_json::from_slice::<Value>(&body_bytes).unwrap_or(Value::Null)
    } else {
        Value::String(String::from_utf8_lossy(&body_bytes).to_string())
    };
    let raw_body = if body_bytes.is_empty() {
        Value::Null
    } else {
        Value::String(base64::engine::general_purpose::STANDARD.encode(&body_bytes))
    };

    let mut request_map = Map::new();
    request_map.insert("method".to_string(), Value::String(method));
    request_map.insert("path".to_string(), Value::String(normalized_path.clone()));
    request_map.insert(
        "url".to_string(),
        Value::String(match query_part {
            Some(ref q) => format!("{normalized_path}?{q}"),
            None => normalized_path.clone(),
        }),
    );
    request_map.insert("query".to_string(), Value::Object(query_map));
    request_map.insert("headers".to_string(), Value::Object(headers_map));
    request_map.insert("body".to_string(), body_value);
    request_map.insert("rawBody".to_string(), raw_body);
    request_map.insert("host".to_string(), host_info.clone());
    request_map.insert("project".to_string(), entry.project.clone());
    request_map.insert("route".to_string(), entry.route.clone());

    let request_context = Value::Object(request_map);
    let meta = json!({
        "project": entry.project.clone(),
        "route": entry.route.clone()
    });
    let result = execute_handler(registry, &entry.handler, &request_context, &meta);

    let result = match result {
        Ok(value) => value,
        Err(err) => {
            let response = Response::from_string(
                json!({
                    "error": err.to_string()
                })
                .to_string(),
            )
            .with_status_code(StatusCode(500))
            .with_header(
                Header::from_bytes("content-type", "application/json")
                    .unwrap_or_else(|_| Header::from_bytes("content-type", "text/plain").unwrap()),
            );
            let _ = request.respond(response);
            return Ok(());
        }
    };

    let status = result
        .get("status")
        .and_then(Value::as_i64)
        .unwrap_or(200)
        .clamp(100, 599) as u16;

    let mut headers = Vec::new();
    if let Some(Value::Object(map)) = result.get("headers") {
        for (name, value) in map {
            match value {
                Value::String(s) => headers.push((name.clone(), s.clone())),
                Value::Array(items) => {
                    for item in items {
                        if let Some(s) = item.as_str() {
                            headers.push((name.clone(), s.to_string()));
                        }
                    }
                }
                other => headers.push((name.clone(), other.to_string())),
            }
        }
    }

    let mut has_body = false;
    let (body_vec, default_content_type) = match result.get("body") {
        None | Some(Value::Null) => (Vec::new(), None),
        Some(Value::String(s)) => {
            has_body = true;
            (s.clone().into_bytes(), None)
        }
        Some(Value::Object(_) | Value::Array(_)) => {
            has_body = true;
            (
                serde_json::to_vec(result.get("body").unwrap()).unwrap_or_default(),
                Some("application/json".to_string()),
            )
        }
        Some(other) => {
            has_body = true;
            (other.to_string().into_bytes(), None)
        }
    };

    let mut response = Response::from_data(body_vec).with_status_code(StatusCode(status));

    let mut has_content_type = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    if !has_content_type {
        if let Some(default) = default_content_type {
            headers.push(("content-type".to_string(), default));
            has_content_type = true;
        }
    }
    if !has_content_type && has_body {
        headers.push((
            "content-type".to_string(),
            "application/octet-stream".to_string(),
        ));
    }

    for (name, value) in headers {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response = response.with_header(header);
        }
    }

    request.respond(response)?;
    Ok(())
}

fn execute_handler(
    registry: &Registry,
    handler: &Value,
    request_context: &Value,
    route_meta: &Value,
) -> Result<Value> {
    let handler_type = handler
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("script");
    let meta_value = json!({
        "route": route_meta,
        "request": request_context
    });

    match handler_type {
        "script" => {
            let source = handler
                .get("source")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Script handler must define a source"))?;
            let mut payload = Map::new();
            payload.insert("source".to_string(), Value::String(source.to_string()));
            if let Some(bindings) = handler.get("bindings") {
                payload.insert("bindings".to_string(), bindings.clone());
            }
            let input = handler
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({ "request": request_context }));
            payload.insert("input".to_string(), input);
            payload.insert("meta".to_string(), meta_value);
            let mut ctx = registry.context();
            ctx.call("lcod://tooling/script@1", Value::Object(payload), None)
        }
        "component" => {
            let target = handler
                .get("call")
                .or_else(|| handler.get("component"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Component handler must provide call/component string"))?;
            let input = handler
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({ "request": request_context }));
            let mut ctx = registry.context();
            ctx.call(target, input, Some(meta_value))
        }
        "compose" => {
            let compose_value = handler
                .get("compose")
                .cloned()
                .ok_or_else(|| anyhow!("Compose handler requires compose array"))?;
            let steps = parse_compose(&compose_value)
                .with_context(|| "invalid compose steps in handler")?;
            let mut initial_state = handler
                .get("initialState")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            if !initial_state.is_object() {
                initial_state = Value::Object(Map::new());
            }
            if let Some(map) = initial_state.as_object_mut() {
                map.entry("request".to_string())
                    .or_insert(request_context.clone());
            }
            let mut ctx = registry.context();
            run_compose(&mut ctx, &steps, initial_state)
        }
        other => Err(anyhow!("Unsupported handler type: {other}")),
    }
}
