use std::collections::HashMap;
use std::fmt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

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
}

#[derive(Debug)]
pub struct CancelledError;

impl fmt::Display for CancelledError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "execution cancelled")
    }
}

impl std::error::Error for CancelledError {}

struct RegistryInner {
    funcs: HashMap<String, Arc<dyn Func>>,
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
    funcs: HashMap<String, Arc<dyn Func>>,
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
        let mut inner = self.inner.lock().expect("registry poisoned");
        inner.funcs.insert(name.into(), Arc::new(func));
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
        let mut missing_contract_binding = false;
        let func = {
            let inner = self.inner.lock().expect("registry poisoned");
            if let Some(entry) = inner.funcs.get(name) {
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
            }
        };
        let Some(func) = func else {
            if missing_contract_binding {
                return Err(anyhow!("No binding for contract: {name}"));
            }
            return Err(anyhow!("function not found: {name}"));
        };
        func.call(ctx, input, meta)
    }

    pub fn context(&self) -> Context {
        Context::new(self.inner.clone(), Arc::new(AtomicBool::new(false)))
    }

    pub fn context_with_cancellation(&self, token: Arc<AtomicBool>) -> Context {
        Context::new(self.inner.clone(), token)
    }
}

pub struct Context {
    registry: Arc<Mutex<RegistryInner>>,
    scope_depth: usize,
    run_slot_handler: Option<Box<dyn SlotExecutor + 'static>>,
    streams: StreamManager,
    http_hosts: HttpHostManager,
    registry_scope_stack: Vec<RegistrySnapshot>,
    log_tag_stack: Vec<Map<String, Value>>,
    spec_captured_logs: Vec<Value>,
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
            spec_captured_logs: Vec::new(),
            cancellation,
        }
    }

    pub fn call(&mut self, name: &str, input: Value, meta: Option<Value>) -> Result<Value> {
        self.ensure_not_cancelled()?;
        let mut missing_contract_binding = false;
        let func = {
            let inner = self.registry.lock().expect("registry poisoned");
            if let Some(entry) = inner.funcs.get(name) {
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
            }
        };
        let Some(func) = func else {
            if missing_contract_binding {
                return Err(anyhow!("No binding for contract: {name}"));
            }
            return Err(anyhow!("function not found: {name}"));
        };
        func.call(self, input, meta)
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
        cloned.spec_captured_logs = self.spec_captured_logs.clone();
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
        self.spec_captured_logs.push(entry);
    }

    pub fn spec_captured_logs(&self) -> &[Value] {
        &self.spec_captured_logs
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.http_hosts.stop_all();
    }
}
