use clap::{Parser, Subcommand};
use david::{App, DavidPaths, Result, RunOptions, TmuxBackend};
use std::{env, io, io::IsTerminal};

#[derive(Debug, Parser)]
#[command(
    name = "david",
    version,
    about = "Manage Git worktrees and agent sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or update the user-scoped agent configuration.
    Setup,
    /// Create or reuse a worktree and attach to its agent session.
    Run {
        /// Name of the managed worktree.
        name: String,
        /// Select a configured agent without opening the picker.
        #[arg(short = 'a', long)]
        agent: Option<String>,
        /// Create or reuse the session without attaching to it.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Prohibit all interactive selection and terminal attachment.
        #[arg(long)]
        no_interactive: bool,
        /// Arguments appended to the configured agent command.
        #[arg(last = true, allow_hyphen_values = true)]
        agent_args: Vec<String>,
    },
    /// Attach to an existing managed agent session.
    Attach { name: String },
    /// Deliver a prompt to an existing managed agent session.
    Prompt {
        /// Name of the existing managed worktree.
        worktree: String,
        /// Exact message to deliver and submit.
        #[arg(allow_hyphen_values = true)]
        message: String,
    },
    /// List managed worktrees and their active agent sessions.
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
    /// Remove a worktree, terminate its managed agent session, and delete its paired branch.
    ///
    /// Without `--force`, dirty worktrees are rejected. With it, uncommitted worktree changes
    /// may be discarded. A clean worktree can be removed without it, even when the branch has
    /// unmerged commits.
    ///
    /// Removal always terminates the session, removes the worktree, atomically deletes the
    /// paired local branch if it remains unchanged, and then removes david's session metadata.
    /// Branch-only commits are intentionally lost. Branch deletion does not require a merged
    /// branch and is not configurable.
    ///
    /// Both `david remove <name> --force` and `david remove --force <name>` are supported.
    Remove {
        name: String,
        /// Discard uncommitted worktree changes; without it, dirty worktrees are rejected. It
        /// does not control branch deletion.
        #[arg(long)]
        force: bool,
    },
}

fn terminal_interaction_allowed(
    no_interactive: bool,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
) -> bool {
    !no_interactive && stdin_is_terminal && stderr_is_terminal
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = DavidPaths::from_env()?;

    match cli.command {
        Command::Setup => paths.setup(),
        command => {
            let cwd = env::current_dir()?;
            let app = App::new(paths, TmuxBackend::default());
            match command {
                Command::Run {
                    name,
                    agent,
                    detach,
                    no_interactive,
                    agent_args,
                } => {
                    let interactive = terminal_interaction_allowed(
                        no_interactive,
                        io::stdin().is_terminal(),
                        io::stderr().is_terminal(),
                    );
                    app.run_with_options(
                        &cwd,
                        &name,
                        RunOptions {
                            agent,
                            agent_args,
                            interactive,
                            attach: !detach && interactive,
                        },
                    )
                }
                Command::Attach { name } => app.attach(&cwd, &name),
                Command::Prompt { worktree, message } => app.prompt(&cwd, &worktree, &message),
                Command::List { porcelain, zero } => {
                    if porcelain {
                        let stdout = io::stdout();
                        let mut output = stdout.lock();
                        app.list_porcelain(&cwd, zero, &mut output)
                    } else if io::stdin().is_terminal() && io::stderr().is_terminal() {
                        app.list_interactive(&cwd)
                    } else {
                        let stdout = io::stdout();
                        let is_terminal = stdout.is_terminal();
                        let mut output = stdout.lock();
                        app.list(&cwd, is_terminal, &mut output)
                    }
                }
                Command::Path { name, zero } => {
                    let stdout = io::stdout();
                    let mut output = stdout.lock();
                    app.path(&cwd, &name, zero, &mut output)
                }
                Command::Remove { name, force } => app.remove(&cwd, &name, force),
                Command::Setup => unreachable!(),
            }
        }
    }
}

fn main() {
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
            "terminates the session",
            "removes the worktree",
            "atomically deletes the",
            "paired local branch if it remains unchanged",
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
    fn run_cli_preserves_runtime_argument_boundaries() {
        let cli = Cli::try_parse_from([
            "david",
            "run",
            "-a",
            "codex",
            "-d",
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
                detach,
                no_interactive,
                agent_args,
            } => {
                assert_eq!(name, "feature-login");
                assert_eq!(agent.as_deref(), Some("codex"));
                assert!(detach);
                assert!(!no_interactive);
                assert_eq!(agent_args, ["--model", "gpt 5.6", "$()"]);
            }
            command => panic!("unexpected command: {command:?}"),
        }
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
