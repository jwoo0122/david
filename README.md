# david

`david` creates Git worktrees in a user-level directory and runs configured agents in attachable tmux sessions.

## Prerequisites

- Git
- tmux
- Rust and Cargo for building

The first version targets macOS and Linux.

## Install

Install the prebuilt binary with Homebrew:

```text
brew install jwoo0122/tap/david
```

Alternatively, install from crates.io with Cargo:

```text
cargo install david
```

## Migrating from tony

This is a hard rebrand. `david` reads `~/.david` and manages `david-...` tmux sessions; it does not read the old `tony` namespace. Existing `tony` state must be migrated before the first `david` run.

## Agent configuration

Create `~/.david/config.toml`:

```toml
[agents.codex]
command = "codex"
args = []

[agents.claude]
command = "claude"
args = []
```

Commands are executed directly, not through a shell. Put flags in `args`.

## First-time setup

Run setup from any directory:

```text
david setup
```

It asks for an agent name, command, and optional arguments. Enter arguments as one shell-like line, for example `--model gpt-5 --profile "fast mode"`. After each agent, the complete configured list is shown. Press `Enter` at the agent-name prompt to finish. Existing agents are preserved, and entering the same name updates that agent.

## Usage

Run from any directory inside the source Git repository:

```text
david run feature-login
```

If the worktree does not exist, `david` creates a new branch from the current `HEAD` at:

```text
~/.david/worktrees/<repo-id>/feature-login
```

If a live agent session already exists, `david run` attaches to it. Otherwise it shows the configured agent list and starts the selected agent.

Move focus with the up/down arrow keys and press `Enter` to select an agent.

List managed worktrees and agents:

```text
david list
```

Detach from tmux without stopping the agent with `Ctrl-b`, then `d`.

Remove a clean worktree, its agent session, and its paired branch:

```text
david remove feature-login
```

Use `--force` only when the worktree's uncommitted changes should be discarded:

```text
david remove feature-login --force
```
