---
id: configuration.agent-setup
status: accepted
scope: configuration
decision_type: workflow
applies_to:
  - setup command
  - agent configuration creation and update
summary: The setup command interactively creates or merges the user-scoped agent configuration without requiring a project repository.
constrains: []
depends_on:
  - runtime.agent-session
supersedes: []
superseded_by: []
last_reviewed: "2026-07-16"
---

# Interactive agent configuration setup

## Decision question

How does a first-time or returning user create and update the configured agent list?

## Current decision

`david setup` MUST work without a Git repository or a live tmux session. It MUST prompt for an agent name, command, and one-line argument string using the CLI's interactive prompt dependency. The argument string MUST be parsed into direct-execution arguments with shell-style quote removal; the agent command MUST NOT be executed while configuring it.

The command MUST load an existing `~/.david/config.toml` when present, preserve agents not mentioned in the current setup session, and replace an existing agent when the entered name matches it. After each completed agent entry it MUST show the complete resulting agent list. An empty agent name MUST finish the loop. The command MUST reject finishing when no agent is configured, then MUST create the user-scoped directory structure and write the configuration.

## Context and forces

Manually creating a hidden directory and TOML file is unnecessary first-run friction. A setup flow needs to preserve explicit user configuration on re-run while allowing a same-name entry to correct or update an agent. Direct process execution requires arguments to remain separate values rather than a shell command string.

## Invariants

- Setup MUST NOT require the current directory to be inside a Git repository.
- Setup MUST NOT require tmux or execute configured agent commands.
- Existing configured agents MUST remain unless their names are explicitly re-entered.
- The agent name, command, and parsed arguments MUST be written to the existing TOML schema. An optional `default_agent` value MUST be preserved when setup merges and rewrites configuration, and runtime loading MUST reject it when it does not name a configured agent.
- A setup session with no configured agents MUST fail without writing an unusable configuration.
- The resulting configuration MUST be stored at `~/.david/config.toml` and the managed state directories MUST be scaffolded.

## Alternatives and trade-offs

- Replacing the entire configuration on every setup is simpler but can silently discard existing agents.
- Requiring manual TOML editing avoids a prompt loop but makes first use needlessly difficult.
- Treating the argument line as one opaque argument would break commands that expect separate flags and values.

## Consequences

The setup command can be run before the user enters a project or installs tmux. Re-running it rewrites the TOML representation of the merged agent map, so hand-written formatting or comments are not preserved.

## Enforcement

Tests MUST cover empty configuration rejection, argument parsing, merge and same-name replacement, directory/config creation, and the existing run flow consuming the resulting configuration. A manual TTY check MUST cover entering multiple agents, seeing the accumulated list, and finishing with an empty name.

## Revisit when

Revisit this decision if configuration must preserve comments or formatting, if setup needs project-specific agents, or if agent launch arguments need shell expansion rather than direct execution.
