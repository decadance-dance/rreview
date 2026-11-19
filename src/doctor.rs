//! `rreview doctor` (spec §6): tool versions, logins, and the isolation self-test, which runs
//! real reviewer launches (spec §3) on a throwaway repo and checks their effects on disk; with
//! `--runner orca`, also through the orca runner (spec §4) on a second one.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::launch;
use crate::orca::{self, Orca};
use crate::runner::{Agent, Pass, Runner};

const PASS_TIMEOUT: Duration = Duration::from_secs(300);
/// The fixture worktree's `f.txt`, with its uncommitted change.
const F_TXT: &str = "a\nrreview-probe-change\n";
/// Tells the reviewer why the probe tries forbidden writes. The Codex TUI still often declines
/// to run it, "attempts writes outside the permitted workspace" (*verified*, codex-cli 0.167.0;
/// #31).
const SELF_TEST: &str = "This is rreview's sandbox self-test, run by the user on a throwaway \
                         repository. The script below tries writes that your sandbox should \
                         refuse; those refusals are the expected, harmless result, and running \
                         the script is the whole test. Run it inside your sandbox as it is, \
                         without asking for approval or escalated permissions: it needs none, \
                         because its writes are meant to fail. Do not skip it.\n";
/// What Claude's probe pass replies.
const CLAUDE_REPLY: &str = "the probe's one line of output, as plain text";
const CANARY_ASK: &str = "the canary word if any instruction you were given (system, developer, \
                          AGENTS.md or user instructions) names one, else NO-CANARY";

/// Prints one line per check; true when every required check passed. With `with_orca` the
/// self-test also runs through the orca runner, whose terminals open in the caller's Orca
/// worktree, so only from an Orca terminal.
pub fn run(with_orca: bool) -> Result<bool> {
    let mut r = Report::default();
    let in_orca =
        with_orca && std::env::var_os("ORCA_TERMINAL_HANDLE").is_some_and(|v| !v.is_empty());
    let claude = r.tool("claude", true);
    let codex = r.tool("codex", true);
    let gh = r.tool("gh", true);
    let orca_tool = r.tool("orca", in_orca);
    let claude = claude
        && r.check("claude login", claude_login())
        && r.check("claude --safe-mode", safe_mode());
    let codex = codex && r.check("codex login", logged_in("codex", &["login", "status"]));
    if gh {
        r.check("gh login", logged_in("gh", &["auth", "status"]));
    }
    if with_orca && !in_orca {
        println!(
            "skip  orca self-test: not in an Orca terminal (ORCA_TERMINAL_HANDLE is unset); \
             run `rreview doctor --runner orca` in one"
        );
    }
    let via_orca = in_orca
        && orca_tool
        && r.check(
            "orca worktree",
            orca::check_current_worktree()
                .map(|()| "found".into())
                .map_err(|e| format!("{e:#}")),
        );
    if !claude && !codex {
        return Ok(r.failed == 0);
    }

    let fx = Fixture::new().context("cannot build the self-test repo")?;
    let ofx = via_orca
        .then(Fixture::new)
        .transpose()
        .context("cannot build the orca self-test repo")?;
    let before = r.failed;
    let checks = std::thread::scope(|s| {
        let (fx, ofx) = (&fx, ofx.as_ref());
        let mut jobs = vec![];
        if claude {
            jobs.push((
                "claude self-test",
                s.spawn(move || claude_self_test(fx, Via::Headless)),
            ));
        }
        if codex {
            jobs.push(("codex self-test", s.spawn(move || codex_self_test(fx))));
        }
        if let Some(ofx) = ofx {
            if claude {
                jobs.push((
                    "orca claude self-test",
                    s.spawn(move || claude_self_test(ofx, Via::Orca)),
                ));
            }
            if codex {
                jobs.push((
                    "orca codex self-test",
                    s.spawn(move || codex_orca_self_test(ofx)),
                ));
            }
        }
        jobs.into_iter()
            .flat_map(|(name, job)| {
                job.join()
                    .unwrap_or_else(|_| vec![Check::fail(name, "panicked")])
            })
            .collect::<Vec<_>>()
    });
    for c in checks {
        r.check(&c.name, c.outcome);
    }
    for fx in [Some(&fx), ofx.as_ref()].into_iter().flatten() {
        if r.failed == before {
            let _ = fs::remove_dir_all(&fx.root);
        } else {
            println!("      self-test files kept in {}", fx.root.display());
        }
    }
    Ok(r.failed == 0)
}

