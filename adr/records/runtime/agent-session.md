---
id: runtime.agent-session
status: accepted
scope: runtime
decision_type: integration
applies_to:
  - configured agent launch
  - tmux session creation and attach
  - prompt delivery to a managed agent session
  - list and remove process state
  - interactive list selection
summary: Each managed worktree owns at most one persistent tmux agent session that the CLI can attach to or prompt.
constrains: []
depends_on:
  - worktree.storage-and-lifecycle
supersedes: []
superseded_by: []
last_reviewed: "2026-07-21"
---

# Persistent agent sessions

## Decision question

How can the CLI preserve an interactive agent and reconnect to it from a later invocation?

## Current decision

The CLI MUST launch each configured agent inside a dedicated tmux session for its managed worktree. The session MUST use the worktree as its working directory and inherit the user's terminal interaction through tmux attach.

`run <worktree-name>` MUST attach to an existing managed session without showing the agent picker when attachment is requested. If the worktree has no live managed session, `run` MUST resolve an agent in this order: the `--agent` option, `DAVID_AGENT`, `default_agent` from configuration, the sole configured agent, and finally the picker only when stdin and stderr are terminals and interaction is enabled. A selected agent MUST be validated before the picker is considered. New sessions MUST start the selected command directly in tmux; arguments supplied after `--` MUST be appended as separate argv values. `--detach` MUST create or reuse the managed session without attaching. A run that cannot interact with a terminal MUST not invoke the picker or attach. A worktree MUST have at most one managed agent session.

Managed tmux sessions MUST use the `david-<repo-id>-<stable-worktree-hash>` naming prefix so the CLI owns a distinct namespace. They MUST set `@david-project`, `@david-worktree`, and `@david-agent` metadata, and every reuse, explicit attach, prompt, list-as-active, and removal operation MUST validate those values against the requested worktree and state before acting; a mismatch MUST be rejected rather than repaired. `attach <worktree-name>` MUST only attach to a live session with matching managed metadata; it MUST NOT create or repair a worktree or session, select an agent, or start a process.

During an in-progress rebase, Git may report a managed worktree as detached. Reattachment through `run` or `attach` MAY proceed only when the rebase metadata records the expected worktree branch and a matching live managed session already exists with a live agent pane. Other detached or mismatched branches MUST remain rejected. Local lifecycle operations for one repository/worktree pair MUST be serialized while checking Git, state, and tmux session identity; changes made concurrently by an external Git or tmux actor remain an operational race and MUST fail closed when detected.

`prompt <worktree> <message>` MUST resolve the same managed worktree and session identity as `run`, require a live session with matching metadata, and deliver the exact received message as literal UTF-8 paste data followed by one `Enter` submission. It MUST NOT create a worktree or session, invoke the agent picker, or attach to tmux. Delivery MUST target the exact managed session and agent pane rather than the caller's current tmux target; prompt data MUST travel through tmux stdin and MUST NOT be interpreted as shell commands or tmux key names.

`list` MAY present an interactive picker (arrow-key navigation + Enter) when stdin and stderr are terminals. Selecting a worktree MUST trigger the same reuse/attach/session-creation flow as `run` for an existing worktree. Non-interactive or piped input, and `--porcelain` output, MUST retain the existing non-interactive table or porcelain output. `list` MUST report only sessions created and tracked by this CLI. Removing a worktree MUST terminate its managed tmux session before removing the checkout.

A managed session MUST provide a session-scoped `Ctrl-]` shortcut that detaches the client without stopping the agent. The shortcut MUST NOT replace root or prefix bindings for unrelated tmux sessions. The standard `Ctrl-b d` tmux sequence MUST remain available as a fallback. While attached, the tmux status line MUST identify the session as `DAVID` and show the project directory name, worktree name, configured agent name, and the detach shortcut.

David MUST keep managed tmux configuration deterministic instead of loading arbitrary user tmux configuration. It MUST enable `mouse` for each managed session and `extended-keys` for the tmux server when configuring a managed session, both when creating and reusing it. These interaction options require tmux 3.2 or newer. `extended-keys` is server-scoped by tmux and therefore can affect other sessions sharing that server; use of a separate server is required if that side effect becomes unacceptable.

A managed session MUST explicitly set the full window and pane styling (`status-style`, `window-style`, `window-active-style`, `pane-border-style`, `pane-active-border-style`) so that global tmux server options from the user's `~/.tmux.conf` do not leak into managed sessions. David creates sessions with `tmux -f /dev/null` to ignore config files, but an already-running tmux server's global options are inherited by all sessions; explicit session-scoped styling overrides prevent that leakage. The status line MUST use an achromatic (grayscale) palette with a light background and dark text, and zones MUST be distinguished by background color (lighter at edges, darker toward center) rather than pipe (`|`) separators.

