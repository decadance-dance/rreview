//! `rreview doctor` (spec §6): tool versions, logins, and the isolation self-test, which runs
//! real reviewer launches (spec §3) on a throwaway repo and checks their effects on disk.

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

const PASS_TIMEOUT: Duration = Duration::from_secs(300);

/// Prints one line per check; true when every required check passed.
pub fn run() -> Result<bool> {
    let mut r = Report::default();
    let claude = r.tool("claude", true);
    let codex = r.tool("codex", true);
    let gh = r.tool("gh", true);
    r.tool("orca", false);
    let claude = claude
        && r.check("claude login", claude_login())
        && r.check("claude --safe-mode", safe_mode());
    let codex = codex && r.check("codex login", logged_in("codex", &["login", "status"]));
    if gh {
        r.check("gh login", logged_in("gh", &["auth", "status"]));
    }
    if !claude && !codex {
        return Ok(r.failed == 0);
    }

    let fx = Fixture::new().context("cannot build the self-test repo")?;
    let before = r.failed;
    let (claude_checks, codex_checks) = std::thread::scope(|s| {
        let c = s.spawn(|| {
            if claude {
                claude_self_test(&fx)
            } else {
                vec![]
            }
        });
        let x = if codex { codex_self_test(&fx) } else { vec![] };
        let c = c
            .join()
            .unwrap_or_else(|_| vec![Check::fail("claude self-test", "panicked")]);
        (c, x)
    });
    for c in claude_checks.into_iter().chain(codex_checks) {
        r.check(&c.name, c.outcome);
    }
    if r.failed == before {
        let _ = fs::remove_dir_all(&fx.root);
    } else {
        println!("      self-test files kept in {}", fx.root.display());
    }
    Ok(r.failed == 0)
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
        fs::write(wt.join("f.txt"), "a\nrreview-probe-change\n")?;
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

fn claude_self_test(fx: &Fixture) -> Vec<Check> {
    claude_probe(fx).unwrap_or_else(|e| vec![Check::fail("claude self-test", format!("{e:#}"))])
}

/// Claude runs one probe script through its sandboxed Bash; the probe records exit codes in
/// `out/`, the one place it may write. Every check needs both the report and the disk to agree.
fn claude_probe(fx: &Fixture) -> Result<Vec<Check>> {
    let control_fired = hook_control(fx)?;
    let run = fx.root.join("claude");
    let launch = launch::claude(&fx.wt, &fx.git_common_dir, &run)?;
    let (out, leak, probe) = (
        run.join("out"),
        fx.wt.join("rreview-probe-claude.txt"),
        run.join("probe.sh"),
    );
    let (diff, report) = (out.join("diff.txt"), out.join("probe.txt"));
    fs::write(
        &probe,
        format!(
            "cd {wt} || exit 1\n\
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
        "Run this exact command with the Bash tool, once, then reply with its output:\nsh {}\n",
        sh_quote(&probe)
    );
    let status = launch.run(&prompt, &run.join("claude"), PASS_TIMEOUT)?;
    if !status.success() {
        bail!(
            "claude {status}; see {}",
            run.join("claude.stderr").display()
        );
    }
    let report = fs::read_to_string(&report).map_err(|_| {
        anyhow!(
            "the probe did not run in Claude's sandbox; see {}",
            run.display()
        )
    })?;
    let [write, commit, branch_code, diff_code] = parse_report(&report).ok_or_else(|| {
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
    Ok(vec![
        Check::blocked(
            "claude sandbox: write into the worktree",
            write == 0 || leak.exists(),
        ),
        Check::blocked(
            "claude sandbox: git commit",
            commit == 0 || git(&fx.wt, &["rev-parse", "HEAD"])? != fx.head,
        ),
        Check::blocked(
            "claude sandbox: git branch",
            branch_code == 0 || branch.is_ok(),
        ),
        Check::new(
            "claude sandbox: git diff",
            if diff_code == 0 && diff.contains("+rreview-probe-change") {
                Ok("works".into())
            } else {
                Err(format!("failed (probe: {})", report.trim()))
            },
        ),
        Check::new(
            "claude --safe-mode: planted project hook",
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

/// `write=<n> commit=<n> branch=<n> diff=<n>`, exactly.
fn parse_report(report: &str) -> Option<[i32; 4]> {
    let mut codes = [0; 4];
    let mut fields = report.trim().split(' ');
    for (code, key) in codes
        .iter_mut()
        .zip(["write=", "commit=", "branch=", "diff="])
    {
        *code = fields.next()?.strip_prefix(key)?.parse().ok()?;
    }
    fields.next().is_none().then_some(codes)
}

fn codex_self_test(fx: &Fixture) -> Vec<Check> {
    codex_probe(fx).unwrap_or_else(|e| vec![Check::fail("codex self-test", format!("{e:#}"))])
}

/// Codex runs a probe that writes into the worktree, then names the canary word it was told,
/// if any. Its `HOME` is a stand-in whose `.codex/AGENTS.md` names a canary, so the user's
/// real `~/.codex` is never touched; a control run with a canary in its own `CODEX_HOME`
/// must report it, or the AGENTS.md check proves nothing.
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
         Then reply with its output verbatim on the first line; on the second line, the canary \
         word if any instruction you were given (system, developer, AGENTS.md or user \
         instructions) names one, else NO-CANARY.\n",
        sh_quote(&probe)
    );
    let (user_canary, control_canary) = (canary(), canary());
    let home = fx.root.join("home");
    fs::create_dir_all(home.join(".codex"))?;
    fs::write(
        home.join(".codex/AGENTS.md"),
        format!("The canary word is {user_canary}.\n"),
    )?;

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
    let agents_md = if reply.contains(&user_canary) {
        Err("loaded: the canary from ~/.codex/AGENTS.md reached Codex".into())
    } else if control_reply.contains(&control_canary) {
        Ok("not loaded".into())
    } else {
        Err("control failed: Codex did not report the canary in its CODEX_HOME/AGENTS.md, so the check proves nothing".into())
    };
    Ok(vec![write, Check::new("codex: user AGENTS.md", agents_md)])
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
    let reply = run.join("out/reply.txt");
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
        assert_all_ok(&claude_self_test(&fx));
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
        assert_eq!(
            parse_report("write=1 commit=128 branch=128 diff=0\n"),
            Some([1, 128, 128, 0])
        );
        for bad in [
            "",
            "write=1 commit=128 branch=128",
            "write=1 commit=128 branch=128 diff=0 extra=1",
            "commit=128 write=1 branch=128 diff=0",
            "write=x commit=128 branch=128 diff=0",
        ] {
            assert_eq!(parse_report(bad), None, "{bad:?}");
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