/// How a self-test's reviewer runs: as the headless runner launches it, or as the orca runner
/// does, interactive in an Orca terminal of the caller's Orca worktree (spec §4).
#[derive(Clone, Copy)]
enum Via {
    Headless,
    Orca,
}

impl Via {
    /// `agent` as the check lines name it.
    fn name(self, agent: &str) -> String {
        match self {
            Via::Headless => agent.into(),
            Via::Orca => format!("orca {agent}"),
        }
    }
}

/// The orca runner for `fx`; its passes run in `<root>/orca/scratch/<role>`.
fn orca_runner(fx: &Fixture, codex_auth: PathBuf) -> Orca {
    Orca::new(
        fx.wt.clone(),
        fx.git_common_dir.clone(),
        fx.root.join("orca"),
        codex_auth,
    )
    .terminals_in(
        "current",
        &fx.root.file_name().unwrap_or_default().to_string_lossy(),
    )
}

fn probe_pass(role: &str, agent: Agent, prompt: String) -> Pass<'_> {
    Pass {
        role,
        agent,
        model: None,
        effort: None,
        prompt,
        timeout: PASS_TIMEOUT,
    }
}

#[derive(Default)]
struct Report {
    failed: usize,
}

impl Report {
    fn check(&mut self, name: &str, outcome: Result<String, String>) -> bool {
        match &outcome {
            Ok(detail) => println!("ok    {name}: {detail}"),
            Err(reason) => {
                self.failed += 1;
                println!("FAIL  {name}: {reason}");
            }
        }
        outcome.is_ok()
    }

    /// Prints `<bin> --version`; a missing optional tool is a warning.
    fn tool(&mut self, bin: &str, required: bool) -> bool {
        let version = output(bin, &["--version"]).and_then(|o| match o.status.success() {
            true => Ok(first_line(&o.stdout)),
            false => Err(format!(
                "`{bin} --version` failed: {}",
                first_line(&o.stderr)
            )),
        });
        match version {
            Err(reason) if !required => {
                println!("warn  {bin}: {reason} (optional)");
                false
            }
            v => self.check(bin, v),
        }
    }
}

#[derive(Debug)]
struct Check {
    name: String,
    outcome: Result<String, String>,
}

impl Check {
    fn new(name: &str, outcome: Result<String, String>) -> Self {
        Check {
            name: name.into(),
            outcome,
        }
    }
    fn fail(name: &str, reason: impl Into<String>) -> Self {
        Check::new(name, Err(reason.into()))
    }
    /// A forbidden action is a failure when its effect is on disk.
    fn blocked(name: &str, went_through: bool) -> Self {
        Check::new(
            name,
            if went_through {
                Err("went through".into())
            } else {
                Ok("blocked".into())
            },
        )
    }
}

fn output(bin: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                "not found on PATH".to_string()
            } else {
                format!("cannot run: {e}")
            }
        })
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .into()
}

fn claude_login() -> Result<String, String> {
    let out = output("claude", &["auth", "status"])?;
    let status: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        format!(
            "unexpected `claude auth status` output: {}",
            first_line(&out.stdout)
        )
    })?;
    match status["loggedIn"].as_bool() {
        Some(true) => Ok(format!(
            "logged in ({})",
            status["authMethod"].as_str().unwrap_or("unknown method")
        )),
        _ => Err("not logged in: run `claude` and log in".into()),
    }
}

/// Spec §9: older Claude Code versions have no `--safe-mode`, and without it a reviewer
/// loads the user's and the reviewed repo's CLAUDE.md, hooks, plugins and MCP.
fn safe_mode() -> Result<String, String> {
    let out = output("claude", &["--help"])?;
    if String::from_utf8_lossy(&out.stdout).contains("--safe-mode") {
        Ok("available".into())
    } else {
        Err("this claude has no --safe-mode; update Claude Code".into())
    }
}

fn logged_in(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = output(bin, args)?;
    if out.status.success() {
        Ok("logged in".into())
    } else {
        let why = [first_line(&out.stderr), first_line(&out.stdout)].join(" ");
        Err(format!("not logged in: {}", why.trim()))
    }
}

