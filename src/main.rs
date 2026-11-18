mod coordinator;
mod doctor;
mod launch;
mod panelist;
mod reply;
mod result;
mod review;
mod runner;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use result::Gate;
use runner::Agent;

/// Isolated, cross-model panel review of a GitHub pull request.
///
/// Exit code: 0 no gated findings, 1 gated findings, 2 the review failed.
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
    /// How reviewers run.
    #[arg(long, value_enum, default_value_t = RunnerKind::Headless)]
    runner: RunnerKind,
    /// The model family that wrote the change; most discovery and the judge go to the other.
    #[arg(long, default_value_t = Agent::Claude, value_parser = parse_agent)]
    author: Agent,
    /// Agent for a role (intent_contracts, reliability, security, coordinator, judge),
    /// e.g. `security=claude`. Repeatable.
    #[arg(long = "role-agent", value_name = "ROLE=AGENT", value_parser = role_agent)]
    role_agents: Vec<(String, Agent)>,
    /// Model for a role, e.g. `judge=gpt-5.5`. Repeatable. Default: the CLI's own.
    #[arg(long = "role-model", value_name = "ROLE=MODEL", value_parser = role_model)]
    role_models: Vec<(String, String)>,
    /// Reasoning effort for a role, e.g. `coordinator=high`. Repeatable.
    #[arg(long = "role-effort", value_name = "ROLE=EFFORT", value_parser = role_effort)]
    role_efforts: Vec<(String, String)>,
    /// The task's acceptance criteria, for the intent lens and the coordinator.
    #[arg(long, value_name = "PATH")]
    context_file: Option<PathBuf>,
    /// The reporting bar, e.g. `p<=2,conf>=0.8` (the default).
    #[arg(long, default_value = "p<=2,conf>=0.8", conflicts_with = "all")]
    gate: Gate,
    /// Report every finding (no gate).
    #[arg(long)]
    all: bool,
    /// Print result.json instead of the summary.
    #[arg(long)]
    json: bool,
    /// Keep the review worktree after the run.
    #[arg(long)]
    keep_worktree: bool,
    /// Per-pass timeout in seconds; a pass that hits it gets one wrap-up turn.
    #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,
    /// The whole review's budget in seconds; a pass never runs past it.
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
    budget: u64,
    /// Run this panelist alone, without coordinator and judge (a development flag).
    #[arg(long)]
    only: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum RunnerKind {
    Headless,
}

#[derive(Subcommand)]
enum Command {
    /// Check claude, codex, gh and orca, their logins, and the reviewer isolation self-test.
    Doctor,
    /// Print the result of the latest finished review of a PR, or of a run dir.
    Show {
        /// A run dir, or a PR number or URL.
        target: String,
        /// The PR's repository (owner/name); default: the current repository.
        #[arg(long)]
        repo: Option<String>,
        /// Print result.json instead of the summary.
        #[arg(long)]
        json: bool,
    },
}

fn parse_agent(s: &str) -> Result<Agent, String> {
    s.parse()
}

fn role_agent(s: &str) -> Result<(String, Agent), String> {
    let (role, agent) = s.split_once('=').ok_or("expected ROLE=AGENT")?;
    Ok((role.to_string(), agent.parse()?))
}

/// A model name goes into the CLI's argv as a flag's value: nothing that reads as a flag.
fn role_model(s: &str) -> Result<(String, String), String> {
    let (role, model) = s.split_once('=').ok_or("expected ROLE=MODEL")?;
    let ok = |c: char| c.is_ascii_alphanumeric() || "._-:/[]".contains(c);
    if model.is_empty() || model.starts_with('-') || !model.chars().all(ok) {
        return Err(format!("bad model name {model:?}"));
    }
    Ok((role.to_string(), model.to_string()))
}

/// Checked against the role's agent once the agents are known.
fn role_effort(s: &str) -> Result<(String, String), String> {
    let (role, effort) = s.split_once('=').ok_or("expected ROLE=EFFORT")?;
    Ok((role.to_string(), effort.to_string()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Spec §2: exit 2 when the review failed, whether in prepare or in a pass.
    let code = match cli.command {
        Some(Command::Doctor) => {
            return match doctor::run() {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        Some(Command::Show { target, repo, json }) => review::show(&target, repo.as_deref(), json),
        None => {
            let RunnerKind::Headless = cli.runner;
            review::run(&review::Args {
                pr: cli.pr.unwrap_or_default(),
                repo: cli.repo,
                only: cli.only,
                author: cli.author,
                role_agents: cli.role_agents,
                role_models: cli.role_models,
                role_efforts: cli.role_efforts,
                context_file: cli.context_file,
                gate: if cli.all { Gate::NONE } else { cli.gate },
                json: cli.json,
                keep_worktree: cli.keep_worktree,
                timeout: Duration::from_secs(cli.timeout),
                budget: Duration::from_secs(cli.budget),
            })
        }
    };
    match code {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
