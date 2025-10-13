# run_compose CLI

The `run_compose` binary embeds the Rust kernel and tooling so that compositions
can be executed without a Node.js runtime. It mirrors the JavaScript runner and
is intended for automation scenarios (CI jobs, cron tasks, etc.).

## Usage

```bash
run-compose --compose path/to/compose.yaml \
            [--state path/to/state.json] \
            [--project path/to/project] \
            [--config path/to/resolve.config.json] \
            [--output path/to/lcp.lock] \
            [--cache-dir path/to/cache] \
            [--serve]
```

Flags:

- `--compose`, `-c` – path to the compose YAML/JSON document *(required)*.
- `--state`, `-s` – optional initial state JSON file.
- `--project`, `--config`, `--output`, `--cache-dir` – overrides for resolver
  composes (mirrors the JavaScript CLI).
- `--serve` – keep HTTP hosts started by the composition alive until Ctrl+C.

## Example (registry refresh)

```bash
run-compose \
  --compose tooling/registry/catalog/refresh.yaml \
  --state tooling/registry/catalog/state.json \
  --output ./build/catalog.lcp.lock
```

This generates the registry catalog lockfile using only the Rust binary. The
registry CI can download the artifact produced by this repository and invoke
the same command instead of running the Node.js tooling.

## Environment

- `SPEC_REPO_PATH` – path to the `lcod-spec` checkout or runtime bundle.
- `LCOD_RESOLVER_PATH` – path to the resolver workspace (required for resolver
  helpers).
- `LCOD_HOME` – optional path to a packaged runtime bundle (overrides the two
  variables above).

If the binary runs next to the spec/resolver repositories or the runtime bundle,
no additional setup is needed.

## Exit codes

- `0` – composition succeeded.
- `> 0` – fatal error (invalid compose file, missing helper, runtime failure).

Log lines are emitted as JSON objects on stdout/stderr using
`lcod://contract/tooling/log@1`. Bind this contract to redirect logs to your
preferred sink or use tools such as `jq` to filter the output.
