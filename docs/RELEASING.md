# Releasing Koelu

Release Please opens the version PR. Check that `Cargo.toml`, `Cargo.lock`, `tools/config/versions.json` and `docs/CHANGELOG.md` agree before merging it.

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

The central repository needs `KOELU_RELEASE_TOKEN` for Release Please and `CARGO_REGISTRY_TOKEN` for crates.io. Keep both in `keys-i/koelu`. Installed repositories do not need release credentials.

Merging the release PR lets the Release workflow create the tag and GitHub release, then publish the matching crate. Keep the Homebrew formula aligned with that tag. Update installed repositories’ solver pins separately when they should use the new code.

## Recovering publication

Use **Actions → Release → Run workflow** with an existing `vX.Y.Z` tag when its GitHub release exists but the crate was not published. The workflow checks out that exact tag, verifies the crate name and version, and skips publication if that version is already on crates.io.

Do not rebuild an older release from current source or move an existing tag.

## Earlier names

The [changelog](CHANGELOG.md) keeps the Rady releases under their original name. The old notice-only crate and deprecated Homebrew formula point to Koelu. They do not move installed CLIs or saved runs. See the [upgrade guide](UPGRADING.md) for the name change and repository consent.
