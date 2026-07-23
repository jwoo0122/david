---
id: worktree.storage-and-lifecycle
status: accepted
scope: worktree
decision_type: boundary
applies_to:
  - worktree creation and removal
  - run and list commands
summary: Managed worktrees live under XDG base directories and are paired with same-named branches for their full lifecycle.
constrains: []
depends_on: []
supersedes: []
superseded_by: []
last_reviewed: "2026-07-22"
---

# Managed worktree storage and lifecycle

## Decision question

Where does the CLI store managed worktrees and other state, and should it use XDG Base Directory specification locations rather than a single hidden root? What lifecycle does a named worktree follow?

## Current decision

The CLI MUST store managed worktree checkouts below `$XDG_DATA_HOME/david/worktrees/<repo-id>/<worktree-name>`. When `$XDG_DATA_HOME` is unset or empty, the fallback MUST be `$HOME/.local/share/david/worktrees/<repo-id>/<worktree-name>`.

Configuration MUST be read from `$XDG_CONFIG_HOME/david/config.toml`, falling back to `$HOME/.config/david/config.toml` when `$XDG_CONFIG_HOME` is unset or empty.

Session state MUST be stored under `$XDG_STATE_HOME/david/sessions/`, falling back to `$HOME/.local/state/david/sessions/` when `$XDG_STATE_HOME` is unset or empty.

XDG environment variables MUST be honored only when they specify absolute paths. Relative values MUST be ignored and the corresponding fallback path MUST be used.

The repository identity MUST include a stable identifier derived from the canonical Git common directory so linked worktrees of one repository share an identity while separate clones cannot collide.

`run <worktree-name>` MUST create the named worktree from the current `HEAD` on a new branch with the same name when it does not exist, and MUST reuse it when it does exist. The source repository MUST be clean before creation. A managed worktree normally reports that same branch, but an in-progress rebase may temporarily report detached HEAD; that state is valid only for reattaching to its already-live matching managed session, not for creating or starting a replacement session. `remove <worktree-name>` MUST refuse a dirty worktree unless `--force` is supplied, then MUST delete the paired branch after removing the worktree. A later `run` with the same name therefore creates a fresh branch from the then-current `HEAD`.

## Context and forces

Sibling directories make managed checkouts visible beside every project and can create naming collisions or clutter. A user-scoped directory centralizes lifecycle management while retaining separate checkouts for each repository.

The XDG Base Directory specification separates configuration, data, and state per their distinct lifecycles and ownership. A single hidden root mixes them, making backup, cleanup, and compliance with platform conventions harder. XDG locations are therefore preferred.

The command is intentionally unified so callers do not need to distinguish creation from reuse. Creating from the current `HEAD` preserves the context of the project directory that invoked the command. Refusing dirty source and target worktrees prevents silent loss of changes.

## Invariants

- Managed checkout paths MUST remain below the resolved XDG data home (`$XDG_DATA_HOME/david` or its fallback).
- Repository identity MUST distinguish canonical Git common directories with the same basename.
- Invocations from linked worktrees MUST resolve to the same repository identity as invocations from the main worktree.
- Creation MUST use the current `HEAD` and a new branch named for the worktree.
- The worktree name and its branch name MUST match whenever Git reports an attached branch.
- A detached worktree MUST not be treated as its expected branch unless an in-progress rebase records that branch and a matching managed session is already live.
- A dirty source repository MUST not be used to create a managed worktree.
- Default removal MUST leave a dirty worktree and its process state untouched.
- Removal MUST delete the paired branch only after the worktree has been removed.
- Forced removal MUST be explicit and MUST remove the worktree after its managed agent session is terminated.
- XDG environment variables MUST be honored only when they are absolute paths; relative values MUST be ignored and the fallback path used.
- Legacy `~/.david` compatibility reads MUST be attempted only when XDG locations have no David data and `~/.david` exists. A warning MUST be printed to stderr when legacy paths are read.
- The `david migrate` command MUST provide explicit migration from `~/.david` to XDG locations.
- Migration MUST never overwrite existing destination files.
- If migration fails mid-operation, legacy source data at `~/.david` MUST NOT be deleted.
- `~/.david` MUST be removed only when it is empty after migration.

## Alternatives and trade-offs

- Sibling worktree directories are easier to discover but pollute project locations and are prone to basename collisions.
- Hashing each linked worktree root would isolate paths but make one repository appear as multiple managed repositories.
- Using the repository default branch instead of the current `HEAD` gives a stable base but discards the caller's current branch context.
- Preserving the branch after worktree removal keeps commits available but breaks the intended one-to-one worktree/branch lifecycle.
- Always forcing removal is simpler but can destroy uncommitted work without an explicit user signal.
- A single hidden root (`~/.david`) is simpler to implement but mixes config, data, and state lifecycles and diverges from platform conventions.

## Consequences

The CLI owns a predictable user-level storage tree distributed across XDG directories and can find managed state without adding files to the source repository. Users must use the CLI's removal flow to clean managed worktrees. Removing a clean worktree also removes its branch and any commits reachable only from that branch, so removal is intentionally destructive at the branch level. The path includes an opaque identifier, which is less readable than a basename-only layout but avoids collisions. The CLI reads existing `~/.david` data with a compatibility fallback and provides an explicit `david migrate` command for moving it to XDG locations. Compatibility reads will eventually be removed.

## Enforcement

Integration tests MUST cover path derivation, same-basename repository separation, creation from `HEAD`, same-name branch pairing, rebase-detached session reattachment, arbitrary-detached and wrong-branch rejection, dirty-source rejection, reuse, dirty-removal rejection, branch deletion, and explicit forced removal. Tests MUST also verify XDG env var resolution including absolute-path enforcement, legacy `~/.david` compatibility reads, `david migrate` behavior, non-overwriting migration, mid-operation failure safety, and empty-only source removal.

## Revisit when

Revisit this decision if users need configurable storage roots beyond XDG, Windows support with different home-directory semantics, a workflow that intentionally carries uncommitted source changes into a new worktree, branch history must survive worktree removal, or legacy `~/.david` compatibility reads are ready to be removed.
