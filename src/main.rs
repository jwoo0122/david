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
    /// Remove a worktree and terminate its managed agent session.
    Remove {
        name: String,
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
                Command::List { porcelain, zero } => {
                    let stdout = io::stdout();
                    let mut output = stdout.lock();
                    if porcelain {
                        app.list_porcelain(&cwd, zero, &mut output)
                    } else {
                        app.list(&cwd, &mut output)
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
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
