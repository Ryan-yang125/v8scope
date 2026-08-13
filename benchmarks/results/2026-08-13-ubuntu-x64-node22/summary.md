# ubuntu-24.04-x64-node22 benchmark

Measured 10 times after 1 warmup run(s) per variant. Each measurement used 20 connections for 5 seconds.

- Host: linux 6.8.0-134-generic, x64, 4 CPU(s), 16 GiB RAM
- CPU: AMD EPYC 9354P 32-Core Processor
- Node: v22.22.0
- V8Scope: v8scope 0.1.1
- Clinic.js: v13.0.0
- Commit: `0d90dc6ba4ac47679a640bb73eeac3c0989b8873`

| Variant | Report success | Requests/s median | vs baseline | p99 median | Peak tree RSS | Finalize |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline | 10/10 | 4607 | — | 8.0 ms | 76.4 MiB | 0.01 s |
| clinic-doctor | 10/10 | 4661 | 1.2% | 8.0 ms | 219.1 MiB | 2.08 s |
| v8scope-diagnose | 10/10 | 4741 | 2.9% | 8.0 ms | 145.9 MiB | 0.43 s |
| clinic-flame | 0/10 | 4010 | -13.0% | 26.0 ms | 228.1 MiB | 32.01 s |
| v8scope-cpu | 10/10 | 4686 | 1.7% | 8.0 ms | 146.4 MiB | 0.43 s |
| clinic-heapprofiler | 0/10 | 4493 | -2.5% | 8.0 ms | 159.1 MiB | 30.01 s |
| v8scope-heap | 10/10 | 4545 | -1.3% | 8.0 ms | 141.3 MiB | 0.21 s |
| clinic-bubbleprof | 6/10 | 822 | -82.2% | 34.0 ms | 507.6 MiB | 7.78 s |
| v8scope-async | 10/10 | 1123 | -75.6% | 30.0 ms | 594.3 MiB | 3.95 s |

Values are medians. `raw.json` includes every sample plus Q1/Q3, exits, failures, artifact size, and cleanup state. Report success requires the expected exit contract, a complete report, and no remaining process group.
