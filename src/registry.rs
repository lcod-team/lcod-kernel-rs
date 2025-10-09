use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::Value;

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
        let func = {
            let inner = self.inner.lock().expect("registry poisoned");
            inner.funcs.get(name).cloned()
        };
        let Some(func) = func else {
            return Err(anyhow!("function not found: {name}"));
        };
        func.call(ctx, input, meta)
    }

    pub fn context(&self) -> Context {
        Context::new(self.inner.clone())
    }
}

pub struct Context {
    registry: Arc<Mutex<RegistryInner>>,
    scope_depth: usize,
    run_slot_handler: Option<Box<dyn SlotExecutor + 'static>>,
    streams: StreamManager,
    http_hosts: HttpHostManager,
    registry_scope_stack: Vec<HashMap<String, String>>,
}

impl Context {
    fn new(registry: Arc<Mutex<RegistryInner>>) -> Self {
        Self {
            registry,
            scope_depth: 0,
            run_slot_handler: None,
            streams: StreamManager::new(),
            http_hosts: HttpHostManager::new(),
            registry_scope_stack: Vec::new(),
        }
    }

    pub fn call(&mut self, name: &str, input: Value, meta: Option<Value>) -> Result<Value> {
        let func = {
            let inner = self.registry.lock().expect("registry poisoned");
            if let Some(func) = inner.funcs.get(name) {
                Some(func.clone())
            } else if name.starts_with("lcod://contract/") {
                inner
                    .bindings
                    .get(name)
                    .and_then(|binding| inner.funcs.get(binding).cloned())
            } else {
                None
            }
        };
        let Some(func) = func else {
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
        let mut handler = self
            .run_slot_handler
            .take()
            .ok_or_else(|| anyhow!("runSlot not available in this context"))?;
        let local = local_state.unwrap_or(Value::Null);
        let slot = slot_vars.unwrap_or(Value::Null);
        let result = handler.run_slot(self, name, local, slot);
        self.run_slot_handler = Some(handler);
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
        let current = {
            let inner = self.registry.lock().expect("registry poisoned");
            inner.bindings.clone()
        };
        self.registry_scope_stack.push(current.clone());
        let mut merged = current;
        if let Some(overrides) = bindings {
            for (contract, implementation) in overrides {
                merged.insert(contract, implementation);
            }
        }
        {
            let mut inner = self.registry.lock().expect("registry poisoned");
            inner.bindings = merged;
        }
        Ok(())
    }

    pub fn leave_registry_scope(&mut self) -> Result<()> {
        if let Some(previous) = self.registry_scope_stack.pop() {
            let mut inner = self.registry.lock().expect("registry poisoned");
            inner.bindings = previous;
        }
        Ok(())
    }

    pub fn fork(&self) -> Context {
        Context::new(self.registry.clone())
    }

    pub fn registry_clone(&self) -> Registry {
        Registry {
            inner: self.registry.clone(),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.http_hosts.stop_all();
    }
}
