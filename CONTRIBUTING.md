# Contributing

Changes need a focused test that fails before the implementation and passes after it. Preserve standard V8 profile interoperability and update the public schema version for contract changes.

Run before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
```

Updates derived from Clinic behavior must keep `tests/clinic-baseline.tsv` complete. CDP changes need a real Node Inspector test and a transport-level failure test where applicable.

Changes under `benchmarks/` must pass the benchmark smoke workflow. Run a new canonical VPS benchmark when a change can affect collection overhead, report finalization, process cleanup, or artifact size. Commit both `raw.json` and `summary.md`, record the exact released V8Scope version, and keep claims in the README limited to measured results.

Clinic migration reports are especially useful. Include the Clinic command, the equivalent V8Scope command, Node version, operating system, a minimal target application, and the complete V8Scope run directory when it is safe to share. Raw profiles can contain source paths and runtime values, so inspect them before uploading.
