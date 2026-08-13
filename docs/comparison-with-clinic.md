# V8Scope and Clinic.js

V8Scope is an independent, maintained replacement for Clinic.js's four public diagnostic workflows. It preserves V8 and Node as the sources of profiling truth while moving orchestration, analysis, process containment, comparison, and reporting into Rust.

Clinic.js currently opens its README with a maintenance warning: its close coupling to Node internals can cause failures or inaccurate results. Its latest npm version is 13.0.0. V8Scope targets active Node LTS and current release lines through stable V8 profile flags, Node performance APIs, and the Inspector protocol.

Sources: [Clinic.js README](https://github.com/clinicjs/node-clinic#readme), [Clinic.js 13.0.0](https://www.npmjs.com/package/clinic/v/13.0.0), [Node CPU profiler flags](https://nodejs.org/api/cli.html#--cpu-prof), and [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/).

## Capability comparison

| Capability | Clinic.js 13.0.0 | V8Scope |
| --- | --- | --- |
| First-pass diagnosis | Doctor | `v8scope diagnose` |
| CPU flame graph | Flame | `v8scope cpu` |
| Sampling heap profile | Heap Profiler | `v8scope heap` |
| Async causality | Bubbleprof | `v8scope async` |
| Combined capture | Separate tool runs | `v8scope all` |
| Attach to a running process | No documented attach workflow | `v8scope attach` through Node Inspector |
| Raw profile interoperability | Clinic-specific capture directories | Standard `.cpuprofile` and `.heapprofile` plus JSON contracts |
| Offline HTML report | Yes | Yes, self-contained with no remote scripts |
| CI performance budgets | No built-in run comparison contract | Fail-closed `v8scope compare` with exit codes and TOML budgets |
| Artifact integrity | No published hash inventory | File sizes and SHA-256 recorded in `manifest.json` |
| Process and worker handling | Tied to Clinic's Node instrumentation | Process-tree containment, worker/cluster propagation, graceful flush, forced-stop partial state |
| Supported Node policy | README says Node `>=16` with a maintenance warning | Node 22, 24, and 26 tested in CI and release gates |
| Prebuilt platforms | npm CLI | macOS arm64, macOS x64, and Linux x64 native binaries through npm, Homebrew, and shell installer |
| Programmable integration | JavaScript module APIs | CLI, versioned JSON, JSON Schema, and stable exit codes |
| Existing Clinic data | `--visualize-only` | New V8Scope captures only |

## Reliability baseline

V8Scope pins five Clinic repositories and reconstructs all 141 executable upstream test paths. Every path maps to a named Rust capability test, and CI verifies both the pinned upstream tree and the mapping. The native suite also runs real Node launch, worker, cluster, Inspector, signal, readiness, load, artifact hash, and comparison tests.

This is a traceability baseline for public behavior. V8Scope's CLI, artifact schema, HTML DOM, and internal architecture start at version 1 and remain independent.

## Current Node failure modes

The design and test suite directly cover failure classes reported by Clinic users, including missing reports on newer Node versions, worker propagation failures, decode failures, incomplete shutdown, and requests to attach to an existing process. The public reports remain useful context; each V8Scope capability is backed by local executable tests rather than an assumption that every upstream issue has the same root cause.

- [Report generation on Node 21.7+](https://github.com/clinicjs/node-clinic/issues/480)
- [Worker `NODE_OPTIONS` failure](https://github.com/clinicjs/node-clinic/issues/469)
- [Trace decoding failure](https://github.com/clinicjs/node-clinic/issues/481)
- [Outdated dependency report](https://github.com/clinicjs/node-clinic/issues/482)
- [Attach to an existing process request](https://github.com/clinicjs/node-clinic/issues/461)

## Performance evidence

The checked-in [VPS benchmark](../benchmarks/README.md) separates Rust control-plane work from target-application collection overhead. Product claims use medians from the raw published samples.

### Rust control plane

On Ubuntu 24.04 x64 with Node 22.22.0, 30 startup samples and 10 offline report rebuilds produced these medians:

| Metric | Clinic.js 13.0.0 | V8Scope npm command | V8Scope native binary |
| --- | ---: | ---: | ---: |
| CLI startup | 219.1 ms | 46.7 ms | 6.4 ms |
| Offline report rebuild | 2274.4 ms | 278.1 ms | 244.9 ms |
| Report rebuild peak RSS | 182.7 MiB | 54.7 MiB | 7.6 MiB |

V8Scope's five-second input was 288.4 KB and Clinic's was 112.3 KB. The public npm command includes the Node platform-selection wrapper; the native column isolates the released Rust process. See all [control-plane samples and quartiles](../benchmarks/results/2026-08-13-ubuntu-x64-node22/control-plane.json).

Separate production npm prefixes measured 96.1 MB, 17,909 files, 693 dependency nodes, and 24 audit findings for Clinic. V8Scope measured 17.6 MB, 85 files, 3 dependency nodes, and zero audit findings. Audit counts describe the registry state on the benchmark date and can change independently of runtime performance.

### Collection reliability and overhead

V8Scope completed 10/10 reports in each of Diagnose, CPU, Heap, and Async. Clinic Doctor completed 10/10, Flame 0/10, Heap Profiler 0/10, and Bubbleprof 6/10 under the same 30-second report window. V8Scope finalized Diagnose 4.8× faster and Async 2.0× faster. CPU and Heap completed in 0.43 and 0.21 seconds while their Clinic counterparts timed out around 30 seconds.

After subtracting the 76.4 MiB baseline Node process tree, V8Scope used 69.5 MiB versus Clinic's 142.7 MiB for Diagnose, 70.0 versus 151.7 MiB for CPU, and 64.9 versus 82.7 MiB for Heap. Async used 517.9 versus 431.2 MiB, the current measured memory tradeoff. Low-overhead throughput distributions overlap; Async completed 36.6% more requests. See the [collection summary](../benchmarks/results/2026-08-13-ubuntu-x64-node22/summary.md) and [all 90 samples](../benchmarks/results/2026-08-13-ubuntu-x64-node22/raw.json).

The benchmark covers one controlled workload and one host class. Re-run it against your service before using the numbers for capacity planning.

## Choose the interface

Use V8Scope when you want maintained Node support, native orchestration, attach mode, standard raw profiles, artifact integrity, or CI budgets. Keep Clinic.js available when a workflow depends on its JavaScript module API, private `.clinic-*` data, existing HTML DOM, or Windows support.
