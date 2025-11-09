use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use lcod_kernel_rs::compose::parse_compose;
use lcod_kernel_rs::{
    register_compose_contracts, register_core, register_flow, register_http_contracts,
    register_tooling, run_compose, Context, Registry,
};
use serde_json::{json, Map, Value};

#[test]
fn env_http_host_serves_routes() -> Result<()> {
    let registry = Registry::new();
    register_core(&registry);
    register_flow(&registry);
    register_compose_contracts(&registry);
    register_tooling(&registry);
    register_http_contracts(&registry);

    let compose_value = json!([
        {
            "call": "lcod://env/http_host@0.1.0",
            "in": {
                "host": "127.0.0.1",
                "port": 0,
                "basePath": "/api"
            },
            "children": {
                "projects": [
                    {
                        "call": "lcod://project/http_app@0.1.0",
                        "in": {
                            "name": "catalog",
                            "basePath": "/catalog"
                        },
                        "out": { "project": "$" },
                        "children": {
                            "sequences": [
                                {
                                    "call": "lcod://tooling/script@1",
                                    "in": {
                                        "source": "async () => ({ sequences: [{ id: 'catalog.list', handler: { type: 'script', source: \"async () => ({ status: 200, body: [{ id: 1, name: 'Keyboard' }] })\" } }] })"
                                    },
                                    "out": { "sequences": "sequences" }
                                }
                            ],
                            "apis": [
                                {
                                    "call": "lcod://tooling/script@1",
                                    "in": {
                                        "source": "async () => ({ routes: [{ method: 'GET', path: '/items', sequenceId: 'catalog.list' }] })"
                                    },
                                    "out": { "routes": "routes" }
                                },
                                {
                                    "call": "lcod://flow/foreach@1",
                                    "in": { "list": "$.routes" },
                                    "children": {
                                        "body": [
                                            {
                                                "call": "lcod://http/api_route@0.1.0",
                                                "in": {
                                                    "method": "$slot.item.method",
                                                    "path": "$slot.item.path",
                                                    "sequenceId": "$slot.item.sequenceId"
                                                },
                                                "out": { "route": "route" }
                                            }
                                        ]
                                    },
                                    "collectPath": "$.route",
                                    "out": { "routes": "results" }
                                }
                            ]
                        }
                    }
                ]
            },
            "out": { "host": "$" }
        }
    ]);

    let compose_steps = parse_compose(&compose_value)?;
    let mut ctx: Context = registry.context();
    let result = run_compose(&mut ctx, &compose_steps, Value::Object(Map::new()))?;
    let host = result
        .get("host")
        .cloned()
        .ok_or_else(|| anyhow!("host output missing"))?;
    let url = host
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("host url missing"))?;

    let endpoint = format!("{}/catalog/items", url.trim_end_matches('/'));

    let body_json: Value;
    let mut attempts = 0;
    loop {
        attempts += 1;
        match ureq::get(&endpoint).call() {
            Ok(response) => {
                let text = response.into_string()?;
                body_json = serde_json::from_str(&text)?;
                break;
            }
            Err(err) => {
                if attempts >= 10 {
                    return Err(anyhow!(
                        "unable to reach HTTP host after {} attempts: {}",
                        attempts,
                        err
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let items = body_json
        .as_array()
        .ok_or_else(|| anyhow!("response body should be an array"))?;
    assert_eq!(items.len(), 1);
    let first = items
        .get(0)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("missing first item object"))?;
    assert_eq!(
        first
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "Keyboard"
    );

    if let Some(handle) = host.get("handle") {
        let _ = ctx.stop_http_host(handle);
    }

    Ok(())
}