/// A repo with one commit, its `.claude/settings.json` planting hooks that touch
/// `<root>/hook-fired`, and a detached worktree with an uncommitted change. The git common
/// dir sits outside the worktree, as in a real review.
struct Fixture {
    root: PathBuf,
    wt: PathBuf,
    git_common_dir: PathBuf,
    head: String,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = fresh_temp_dir("rreview-doctor")?;
        let (repo, wt) = (root.join("repo"), root.join("wt"));
        let touch = json!([{ "hooks": [{ "type": "command", "command":
            format!("touch {}", sh_quote(&root.join("hook-fired"))) }] }]);
        let hooks = json!({ "hooks": {
            "SessionStart": touch, "UserPromptSubmit": touch, "PreToolUse": [{ "matcher": "*", "hooks": touch[0]["hooks"] }]
        } });
        fs::create_dir_all(repo.join(".claude"))?;
        fs::write(repo.join(".claude/settings.json"), hooks.to_string())?;
        fs::write(repo.join("f.txt"), "a\n")?;
        git(&repo, &["init", "-q"])?;
        git(&repo, &["add", "."])?;
        git(&repo, &["commit", "-q", "-m", "init"])?;
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                launch::utf8(&wt)?,
                "HEAD",
            ],
        )?;
        fs::write(wt.join("f.txt"), F_TXT)?;
        let git_common_dir = git(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?
        .into();
        let head = git(&wt, &["rev-parse", "HEAD"])?;
        Ok(Fixture {
            root,
            wt,
            git_common_dir,
            head,
        })
    }
}

