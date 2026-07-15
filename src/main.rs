use clap::{Parser, Subcommand};
use std::{env, io};
use tony::{App, Result, TmuxBackend, TonyPaths};

#[derive(Debug, Parser)]
#[command(
    name = "tony",
    version,
    about = "Manage Git worktrees and agent sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
    let cwd = env::current_dir()?;
    let app = App::new(TonyPaths::from_env()?, TmuxBackend::default());

    match cli.command {
        Command::Run { name } => {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            let stdout = io::stdout();
            let mut output = stdout.lock();
            app.run(&cwd, &name, &mut input, &mut output)
        }
        Command::List => {
            let stdout = io::stdout();
            let mut output = stdout.lock();
            app.list(&cwd, &mut output)
        }
        Command::Remove { name, force } => app.remove(&cwd, &name, force),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
