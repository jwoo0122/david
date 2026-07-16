---
id: delivery.release-automation
status: accepted
scope: delivery
decision_type: pipeline
applies_to:
  - version bump workflow
  - GitHub Release workflow
  - crates.io publication
  - Homebrew formula publication
summary: Main pushes are converted into Conventional Commit-driven release commits, and the resulting tag publishes the crate, binary artifacts, and Homebrew formula.
constrains: []
depends_on: []
supersedes: []
superseded_by: []
last_reviewed: "2026-07-16"
---

# Automated package releases

## Decision question

How should a change merged to `main` become a coordinated Cargo, GitHub, and Homebrew release?

## Current decision

The project MUST use `1.0.0` as its initial package version. A workflow triggered by pushes to `main` MUST use release-plz's Conventional Commit analysis to decide whether a new version is needed. `feat` commits MUST increment minor, `fix` and `perf` commits MUST increment patch, and a `BREAKING CHANGE` footer or `!` marker MUST increment major. `docs`, `chore`, `test`, and `refactor` commits MUST not create a release unless they carry a breaking-change marker.

When a release is needed, the workflow MUST commit the resulting manifest version as `chore(release): v<version>` and create the matching `v<version>` tag. The release commit MUST be the source revision for the release.

The release pipeline MUST use cargo-dist to build macOS Apple Silicon, macOS Intel, and x86_64 Linux artifacts, create the GitHub Release, and publish the generated `Formula/david.rb` to `jwoo0122/homebrew-tap`. A separate release job MUST publish the same package version for the `david` package to crates.io using the `CARGO_TOKEN` repository secret. The Homebrew publishing job MUST use the `HOMEBREW_TAP_TOKEN` repository secret and MUST not store either secret in the repository.

The release workflow MUST be reusable by the version-bump workflow. This avoids depending on a second workflow event after a commit or tag written with the default `GITHUB_TOKEN`.

## Context and forces

The project needs a single release source of truth while keeping the user-facing command available as prebuilt binaries. release-plz supplies Conventional Commit and SemVer analysis, while cargo-dist supplies cross-platform archives and a formula that references GitHub Release assets. GitHub Actions' default token cannot be used as a reliable trigger for another workflow, so the version bump and release workflows must be connected through `workflow_call` in the same run.

## Invariants

- The manifest, release commit, tag, Cargo package, GitHub Release, and Homebrew formula MUST agree on one version.
- A release workflow MUST never run for an ordinary non-release `main` push.
- The version-bump workflow MUST not recursively create release commits.
- Release artifacts MUST come from the tagged release commit.
- Homebrew MUST install prebuilt artifacts and MUST not require Rust at installation time.
- The formula MUST be written only to `jwoo0122/homebrew-tap` at `Formula/david.rb`.
- The public Cargo package and Homebrew formula identity MUST be `david`; previously published `tony` artifacts remain historical and are not silently republished under the new name.
- The workflow MUST fail closed when its required repository secrets are unavailable; no fallback token or checked-in credential is allowed.

## Alternatives and trade-offs

- A release-plz release PR would provide a review gate, but it conflicts with the requested automatic release commit directly on `main`.
- A source-building Homebrew formula would need less artifact CI, but it would require users to have a Rust toolchain and would not satisfy the prebuilt-binary installation goal.
- A tag-triggered workflow alone is simpler, but a tag pushed with `GITHUB_TOKEN` does not reliably start another workflow, so the caller uses a reusable workflow instead.

## Consequences

Every qualifying push can add a bot-authored commit to `main`. A failed package or tap publication can leave a GitHub Release partially complete. Crates.io publication can be retried with the manual `Publish crate` workflow for the existing release commit and version. The first `main` push after this pipeline is installed bootstraps `v1.0.0` even if the preceding development commits were not Conventional Commits. Existing `tony` installations are not upgraded by the new package name and remain on their historical package and formula identities.

## Enforcement

CI configuration checks MUST verify the release-plz filter, initial `1.0.0` bootstrap, `david` package identity, version-bump commit format, tag/ref handoff, secret names, target list, Homebrew tap path, and crate recovery workflow. `dist plan --tag v1.0.0` MUST succeed locally. The ordinary Rust test, format, clippy, and locked release-build checks MUST remain green.

## Revisit when

Revisit this decision if the project adopts a reviewed release PR flow, changes its binary distribution targets, moves the Homebrew tap, or replaces GitHub Actions with another release authority.
