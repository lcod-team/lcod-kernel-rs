# lcod-kernel-rs

Rust reference implementation of the LCOD kernel. It exposes:

- A lightweight `Registry`/`Context` to call contracts, implementations and flow blocks.
- Compose runner with slot orchestration and stream handles.
- Minimal tooling (demo registry, test harness) mirroring the JavaScript substrate.

See [`docs/runtime-rust.md`](https://github.com/lcod-team/lcod-spec/blob/main/docs/runtime-rust.md) in the spec repository for the
architecture blueprint and current contract coverage. In short:

- Filesystem, hashing, parsing and stream contracts are implemented with parity tests.
- HTTP and Git bindings are implemented via `curl`/libgit2 and now power resolver-style
  flows end-to-end (buffered + stream responses, refs/depth/subdir handling).
- `register_resolver_axioms(&Registry)` aliases the available contracts under
  their `axiom://` identifiers so the resolver example can run with the Rust substrate.

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
