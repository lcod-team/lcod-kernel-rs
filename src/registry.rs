use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::Value;

pub trait Func: Send + Sync {
    fn call(&self, input: Value) -> Result<Value>;
}

impl<F> Func for F
where
    F: Fn(Value) -> Result<Value> + Send + Sync + 'static,
{
    fn call(&self, input: Value) -> Result<Value> {
        (self)(input)
    }
}

#[derive(Default)]
pub struct Registry {
    funcs: HashMap<String, Arc<dyn Func>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            funcs: HashMap::new(),
        }
    }

    pub fn register<F>(&mut self, name: impl Into<String>, func: F)
    where
        F: Func + 'static,
    {
        self.funcs.insert(name.into(), Arc::new(func));
    }

    pub fn call(&self, name: &str, input: Value) -> Result<Value> {
        let Some(func) = self.funcs.get(name) else {
            return Err(anyhow!("function not found: {name}"));
        };
        func.call(input)
    }
}