## Context and forces

A directly exec'd child process is tied to the invoking terminal and cannot provide a later interactive attach point. tmux supplies a persistent pseudo-terminal without modifying the agent command or adding a custom agent UI. It is an explicit runtime dependency for the first macOS/Linux implementation.

## Invariants

- Agent commands MUST come from the user configuration and MUST run without changing their working directory away from the managed worktree.
- Session identity MUST be deterministic for a repository/worktree pair and MUST not collide with another managed pair.
- A live session MUST be preferred over agent selection on `run`; an explicit unknown agent MUST fail without invoking the picker when agent resolution is required.
- A session MUST be considered live for reuse or reattachment only when its managed agent pane is live; dead panes MUST be rejected for those operations and MUST be reported as inactive by `list`.
- The `@david-project`, `@david-worktree`, and `@david-agent` tmux metadata MUST exactly match the requested managed worktree and state before reuse, attach, prompt, active listing, or removal; metadata mismatches MUST not be repaired implicitly.
- Local lifecycle operations for the same repository/worktree pair MUST hold an exclusive lock across their Git, state, and tmux checks and mutations.
- Agent selection MUST be deterministic outside an allowed terminal interaction, and selection failure MUST not wait for input.
- Runtime agent arguments MUST remain separate argv values and MUST not pass through a shell.
- Prompt delivery MUST require a live, metadata-matching managed session and MUST never fall back to agent selection or session creation.
- Explicit attach MUST require an existing live managed session and MUST have no creation or process-start side effects.
- A detached worktree MUST only be accepted for reattachment when recognized in-progress rebase metadata names the expected branch and the managed session is already live.
- Prompt delivery MUST preserve UTF-8 and line feeds, use literal paste semantics, and submit only after the complete message has been pasted.
- Prompt delivery MUST use an exact session/pane target and MUST not pass the message through a shell or tmux key-name parser.
- Managed tmux commands MUST use the CLI's configuration-isolation behavior rather than sourcing the user's tmux configuration.
- Managed sessions MUST have mouse capture enabled, and the server MUST have extended-key reporting enabled before the agent is attached.
- New session metadata MUST retain the created agent pane identity so later prompts do not depend on the caller's current pane.
- Dead agent panes MUST not receive prompts or be reported as active agents.
- Stale session metadata MUST never be reported as an active agent.
- Removal MUST terminate the session before deleting its worktree.
- The CLI MUST report a clear prerequisite error when tmux is unavailable.
- A managed session MUST explicitly set window, pane, and status styling to prevent global tmux option leakage.
- Interactive list selection MUST reuse the run flow for an existing worktree and MUST NOT create a new worktree.

## Alternatives and trade-offs

- A Rust-owned PTY broker would remove the external dependency but substantially increases implementation and failure-management cost.
- Direct process execution preserves the simplest terminal model but cannot satisfy reattachment.
- Scanning arbitrary processes would show agents launched outside the CLI but creates unreliable ownership and identification semantics.

## Consequences

Users can detach with the direct `Ctrl-]` shortcut or tmux's standard `Ctrl-b d` sequence and later reattach through `run` or `attach`. Scripts can create a session with deterministic agent selection and `--detach` without opening a terminal UI. The status line makes the managed-session context and detach action visible without changing the agent command. The CLI must manage session naming, stale state, tmux ownership metadata, pane liveness, prompt delivery, rebase-detached reattachment, serialized local lifecycle operations, and forced termination. tmux installation is required on supported systems. Prompt delivery is terminal input transport rather than an agent-level acknowledgement protocol; callers remain responsible for shell quoting before invoking the CLI.

## Enforcement

Integration tests MUST cover session creation, reuse and attach, picker suppression for live sessions, deterministic agent precedence, unknown-agent and missing-selection failures, non-terminal and detach behavior, literal runtime argv, explicit attach without creation or restart, prompt delivery without attach or picker interaction, literal Unicode and multiline prompt content, missing/dead session handling, stale-session handling, tmux ownership metadata mismatch, rebase-detached reattachment, arbitrary-detached and wrong-branch rejection, list output, tmux prerequisite failure, removal ordering, serialized lifecycle operations, the managed-session status line and detach binding, and the managed mouse and extended-key options. Integration tests MUST verify that session-scoped window, pane, and status styling options are explicitly set and override global defaults. Integration tests MUST verify interactive list selection triggers run-reuse for the selected worktree and non-interactive list retains table output.

## Revisit when

Revisit this decision if tmux is unavailable on a required platform, if agent UI fidelity is materially affected, or if a smaller reliable PTY-session implementation becomes preferable.
