# tool

`tool` creates Git worktrees in a user-level directory and runs configured agents in attachable tmux sessions.

## Prerequisites

- Git
- tmux
- Rust and Cargo for building

The first version targets macOS and Linux.

## Agent configuration

Create `~/.tool/config.toml`:

```toml
[agents.codex]
command = "codex"
args = []

[agents.claude]
command = "claude"
args = []
```

Commands are executed directly, not through a shell. Put flags in `args`.

## Usage

Run from any directory inside the source Git repository:

```text
tool run feature-login
```

If the worktree does not exist, `tool` creates a new branch from the current `HEAD` at:

```text
~/.tool/worktrees/<repo-id>/feature-login
```

If a live agent session already exists, `tool run` attaches to it. Otherwise it shows the configured agent list and starts the selected agent.

List managed worktrees and agents:

```text
tool list
```

Detach from tmux without stopping the agent with `Ctrl-b`, then `d`.

Remove a clean worktree, its agent session, and its paired branch:

```text
tool remove feature-login
```

Use `--force` only when the worktree's uncommitted changes should be discarded:

```text
tool remove feature-login --force
```
