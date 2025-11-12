use std::collections::HashMap;
use std::fmt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::http::manager::{HttpHostControl, HttpHostManager};
use crate::streams::StreamManager;

pub trait SlotExecutor {
    fn run_slot(
        &mut self,
        ctx: &mut Context,
        name: &str,
        local_state: Value,
        slot_vars: Value,
    ) -> Result<Value>;

    fn into_fallback(self: Box<Self>) -> Option<Box<dyn SlotExecutor + 'static>> {
        None
    }
}

#[derive(Debug)]
pub struct CancelledError;

impl fmt::Display for CancelledError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "execution cancelled")
    }
}

impl std::error::Error for CancelledError {}

#[derive(Clone, Debug)]
pub struct ComponentMetadata {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub slots: Vec<String>,
}

impl ComponentMetadata {
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty() && self.slots.is_empty()
    }
}

struct ComponentEntry {
    func: Arc<dyn Func>,
    outputs: Option<Arc<Vec<String>>>,
    metadata: Option<Arc<ComponentMetadata>>,
}

impl ComponentEntry {
    fn new(
        func: Arc<dyn Func>,
        outputs: Option<Arc<Vec<String>>>,
        metadata: Option<Arc<ComponentMetadata>>,
    ) -> Self {
        Self {
            func,
            outputs,
            metadata,
        }
    }
}

struct RegistryInner {
    funcs: HashMap<String, Arc<ComponentEntry>>,
    bindings: HashMap<String, String>,
}

impl RegistryInner {
    fn new() -> Self {
        Self {
            funcs: HashMap::new(),
            bindings: HashMap::new(),
        }
    }
}

#[derive(Clone)]
struct RegistrySnapshot {
    bindings: HashMap<String, String>,
    funcs: HashMap<String, Arc<ComponentEntry>>,
}

pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Registry {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

pub trait Func: Send + Sync {
    fn call(&self, ctx: &mut Context, input: Value, meta: Option<Value>) -> Result<Value>;
}

impl<F> Func for F
where
    F: Fn(&mut Context, Value, Option<Value>) -> Result<Value> + Send + Sync + 'static,
{
    fn call(&self, ctx: &mut Context, input: Value, meta: Option<Value>) -> Result<Value> {
        (self)(ctx, input, meta)
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner::new())),
        }
    }

    pub fn register<F>(&self, name: impl Into<String>, func: F)
    where
        F: Func + 'static,
    {
        self.register_entry(name, func, None, None);
    }

    pub fn register_with_outputs<F>(
        &self,
        name: impl Into<String>,
        func: F,
        outputs: Option<Arc<Vec<String>>>,
    ) where
        F: Func + 'static,
    {
        self.register_entry(name, func, outputs, None);
    }

    pub fn register_with_metadata<F>(
        &self,
        name: impl Into<String>,
        func: F,
        metadata: Option<Arc<ComponentMetadata>>,
    ) where
        F: Func + 'static,
    {
        self.register_entry(name, func, None, metadata);
    }

    fn register_entry<F>(
        &self,
        name: impl Into<String>,
        func: F,
        outputs: Option<Arc<Vec<String>>>,
        metadata: Option<Arc<ComponentMetadata>>,
    ) where
        F: Func + 'static,
    {
        let func_arc: Arc<dyn Func> = Arc::new(func);
        let entry = Arc::new(ComponentEntry::new(func_arc, outputs, metadata));
        let mut inner = self.inner.lock().expect("registry poisoned");
        inner.funcs.insert(name.into(), entry);
    }

    pub fn set_binding(&self, contract: impl Into<String>, implementation: impl Into<String>) {
        let mut inner = self.inner.lock().expect("registry poisoned");
        inner
            .bindings
            .insert(contract.into(), implementation.into());
    }

    pub fn call(
        &self,
        ctx: &mut Context,
        name: &str,
        input: Value,
        meta: Option<Value>,
    ) -> Result<Value> {
        ctx.call(name, input, meta)
    }

    pub fn context(&self) -> Context {
        Context::new(self.inner.clone(), Arc::new(AtomicBool::new(false)))
    }

    pub fn context_with_cancellation(&self, token: Arc<AtomicBool>) -> Context {
        Context::new(self.inner.clone(), token)
    }
}

fn enforce_outputs(value: Value, allowed: &[String]) -> Value {
    if allowed.is_empty() {
        return value;
    }
    match value {
        Value::Object(mut map) => {
            let mut filtered = Map::new();
            for key in allowed {
                let entry = map.remove(key).unwrap_or(Value::Null);
                filtered.insert(key.clone(), entry);
            }
            Value::Object(filtered)
        }
        other => other,
    }
}