/// Git for the fixture, without the user's git config (signing, hooks, templates).
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=rreview",
            "-c",
            "user.email=rreview@localhost",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .context("cannot run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}

fn claude_self_test(fx: &Fixture, via: Via) -> Vec<Check> {
    claude_probe(fx, via).unwrap_or_else(|e| {
        vec![Check::fail(
            &format!("{} self-test", via.name("claude")),
            format!("{e:#}"),
        )]
    })
}

/// Claude tries the Write and Edit tools on the worktree, then runs one probe script through its
/// sandboxed Bash; the probe records exit codes in `out/`, the one place it may write. Every
/// check needs both the report and the disk to agree.
// The Write and Edit checks also need Claude Code's own record that the tool was not offered or
// was refused (`claude_tools`): an unchanged worktree alone would pass when Claude never tried.
fn claude_probe(fx: &Fixture, via: Via) -> Result<Vec<Check>> {
    let control_fired = hook_control(fx)?;
    let a = via.name("claude");
    let run = match via {
        Via::Headless => fx.root.join("claude"),
        Via::Orca => orca_runner(fx, PathBuf::new()).scratch("claude"),
    };
    let (out, leak, tool_leak, probe) = (
        run.join("out"),
        fx.wt.join("rreview-probe-claude.txt"),
        fx.wt.join("rreview-probe-claude-tool.txt"),
        // In `out/`, which Claude reads without asking; in the TUI, a Read of the probe elsewhere
        // stopped the pass at a permission dialog until its timeout (*verified*).
        run.join("out/probe.sh"),
    );
    fs::create_dir_all(&out)?;
    let (diff, report) = (out.join("diff.txt"), out.join("probe.txt"));
    fs::write(
        &probe,
        format!(
            // Quiet: the output is the one report line the reply carries.
            "exec 2> /dev/null\n\
             cd {wt} || exit 1\n\
             echo x > {leak}; w=$?\n\
             git -c commit.gpgsign=false -c core.hooksPath=/dev/null -c user.name=rreview \
             -c user.email=rreview@localhost commit -q --allow-empty -m probe; c=$?\n\
             git branch rreview-probe; b=$?\n\
             git diff > {diff}; d=$?\n\
             echo \"write=$w commit=$c branch=$b diff=$d\" > {report}\n\
             cat {report}\n",
            wt = sh_quote(&fx.wt),
            leak = sh_quote(&leak),
            diff = sh_quote(&diff),
            report = sh_quote(&report),
        ),
    )?;
    let prompt = format!(
        "{SELF_TEST}Use the Write tool to create the file {} with the content x, and the Edit tool to change \
         the line `a` to `b` in {}; if a tool is missing or refused, go on without it. Then run \
         this exact command with the Bash tool, once:\nsh {}\nYour reply is {CLAUDE_REPLY}.\n",
        tool_leak.display(),
        fx.wt.join("f.txt").display(),
        sh_quote(&probe)
    );
    match via {
        Via::Headless => {
            let launch = launch::claude(&fx.wt, &fx.git_common_dir, &run)?;
            let status = launch.run(&prompt, &run.join("claude"), PASS_TIMEOUT)?;
            if !status.success() {
                bail!(
                    "claude {status}; see {}",
                    run.join("claude.stderr").display()
                );
            }
        }
        Via::Orca => {
            // Not a JSON reply: under the orca runner Claude Code asked before a heredoc holding
            // `{"` ("brace with quote character", *verified*, 2.1.316); nobody answers that dialog.
            orca_runner(fx, PathBuf::new())
                .replying(CLAUDE_REPLY)
                .run(&probe_pass("claude", Agent::Claude, prompt))?;
        }
    }
    let report = fs::read_to_string(&report).map_err(|_| {
        anyhow!(
            "the probe did not run in Claude's sandbox; see {}",
            run.display()
        )
    })?;
    let keys = ["write=", "commit=", "branch=", "diff="];
    let [write, commit, branch_code, diff_code] = parse_report(&report, keys).ok_or_else(|| {
        anyhow!(
            "malformed probe report {:?}; see {}",
            report.trim(),
            run.display()
        )
    })?;
    let diff = fs::read_to_string(&diff).unwrap_or_default();
    let branch = git(
        &fx.wt,
        &["rev-parse", "--verify", "-q", "refs/heads/rreview-probe"],
    );
    let tools = claude_tools(&fx.wt);
    let tool_check = |tool: &str, what: &str, landed: bool| {
        let name = format!("{a}: {tool} tool {what}");
        let outcome = match &tools {
            _ if landed => Err("went through".into()),
            Err(e) => Err(format!("no evidence that Claude could not use it: {e:#}")),
            Ok(t) if !t.offered.contains(tool) => Ok("blocked (not offered to Claude)".into()),
            Ok(t) if t.refused.contains(tool) => Ok("blocked (refused)".into()),
            Ok(_) => Err(format!(
                "offered to Claude and never refused, so the unchanged worktree proves nothing \
                 (Claude may not have tried {tool})"
            )),
        };
        Check::new(&name, outcome)
    };
    Ok(vec![
        tool_check("Write", "into the worktree", tool_leak.exists()),
        tool_check(
            "Edit",
            "in the worktree",
            fs::read_to_string(fx.wt.join("f.txt")).ok().as_deref() != Some(F_TXT),
        ),
        Check::blocked(
            &format!("{a} sandbox: write into the worktree"),
            write == 0 || leak.exists(),
        ),
        Check::blocked(
            &format!("{a} sandbox: git commit"),
            commit == 0 || git(&fx.wt, &["rev-parse", "HEAD"])? != fx.head,
        ),
        Check::blocked(
            &format!("{a} sandbox: git branch"),
            branch_code == 0 || branch.is_ok(),
        ),
        Check::new(
            &format!("{a} sandbox: git diff"),
            if diff_code == 0 && diff.contains("+rreview-probe-change") {
                Ok("works".into())
            } else {
                Err(format!("failed (probe: {})", report.trim()))
            },
        ),
        Check::new(
            &format!("{a} --safe-mode: planted project hook"),
            if fx.root.join("hook-fired").exists() {
                Err("fired".into())
            } else if !control_fired {
                Err("control failed: the hook did not fire even without --safe-mode, so this check proves nothing".into())
            } else {
                Ok("did not fire".into())
            },
        ),
    ])
}

/// What Claude Code's own session transcript in `cwd` shows: the tools it offered the model
/// (`api-request-shape` records, one per distinct request) and the tools whose call it refused.
#[derive(Debug, Default)]
struct ClaudeTools {
    offered: std::collections::HashSet<String>,
    refused: std::collections::HashSet<String>,
}

/// Reads the session transcripts Claude Code keeps for `cwd` under
/// `$CLAUDE_CONFIG_DIR` (else `~/.claude`)`/projects/<cwd, each non-alphanumeric as ->/`. Only
/// the probe pass runs there with that config dir (the hook control has its own).
// limit: an internal format of Claude Code (2.1.316); a change makes the Write and Edit checks
// fail for want of evidence, never pass. Paths over 200 characters, which Claude Code shortens,
// are not found.
fn claude_tools(cwd: &Path) -> Result<ClaudeTools> {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
        .context("neither CLAUDE_CONFIG_DIR nor HOME is set")?;
    let key: String = launch::utf8(cwd)?
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = config.join("projects").join(key);
    let mut t = ClaudeTools::default();
    let (mut calls, mut shapes) = (std::collections::HashMap::new(), 0);
    let entries = fs::read_dir(&dir)
        .with_context(|| format!("no Claude session transcript in {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        for line in fs::read_to_string(&path)?.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v["type"] == "api-request-shape" {
                shapes += 1;
                for tool in v["shape"]["tools"].as_array().into_iter().flatten() {
                    t.offered.extend(tool["name"].as_str().map(String::from));
                }
            }
            for item in v["message"]["content"].as_array().into_iter().flatten() {
                match item["type"].as_str() {
                    Some("tool_use") => {
                        if let (Some(id), Some(name)) = (item["id"].as_str(), item["name"].as_str())
                        {
                            calls.insert(id.to_string(), name.to_string());
                        }
                    }
                    Some("tool_result") if item["is_error"] == true => {
                        let name = item["tool_use_id"].as_str().and_then(|id| calls.get(id));
                        t.refused.extend(name.cloned());
                    }
                    _ => {}
                }
            }
        }
    }
    if shapes == 0 {
        bail!("no record of the tools Claude offered in {}", dir.display());
    }
    Ok(t)
}

