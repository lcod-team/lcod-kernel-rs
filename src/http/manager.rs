use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tiny_http::Server;

pub struct HttpHostControl {
    server: Arc<Server>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HttpHostControl {
    pub fn new(server: Arc<Server>, running: Arc<AtomicBool>, thread: JoinHandle<()>) -> Self {
        running.store(true, Ordering::SeqCst);
        Self {
            server,
            running,
            thread: Some(thread),
        }
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.running.swap(false, Ordering::SeqCst) {
            let _ = self.server.unblock();
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Drop for HttpHostControl {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Default)]
pub struct HttpHostManager {
    next_id: u64,
    hosts: HashMap<String, HttpHostControl>,
}

impl HttpHostManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, control: HttpHostControl) -> Value {
        self.next_id += 1;
        let handle_id = format!("http-host-{}", self.next_id);
        self.hosts.insert(handle_id.clone(), control);
        json!({
            "id": handle_id,
            "type": "lcod://env/http_host@0.1.0"
        })
    }

    pub fn stop(&mut self, handle: &Value) -> Result<Value> {
        let id = extract_handle_id(handle)?;
        if let Some(mut control) = self.hosts.remove(&id) {
            control.stop()?;
            Ok(json!({ "stopped": true }))
        } else {
            Err(anyhow!("Unknown HTTP host handle: {id}"))
        }
    }

    pub fn stop_all(&mut self) {
        let mut hosts = HashMap::new();
        std::mem::swap(&mut self.hosts, &mut hosts);
        for (_id, mut control) in hosts {
            let _ = control.stop();
        }
    }
}

fn extract_handle_id(handle: &Value) -> Result<String> {
    let obj = handle
        .as_object()
        .ok_or_else(|| anyhow!("Invalid HTTP host handle"))?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("HTTP host handle missing id"))?;
    Ok(id.to_string())
}
