---
id: worktree.storage-and-lifecycle
status: accepted
scope: worktree
decision_type: boundary
applies_to:
  - worktree creation and removal
  - run and list commands
summary: Managed worktrees live under a user-scoped hidden directory and are paired with same-named branches for their full lifecycle.
constrains: []
depends_on: []
supersedes: []
superseded_by: []
last_reviewed: "2026-07-16"
---

# Managed worktree storage and lifecycle

## Decision question

Where does the CLI store managed worktrees, and what lifecycle does a named worktree follow?

## Current decision

The CLI MUST store managed worktree checkouts below `~/.tool/worktrees/<repo-id>/<worktree-name>`. The repository identity MUST include a stable identifier derived from the canonical Git common directory so linked worktrees of one repository share an identity while separate clones cannot collide.

`run <worktree-name>` MUST create the named worktree from the current `HEAD` on a new branch with the same name when it does not exist, and MUST reuse it when it does exist. The source repository MUST be clean before creation. `remove <worktree-name>` MUST refuse a dirty worktree unless `--force` is supplied, then MUST delete the paired branch after removing the worktree. A later `run` with the same name therefore creates a fresh branch from the then-current `HEAD`.

## Context and forces

Sibling directories make managed checkouts visible beside every project and can create naming collisions or clutter. A user-scoped directory centralizes lifecycle management while retaining separate checkouts for each repository.

The command is intentionally unified so callers do not need to distinguish creation from reuse. Creating from the current `HEAD` preserves the context of the project directory that invoked the command. Refusing dirty source and target worktrees prevents silent loss of changes.

## Invariants

- Managed checkout paths MUST remain below the configured user-scoped `.tool/worktrees` directory.
- Repository identity MUST distinguish canonical Git common directories with the same basename.
- Invocations from linked worktrees MUST resolve to the same repository identity as invocations from the main worktree.
- Creation MUST use the current `HEAD` and a new branch named for the worktree.
- The worktree name and its branch name MUST match.
- A dirty source repository MUST not be used to create a managed worktree.
- Default removal MUST leave a dirty worktree and its process state untouched.
- Removal MUST delete the paired branch only after the worktree has been removed.
- Forced removal MUST be explicit and MUST remove the worktree after its managed agent session is terminated.

## Alternatives and trade-offs

- Sibling worktree directories are easier to discover but pollute project locations and are prone to basename collisions.
- Hashing each linked worktree root would isolate paths but make one repository appear as multiple managed repositories.
- Using the repository default branch instead of the current `HEAD` gives a stable base but discards the caller's current branch context.
- Preserving the branch after worktree removal keeps commits available but breaks the intended one-to-one worktree/branch lifecycle.
- Always forcing removal is simpler but can destroy uncommitted work without an explicit user signal.

## Consequences

The CLI owns a predictable user-level storage tree and can find managed state without adding files to the source repository. Users must use the CLI's removal flow to clean managed worktrees. Removing a clean worktree also removes its branch and any commits reachable only from that branch, so removal is intentionally destructive at the branch level. The path includes an opaque identifier, which is less readable than a basename-only layout but avoids collisions.

## Enforcement

Integration tests MUST cover path derivation, same-basename repository separation, creation from `HEAD`, same-name branch pairing, dirty-source rejection, reuse, dirty-removal rejection, branch deletion, and explicit forced removal.

## Revisit when

Revisit this decision if users need configurable storage roots, Windows support with different home-directory semantics, a workflow that intentionally carries uncommitted source changes into a new worktree, or branch history must survive worktree removal.
