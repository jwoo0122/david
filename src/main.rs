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
    /// List managed worktrees and their active agent sessions.
    List,
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
