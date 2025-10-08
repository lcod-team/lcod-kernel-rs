use lcod_kernel_rs::compose::parse_compose;
use serde_json::json;

#[test]
fn parse_compose_expands_identity_inputs_and_outputs() {
    let steps = parse_compose(&json!([
        {
            "call": "lcod://impl/echo@1",
            "in": {
                "foo": "-",
                "nested": { "path": "value" }
            },
            "out": {
                "bar": "-"
            }
        }
    ])).expect("compose parsed");

    let step = &steps[0];
    assert_eq!(step.inputs.get("foo").unwrap(), "$.foo");
    assert_eq!(step.out.get("bar").unwrap(), "bar");
}

#[test]
fn parse_compose_normalizes_children() {
    let steps = parse_compose(&json!([
        {
            "call": "lcod://flow/if@1",
            "in": { "cond": "-" },
            "children": {
                "then": [
                    {
                        "call": "lcod://impl/echo@1",
                        "in": { "value": "-" },
                        "out": { "result": "-" }
                    }
                ]
            }
        }
    ])).expect("compose parsed");

    let step = &steps[0];
    assert_eq!(step.inputs.get("cond").unwrap(), "$.cond");
    if let Some(children) = &step.children {
        match children {
            lcod_kernel_rs::compose::StepChildren::Map(map) => {
                let then_steps = &map["then"];
                let child = &then_steps[0];
                assert_eq!(child.inputs.get("value").unwrap(), "$.value");
                assert_eq!(child.out.get("result").unwrap(), "result");
            }
            lcod_kernel_rs::compose::StepChildren::List(_) => panic!("unexpected list form"),
        }
    } else {
        panic!("missing children");
    }
}
