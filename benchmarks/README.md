# V8Scope vs Clinic.js benchmark

This benchmark answers two separate questions:

1. How much does the Rust control plane improve CLI startup, offline report processing, memory use, and distribution size?
2. How much overhead does each profiler add to the same Node.js application, and does it finish a valid report?

The target application continues to execute in Node/V8, so its JavaScript throughput remains governed by the same runtime. CPU and heap samples come from Node/V8, and async causality observes Node resources. Rust accelerates orchestration, analysis, report generation, integrity checks, and process cleanup.

## Control-plane method

- V8Scope 0.2.0 and Clinic.js 13.0.0 are installed into separate npm prefixes.
- CLI startup uses 1 warmup and 30 measured invocations in deterministic rotated order.
- V8Scope is measured through both the public npm launcher and its released native binary. This exposes wrapper cost without hiding the user-facing command path.
- Offline report rebuilding uses 1 warmup and 10 measured runs. Every run receives a fresh copy of one equivalent five-second Diagnose or Doctor capture.
- Input copying is excluded from timing. Input byte counts are published because the native formats differ.
- GNU time records wall time and peak RSS.
- Installed bytes and file counts cover each complete production `node_modules` tree. `npm ls --all --omit=dev --parseable` counts installed dependency nodes, and `npm audit --omit=dev` records the registry's findings at measurement time.
- `control-plane.json` retains every startup and report sample plus Q1/Q3.

## Collection method

- One warmup and 10 measured runs per variant.
- Deterministic rotation and reversal of variant order between rounds.
- Autocannon 8.0.0 drives the loopback workload from a process outside both profilers.
- Every run uses the same connections, duration, Node executable, and application fixture.
- Every profiler run gets an empty isolated working directory so trace files from one mode cannot affect another.
- Process-tree RSS is sampled every 100 ms.
- Canonical runs allow 30 seconds for shutdown and report generation after load finishes; the CLI exposes `--timeout` for longer application-specific probes.
- A report succeeds only when the load finishes without errors, the profiler exits normally for its interrupt contract, the expected report is complete, and every observed process group has stopped. Clinic.js maps an interactive interrupt to exit `0`; V8Scope preserves the shell-standard exit `130` while still finalizing a complete run.
- Raw samples, quartiles, exits, stderr, artifact sizes, and cleanup status are retained in `raw.json`.
- Machine-specific repository and temporary paths are redacted from retained stdout and stderr.

The workload combines JavaScript CPU work, short-lived allocations, filesystem promises, timers, and HTTP I/O so all four diagnostic modes observe meaningful activity. The harness follows the warmup, repeated-iteration, raw-result, and separated process-memory conventions used by the [agent-browser native benchmark](https://github.com/vercel-labs/agent-browser/tree/main/benchmarks). [Autocannon](https://github.com/mcollina/autocannon) supplies the load and latency histograms.

## Install the collection harness

```sh
cd benchmarks
npm ci --ignore-scripts
cargo build --release --locked
```

Clinic.js 13.0.0 and Autocannon 8.0.0 are pinned as benchmark-only development dependencies by `package-lock.json`. They never ship in V8Scope's binary or npm package. `npm audit --omit=dev` reports the production dependency surface as empty; the intentionally pinned Clinic comparator retains its published legacy dependency findings. The default V8Scope binary is `../target/release/v8scope`; use `--v8scope-bin` to measure a packaged release.

## Run collection overhead

```sh
npm run benchmark -- \
  --label linux-x64-node22 \
  --results results/2026-08-13-linux-x64-node22
```

Useful overrides:

```sh
node scripts/run.mjs \
  --iterations 10 \
  --warmup 1 \
  --duration 5 \
  --connections 20 \
  --variants baseline,clinic-doctor,v8scope-diagnose \
  --v8scope-bin /path/to/v8scope \
  --results results/local
```

The output directory must be empty. Each run writes:

```text
raw.json
summary.md
```

## Run the Rust control plane

Use separate installation prefixes so npm cannot deduplicate Clinic dependencies into the V8Scope tree:

```sh
control_root=$(mktemp -d)
npm install --prefix "$control_root/v8scope" --no-audit --no-fund v8scope@0.2.0
npm install --prefix "$control_root/clinic" --no-audit --no-fund clinic@13.0.0

node scripts/control-plane.mjs \
  --label ubuntu-24.04-x64-node22 \
  --v8scope-launcher "$control_root/v8scope/node_modules/.bin/v8scope" \
  --v8scope-native "$control_root/v8scope/node_modules/v8scope/node_modules/.bin_real/v8scope" \
  --clinic-bin "$control_root/clinic/node_modules/.bin/clinic" \
  --v8scope-prefix "$control_root/v8scope" \
  --clinic-prefix "$control_root/clinic" \
  --results results/local-control-plane
```

It writes:

```text
control-plane.json
control-plane.md
```

Canonical checked-in results come from the dedicated Ubuntu x64 VPS. GitHub Actions provides a manual smoke workflow for harness changes; hosted-runner numbers are excluded from product claims because shared runners are noisy.

The current canonical directory contains the v0.1.1 collection [summary](results/2026-08-13-ubuntu-x64-node22/summary.md) and [raw JSON](results/2026-08-13-ubuntu-x64-node22/raw.json), plus the v0.2.0 control-plane [summary](results/2026-08-13-ubuntu-x64-node22/control-plane.md) and [raw samples](results/2026-08-13-ubuntu-x64-node22/control-plane.json). V0.2.0 changed package positioning and CLI description while leaving every collector unchanged.

## Interpretation

Lead with report success, finalization, incremental RSS, report rebuild, and distribution surface when evaluating the Rust rewrite. Incremental collection RSS subtracts the baseline Node process tree. Throughput and p99 latency then verify the instrumentation cost on the target application.

The current control-plane result shows the public V8Scope npm command starting 4.7× faster and rebuilding its report 8.2× faster than Clinic. The released native binary starts 34.2× faster and rebuilds the report 9.3× faster. V8Scope's isolated production installation contains 85 files and 3 dependency nodes versus Clinic's 17,909 files and 693 dependency nodes.

These measurements characterize one controlled workload. They do not claim universal application speedups. Use the raw files, rerun on your own workload, and compare distributions before making capacity decisions.
