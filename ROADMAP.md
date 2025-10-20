# Roadmap — lcod-kernel-rs

## M0 — Core runtime
- [x] Load and validate `lcp.toml` (strict TOML + JSON Schema).
- [x] Register contracts, implementations, and flows inside the embedded registry.
- [x] Provide a minimal CLI (`cargo run --bin run_compose`) capable of executing a compose with host bindings.

## M1 — Composition & tests
- [x] Flow operators (`flow/if@1`, `flow/foreach@1`, `flow/break@1`, `flow/continue@1`, `flow/throw@1`).
- [x] Nested slot support (`ctx.run_slot`, `ctx.replace_run_slot_handler`) and scope cleanup.
- [x] Coverage via `cargo test` plus mirrored spec fixtures (`tests/flow_blocks.rs`, `cargo run --bin test_specs`).
- [ ] Complete `flow/parallel@1` and `flow/try@1` (structured error propagation).

## M2 — Tooling & CI
- [ ] Publish a rustfmt/clippy CI workflow.
- [x] Parity tests for `tooling/script@1` (QuickJS sandbox, timeouts, `run_slot`).

## M3 — Runtime parity

Goal: reach functional parity with the Node reference runtime.

Delivered:
- [x] Infrastructure contracts (`core/fs`, `core/http`, `core/git`, `core/hash`, `core/parse`, `core/stream`) via `register_core`.
- [x] Resolver CLI (`cargo run --bin run_compose -- --resolver`) plus workspace helpers (canonical ID handling).
- [x] Shared tooling (`tooling/test_checker@1`, `tooling/script@1`) and conformance diff (driven by `node scripts/run-conformance.mjs`).
- [x] Registry scope chaining via `tooling/registry/scope@1` (scoped contract bindings with automatic restoration; inline helper registration pending).

Next:
- [ ] M3-04b Finalise advanced bindings (git/http, manifest packaging) and document in `docs/runtime-rust.md`.
- [x] Extend scoped registries to handle inline helper/component registration (inline `compose` snippets registered ephemerally within the scope).

## M4 — Observability & logging
- [x] Integrate `lcod://tooling/log@1` once defined in the spec (structured log serialization + host bridges).
- [x] tooling/script: forward `console.*` calls to the logging contract so script logs flow through `tooling/log@1`.
- [x] `lcod-run` CLI exposes its version (`--version`) and surfaces the flag in `--help`; bundle metadata follow-up tracked separately.
- [ ] Add hierarchical log level configuration (`--log kernel=info,resolver=debug`) with sane defaults for CI.
- [x] Instrument compose execution in the kernel (`run_steps`) to emit step/slot lifecycle logs (info/debug/trace) — parity across Rust & JS kernels.
- [ ] Provide a lint entry point that resolves composes, enumerates logger usage, and validates against `lcp.toml[logging]` metadata (align with spec M4-05).
- [ ] Expose a trace mode (`--trace`) in `run_compose` to inspect scope/slot mutations.

## M5 — Packaging & distribution
- [ ] Publish a crate/binary `lcod-kernel-rs-cli`.
- [ ] Implement `--assemble/--ship/--build` (aligned with the spec packaging roadmap).
- [x] Keep `tooling/compose/normalize@1` aligned with the spec.
- [x] Integrate the shared runtime bundle (`LCOD_HOME`): download, checksum/signature verification, decompression.
- [x] Resolve composes/axioms from the bundle by default (fallback to `SPEC_REPO_PATH` for developers).
- [x] Add an integration test running `tooling/registry/catalog/generate@*` via the bundle to ensure parity with the Node kernel.
- [x] Release tarball embeds the runtime bundle so `run-compose` works standalone (refs #16).
- [ ] Build the autonomous `lcod-run` CLI (embedded bundle, resolver, caching UX) — coordinate with the spec draft `docs/lcod-run-cli.md`.
  - [ ] Prepare release workflow for cross-platform binaries (bundle embed + artefacts).

## M6 — Service demo
- [x] HTTP demo (`env/http_host@0.1.0`, `project/http_app@0.1.0`): parity with Node plus tests.

## M8 — Standard library primitives
- [x] M8-02 Implement `core/object/merge@1`, `core/array/append@1`, `core/string/format@1`,
  and `core/json/{encode,decode}@1`; added unit coverage and spec fixture `std_primitives`.
- [ ] M8-03 Wire the primitives into higher-level toolchains (resolver/registry) once the JS substrate is updated.