fn sanitize_component_input(value: Value, metadata: &ComponentMetadata) -> (Value, Value) {
    let original = match value {
        Value::Object(map) => map,
        other => {
            let mut wrapper = Map::new();
            wrapper.insert("value".to_string(), other);
            wrapper
        }
    };

    if metadata.inputs.is_empty() {
        let snapshot = original.clone();
        return (Value::Object(snapshot), Value::Object(original));
    }

    let mut filtered = Map::new();
    for key in &metadata.inputs {
        if let Some(entry) = original.get(key) {
            filtered.insert(key.clone(), entry.clone());
        }
    }

    (Value::Object(filtered), Value::Object(original))
}

fn needs_raw_snapshot(name: &str) -> bool {
    matches!(name, "lcod://tooling/sanitizer/probe@0.1.0")
}

fn find_entry(inner: &RegistryInner, name: &str) -> (Option<Arc<ComponentEntry>>, bool) {
    let mut missing_contract_binding = false;
    let entry = if let Some(entry) = inner.funcs.get(name) {
        Some(entry.clone())
    } else if let Some(binding) = inner.bindings.get(name) {
        if binding != name {
            inner.funcs.get(binding).cloned()
        } else {
            inner.funcs.get(name).cloned()
        }
    } else if name.starts_with("lcod://contract/") {
        if !inner.bindings.contains_key(name) {
            missing_contract_binding = true;
        }
        None
    } else {
        None
    };
    (entry, missing_contract_binding)
}

pub struct Context {
    registry: Arc<Mutex<RegistryInner>>,
    scope_depth: usize,
    run_slot_handler: Option<Box<dyn SlotExecutor + 'static>>,
    streams: StreamManager,
    http_hosts: HttpHostManager,
    registry_scope_stack: Vec<RegistrySnapshot>,
    log_tag_stack: Vec<Map<String, Value>>,
    raw_input_stack: Vec<Value>,
    spec_captured_logs: Vec<Value>,
    spec_logs_truncated: bool,
    cancellation: Arc<AtomicBool>,
}

impl Context {
    fn new(registry: Arc<Mutex<RegistryInner>>, cancellation: Arc<AtomicBool>) -> Self {
        Self {
            registry,
            scope_depth: 0,
            run_slot_handler: None,
            streams: StreamManager::new(),
            http_hosts: HttpHostManager::new(),
            registry_scope_stack: Vec::new(),
            log_tag_stack: Vec::new(),
            raw_input_stack: Vec::new(),
            spec_captured_logs: Vec::new(),
            spec_logs_truncated: false,
            cancellation,
        }
    }

    pub fn call(&mut self, name: &str, input: Value, meta: Option<Value>) -> Result<Value> {
        self.ensure_not_cancelled()?;
        let (maybe_entry, missing_contract_binding) = {
            let inner = self.registry.lock().expect("registry poisoned");
            find_entry(&inner, name)
        };
        let Some(entry) = maybe_entry else {
            if missing_contract_binding {
                return Err(anyhow!("No binding for contract: {name}"));
            }
            return Err(anyhow!("function not found: {name}"));
        };
        let func = entry.func.clone();
        let outputs = entry.outputs.clone();
        let metadata = entry.metadata.clone();

        let mut prepared_input = input;
        let mut raw_snapshot = None;
        if let Some(component_meta) = metadata.as_ref() {
            let (sanitized, raw) = sanitize_component_input(prepared_input, component_meta);
            prepared_input = sanitized;
            if needs_raw_snapshot(name) {
                raw_snapshot = Some(raw);
            }
        }

        let pushed_raw = if let Some(raw_value) = raw_snapshot {
            self.push_raw_input(raw_value);
            true
        } else {
            false
        };

        let mut value = match func.call(self, prepared_input, meta) {
            Ok(result) => result,
            Err(err) => {
                if pushed_raw {
                    self.pop_raw_input();
                }
                return Err(err);
            }
        };

        if pushed_raw {
            self.pop_raw_input();
        }

        if let Some(allowed) = outputs {
            value = enforce_outputs(value, allowed.as_ref());
        }
        Ok(value)
    }

