# lcod-kernel-rs

Rust reference implementation of the LCOD kernel. It exposes:

- A lightweight `Registry`/`Context` to call contracts, implementations and flow blocks.
- Compose runner with slot orchestration and stream handles.
- Minimal tooling (demo registry, test harness) mirroring the JavaScript substrate.

## Running the tests

```bash
cargo test                 # kernel unit/integration tests
cargo run --bin test_specs # execute shared spec fixtures from lcod-spec/tests/spec
```

The spec fixtures require the `lcod-spec` repository to be accessible. By default
we look for sibling directories; override with `SPEC_REPO_PATH=/path/to/lcod-spec`
when running locally or in CI.

## Shared fixtures

The reusable compose fixtures live under `lcod-spec/tests/spec` and are executed
through both kernels. They rely on `tooling/test_checker@1` and provide parity
coverage across substrates (`foreach` demos, streaming, scripting, slots).
