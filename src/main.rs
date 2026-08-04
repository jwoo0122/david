use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{CompleteEnv, engine::CompletionCandidate};
use david::{App, DavidPaths, Git, Result, RunOptions, SessionBackend, TmuxBackend, ToolError};

mod backend;

use backend::{DirectBackend, direct_agent_is_resolvable};
use std::{env, io, io::IsTerminal, path::Path};

#[derive(Debug, Parser)]
#[command(
    name = "david",
    version,
    about = "Manage Git worktrees and run coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or update the user-scoped agent configuration.
    Setup,
    /// Migrate legacy ~/.david storage to XDG base directories.
    Migrate {
        /// Report planned moves and conflicts without making changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Create or reuse a worktree and run its agent in the current terminal.
    Run {
        /// Name of the managed worktree. If omitted in an interactive terminal, a picker is shown.
        #[arg(add = clap_complete::ArgValueCompleter::new(worktree_completions))]
        name: Option<String>,
        /// Select a configured agent without opening the picker.
        #[arg(short = 'a', long)]
        agent: Option<String>,
        /// Use the legacy persistent tmux session backend.
        #[arg(long)]
        tmux: bool,
        /// Use the legacy tmux backend and create or reuse the session without attaching to it.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Prohibit interactive selection. In tmux mode, also prohibit attachment.
        #[arg(long)]
        no_interactive: bool,
        /// Arguments appended to the configured agent command.
        #[arg(last = true, allow_hyphen_values = true)]
        agent_args: Vec<String>,
    },
    /// Open a managed worktree in the program named by EDITOR.
    Edit {
        /// Name of the existing managed worktree.
        #[arg(add = clap_complete::ArgValueCompleter::new(worktree_completions))]
        worktree: String,
    },
    /// Attach to an existing legacy tmux agent session.
    Attach { name: String },
    /// Deliver a prompt to an existing legacy tmux agent session.
    Prompt {
        /// Name of the existing managed worktree.
        worktree: String,
        /// Exact message to deliver and submit.
        #[arg(allow_hyphen_values = true)]
        message: String,
    },
    /// List managed worktrees and any active legacy tmux sessions.
    List {
        /// Emit stable machine-readable records instead of the human table.
        #[arg(long)]
        porcelain: bool,
        /// Terminate each porcelain item with NUL instead of LF.
        #[arg(short = 'z', requires = "porcelain")]
        zero: bool,
    },
    /// Print the absolute path of a managed worktree.
    Path {
        /// Terminate the path with NUL instead of LF.
        #[arg(short = '0')]
        zero: bool,
        name: String,
    },
    /// Remove a worktree, terminate its legacy tmux session if present, and delete its branch.
    ///
    /// Without `--force`, dirty worktrees are rejected. With it, uncommitted worktree changes
    /// may be discarded. A clean worktree can be removed without it, even when the branch has
    /// unmerged commits.
    ///
    /// Removal terminates a legacy managed session when one exists, removes the worktree,
    /// atomically deletes the paired local branch if it remains unchanged, and then removes
    /// David's session metadata. Branch-only commits are intentionally lost. Branch deletion
    /// does not require a merged branch and is not configurable.
    ///
    /// Both `david remove <name> --force` and `david remove --force <name>` are supported.
    Remove {
        #[arg(add = clap_complete::ArgValueCompleter::new(worktree_completions))]
        name: String,
        /// Discard uncommitted worktree changes; without it, dirty worktrees are rejected. It
        /// does not control branch deletion.
        #[arg(long)]
        force: bool,
    },
}

