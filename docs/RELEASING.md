# Releasing Drukal

Release Please opens the version PR. Check that `Cargo.toml`, `Cargo.lock`, `tools/config/versions.json` and `docs/CHANGELOG.md` agree before merging it.

For the first Drukal release, complete the repository, App and credential cutover in the [upgrade guide](UPGRADING.md). The renamed crate, App and repository are not created by changing this checkout

## Before merging

Run the same checks as CI, including the documentation examples

```sh
cargo fmt --check
bash .github/workflows/scripts/check.sh
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --no-fail-fast --locked
cargo test --doc --locked
cargo build --release --locked
```

Review the package contents before publication with `cargo package --list --locked`. The manifest uses directory patterns so new modules, tests, workflow scripts and assets are included without adding a line for each file.

The central repository needs `DRUKAL_RELEASE_TOKEN` for Release Please and `CARGO_REGISTRY_TOKEN` for crates.io. Keep both in `keys-i/drukal`. Installed repositories do not need release credentials.

Merging the release PR lets the Release workflow create the tag and GitHub release, then publish the matching crate. Keep the Homebrew formula aligned with that tag. Update installed repositories’ solver pins separately when they should use the new code.

## Recovering publication

Use **Actions → Release → Run workflow** with an existing `vX.Y.Z` tag when its GitHub release exists but the crate was not published. The workflow checks out that exact tag, verifies the crate name and version, and skips publication if that version is already on crates.io.

Do not rebuild an older release from current source or move an existing tag.

## Earlier names

The [changelog](CHANGELOG.md) keeps earlier releases under their original names. The deprecated Rady Homebrew formula names Drukal as its replacement. Previously published crates are unchanged. Installed CLIs and saved runs are not moved automatically. See the [upgrade guide](UPGRADING.md) for the name change and repository consent
