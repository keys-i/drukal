# Contributing

Use Rust 1.85 or newer. Keep changes focused. Test the behaviour you change, including its failure cases. Give each test one clear concern and use small case tables when they help.

Before opening a pull request, run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --no-fail-fast --locked
cargo test --doc --locked
cargo build --release --locked
```

Run `cargo fmt` to apply formatting. For a quick loop, target the relevant module or test, such as `cargo test quality::tests`.

If you change a workflow, run `actionlint .github/workflows/*.yml` when it is installed. Never put subscription credentials or model API keys in public workflows; read [Security](SECURITY.md) before changing runner or credential boundaries.

Use Conventional Commit titles: `fix:` makes a patch release, `feat:` a minor release, and `!` or `BREAKING CHANGE:` a major release. Release Please updates `Cargo.toml` and the changelog, then creates the tag and release once its release PR merges.

Contributions are under the [MIT License](../LICENSE).