fn worktree_completions(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(paths) = DavidPaths::from_env() else {
        return Vec::new();
    };
    let Ok(cwd) = env::current_dir() else {
        return Vec::new();
    };
    let Ok(names) = paths.worktree_names(&cwd) else {
        return Vec::new();
    };
    let current = current.to_string_lossy();
    names
        .into_iter()
        .filter(|name| name.starts_with(current.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

fn terminal_interaction_allowed(
    no_interactive: bool,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
) -> bool {
    !no_interactive && stdin_is_terminal && stderr_is_terminal
}

fn execute_run<S: SessionBackend>(
    app: &App<S>,
    cwd: &Path,
    name: Option<String>,
    options: RunOptions,
    name_selection_allowed: bool,
) -> Result<()> {
    match name {
        Some(name) => app.run_with_options(cwd, &name, options),
        None if name_selection_allowed => app.run_interactive(cwd, options),
        None => Err(ToolError::Message(
            "non-interactive run requires a worktree name".to_owned(),
        )),
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = DavidPaths::from_env()?;

    match cli.command {
        Command::Setup => paths.setup(),
        Command::Migrate { dry_run } => {
            let git = Git::default();
            paths.migrate(&git, dry_run)
        }
        command => {
            let cwd = env::current_dir()?;
            match command {
                Command::Run {
                    name,
                    agent,
                    tmux,
                    detach,
                    no_interactive,
                    agent_args,
                } => {
                    let selection_interactive = terminal_interaction_allowed(
                        no_interactive,
                        io::stdin().is_terminal(),
                        io::stderr().is_terminal(),
                    );
                    if name.is_none() && !selection_interactive {
                        return Err(ToolError::Message(
                            "non-interactive run requires a worktree name".to_owned(),
                        ));
                    }
                    if tmux || detach {
                        let options = RunOptions {
                            agent,
                            agent_args,
                            interactive: selection_interactive,
                            attach: !detach && selection_interactive,
                        };
                        execute_run(
                            &App::new(paths, TmuxBackend::default()),
                            &cwd,
                            name,
                            options,
                            selection_interactive,
                        )
                    } else {
                        if !selection_interactive
                            && !direct_agent_is_resolvable(&paths, agent.as_deref())?
                        {
                            return Err(ToolError::AgentSelectionUnavailable);
                        }
                        let options = RunOptions {
                            agent,
                            agent_args,
                            interactive: true,
                            attach: true,
                        };
                        execute_run(
                            &App::new(paths, DirectBackend::default()),
                            &cwd,
                            name,
                            options,
                            selection_interactive,
                        )
                    }
                }
                Command::Edit { worktree } => {
                    App::new(paths, DirectBackend::default()).edit(&cwd, &worktree)
                }
                Command::Attach { name } => {
                    App::new(paths, TmuxBackend::default()).attach(&cwd, &name)
                }
                Command::Prompt { worktree, message } => {
                    App::new(paths, TmuxBackend::default()).prompt(&cwd, &worktree, &message)
                }
                Command::List { porcelain, zero } => {
                    let app = App::new(paths, DirectBackend::default());
                    let stdout = io::stdout();
                    let is_terminal = stdout.is_terminal();
                    let mut output = stdout.lock();
                    if porcelain {
                        app.list_porcelain(&cwd, zero, &mut output)
                    } else {
                        app.list(&cwd, is_terminal, &mut output)
                    }
                }
                Command::Path { name, zero } => {
                    let stdout = io::stdout();
                    let mut output = stdout.lock();
                    App::new(paths, DirectBackend::default()).path(&cwd, &name, zero, &mut output)
                }
                Command::Remove { name, force } => {
                    App::new(paths, DirectBackend::default()).remove(&cwd, &name, force)
                }
                Command::Setup => unreachable!(),
                Command::Migrate { .. } => unreachable!(),
            }
        }
    }
}

fn main() {
    CompleteEnv::with_factory(Cli::command).complete();
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn remove_cli_accepts_force_before_or_after_name() {
        for arguments in [
            ["david", "remove", "feature-login", "--force"],
            ["david", "remove", "--force", "feature-login"],
        ] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            assert!(matches!(
                cli.command,
                Command::Remove {
                    name,
                    force: true
                } if name == "feature-login"
            ));
        }
    }

    #[test]
    fn remove_cli_help_describes_destructive_lifecycle_and_force_scope() {
        let mut cli = Cli::command();
        let help = cli
            .find_subcommand_mut("remove")
            .unwrap()
            .render_long_help()
            .to_string();

        for expected in [
            "terminates a legacy managed session",
            "removes the worktree",
            "atomically deletes the paired local branch if it remains unchanged",
            "Branch-only commits are intentionally lost",
            "Without `--force`, dirty worktrees are rejected",
            "Discard uncommitted worktree changes; without it, dirty worktrees are rejected. It does not control branch deletion",
            "Branch deletion does not require a merged branch",
            "is not configurable",
            "david remove <name> --force",
            "david remove --force <name>",
        ] {
            assert!(help.contains(expected), "missing {expected:?} in:\n{help}");
        }
    }

    #[test]
    fn run_cli_defaults_to_direct_and_preserves_runtime_argument_boundaries() {
        let cli = Cli::try_parse_from([
            "david",
            "run",
            "-a",
            "codex",
            "feature-login",
            "--",
            "--model",
            "gpt 5.6",
            "$()",
        ])
        .unwrap();

        match cli.command {
            Command::Run {
                name,
                agent,
                tmux,
                detach,
                no_interactive,
                agent_args,
            } => {
                assert_eq!(name.as_deref(), Some("feature-login"));
                assert_eq!(agent.as_deref(), Some("codex"));
                assert!(!tmux);
                assert!(!detach);
                assert!(!no_interactive);
                assert_eq!(agent_args, ["--model", "gpt 5.6", "$()"]);
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn run_cli_supports_explicit_legacy_tmux_mode() {
        let cli =
            Cli::try_parse_from(["david", "run", "--tmux", "-a", "claude", "feature-login"])
                .unwrap();

        assert!(matches!(
            cli.command,
            Command::Run {
                tmux: true,
                detach: false,
                ..
            }
        ));
    }

    #[test]
    fn detach_remains_available_as_a_legacy_tmux_operation() {
        let cli = Cli::try_parse_from(["david", "run", "-d", "feature-login"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Run {
                tmux: false,
                detach: true,
                ..
            }
        ));
    }

    #[test]
    fn noninteractive_or_nonterminal_input_disables_interaction() {
        assert!(terminal_interaction_allowed(false, true, true));
        assert!(!terminal_interaction_allowed(true, true, true));
        assert!(!terminal_interaction_allowed(false, false, true));
        assert!(!terminal_interaction_allowed(false, true, false));
    }

    #[test]
    fn attach_cli_parses_the_worktree_name() {
        let cli = Cli::try_parse_from(["david", "attach", "feature-login"]).unwrap();
        assert!(matches!(cli.command, Command::Attach { name } if name == "feature-login"));
    }

    #[test]
    fn prompt_cli_preserves_message_bytes_received_by_clap() {
        let message = "--literal 'quotes' $() 😀\tline one\nline two";
        let cli = Cli::try_parse_from(["david", "prompt", "feature", message]).unwrap();

        match cli.command {
            Command::Prompt {
                worktree,
                message: parsed,
            } => {
                assert_eq!(worktree, "feature");
                assert_eq!(parsed, message);
            }
            command => panic!("unexpected command: {command:?}"),
        }

        let cli = Cli::try_parse_from(["david", "prompt", "feature", ""]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Prompt { message, .. } if message.is_empty()
        ));

        let cli = Cli::try_parse_from(["david", "prompt", "feature", "--", "--help"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Prompt { message, .. } if message == "--help"
        ));
    }

    #[test]
    fn list_zero_requires_porcelain() {
        let error = Cli::try_parse_from(["david", "list", "-z"]).unwrap_err();

        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn list_cli_parses_porcelain_and_zero_options() {
        let cli = Cli::try_parse_from(["david", "list", "--porcelain", "-z"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::List {
                porcelain: true,
                zero: true
            }
        ));
    }

    #[test]
    fn path_cli_parses_zero_option_and_name() {
        let cli = Cli::try_parse_from(["david", "path", "-0", "feature"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Path {
                name,
                zero: true
            } if name == "feature"
        ));
    }
}
