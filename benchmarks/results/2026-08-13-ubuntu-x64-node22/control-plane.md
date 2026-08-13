# ubuntu-24.04-x64-node22 control-plane benchmark

Startup uses 30 measured runs after 1 warmup. Report rebuilding uses 10 measured runs after 1 warmup against one fresh copy of each tool's equivalent 5-second diagnosis capture per run.

- Host: linux 6.8.0-134-generic, x64, 4 CPU(s), 16 GiB RAM
- CPU: AMD EPYC 9354P 32-Core Processor
- Node: v22.22.0
- V8Scope: v8scope 0.2.0
- Clinic.js: v13.0.0
- Commit: `33854a7e14b683e50e3f58697b07bae66a310fda`

## CLI startup

| Entry point | Median | Q1–Q3 | Peak RSS median |
| --- | ---: | ---: | ---: |
| clinic-cli | 219.1 ms | 216.0–228.5 ms | 73.8 MiB |
| v8scope-npm | 46.7 ms | 44.2–49.9 ms | 54.7 MiB |
| v8scope-native | 6.4 ms | 5.7–6.9 ms | 5.2 MiB |

`v8scope-npm` is the public npm launcher; `v8scope-native` is the same released Rust binary after platform selection.

## Offline report rebuild

| Workflow | Median | Q1–Q3 | Peak RSS median | Input bytes |
| --- | ---: | ---: | ---: | ---: |
| clinic-doctor | 2274.4 ms | 2261.7–2308.3 ms | 182.7 MiB | 112253 |
| v8scope-npm | 278.1 ms | 276.0–281.1 ms | 54.7 MiB | 288384 |
| v8scope-native | 244.9 ms | 237.2–248.6 ms | 7.6 MiB | 288384 |

## Installed production surface

| Package | Installed bytes | Files | npm dependency nodes | npm audit findings |
| --- | ---: | ---: | ---: | ---: |
| Clinic.js 13.0.0 | 96146948 | 17909 | 693 | 24 |
| V8Scope 0.2.0 | 17599658 | 85 | 3 | 0 |

Wall time includes process startup and the public command path. GNU time records peak RSS. Input copying is excluded from report timing. Raw samples and audit counts are retained in `control-plane.json`.
