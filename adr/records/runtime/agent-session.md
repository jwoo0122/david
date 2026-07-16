---
id: runtime.agent-session
status: accepted
scope: runtime
decision_type: integration
applies_to:
  - configured agent launch
  - tmux session creation and attach
  - list and remove process state
summary: Each managed worktree owns at most one persistent tmux agent session that the CLI can attach to.
constrains: []
depends_on:
  - worktree.storage-and-lifecycle
supersedes: []
superseded_by: []
last_reviewed: "2026-07-16"
---

# Persistent agent sessions

## Decision question

How can the CLI preserve an interactive agent and reconnect to it from a later invocation?

## Current decision

The CLI MUST launch each configured agent inside a dedicated tmux session for its managed worktree. The session MUST use the worktree as its working directory and inherit the user's terminal interaction through tmux attach.

`run <worktree-name>` MUST attach to an existing managed session without showing the agent picker. If the worktree has no live managed session, `run` MUST show the configured agent list, start the selected agent in a new session, and attach to it. A worktree MUST have at most one managed agent session.

Managed tmux sessions MUST use the `david-<repo-id>-<stable-worktree-hash>` naming prefix so the CLI owns a distinct namespace.

`list` MUST report only sessions created and tracked by this CLI. Removing a worktree MUST terminate its managed tmux session before removing the checkout.

A managed session MUST provide a session-scoped `Ctrl-]` shortcut that detaches the client without stopping the agent. The shortcut MUST NOT replace root or prefix bindings for unrelated tmux sessions. The standard `Ctrl-b d` tmux sequence MUST remain available as a fallback. While attached, the tmux status line MUST identify the session as `DAVID` and show the project directory name, worktree name, configured agent name, and the detach shortcut.

## Context and forces

A directly exec'd child process is tied to the invoking terminal and cannot provide a later interactive attach point. tmux supplies a persistent pseudo-terminal without modifying the agent command or adding a custom agent UI. It is an explicit runtime dependency for the first macOS/Linux implementation.

## Invariants

- Agent commands MUST come from the user configuration and MUST run without changing their working directory away from the managed worktree.
- Session identity MUST be deterministic for a repository/worktree pair and MUST not collide with another managed pair.
- A live session MUST be preferred over agent selection on `run`.
- Stale session metadata MUST never be reported as an active agent.
- Removal MUST terminate the session before deleting its worktree.
- The CLI MUST report a clear prerequisite error when tmux is unavailable.

## Alternatives and trade-offs

- A Rust-owned PTY broker would remove the external dependency but substantially increases implementation and failure-management cost.
- Direct process execution preserves the simplest terminal model but cannot satisfy reattachment.
- Scanning arbitrary processes would show agents launched outside the CLI but creates unreliable ownership and identification semantics.

## Consequences

Users can detach with the direct `Ctrl-]` shortcut or tmux's standard `Ctrl-b d` sequence and later reattach through `run`. The status line makes the managed-session context and detach action visible without changing the agent command. The CLI must manage session naming, stale state, and forced termination. tmux installation is required on supported systems.

## Enforcement

Integration tests MUST cover session creation, reuse and attach, picker suppression for live sessions, stale-session handling, list output, tmux prerequisite failure, removal ordering, and the managed-session status line and detach binding.

## Revisit when

Revisit this decision if tmux is unavailable on a required platform, if agent UI fidelity is materially affected, or if a smaller reliable PTY-session implementation becomes preferable.
