# Contributing

Changes need a focused test that fails before the implementation and passes after it. Preserve standard V8 profile interoperability and update the public schema version for contract changes.

Run before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
```

Updates derived from Clinic behavior must keep `tests/clinic-baseline.tsv` complete. CDP changes need a real Node Inspector test and a transport-level failure test where applicable.
