# Release checklist

Use this checklist for GitHub binary releases.

## Prepare

- [ ] Confirm the release scope and close or explicitly defer incomplete issues.
- [ ] Update `CHANGELOG.md`, replacing `Unreleased` with the release date.
- [ ] Set the workspace version in `Cargo.toml` and matching `memzoi-core` dependency versions.
- [ ] Refresh `Cargo.lock`.
- [ ] Snapshot the release docs with `pnpm docusaurus docs:version <version>` and make that version Docusaurus `lastVersion`.
- [ ] Run `scripts/check-release-metadata.sh v<version>`.

## Verify

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace`.
- [ ] Run `make eval` and confirm the checked-in trust baseline passes.
- [ ] Run `cargo +1.96.1 check --workspace --all-targets --locked`.
- [ ] Run `make onboarding-smoke`.
- [ ] Run `pnpm docs:build` under `website/`.
- [ ] Run the release workflow manually with `ref: main` and `upload: false`; inspect every platform artifact.
- [ ] Extract one archive, run both binaries with `--version`, and verify its downloaded sidecar with `shasum -a 256 -c <archive>.sha256` (or `Get-FileHash` on Windows).
- [ ] Open a repository created by the previous release and smoke-test `doctor`, `search`, `context`, and `rebuild` with the candidate binaries.

## Publish

- [ ] Create a draft GitHub release for `v<version>` from the verified commit using the changelog section as release notes.
- [ ] Push/create the matching `v<version>` tag so the release workflow builds and uploads signed-off artifacts.
- [ ] Confirm all expected archives and checksum sidecars are attached before publishing the draft.
- [ ] Publish the release and verify the install scripts select it as latest.
- [ ] Test `memzoi update --check` from the previous release and one clean install on a supported platform.
