mod doctor;
mod launch;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Isolated, cross-model panel review of a GitHub pull request.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check claude, codex, gh and orca, their logins, and the reviewer isolation self-test.
    Doctor,
}

fn main() -> anyhow::Result<ExitCode> {
    match Cli::parse().command {
        Command::Doctor => Ok(if doctor::run()? {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }),
    }
}