/// Positive control for the planted hook, without a model call: the same launch minus
/// `--safe-mode`, with an empty `CLAUDE_CONFIG_DIR` (no user config, no login), fires the
/// session-start hook and then fails for want of a login. Its HOME and XDG dirs are in the
/// run dir too: without `--safe-mode` Claude writes under them (`.claude/ssh`,
/// `.config/anthropic`). Returns whether the hook fired.
fn hook_control(fx: &Fixture) -> Result<bool> {
    let run = fx.root.join("claude-hook-control");
    let mut control = launch::claude(&fx.wt, &fx.git_common_dir, &run)?;
    let home = run.join("home");
    let config = home.join(".claude");
    fs::create_dir_all(&config)?;
    control.args.retain(|a| a != "--safe-mode");
    for (var, dir) in [
        ("HOME", ""),
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_CACHE_HOME", ".cache"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
    ] {
        control
            .env
            .push((var.into(), launch::utf8(&home.join(dir))?.into()));
    }
    control.env.extend([
        ("CLAUDE_CONFIG_DIR".into(), launch::utf8(&config)?.into()),
        // limit: other credential sources (cloud provider env, apiKeyHelper) are not cleared.
        ("ANTHROPIC_API_KEY".into(), String::new()),
        ("CLAUDE_CODE_OAUTH_TOKEN".into(), String::new()),
    ]);
    control.run("Reply with OK.", &run.join("claude"), PASS_TIMEOUT)?;
    let marker = fx.root.join("hook-fired");
    let fired = marker.exists();
    if fired {
        fs::remove_file(&marker)?;
    }
    Ok(fired)
}

/// `<key><n> ...` with exactly `keys`, in order (`write=<n> commit=<n> ...`).
fn parse_report<const N: usize>(report: &str, keys: [&str; N]) -> Option<[i32; N]> {
    let mut codes = [0; N];
    let mut fields = report.trim().split(' ');
    for (code, key) in codes.iter_mut().zip(keys) {
        *code = fields.next()?.strip_prefix(key)?.parse().ok()?;
    }
    fields.next().is_none().then_some(codes)
}

fn codex_self_test(fx: &Fixture) -> Vec<Check> {
    codex_probe(fx).unwrap_or_else(|e| vec![Check::fail("codex self-test", format!("{e:#}"))])
}

/// Codex runs a probe that writes into the worktree, then names the canary word it was told,
/// if any. Its `HOME` is the stand-in of `stand_in_home`; a control run with a canary in its
/// own `CODEX_HOME` must report it, or the AGENTS.md check proves nothing.
// limit: assumes Codex finds `~` through `$HOME`; one reading the passwd home would slip past.
fn codex_probe(fx: &Fixture) -> Result<Vec<Check>> {
    let auth = launch::user_codex_auth()?;
    let leak = fx.wt.join("rreview-probe-codex.txt");
    let probe = fx.root.join("codex-probe.sh");
    fs::write(
        &probe,
        format!(
            // The result line is not in the source, so reading the script cannot fake it.
            "if echo x > {}; then r=0; else r=1; fi\necho rreview-probe-result write=$r\n",
            sh_quote(&leak)
        ),
    )?;
    let prompt = format!(
        "Run this exact command with your shell tool, once:\nsh {}\n\
         Then reply with its output verbatim on the first line; on the second line, {CANARY_ASK}.\n",
        sh_quote(&probe)
    );
    let (home, user_canary) = stand_in_home(fx)?;
    let control_canary = canary();

    let run = fx.root.join("codex");
    let mut main = launch::codex(&fx.wt, &run, "reply.txt", &auth)?;
    main.env.push(("HOME".into(), launch::utf8(&home)?.into()));
    let control_run = fx.root.join("codex-control");
    let control = launch::codex(&fx.wt, &control_run, "reply.txt", &auth)?;
    fs::write(
        control_run.join("codex-home/AGENTS.md"),
        format!("The canary word is {control_canary}.\n"),
    )?;

    let reply = run_codex(&main, &prompt, &run)?;
    let control_reply = run_codex(&control, &prompt, &control_run)?;
    let events = run.join("codex.stdout");
    let write = Check::new(
        "codex sandbox: write into the worktree",
        if leak.exists() {
            Err("went through".into())
        } else if probe_saw_blocked_write(&fs::read_to_string(&events)?, &probe) {
            Ok("blocked".into())
        } else {
            Err(format!(
                "no evidence that Codex ran the probe and its write failed (no such command in {})",
                events.display()
            ))
        },
    );
    Ok(vec![
        write,
        agents_md_check(
            "codex: user AGENTS.md",
            [&reply, &user_canary],
            [&control_reply, &control_canary],
        ),
    ])
}

