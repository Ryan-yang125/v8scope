# Releasing

V8Scope releases run entirely in GitHub Actions. The repository, npm Trusted Publisher, and `Ryan-yang125/homebrew-tap` deploy key are already connected.

## Normal release flow

1. Merge a conventional feature or fix pull request into `main`.
2. Wait for `release-plz` to create or update the Release PR. Conventional `feat` commits increment the minor version; fixes increment the patch version.
3. Review the version, `Cargo.toml`, `Cargo.lock`, and generated `CHANGELOG.md` in the Release PR.
4. Merge the Release PR.
5. `release-plz` creates the matching `vX.Y.Z` tag and draft GitHub release, then dispatches `release.yml` with that exact tag.
6. The release workflow reruns Linux Node 22, Node 24 and 26 runtime integration, macOS ARM64 smoke, MSRV, and the pinned Clinic baseline. It builds macOS ARM64, macOS x64, and Linux x64 artifacts, checksums, installers, CycloneDX SBOMs, auditable dependency data, and GitHub provenance attestations.
7. GitHub Actions publishes npm through Trusted Publishing with provenance, updates the Homebrew tap through its deploy key, uploads every artifact, and publishes the GitHub release.

No local publish command or long-lived npm token participates in this flow.

## Release verification

- Every release workflow job succeeds.
- GitHub Release is public and contains three native archives, checksums, SBOMs, shell installer, npm tarball, and Homebrew formula.
- `npm view v8scope version` matches the tag, and a clean temporary-prefix install prints the same `v8scope --version`.
- `brew fetch --formula Ryan-yang125/tap/v8scope` succeeds and resolves the same version.
- `gh attestation verify <artifact> --repo Ryan-yang125/v8scope` validates a downloaded archive.
- The README, npm package, and Homebrew formula expose the same supported platform set.

## Reproducibility checks

```sh
dist plan
dist generate --check
```

The checked-in release workflow carries audited cargo-dist 0.32 deltas: the CycloneDX output expression uses GitHub's `outputs` context, and local builders install exact locked versions of cargo-dist and cargo-auditable instead of executing generated remote installer pipes. `dist-workspace.toml` records these intentional generated-file deltas.

## Benchmark releases

Run the canonical benchmark on the dedicated Ubuntu x64 VPS when collection overhead, shutdown, report generation, process containment, or artifact size changes. Commit the dated `raw.json` and `summary.md`, update the README result block from those samples, and record the exact already-published V8Scope version and commit. Hosted CI only runs a short harness smoke test.
