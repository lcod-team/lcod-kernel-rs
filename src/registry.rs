use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::Value;

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
}

impl RegistryInner {
    fn new() -> Self {
        Self {
            funcs: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
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
}

impl Context {
    fn new(registry: Arc<Mutex<RegistryInner>>) -> Self {
        Self {
            registry,
            scope_depth: 0,
            run_slot_handler: None,
            streams: StreamManager::new(),
        }
    }

    pub fn call(&mut self, name: &str, input: Value, meta: Option<Value>) -> Result<Value> {
        let func = {
            let inner = self.registry.lock().expect("registry poisoned");
            inner.funcs.get(name).cloned()
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
}
