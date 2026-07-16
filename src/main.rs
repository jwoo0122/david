use clap::{Parser, Subcommand};
use david::{App, DavidPaths, Result, TmuxBackend};
use std::{env, io};

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
    Run { name: String },
    /// Deliver a prompt to an existing managed agent session.
    Prompt {
        /// Name of the existing managed worktree.
        worktree: String,
        /// Exact message to deliver and submit.
        #[arg(allow_hyphen_values = true)]
        message: String,
    },
    /// List managed worktrees and their active agent sessions.
    List,
    /// Remove a worktree, terminate its managed agent session, and delete its paired branch.
    ///
    /// Without `--force`, dirty worktrees are rejected. With it, uncommitted worktree changes
    /// may be discarded. A clean worktree can be removed without it, even when the branch has
    /// unmerged commits.
    ///
    /// Removal always terminates the session, removes the worktree, force-deletes the paired
    /// local branch (`git branch -D -- <name>`), and removes david's session metadata, in that
    /// order. Branch-only commits are intentionally lost. Branch deletion is always forced and
    /// is not configurable.
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = DavidPaths::from_env()?;

    match cli.command {
        Command::Setup => paths.setup(),
        command => {
            let cwd = env::current_dir()?;
            let app = App::new(paths, TmuxBackend::default());
            match command {
                Command::Run { name } => app.run(&cwd, &name),
                Command::Prompt { worktree, message } => app.prompt(&cwd, &worktree, &message),
                Command::List => {
                    let stdout = io::stdout();
                    let mut output = stdout.lock();
                    app.list(&cwd, &mut output)
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
        std::process::exit(1);
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
            "git branch -D -- <name>",
            "Branch-only commits are intentionally lost",
            "Without `--force`, dirty worktrees are rejected",
            "Discard uncommitted worktree changes; without it, dirty worktrees are rejected. It does not control branch deletion",
            "Branch deletion is always forced",
            "is not configurable",
            "david remove <name> --force",
            "david remove --force <name>",
        ] {
            assert!(help.contains(expected), "missing {expected:?} in:\n{help}");
        }
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
}
