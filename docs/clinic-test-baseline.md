# Clinic test baseline

V8Scope uses the 141 executable Clinic test files as its minimum traceability baseline. The machine-checked inventory is [tests/clinic-baseline.tsv](../tests/clinic-baseline.tsv).

| Upstream repository | Commit | Test files |
| --- | --- | ---: |
| `clinicjs/node-clinic` | `19262605b5647d34e660294e93c0cee62ea3a746` | 42 |
| `clinicjs/node-clinic-doctor` | `00749d83f04cb30ed24e91f29e89ad25e267ac33` | 25 |
| `clinicjs/node-clinic-flame` | `747b6919fdfec27f415bc70f99b762fe78928fad` | 16 |
| `clinicjs/node-clinic-bubbleprof` | `cf801d49299b8ecd25c5d9e88f662defc4bbad71` | 52 |
| `clinicjs/node-clinic-heap-profiler` | `2135fbfeaaf22d8a567d0293ec0b53c7f4739f49` | 6 |

The Rust suite consolidates implementation-specific JavaScript tests into capability baselines. `tests/clinic-upstream-lock.json` fixes every repository to a full commit and inventory digest. `scripts/verify-clinic-upstream.sh` fetches those exact commits and compares their real `test` and `test-local` trees with `tests/clinic-baseline.tsv`. `tests/clinic-coverage.tsv` binds each replacement capability to named executable Rust tests, and `scripts/verify-coverage-targets.sh` verifies those names against Cargo's test inventory. CI and release tags run both checks.

The 141 figure is the pinned upstream test inventory. The native suite consolidates private JavaScript implementation tests around public behaviors, so the executable Rust test count is reported separately.

| Coverage target | Rust baseline |
| --- | --- |
| `cli-contract` | Clap command validation, help/version generation, and non-Node rejection |
| `run-lifecycle` | atomic run creation, process containment, orderly profile flush, and cleanup |
| `e2e` | real Node launch, collection, reanalysis, report, worker, readiness, and load tests |
| `telemetry-contract` | probe NDJSON parsing, sampling, GC constants, process aggregation, and versioned schemas |
| `doctor-cpu` | deterministic HMM tests across Clinic's one-mode, two-mode, opposite-cluster, small-cluster, noise, and insufficient-data vectors |
| `event-loop-analysis` | delay percentiles, utilization median, and Clinic threshold findings |
| `diagnostic-findings` | CPU, memory, GC blocking-window, and active-resource findings |
| `cpu-profile-analysis` | real V8 profiles, self/subtree aggregation, source maps, workers, and Inferno report output |
| `heap-profile-analysis` | real V8 sampling profiles, allocation aggregation, and streamed Inspector snapshots |
| `async-causality-analysis` | bounded async lifecycle parsing, causal edges, chains, slow callbacks, and worker propagation |
| `offline-report` | self-contained report generation with escaped embedded data and local artifact links |

The public contract starts at V8Scope schema version 1. Clinic's private data files, internal JavaScript modules, and HTML DOM structure are excluded from the contract. Their user-visible capabilities remain represented by the mapped CPU, heap, async, diagnostic, lifecycle, and report tests.
