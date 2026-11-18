mod doctor;
mod launch;
mod panelist;
mod review;
mod runner;

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use runner::Agent;

/// Isolated, cross-model panel review of a GitHub pull request.
#[derive(Parser)]
#[command(
    version,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// The pull request: number or URL.
    #[arg(required = true)]
    pr: Option<String>,
    /// The PR's repository (owner/name); default: the current repository.
    #[arg(long)]
    repo: Option<String>,
    /// Run this panelist only (development flag; only `reliability` is ported so far).
    #[arg(long, required = true)]
    only: Option<String>,
    /// Agent for a role, e.g. `reliability=codex`. Repeatable.
    #[arg(long = "role-agent", value_name = "ROLE=AGENT", value_parser = role_agent)]
    role_agents: Vec<(String, Agent)>,
    /// Keep the review worktree after the run.
    #[arg(long)]
    keep_worktree: bool,
    /// Per-pass timeout in seconds; a pass that hits it gets one wrap-up turn.
    #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Check claude, codex, gh and orca, their logins, and the reviewer isolation self-test.
    Doctor,
}

fn role_agent(s: &str) -> Result<(String, Agent), String> {
    let (role, agent) = s.split_once('=').ok_or("expected ROLE=AGENT")?;
    Ok((role.to_string(), agent.parse()?))
}

fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    if let Some(Command::Doctor) = cli.command {
        return Ok(if doctor::run()? {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let args = review::Args {
        pr: cli.pr.unwrap_or_default(),
        repo: cli.repo,
        only: cli.only.unwrap_or_default(),
        role_agents: cli.role_agents,
        keep_worktree: cli.keep_worktree,
        timeout: Duration::from_secs(cli.timeout),
    };
    // Spec §2: exit 2 when the review failed, whether in prepare or in a pass.
    Ok(match review::run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    })
}