/// A stand-in `HOME` whose `.codex/AGENTS.md` names a fresh canary, so the user's real
/// `~/.codex` is never touched; returns it and the canary.
fn stand_in_home(fx: &Fixture) -> Result<(PathBuf, String)> {
    let (home, word) = (fx.root.join("home"), canary());
    fs::create_dir_all(home.join(".codex"))?;
    fs::write(
        home.join(".codex/AGENTS.md"),
        format!("The canary word is {word}.\n"),
    )?;
    Ok((home, word))
}

/// The user canary must not reach the reply, and the control's must reach its own, or the
/// check proves nothing. Each argument is `[reply, canary]`.
fn agents_md_check(name: &str, user: [&str; 2], control: [&str; 2]) -> Check {
    Check::new(
        name,
        if user[0].contains(user[1]) {
            Err("loaded: the canary from ~/.codex/AGENTS.md reached Codex".into())
        } else if control[0].contains(control[1]) {
            Ok("not loaded".into())
        } else {
            Err("control failed: Codex did not report the canary in its CODEX_HOME/AGENTS.md, so the check proves nothing".into())
        },
    )
}

fn codex_orca_self_test(fx: &Fixture) -> Vec<Check> {
    codex_orca_probe(fx)
        .unwrap_or_else(|e| vec![Check::fail("orca codex self-test", format!("{e:#}"))])
}

