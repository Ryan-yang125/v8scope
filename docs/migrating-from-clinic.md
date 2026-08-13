# Migrate from Clinic.js

V8Scope replaces the public user-facing workflows of Clinic.js Doctor, Flame, Heap Profiler, and Bubbleprof with one maintained native CLI. Start with the command map, then update automation to consume V8Scope's versioned JSON artifacts.

## Install

```sh
npm install --global v8scope
```

Homebrew and the shell installer are also available from the [README](../README.md#install).

## Command map

| Clinic.js | V8Scope | Purpose |
| --- | --- | --- |
| `clinic doctor -- node server.js` | `v8scope diagnose -- node server.js` | First-pass CPU, event-loop, GC, memory, and resource diagnosis |
| `clinic flame -- node server.js` | `v8scope cpu -- node server.js` | CPU profile, hotspots, and flame graph |
| `clinic heapprofiler -- node server.js` | `v8scope heap -- node server.js` | Sampling heap profile and allocation hotspots |
| `clinic bubbleprof -- node server.js` | `v8scope async -- node server.js` | Async resource causality and callback timing |
| Run several Clinic tools separately | `v8scope all -- node server.js` | CPU, heap, runtime, and async data in one capture |
| `clinic TOOL --visualize-only DATA` | `v8scope analyze RUN_DIR` | Rebuild a report from captured artifacts |

## Option map

| Clinic.js option | V8Scope equivalent |
| --- | --- |
| `--dest .clinic` | `--output .v8scope` |
| `--name checkout` | `--name checkout` |
| `--open=false` | Default behavior; add `--open` to open the report |
| `--collect-only` | `--no-report` |
| `--visualize-only DATA` | `v8scope analyze RUN_DIR` |
| `--on-port 'COMMAND'` | `--ready-url URL --on-ready 'COMMAND'` |
| `--autocannon [...]` | `--ready-url URL --load-url URL` plus `--connections`, `--rate`, and `--load-duration` |
| Manual Ctrl+C after a fixed window | `--duration 30s` |

Example HTTP workload migration:

```sh
v8scope diagnose \
  --ready-url http://127.0.0.1:3000/health \
  --load-url http://127.0.0.1:3000/api/items \
  --connections 20 \
  --rate 200 \
  --load-duration 30s \
  -- node server.js
```

## Attach to a running process

Start the target with a loopback Inspector endpoint, then attach without relaunching it:

```sh
node --inspect=127.0.0.1:9229 server.js
v8scope attach --url http://127.0.0.1:9229 --mode all --duration 30s --open
```

Remote Inspector access requires `--allow-remote-inspector`. Treat an Inspector endpoint as privileged access and secure the network path.

## Artifact contract

Clinic's private `.clinic-*` data, JavaScript module APIs, and report DOM do not carry forward. V8Scope writes standard `.cpuprofile` and `.heapprofile` files, schema-versioned `manifest.json`, `summary.json`, and `comparison.json`, a self-contained HTML report, and a SHA-256 inventory.

Open raw CPU and heap profiles in Chrome DevTools. CPU profiles also open in Speedscope. Use `v8scope schema` to generate machine-readable schemas for integrations.

V8Scope cannot import existing Clinic private data. Keep Clinic installed only for reports that still require `--visualize-only`, and use V8Scope for new captures.

## Automation behavior

- Node.js 22, 24, and 26 are tested.
- Prebuilt releases support macOS arm64, macOS x64, and Linux x64.
- Interactive Ctrl+C produces a complete report and preserves shell-standard interrupt status `130`.
- `v8scope compare` returns `0` when budgets pass, `10` when a budget fails, and `70` for invalid or incomparable runs.
- Absolute project paths are redacted from analyzed output by default. Raw V8 profiles can still contain application details.

See the [Clinic.js comparison](comparison-with-clinic.md), [artifact architecture](architecture.md), and [141-test traceability baseline](clinic-test-baseline.md) for the complete contract.
