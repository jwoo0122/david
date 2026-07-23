# david

`david` creates Git worktrees in a user-level directory and runs configured agents in persistent tmux sessions.

## Install

Supported platforms are macOS and Linux. Install one of the following:

### Homebrew

```sh
brew install jwoo0122/tap/david
```

This installs a prebuilt binary. Git and tmux 3.2 or newer are also required.

### Cargo

```sh
cargo install david
```

Cargo installation additionally requires Rust and Cargo.

## Configuration

Configuration, worktrees, and session state follow the XDG Base Directory layout:

| Data | Path |
| --- | --- |
| Configuration | `$XDG_CONFIG_HOME/david/config.toml` or `~/.config/david/config.toml` |
| Worktrees | `$XDG_DATA_HOME/david/worktrees/<repo-id>/<worktree>` or `~/.local/share/david/worktrees/<repo-id>/<worktree>` |
| Session state | `$XDG_STATE_HOME/david/sessions/` or `~/.local/state/david/sessions/` |

Only absolute XDG paths are used; relative values fall back to the paths above.

Create or update the configuration interactively:

```sh
david setup
```

`setup` can run from any directory. It merges with the existing agent list, replaces an agent when its name is entered again, and preserves `default_agent`.

You can also edit `config.toml` directly:

```toml
default_agent = "codex"

[agents.codex]
command = "codex"
args = ["--profile", "default"]

[agents.claude]
command = "claude"
args = []
```

`default_agent`, when present, must name a configured agent. Commands run directly rather than through a shell. Put persistent flags in `args`.

If data is still stored in `~/.david`, move it to the XDG locations with:

```sh
david migrate
# Preview the changes without modifying files.
david migrate --dry-run
```

Migration does not overwrite existing destination files. Resolve any reported conflict and retry.

## Features and usage

### Create or reuse a worktree

Run from a directory inside the source Git repository:

```sh
david run feature-login
```

When the worktree does not exist, `david` creates it from the current `HEAD`, on a same-named branch, under the configured data directory. The source repository must be clean. Existing worktrees and their live managed sessions are reused.

Agent selection, in order:

1. `--agent <name>` or `-a <name>`
2. `DAVID_AGENT`
3. `default_agent` in the configuration
4. The sole configured agent
5. An interactive picker, when terminal interaction is available

A live session is reused before agent selection. Without a worktree name, `run` opens an interactive picker when possible; non-interactive runs require a name. Use `--no-interactive` to disable terminal interaction and `--detach`/`-d` to create or reuse a session without attaching:

```sh
david run -a codex -d feature-login
DAVID_AGENT=claude david run feature-login -- --model sonnet
```

Arguments after `--` are appended to the configured command as literal argv values:

```sh
david run -a codex feature-login -- --model gpt-5.6
```

### Attach and send prompts

Attach only to an existing managed session:

```sh
david attach feature-login
```

Send a prompt without attaching or starting a session:

```sh
david prompt feature-login "Review the failing tests"
```

The message is delivered exactly as received, including Unicode and newlines. Quote it for your shell; shell parsing happens before `david` receives it.

### Inspect managed worktrees

List worktrees and session status:

```sh
david list
# Stable machine-readable output:
david list --porcelain
david list --porcelain -z
```

Print one worktree's absolute path without querying tmux:

```sh
david path feature-login
david path -0 feature-login
```

### Remove a worktree

Removal terminates its managed session, removes the worktree, and deletes the paired local branch:

```sh
david remove feature-login
```

A dirty worktree is rejected unless `--force` is supplied. Branch deletion does not require a merge, so branch-only commits may be lost. `--force` applies to uncommitted worktree changes, not branch deletion.

### tmux sessions

Each worktree has at most one managed agent session. `david` uses a dedicated tmux server, does not load `~/.tmux.conf`, and explicitly configures session styling and interaction options. Detach with `Ctrl-]`; `Ctrl-b`, then `d`, remains available as a fallback.

Commands return `0` on success, `1` for runtime errors, and `2` for invalid command lines or unavailable agent selection.
