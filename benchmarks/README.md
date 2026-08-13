# V8Scope vs Clinic.js benchmark

This benchmark compares the collection overhead and end-to-end reliability of V8Scope and Clinic.js against the same Node.js application and the same external HTTP load.

## Method

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

## Install

```sh
cd benchmarks
npm ci --ignore-scripts
cargo build --release --locked
```

Clinic.js 13.0.0 and Autocannon 8.0.0 are pinned as benchmark-only development dependencies by `package-lock.json`. They never ship in V8Scope's binary or npm package. `npm audit --omit=dev` reports the production dependency surface as empty; the intentionally pinned Clinic comparator retains its published legacy dependency findings. The default V8Scope binary is `../target/release/v8scope`; use `--v8scope-bin` to measure a packaged release.

## Run

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

Canonical checked-in results come from the dedicated Ubuntu x64 VPS. GitHub Actions provides a manual smoke workflow for harness changes; hosted-runner numbers are excluded from product claims because shared runners are noisy.

The current canonical [summary](results/2026-08-13-ubuntu-x64-node22/summary.md) and [raw JSON](results/2026-08-13-ubuntu-x64-node22/raw.json) are checked in with the harness.

## Interpretation

Throughput and p99 latency are measured during collection. Finalize time covers profiler shutdown, profile processing, and report generation after load ends. Peak RSS includes the profiler and its current descendants, which captures the runtime cost of the CLI as well as the instrumented Node process.

These measurements characterize one controlled workload. They do not claim universal application speedups. Use the raw files, rerun on your own workload, and compare distributions before making capacity decisions.
