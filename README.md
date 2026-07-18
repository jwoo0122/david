# david

`david` creates Git worktrees in a user-level directory and runs configured agents in attachable tmux sessions.

## Prerequisites

- Git
- tmux 3.2 or newer
- Rust and Cargo for building

The first version targets macOS and Linux.

David-managed sessions do not load `~/.tmux.conf`; david explicitly enables session-scoped mouse support and server-scoped extended keys for them. The latter affects other sessions sharing the tmux server; use a separate server if that is unacceptable.

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
default_agent = "codex"

[agents.codex]
command = "codex"
args = ["--profile", "default"]

[agents.claude]
command = "claude"
args = []
```

`default_agent`, when present, must name one of the configured `[agents.<name>]` entries. Commands are executed directly, not through a shell. Put persistent flags in `args`.

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

If a live managed session already exists, `run` reuses it without selecting an agent. Otherwise the agent is resolved in this order:

1. `--agent <name>` / `-a`
2. `DAVID_AGENT`
3. `default_agent` in the config
4. the sole configured agent
5. the interactive picker, only when interaction is enabled and both stdin and stderr are terminals

An unknown explicit agent fails without opening the picker. In a non-interactive run, no picker or terminal attachment is attempted; a missing selection fails immediately with exit code `2`. Use `--no-interactive` to enforce this even from a terminal. Use `--detach`/`-d` to create or reuse the session and return without attaching:

```text
david run -a codex -d feature-login
DAVID_AGENT=claude david run feature-login -- --model sonnet
```

Arguments after `--` are appended to the configured `args` and passed as literal argv values, without shell interpretation:

```text
david run -a codex feature-login -- --model gpt-5.6
```

Attach explicitly only to an existing managed session:

```text
david attach feature-login
```

`attach` never creates a worktree or session, selects an agent, or starts a process. During an in-progress rebase, `run` and `attach` still require the rebase metadata to identify the expected worktree branch and a matching live managed session with a live agent pane; arbitrary detached, wrong-branch, or dead-pane sessions are rejected.

Send a prompt to an existing live managed agent session:

```text
david prompt feature-login "Review the failing tests"
```

The single `message` argument is delivered exactly as received, including spaces, quotes, shell metacharacters, Unicode, and multiline content, then submitted. Quote or escape it for your shell; shell parsing happens before `david` receives it. Use `--` before a message that is itself a CLI option, for example `david prompt feature-login -- --help`. `david prompt` does not attach to, start, or select an agent. It fails if `<worktree>` is not an existing managed worktree on its expected branch, if the corresponding session is missing, stopped, unmanaged, or has mismatched metadata, or if tmux is unavailable or cannot deliver the prompt.

List managed worktrees and agents:

```text
david list
```

The default table is intended for human use. For automation, use porcelain output:

```text
david list --porcelain
# Add -z for NUL-terminated fields and records.
david list --porcelain -z
```

Each porcelain record contains these fields in this order:

```text
name <task-name>
branch <branch-or-(detached)>
agent <running-agent-or->
session <active|inactive|unknown>
path <absolute-worktree-path>
```

LF output terminates fields and records are separated by one additional LF. NUL output terminates fields and uses one additional NUL only between records; embedded newlines in values are preserved. An empty porcelain list writes zero bytes.

Print one managed worktree path without starting or querying tmux:

```text
david path feature-login
# Add -0 for one NUL terminator.
david path -0 feature-login
```

`david path` requires an existing managed worktree on its same-named branch and writes only its absolute path plus the selected terminator.

Exit statuses are `0` for successful commands, `1` for Git/tmux/filesystem and other runtime errors, and `2` for malformed command lines rejected by Clap. In particular, `david list -z` is rejected; `-z` is valid only with `--porcelain`.

While attached, the tmux status line shows the `DAVID` marker, project/worktree/agent names, and the detach shortcut. Detach without stopping the agent with `Ctrl-]`. The standard `Ctrl-b`, then `d`, sequence remains available as a fallback.

Remove a clean worktree, terminate its agent session, and delete its paired local branch:

```text
david remove feature-login
```

Removal always terminates the agent session, removes the worktree, atomically deletes the paired
local branch if it remains at the revision validated before removal, and then removes david's
session metadata. Branch deletion does not require a merged branch and is not configurable:
commits reachable only from that branch are intentionally lost, including unmerged branch-only
commits. If the branch changes or deletion fails after the worktree is removed, david retains the
session metadata and reports the failure.

Without `--force`, removal rejects a worktree with uncommitted changes. With it, those changes
may be discarded; it is not needed for a clean worktree, even when its branch is unmerged. Use
either argument order:

```text
david remove feature-login --force
david remove --force feature-login
```
