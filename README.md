# tony

`tony` creates Git worktrees in a user-level directory and runs configured agents in attachable tmux sessions.

## Prerequisites

- Git
- tmux
- Rust and Cargo for building

The first version targets macOS and Linux.

## Agent configuration

Create `~/.tony/config.toml`:

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
tony setup
```

It asks for an agent name, command, and optional arguments. Enter arguments as one shell-like line, for example `--model gpt-5 --profile "fast mode"`. After each agent, the complete configured list is shown. Press `Enter` at the agent-name prompt to finish. Existing agents are preserved, and entering the same name updates that agent.

## Usage

Run from any directory inside the source Git repository:

```text
tony run feature-login
```

If the worktree does not exist, `tony` creates a new branch from the current `HEAD` at:

```text
~/.tony/worktrees/<repo-id>/feature-login
```

If a live agent session already exists, `tony run` attaches to it. Otherwise it shows the configured agent list and starts the selected agent.

Move focus with the up/down arrow keys and press `Enter` to select an agent.

List managed worktrees and agents:

```text
tony list
```

Detach from tmux without stopping the agent with `Ctrl-b`, then `d`.

Remove a clean worktree, its agent session, and its paired branch:

```text
tony remove feature-login
```

Use `--force` only when the worktree's uncommitted changes should be discarded:

```text
tony remove feature-login --force
```

## Releases

The package starts at version `1.0.0`. Pushes to `main` are inspected for Conventional Commits:

- `feat` creates a minor release.
- `fix` and `perf` create a patch release.
- `BREAKING CHANGE` or `!` creates a major release.
- `docs`, `chore`, `test`, and `refactor` do not create a release unless they are breaking.

The workflow adds a `chore(release): v<version>` commit and matching tag, then publishes the crate, GitHub Release artifacts, and the Homebrew formula.

The `tony` repository must have these Actions secrets:

- `CARGO_TOKEN`: crates.io API token.
- `HOMEBREW_TAP_TOKEN`: a GitHub token with write access to `jwoo0122/homebrew-tap`.

Repository Actions settings must allow `Read and write permissions`, because the version-bump workflow pushes the release commit and tag to `main`.

Homebrew users can install the prebuilt binary with:

```text
brew install jwoo0122/tap/tony
```
