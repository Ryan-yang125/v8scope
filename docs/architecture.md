# Architecture

## Boundary

V8Scope keeps V8 inside Node.js. Rust owns orchestration, process trees, artifact integrity, CDP transport, analysis, comparison, and reporting. This boundary tracks Node's supported interfaces and avoids maintaining a second V8 embedder with a tightly coupled V8 toolchain.

Launch mode uses Node's stable [`--cpu-prof`](https://nodejs.org/api/cli.html#--cpu-prof) and [`--heap-prof`](https://nodejs.org/api/cli.html#--heap-prof) flags. A zero-dependency CommonJS preload reads [`perf_hooks`](https://nodejs.org/api/perf_hooks.html), `process.cpuUsage()`, `process.memoryUsage()`, and `process.getActiveResourcesInfo()`. `NODE_OPTIONS` propagates the probe and V8 flags into cluster and worker processes; V8 assigns collision-free profile names.

Attach mode uses the [Chrome DevTools Profiler](https://chromedevtools.github.io/devtools-protocol/tot/Profiler/) and [HeapProfiler](https://chromedevtools.github.io/devtools-protocol/tot/HeapProfiler/) domains exposed by Node Inspector. Commands have unique IDs, a pending oneshot map, cancellation cleanup, bounded timeouts, TCP and WebSocket keepalives, disconnect cleanup, and a bounded event broadcast. Full heap snapshots stream chunks to disk and fail on receiver lag.

## Agent-browser research

The native Rust implementation in agent-browser was reviewed at commit [`548b159`](https://github.com/vercel-labs/agent-browser/tree/548b159b30eef119ccf6846c8bc807d0eaa3f6f8/cli/src/native/cdp). V8Scope adopted its proven CDP transport patterns: split WebSocket ownership, request IDs with oneshot response routing, pending-entry cancellation guards, disconnect cleanup, command deadlines, large-frame support, and keepalives. V8Scope keeps a small protocol surface limited to Node's Runtime, Profiler, and HeapProfiler methods, so direct serde contracts remain easier to audit than a generated full browser protocol.

## Analysis

- CPU: parses V8 `.cpuprofile`, aggregates self and subtree time, applies adjacent source maps, and generates an interactive offline SVG through the maintained Rust `inferno` crate.
- Heap: parses V8 sampling `.heapprofile` trees and ranks allocation sites.
- Doctor: preserves Clinic Doctor's two-state Gaussian HMM decision boundary for separating application and V8 CPU modes. The implementation is deterministic and covered by Clinic's one-mode, two-mode, and insufficient-data vectors.
- Runtime: computes event-loop utilization and delay distributions, GC pause totals by Node `perf_hooks` constants, memory growth, and active-resource growth. Telemetry is grouped by process and worker-thread identity before isolate-level memory and resource values are combined.
- Async: records bounded `async_hooks` init, callback, resolve, and destroy events. Analysis separates resource wait time from callback execution, aggregates resource types and causal edges, and renders an interactive offline topology. This mode is explicit because Node documents `async_hooks` as experimental and instrumentation affects the measured system.
- Compare: validates artifact hashes, run completion, collector equality, mode, runtime, and platform before calculating deltas. Unknown or unavailable budget metrics fail closed.

## Failure behavior

The manifest is written before process launch and finalized with SHA-256 metadata after analysis. `analyze` and `compare` refresh the inventory after changing artifacts. One truncated final NDJSON record is tolerated and marks telemetry incomplete; malformed earlier records fail analysis. Every started isolate must emit `finish` before telemetry is complete. Unix targets receive `SIGINT` through a dedicated process group; PID and start-time identities discovered from the live process tree and Node telemetry also settle descendants that create a new process group. Windows targets receive an in-process `SIGINT` request through the preloaded probe and remain contained by a Job Object. Application signal handlers retain shutdown ownership and may finish asynchronous cleanup. Both platforms receive a five-second graceful window followed by contained-tree termination. Readiness and workload commands run in their own contained process tree and cannot keep writing after finalization.

Loopback Inspector endpoints are the default security boundary. Remote attachment requires an explicit flag. CDP frames and complete messages have explicit 16 MiB and 128 MiB limits. Path redaction is enabled by default. Reports contain no remote scripts or network dependencies.

## Public contract

Schema version 1 begins with V8Scope. Clinic's private data files, JavaScript module APIs, and report DOM are outside this contract. Standard V8 profiles remain interoperable with DevTools and Speedscope.
