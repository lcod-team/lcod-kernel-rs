# Roadmap — Rust Kernel (lcod-kernel-rs)

## M0 — Core format
- [ ] Load and validate `lcp.toml` descriptors (parsing + JSON Schema enforcement).
- [ ] Register spec packages (contracts, implementations, flows) from disk and expose bindings.
- [ ] Provide a minimal CLI to execute a compose with host-provided bindings.

## M1 — Composition & tests
- [x] Implement the core flow set (`flow/if@1`, `flow/foreach@1`, `flow/break@1`, `flow/continue@1`, `flow/throw@1`).
- [x] Preserve slot state across nested flows (`ctx.runSlot` / `ctx.runChildren`) and scope cleanup.
- [x] Add regression tests mirroring the spec `foreach` scenarios (`tests/flow_blocks.rs`).
- [ ] Support `flow/parallel@1`, `flow/try@1` (catch/finally semantics) and structured error propagation.
- [ ] Publish continuous integration using `cargo test` and Rustfmt.

## M3 — Runtime substrates
- [x] M3-04a: Boot the Rust substrate skeleton (compose runner, registry, stream manager) and execute the spec streaming compose.
- [ ] M3-04b: Implement the core contract matrix (`core/fs`, `core/http`, `core/stream`, `core/git`, `core/hash`, `core/parse`) against native Rust APIs.
- [ ] M3-05: Join the cross-runtime conformance harness (diff Node vs Rust outputs on shared fixtures).
  - [ ] Execute spec `tooling/test_checker@1` suites and publish parity reports
- [ ] M3-06: Expose configurable sandbox hooks (`$api.run`, `$api.config`) for embedded logic.

## M4 — Packaging & distribution
- [ ] Package the Rust runtime as a reusable crate and binary (`lcod-kernel-rs-cli`).
- [ ] Document release process, versioning and contract compatibility matrix.
