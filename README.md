# lcod-kernel-rs

Rust reference implementation of the LCOD kernel. It exposes:

- A lightweight `Registry`/`Context` to call contracts, implementations and flow blocks.
- Compose runner with slot orchestration and stream handles.
- Minimal tooling (demo registry, test harness) mirroring the JavaScript substrate.
- Core library primitives (`core/object`, `core/array`, `core/string`, `core/json`) published as axioms so most `tooling/script@1` use-cases can be expressed declaratively.

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
SPEC_REPO_PATH=/path/to/lcod-spec LCOD_SPEC_PATH=/path/to/lcod-spec cargo run --bin test_specs
# execute shared spec fixtures from lcod-spec/tests/spec (std_primitives included)
cargo run --bin run_compose -- --compose ../lcod-spec/examples/env/http_demo/compose.yaml --serve
# run an LCOD compose (registers core/flow/tooling/http) and keep servers alive until Ctrl+C
# resolver helpers: --project <dir>, --config <file>, --output <file>, --cache-dir <dir>
#   e.g. cargo run --bin run_compose -- --compose ../lcod-resolver/compose.yaml \
#        --project ./examples/tooling/resolver --output ./lcp.lock --cache-dir /tmp/lcod-cache
```

The spec fixtures require the `lcod-spec` repository to be accessible. By default
we look for sibling directories; override with `SPEC_REPO_PATH=/path/to/lcod-spec`
and `LCOD_SPEC_PATH=/path/to/lcod-spec` when running locally or in CI.

## Prebuilt CLI

The CI publishes a `run_compose` binary artefact for Linux on every push to `main`. Tagging the repository with `v*.*.*` (or `run-compose-v*.*.*`) now creates a GitHub Release containing the same tarball plus a checksum, so downstream jobs can pin exact versions. See [`docs/run_compose_cli.md`](docs/run_compose_cli.md) for details.

## Shared fixtures

The reusable compose fixtures live under `lcod-spec/tests/spec` and are executed
through both kernels. They rely on `tooling/test_checker@1` and provide parity
coverage across substrates (`foreach` demos, streaming, scripting, slots).
## Windows setup

This repository defaults to the MinGW toolchain when running on Windows.
Install MSYS2 (or another MinGW distribution) so that `C:/msys64/mingw64/bin` provides `gcc`, `dlltool`, and companions,
then install the GNU Rust toolchain and set the override once inside the checkout:

```powershell
$Env:Path = 'C:/msys64/mingw64/bin;' + $Env:Path  # make dlltool/gcc reachable
D:/DevDrive/pkg-cache/cargo/bin/rustup.exe override set stable-x86_64-pc-windows-gnu
# afterwards plain `cargo test` will reuse the override
```

The `.cargo/config.toml` file pins the MinGW linker so CI and Windows developers stay aligned.
