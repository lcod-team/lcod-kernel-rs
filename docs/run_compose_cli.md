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

- `SPEC_REPO_PATH` – path to the `lcod-spec` checkout or runtime bundle (useful
  for local development).
- `LCOD_RESOLVER_PATH` – path to the resolver workspace (only required when the
  runtime bundle is not available).
- `LCOD_HOME` – optional path to a packaged runtime bundle; when unset the CLI
  looks for a `runtime/` directory next to the executable (this is how the
  published tarball is structured) and falls back to the two variables above.

When you extract a release archive, the runtime bundle ships alongside the
binary, so no additional setup is required. For development checkouts you can
still point the environment variables at local repositories.

## Distribution

- The “Build Binary” workflow packages a Linux tarball (`run-compose-linux-x86_64.tar.gz`) as a GitHub Actions artefact on every push to `main`.
- Tagging the repository with `v*.*.*` (or `run-compose-v*.*.*`) triggers the “Release Binary” workflow, which now embeds the runtime bundle under `runtime/` and publishes the archive alongside a SHA-256 checksum. Downstream pipelines can download this artefact and run `run-compose` without cloning `lcod-spec`.

## Exit codes

- `0` – composition succeeded.
- `> 0` – fatal error (invalid compose file, missing helper, runtime failure).

Log lines are emitted as JSON objects on stdout/stderr using
`lcod://contract/tooling/log@1`. Bind this contract to redirect logs to your
preferred sink or use tools such as `jq` to filter the output.