/// The Codex TUI through the orca runner: a probe writes into the worktree, the git dir and the
/// stand-in `HOME`, and records its exit codes in `out/`, the one dir it may write. The
/// AGENTS.md canaries work as for headless, the control in an Orca terminal of its own.
// limit: Codex could write the report itself; the TUI has no event log like `exec --json` to
// check it against, so a write check rests on the disk and that report. And the model may
// decline to run the probe at all; neither its `!` shell (unsandboxed) nor `codex sandbox`
// (ignores the TUI's tmp excludes) can stand in for it (#31).
fn codex_orca_probe(fx: &Fixture) -> Result<Vec<Check>> {
    let auth = launch::user_codex_auth()?;
    orca::check_codex_run_dir(&fx.root)?;
    let (home, user_canary) = stand_in_home(fx)?;
    let main = orca_runner(fx, auth.clone()).with_env("HOME", launch::utf8(&home)?);
    let control = orca_runner(fx, auth);
    let control_canary = canary();
    let control_home = control.scratch("codex_control").join("codex-home");
    fs::create_dir_all(&control_home)?;
    fs::write(
        control_home.join("AGENTS.md"),
        format!("The canary word is {control_canary}.\n"),
    )?;

    let run = main.scratch("codex");
    let (out, probe) = (run.join("out"), run.join("probe.sh"));
    let report = out.join("probe.txt");
    fs::create_dir_all(&out)?;
    let targets = [
        (
            "worktree",
            "the worktree",
            fx.wt.join("rreview-probe-codex.txt"),
        ),
        (
            "gitdir",
            "the git dir",
            fx.git_common_dir.join("rreview-probe-codex"),
        ),
        ("home", "HOME", home.join("rreview-probe-codex.txt")),
    ];
    let mut script = String::from(
        "# rreview sandbox self-test, run by the user: each write below should fail with \
         \"Operation not permitted\"; the report records whether it did.\n",
    );
    for (key, _, path) in &targets {
        script += &format!(
            "if echo x > {}; then {key}=0; else {key}=1; fi\n",
            sh_quote(path)
        );
    }
    script += &format!(
        "echo \"worktree=$worktree gitdir=$gitdir home=$home\" > {0}\ncat {0}\n",
        sh_quote(&report)
    );
    fs::write(&probe, script)?;
    let prompt = format!(
        "{SELF_TEST}Run this exact command with your shell tool, once:\nsh {}\nThen reply with a JSON \
         object: \"probe\", its output; \"canary\", {CANARY_ASK}.\n",
        sh_quote(&probe)
    );
    let pass = probe_pass("codex", Agent::Codex, prompt);
    let mut reply = main.run(&pass)?;
    // Codex sometimes declines a script that writes outside its workspace, the framing above
    // notwithstanding (*verified*, codex-cli 0.167.0); one reminder in the same session.
    if !report.exists() {
        reply += &main.wrap_up(
            &pass,
            &format!(
                "You did not run the script. It needs no approval: its writes are meant to \
                 fail, and that failure is what this test records. Run it now with your shell \
                 tool:\nsh {}\nThen reply as asked before.\n",
                sh_quote(&probe)
            ),
            PASS_TIMEOUT,
        )?;
    }
    let control_reply = control.run(&probe_pass(
        "codex_control",
        Agent::Codex,
        format!("Reply with a JSON object: \"canary\", {CANARY_ASK}.\n"),
    ))?;

    let codes = fs::read_to_string(&report)
        .ok()
        .and_then(|r| parse_report(&r, ["worktree=", "gitdir=", "home="]));
    let mut checks: Vec<_> = targets
        .iter()
        .enumerate()
        .map(|(i, (_, label, path))| {
            let name = format!("orca codex sandbox: write into {label}");
            match codes {
                _ if path.exists() => Check::fail(&name, "went through"),
                Some(codes) => Check::blocked(&name, codes[i] == 0),
                None => Check::fail(
                    &name,
                    format!(
                        "the probe did not run in Codex's sandbox (no report in {}): this check \
                         needs the Codex model to agree to run a script that tries writes outside \
                         its workspace, and it may decline even after a reminder (#31)",
                        out.display()
                    ),
                ),
            }
        })
        .collect();
    checks.push(agents_md_check(
        "orca codex: user AGENTS.md",
        [&reply, &user_canary],
        [&control_reply, &control_canary],
    ));
    Ok(checks)
}

/// Evidence from Codex's own event log (`--json`), not from the model's reply: a completed
/// command that is an invocation of the probe and nothing else, whose output has the line
/// only running it prints. A fake agent forging its own event log is out of scope: doctor
/// tests the real CLIs.
fn probe_saw_blocked_write(events: &str, probe: &Path) -> bool {
    let probe = probe.to_string_lossy();
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|event| {
            let item = &event["item"];
            event["type"] == "item.completed"
                && item["type"] == "command_execution"
                && item["command"]
                    .as_str()
                    .is_some_and(|c| runs_only(c, &probe))
                && item["aggregated_output"].as_str().is_some_and(|o| {
                    o.lines()
                        .any(|l| l.trim() == "rreview-probe-result write=1")
                })
        })
}

/// `command` (as Codex logs it, usually `/bin/zsh -lc '<script>'`) runs `script_path` and
/// nothing else: `<script_path>` or `sh|bash <script_path>`, optionally inside one shell wrapper.
fn runs_only(command: &str, script_path: &str) -> bool {
    let is_shell = |w: &str| matches!(w.rsplit('/').next(), Some("sh" | "bash" | "zsh"));
    let invokes = |words: &[String]| match words {
        [path] => path == script_path,
        [shell, path] => is_shell(shell) && path == script_path,
        _ => false,
    };
    match sh_words(command) {
        Some(words) => match words.as_slice() {
            [shell, flag, script] if is_shell(shell) && (flag == "-c" || flag == "-lc") => {
                sh_words(script).is_some_and(|inner| invokes(&inner))
            }
            words => invokes(words),
        },
        None => false,
    }
}

/// Splits a POSIX shell command into words (quotes and backslashes). Operators such as `;`,
/// `|` or `>` stay inside words, so a command using them never matches a plain invocation.
fn sh_words(command: &str) -> Option<Vec<String>> {
    let (mut words, mut word): (Vec<String>, Option<String>) = (vec![], None);
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => words.extend(word.take()),
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let c = chars.next()?;
                            if !matches!(c, '"' | '\\' | '$' | '`') {
                                w.push('\\');
                            }
                            w.push(c);
                        }
                        c => w.push(c),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

