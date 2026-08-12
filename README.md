# V8Scope

V8Scope is a Rust-first performance diagnostics toolkit for Node.js. It captures standard V8 CPU and heap profiles, event-loop and process telemetry, opt-in async causality, and offline reports. The raw profiles open directly in Chrome DevTools and Speedscope.

It covers Clinic.js Doctor, Flame, Heap Profiler, and Bubbleprof's maintained user-facing diagnostic workflows with one native CLI, versioned JSON contracts, running-process attachment, repeatable workload capture, and CI performance budgets.

## Requirements

- Node.js 22, 24, or 26
- macOS on arm64 or x64, or Linux on x64

## Install

For development from this checkout:

```sh
cargo install --path .
```

Tagged releases produce a shell installer, a Homebrew formula, an npm package, checksums, CycloneDX SBOMs, and GitHub build attestations. Other targets can build from source with Cargo.

```sh
npm install --global v8scope
brew tap Ryan-yang125/tap
brew trust --formula Ryan-yang125/tap/v8scope
brew install v8scope
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Ryan-yang125/v8scope/releases/latest/download/v8scope-installer.sh | sh
```

## Diagnose an application

```sh
v8scope diagnose -- node server.js
```

Capture for a fixed interval:

```sh
v8scope diagnose --duration 30s --open -- node server.js
```

Wait for a server and drive repeatable HTTP load:

```sh
v8scope all \
  --ready-url http://127.0.0.1:3000/health \
  --load-url http://127.0.0.1:3000/api/items \
  --connections 20 \
  --rate 200 \
  --load-duration 30s \
  -- node server.js
```

`diagnose` captures low-overhead telemetry and CPU data. `cpu`, `heap`, and `async` isolate one diagnostic mode. `all` enables every collector. Async causality uses `async_hooks`, records bounded stack and lifecycle events, separates resource wait from callback execution, and builds an interactive aggregate topology. It carries measurable overhead; select it for focused captures.

## Attach to a running process

Start Node with a loopback Inspector endpoint:

```sh
node --inspect=127.0.0.1:9229 server.js
v8scope attach --url http://127.0.0.1:9229 --mode all --duration 30s --open
```

V8Scope accepts Inspector discovery HTTP URLs and direct WebSocket URLs. Remote endpoints require `--allow-remote-inspector`. The Inspector target identifier is excluded from the manifest.

## Compare runs in CI

Copy [v8scope.toml.example](v8scope.toml.example) to `v8scope.toml`, then compare two run directories:

```sh
v8scope compare .v8scope/baseline .v8scope/candidate --config v8scope.toml
```

Exit code `0` means the budgets pass, `10` means a budget failed, and `70` means the runs are incomparable or the command failed. Comparability requires finished, non-partial runs with matching collectors, mode, Node major, V8 major, OS, and architecture. Compare validates every configured metric and the complete artifact inventory before applying budgets.

## Run artifacts

Each run is self-contained:

```text
manifest.json
summary.json
comparison.json
telemetry.ndjson
process.ndjson
profiles/cpu/*.cpuprofile
profiles/heap/*.heapprofile
profiles/async/events.ndjson
report/index.html
report/assets/cpu-flamegraph.svg
```

`manifest.json`, `summary.json`, and `comparison.json` are schema-versioned. Generate their JSON Schemas with `v8scope schema`. Absolute project paths are redacted from manifest, summary, and report by default; use `--redact-paths=false` only for trusted local output. Raw V8 profiles can still contain file names, function names, source locations, URLs, and runtime values. Every mutating command refreshes the manifest's exact file inventory, byte counts, and SHA-256 values.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
```

The suite runs real Node/V8 and Inspector end-to-end tests. The [Clinic baseline](docs/clinic-test-baseline.md) pins five upstream commits, reconstructs all 141 executable upstream test paths, and binds every replacement capability to executable Rust tests. See [architecture](docs/architecture.md) for the V8 and CDP decisions.

Release operators should follow the [release checklist](docs/releasing.md).

## License

MIT. See [NOTICE.md](NOTICE.md) for research and upstream acknowledgements.
