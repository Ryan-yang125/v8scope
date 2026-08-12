# Releasing

1. Create or connect the `Ryan-yang125/v8scope` GitHub repository.
2. Create `Ryan-yang125/homebrew-tap`, add a write-enabled deploy key whose private key is stored as the `HOMEBREW_TAP_SSH_KEY` Actions secret in this repository, and protect the `main` branch.
3. Configure the npm package's trusted publisher for `Ryan-yang125/v8scope`, workflow `release.yml`, and the `npm publish` action. The workflow uses OIDC and npm provenance without a long-lived npm token.
4. Protect release tags and keep the version in `Cargo.toml` identical to a tag such as `v0.1.0`.
5. The release workflow runs the complete suite on Linux x64 with Node 22, the runtime integration suite with Node 24 and 26, a macOS ARM64 integration smoke, MSRV validation, and pinned Clinic baseline verification for the exact tag SHA. It then builds macOS ARM64, macOS x64, and Linux x64 archives, installers and checksums, generates CycloneDX SBOMs, embeds auditable dependency data, adds GitHub provenance attestations, publishes npm and Homebrew, and creates the GitHub release.
6. Verify each supported platform's downloaded archive and installer smoke test. Verify provenance with `gh attestation verify <artifact> --repo Ryan-yang125/v8scope` and the published SHA-256 file.

The release plan is reproducible with:

```sh
dist plan
dist generate --check
```

The checked-in release workflow carries audited cargo-dist 0.32 deltas: the CycloneDX output expression uses GitHub's `outputs` context, and local builders install exact locked versions of cargo-dist and cargo-auditable instead of executing generated remote installer pipes. `dist-workspace.toml` records these intentional generated-file deltas.

The Homebrew formula is attached to each GitHub release and published to `Ryan-yang125/homebrew-tap` after the release is created.