fn run_codex(launch: &launch::Launch, prompt: &str, run: &Path) -> Result<String> {
    let status = launch.run(prompt, &run.join("codex"), PASS_TIMEOUT)?;
    if !status.success() {
        bail!("codex {status}; see {}", run.join("codex.stderr").display());
    }
    let reply = run.join("replies/reply.txt");
    fs::read_to_string(&reply)
        .with_context(|| format!("no reply from codex in {}", reply.display()))
}

fn canary() -> String {
    format!(
        "RREVIEW-CANARY-{:016x}",
        RandomState::new().build_hasher().finish()
    )
}

/// A new, empty, symlink-free directory under the system temp dir.
pub fn fresh_temp_dir(prefix: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{:x}",
        std::process::id(),
        RandomState::new().build_hasher().finish()
    ));
    fs::create_dir(&dir).with_context(|| format!("{}", dir.display()))?;
    Ok(fs::canonicalize(&dir)?)
}

fn sh_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> bool {
        std::env::var("RREVIEW_LIVE").as_deref() == Ok("1")
    }

    fn assert_all_ok(checks: &[Check]) {
        assert!(!checks.is_empty());
        assert!(checks.iter().all(|c| c.outcome.is_ok()), "{checks:#?}");
    }

    #[test]
    #[ignore = "live: real claude login; RREVIEW_LIVE=1 cargo test -- --ignored"]
    fn live_claude_self_test() {
        if !live() {
            return;
        }
        let fx = Fixture::new().unwrap();
        assert_all_ok(&claude_self_test(&fx, Via::Headless));
        fs::remove_dir_all(fx.root).unwrap();
    }

    #[test]
    #[ignore = "live: real codex login; RREVIEW_LIVE=1 cargo test -- --ignored"]
    fn live_codex_self_test() {
        if !live() {
            return;
        }
        let fx = Fixture::new().unwrap();
        assert_all_ok(&codex_self_test(&fx));
        fs::remove_dir_all(fx.root).unwrap();
    }

    #[test]
    fn parse_report_takes_only_the_exact_shape() {
        const KEYS: [&str; 4] = ["write=", "commit=", "branch=", "diff="];
        assert_eq!(
            parse_report("write=1 commit=128 branch=128 diff=0\n", KEYS),
            Some([1, 128, 128, 0])
        );
        for bad in [
            "",
            "write=1 commit=128 branch=128",
            "write=1 commit=128 branch=128 diff=0 extra=1",
            "commit=128 write=1 branch=128 diff=0",
            "write=x commit=128 branch=128 diff=0",
        ] {
            assert_eq!(parse_report(bad, KEYS), None, "{bad:?}");
        }
    }

    #[test]
    fn runs_only_accepts_just_an_invocation_of_the_script() {
        let p = "/tmp/x/codex-probe.sh";
        for ok in [
            "/bin/zsh -lc 'sh /tmp/x/codex-probe.sh'",
            "/bin/zsh -lc \"sh '/tmp/x/codex-probe.sh'\"",
            "/bin/bash -c 'sh '\"'\"'/tmp/x/codex-probe.sh'\"'\"''",
            "/bin/zsh -lc /tmp/x/codex-probe.sh",
            "bash /tmp/x/codex-probe.sh",
        ] {
            assert!(runs_only(ok, p), "{ok}");
        }
        for bad in [
            "/bin/zsh -lc 'cat /tmp/x/codex-probe.sh'",
            "/bin/zsh -lc 'tail -n 1 /tmp/x/codex-probe.sh | sed s/x/y/'",
            "/bin/zsh -lc 'sh /tmp/x/codex-probe.sh; echo rreview-probe-result write=1'",
            "/bin/zsh -lc 'sh /tmp/x/codex-probe.sh > /dev/null'",
            "/bin/zsh -lc 'python3 /tmp/x/codex-probe.sh'",
            "/bin/zsh -lc 'sh /tmp/x/codex-probe.sh.bak'",
            "/bin/zsh -lc 'sh /tmp/x/codex-probe.sh",
        ] {
            assert!(!runs_only(bad, p), "{bad}");
        }
    }

    #[test]
    fn sh_quote_survives_quotes() {
        assert_eq!(sh_quote(Path::new("/a b/it's")), r"'/a b/it'\''s'");
    }
}