    pub fn replace_run_slot_handler(
        &mut self,
        handler: Option<Box<dyn SlotExecutor + 'static>>,
    ) -> Option<Box<dyn SlotExecutor + 'static>> {
        std::mem::replace(&mut self.run_slot_handler, handler)
    }

    pub fn run_slot(
        &mut self,
        name: &str,
        local_state: Option<Value>,
        slot_vars: Option<Value>,
    ) -> Result<Value> {
        self.ensure_not_cancelled()?;
        let mut handler = self
            .run_slot_handler
            .take()
            .ok_or_else(|| anyhow!("runSlot not available in this context"))?;
        let local = local_state.unwrap_or(Value::Null);
        let slot = slot_vars.unwrap_or(Value::Null);
        let result = handler.run_slot(self, name, local, slot);
        self.run_slot_handler = Some(handler);
        self.ensure_not_cancelled()?;
        result
    }

    pub fn push_scope(&mut self) {
        self.scope_depth += 1;
    }

    pub fn pop_scope(&mut self) {
        if self.scope_depth > 0 {
            self.scope_depth -= 1;
        }
    }

    pub fn streams_mut(&mut self) -> &mut StreamManager {
        &mut self.streams
    }

    pub fn streams(&self) -> &StreamManager {
        &self.streams
    }

    pub fn register_http_host(&mut self, control: HttpHostControl) -> Value {
        self.http_hosts.register(control)
    }

    pub fn stop_http_host(&mut self, handle: &Value) -> Result<Value> {
        self.http_hosts.stop(handle)
    }

    pub fn stop_all_http_hosts(&mut self) {
        self.http_hosts.stop_all();
    }

    pub fn enter_registry_scope(
        &mut self,
        bindings: Option<HashMap<String, String>>,
    ) -> Result<()> {
        let snapshot = {
            let inner = self.registry.lock().expect("registry poisoned");
            RegistrySnapshot {
                bindings: inner.bindings.clone(),
                funcs: inner.funcs.clone(),
            }
        };
        let mut merged_bindings = snapshot.bindings.clone();
        if let Some(overrides) = bindings {
            for (contract, implementation) in overrides {
                merged_bindings.insert(contract, implementation);
            }
        }
        {
            let mut inner = self.registry.lock().expect("registry poisoned");
            inner.bindings = merged_bindings;
            inner.funcs = snapshot.funcs.clone();
        }
        self.registry_scope_stack.push(snapshot);
        Ok(())
    }

    pub fn leave_registry_scope(&mut self) -> Result<()> {
        if let Some(previous) = self.registry_scope_stack.pop() {
            let mut inner = self.registry.lock().expect("registry poisoned");
            inner.bindings = previous.bindings;
            inner.funcs = previous.funcs;
        }
        Ok(())
    }

    pub fn fork(&self) -> Context {
        let mut cloned = Context::new(self.registry.clone(), self.cancellation.clone());
        cloned.log_tag_stack = self.log_tag_stack.clone();
        cloned.raw_input_stack = self.raw_input_stack.clone();
        cloned.spec_captured_logs = self.spec_captured_logs.clone();
        cloned.spec_logs_truncated = self.spec_logs_truncated;
        cloned
    }

    pub fn registry_clone(&self) -> Registry {
        Registry {
            inner: self.registry.clone(),
        }
    }

    pub fn cancellation_token(&self) -> Arc<AtomicBool> {
        self.cancellation.clone()
    }

    pub fn set_cancellation_token(&mut self, token: Arc<AtomicBool>) {
        self.cancellation = token;
    }

    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::SeqCst)
    }

    pub fn ensure_not_cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(CancelledError.into())
        } else {
            Ok(())
        }
    }

    pub fn push_log_tags(&mut self, tags: Map<String, Value>) {
        if tags.is_empty() {
            return;
        }
        self.log_tag_stack.push(tags);
    }

    pub fn pop_log_tags(&mut self) {
        self.log_tag_stack.pop();
    }

    pub fn log_tag_stack(&self) -> &[Map<String, Value>] {
        &self.log_tag_stack
    }

    pub fn binding_for(&self, contract: &str) -> Option<String> {
        let inner = self.registry.lock().expect("registry poisoned");
        inner.bindings.get(contract).cloned()
    }

    pub fn push_spec_log(&mut self, entry: Value) {
        const SPEC_LOG_LIMIT: usize = 1024;
        if self.spec_captured_logs.len() >= SPEC_LOG_LIMIT {
            if !self.spec_logs_truncated {
                let notice = json!({
                    "level": "warn",
                    "message": "Spec log buffer truncated",
                    "tags": { "component": "kernel", "scope": "registry-scope", "reason": "log-overflow" }
                });
                self.spec_captured_logs.push(notice);
                self.spec_logs_truncated = true;
            }
            return;
        }
        self.spec_captured_logs.push(entry);
    }

    pub fn spec_captured_logs(&self) -> &[Value] {
        &self.spec_captured_logs
    }

    pub fn push_raw_input(&mut self, value: Value) {
        self.raw_input_stack.push(value);
    }

    pub fn pop_raw_input(&mut self) {
        self.raw_input_stack.pop();
    }

    pub fn current_raw_input(&self) -> Option<&Value> {
        self.raw_input_stack.last()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.http_hosts.stop_all();
    }
}
