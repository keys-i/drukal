# Koelu service terms

Effective 27 September 2026 · version `2026-09-27-t5`

These are the current hosted-service terms, not a retroactive agreement for earlier Rady or Pekin releases.

These terms cover the hosted Koelu service maintained through `keys-i/koelu`. The MIT licence covers the Koelu source code; a self-hosted copy is not this hosted service.

## Your agreement

You must be authorised to connect the selected GitHub account, organisation, and repositories. By passing `--accept-terms`, you accept these terms and the matching [Privacy policy](PRIVACY.md). Koelu records that acceptance in a closed GitHub issue and the repository’s public `.github/koelu.toml` file.

## What Koelu does

Koelu reads available repository metadata, issues, pull requests, diffs, and check results to answer `@koelu[bot]` and review eligible dependency updates. It may post a comment or review. For an explicit write request, it can prepare an isolated branch and pull request only after the same author approves that exact request. It does not enable, disable, or perform merges. You decide whether to merge.

An administrator may enable `--autofix` during setup. This authorises Koelu to open a checked replacement pull request for a verified Cargo Dependabot update with a merge conflict or failed required check. It never edits the Dependabot branch or merges the replacement. Existing `t4` agreements continue to permit reviews and individually approved writes but do not authorise automatic repairs.

Before an approved write, Koelu rechecks the request, approval, current author access, recorded proposal, and recorded base revision. It uses a short-lived token scoped to that repository. If a check fails, the base advances, work is cancelled, or validation fails, it stops without changing the default branch and posts a terminal result. Each approval is claimed once to prevent duplicate delivery; make a fresh request and approval after resolving a failed dispatch.

For an opted-in repair, Koelu rechecks the signed Dependabot commit, original head and base, blocking state, repository agreement, and prior replacement pull requests before publishing. It runs the local Cargo test and leaves the final merge to a maintainer.

The hosted service polls a bounded recent window and is not real time. Unsupported, grouped, or ambiguous dependency updates are left for manual review. Mention and review requests may send bounded evidence to a centrally configured model provider. An approved hosted edit runs a constrained provider CLI that may read and send repository files it selects for that approved task, as described in the Privacy policy.

## Your responsibilities

Use Koelu lawfully and only where you have authority. Do not use it to expose secrets or personal data, attack a service, evade provider limits, mislead contributors, or disclose content to a provider without permission. Keep branch protection and App access appropriate for your project.

## Availability and changes

Koelu is provided as available and can be incomplete or wrong. Check material advice and changes yourself. Access may be limited or suspended to protect repositories, people, providers, or the service. You can stop processing by uninstalling the App and removing the repository configuration.

Material changes receive a new version. Re-run `koelu setup` as an administrator to accept them before central processing resumes. For security reports, use [Security](../.github/SECURITY.md); do not include confidential repository content in public requests.
