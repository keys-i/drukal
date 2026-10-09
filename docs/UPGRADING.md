# Upgrading to Drukal

Drukal replaces Koelu and the unpublished Koela rename. Earlier names were Pekin and Rady. The [changelog](CHANGELOG.md) keeps released commands and names in their historical entries

## Install the new CLI

Install Drukal with Cargo or Homebrew

```sh
cargo install drukal --locked
drukal --help
```

```sh
brew tap keys-i/drukal https://github.com/keys-i/drukal
brew install keys-i/drukal/drukal
```

Remove a previous CLI only after Drukal works on your machine. Use `cargo uninstall` with its old package name or uninstall its Homebrew formula. Saved run folders are not moved automatically. To reuse one, set `DRUKAL_RUNS_DIR` to its existing path

## Move the hosted service

Complete this cutover before deploying the renamed workflow

1. Rename the existing `keys-i/koelu` repository to `keys-i/drukal`, and update local Git remotes and the Homebrew tap URL
2. Rename the existing GitHub App to **Drukal**, with slug `drukal`, preserving its identity, installations and permissions under `keys-i`
3. The central workflows pass credentials to Drukal using `DRUKAL_*` environment variables. They prefer the matching Drukal secrets and reuse existing `KOELU_*` App and provider secrets during the transition, including `RADY_CEREBRAS_API_KEY`. They also reuse `KOELU_APP_CLIENT_ID` and default the App slug to `drukal`. To retire these fallbacks, add the matching Drukal secrets through Settings using your existing credentials. Installed target repositories do not need these credentials
4. Commit the renamed source and deploy the Drukal workflow after the App and credentials are ready. This version reads `DRUKAL_*` environment variables
5. Run setup as an administrator in each connected repository. Review and commit the generated `.github/drukal.toml`, including its new immutable solver revision. An older solver pin does not contain the Drukal binary

```sh
drukal setup --repo owner/repo --check test --accept-terms
```

The existing Terms `2026-09-27-t5` and Privacy `2026-09-27-p5` still apply. Setup records the agreement and pins the trusted `keys-i/drukal` source. To retain automatic Cargo Dependabot repair, include `--autofix` when running setup

Remove obsolete `.github/koelu.toml`, `.github/koelu.json`, `.github/koela.toml` or older configuration only after the new setup succeeds and its contents are no longer needed. Keep the old workflow's credentials until that workflow is retired

## PRs and mentions

Start a comment with either form

```text
@drukal What changed here, and what should I check?
@drukal[bot] Review this line
```

Leading whitespace, a newline after the name and case differences are accepted. A name embedded later in a comment does not invoke Drukal. Old product mentions are not aliases

Inline PR questions reply in the review thread. Put change requests and approvals in the PR's Conversation tab. The same author must approve the exact proposal before Drukal creates a branch and pull request. Existing hidden request receipts remain readable to prevent duplicate replies and dispatches

The trusted solver pin, selected checks, administrator agreement and author permissions remain required. Drukal never merges for you
