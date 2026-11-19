//! Builders for isolated reviewer launches (spec §3). The review runner and `rreview doctor`
//! both launch reviewers through these, so the doctor's self-test checks the real flags.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::json;

/// What every reviewer inherits from the environment it starts in (spec §3); every other
/// variable, such as a `GH_TOKEN` meant for rreview's own `gh`, is dropped.
const INHERIT: &[&str] = &[
    // Run at all, find the user's login (the macOS keychain), give the sandboxed Bash a shell.
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    // Locale and the terminal the TUI draws in (the Orca runner).
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_COLLATE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "TERM",
    "COLORTERM",
    // Reach the API from a network behind a proxy or a private CA. A proxy URL may carry the
    // proxy's user and password; when set, they reach the CLI and so the reviewer.
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
];

/// Claude's own subscription login: its config dir (an Orca account home, say) or a
/// `claude setup-token` OAuth token, which then reaches the reviewer. Not `ANTHROPIC_API_KEY`:
/// it would move the review to API billing (spec §1: subscriptions, not API keys). Codex needs
/// none: its login is the `auth.json` in its run-scoped home.
const CLAUDE_INHERIT: &[&str] = &["CLAUDE_CONFIG_DIR", "CLAUDE_CODE_OAUTH_TOKEN"];

/// One reviewer process: program, argument vector, the variables it inherits (see [`INHERIT`]),
/// extra env and working directory.
#[derive(Clone, Debug)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub inherit: Vec<&'static str>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

impl Launch {
    /// Whether the reviewer inherits `name` from the environment it starts in.
    pub fn inherits(&self, name: &str) -> bool {
        self.inherit.contains(&name)
    }

    /// Runs with `prompt` on stdin. Stdout and stderr go to `<log>.stdout` and `<log>.stderr`.
    /// The child leads its own process group, so a timeout or an interrupt (see
    /// [`catch_interrupts`]) kills everything it started.
    pub fn run(&self, prompt: &str, log: &Path, timeout: Duration) -> Result<ExitStatus> {
        interrupted()?;
        let stdout = log.with_extension("stdout");
        let stderr = log.with_extension("stderr");
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(|k| self.inherits(k))))
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(File::create(&stdout).with_context(|| format!("{}", stdout.display()))?)
            .stderr(File::create(&stderr).with_context(|| format!("{}", stderr.display()))?)
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot start {}", self.program))?;
        // A writer thread, so a child that never reads stdin cannot block the timeout below.
        if let Some(mut stdin) = child.stdin.take() {
            let prompt = prompt.to_owned();
            std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        }
        let started = Instant::now();
        loop {
            if exited(&child)? {
                // What the reviewer left running in its group goes with it; the unreaped leader
                // keeps the group id from being reused before the signal.
                kill_group(&mut child);
                return Ok(child.wait()?);
            }
            if let Err(e) = interrupted() {
                kill_group(&mut child);
                return Err(e);
            }
            if started.elapsed() >= timeout {
                kill_group(&mut child);
                return Err(TimedOut(format!(
                    "{} timed out after {}s",
                    self.program,
                    timeout.as_secs()
                ))
                .into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Runs `cmd` to its end with no stdin, its output captured. When the caller put it in its own
/// process group, that group is killed at `deadline` ([`TimedOut`]; past it, `cmd` does not
/// start) and, if `interruptible`, on an interrupt ([`Interrupted`]; after one, it does not
/// start). A cleanup step is not interruptible: it runs to its end whatever the flag says.
pub fn output(cmd: &mut Command, deadline: Option<Instant>, interruptible: bool) -> Result<Output> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let timed_out = || TimedOut(format!("{program} did not finish in time"));
    let past = || deadline.is_some_and(|d| Instant::now() >= d);
    if interruptible {
        interrupted()?;
    }
    if past() {
        return Err(timed_out().into());
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    // Reader threads, so a full pipe cannot stall the child while we wait on it.
    fn read(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = vec![];
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let (stdout, stderr) = (read(child.stdout.take()), read(child.stderr.take()));
    // From 1 ms up: a quick call (most Orca calls) returns at once, a long wait polls at 20 ms.
    let mut nap = Duration::from_millis(1);
    loop {
        // A descendant can hold the pipes after the leader exits, so the drain is under the
        // deadline too: done only when the leader has exited and both pipes reached EOF. The
        // leader stays unreaped until then, so its group id is not reused before a kill.
        if exited(&child)? && stdout.is_finished() && stderr.is_finished() {
            return Ok(Output {
                status: child.wait()?,
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            });
        }
        if interruptible && let Err(e) = interrupted() {
            kill_group(&mut child);
            return Err(e);
        }
        if past() {
            kill_group(&mut child);
            return Err(timed_out().into());
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(20));
    }
}

/// `rreview exec-reviewer` (the orca runner's shim, spec §3): replaces this process with
/// `command`, given only the `inherit` variables of this environment that are set plus `set`.
/// Values stay in the environment across the exec, out of every argv. Returns only on failure.
pub fn exec_reviewer(inherit: &[String], set: &[String], command: &[String]) -> anyhow::Error {
    let Some((program, args)) = command.split_first() else {
        return anyhow::anyhow!("exec-reviewer: no program");
    };
    let mut cmd = Command::new(program);
    cmd.args(args).env_clear().envs(
        std::env::vars_os()
            .filter(|(k, _)| k.to_str().is_some_and(|k| inherit.iter().any(|i| i == k))),
    );
    for kv in set {
        match kv.split_once('=') {
            Some((k, v)) => cmd.env(k, v),
            None => return anyhow::anyhow!("exec-reviewer: --set {kv:?} is not NAME=VALUE"),
        };
    }
    anyhow::Error::new(cmd.exec()).context(format!("cannot start {program}"))
}

/// Kills `child`, which leads its own process group, and everything it started; reaps it.
/// The signal goes straight from rreview: a `kill` helper would run in rreview's foreground
/// group, where a second Ctrl-C could kill it before it signals.
pub fn kill_group(child: &mut Child) {
    // SAFETY: a plain syscall on our own child's group id; the unreaped child keeps it reserved.
    unsafe { killpg(child.id() as i32, SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether `child` has exited, without reaping it, so its pid and group id stay reserved.
fn exited(child: &Child) -> std::io::Result<bool> {
    // Larger than siginfo_t on Linux (128 bytes) and macOS (104); zeroed, and si_signo (the
    // first field on both) is set only when the child has exited.
    let mut info = [0u64; 32];
    // SAFETY: `info` outlives the call and is larger than the siginfo_t it receives.
    let r = unsafe {
        waitid(
            P_PID,
            child.id(),
            info.as_mut_ptr(),
            WEXITED | WNOHANG | WNOWAIT,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info[0] != 0)
}

/// A launch killed at its timeout; callers tell it from other failures to give a wrap-up turn.
#[derive(Debug)]
pub struct TimedOut(pub String);

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TimedOut {}

/// The signal that interrupted rreview, 0 for none.
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// Whether [`catch_interrupts`] ran.
static CATCHING: AtomicBool = AtomicBool::new(false);
const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const P_PID: u32 = 1;
const WNOHANG: i32 = 1;
const WEXITED: i32 = 4;
#[cfg(target_os = "macos")]
const WNOWAIT: i32 = 0x20;
#[cfg(not(target_os = "macos"))]
const WNOWAIT: i32 = 0x0100_0000; // Linux

// libc's (the same values on macOS and Linux unless marked); std exposes no signal or waitid
// API, and a few declarations do not earn a crate.
unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
    fn killpg(pgrp: i32, sig: i32) -> i32;
    fn waitid(idtype: u32, id: u32, infop: *mut u64, options: i32) -> i32;
}

/// Only stores the first signal, so a second one cannot cut the cleanup short (SIGKILL still
/// can) nor change what the run reports.
extern "C" fn on_signal(signum: i32) {
    let _ = SIGNAL.compare_exchange(0, signum, Ordering::SeqCst, Ordering::SeqCst);
}

/// SIGINT and SIGTERM only set a flag: the waits that poll it ([`Launch::run`], [`output`]) kill
/// their process group and return [`Interrupted`], which unwinds through the run's cleanup
/// guards (spec §4). Every reviewer and every cleanup step leads its own process group, so the
/// terminal's Ctrl-C reaches rreview alone.
pub fn catch_interrupts() {
    CATCHING.store(true, Ordering::SeqCst);
    for s in [SIGINT, SIGTERM] {
        // SAFETY: the handler only stores an atomic, which is async-signal-safe.
        unsafe { signal(s, on_signal as extern "C" fn(i32) as usize) };
    }
}

/// Whether rreview catches SIGINT and SIGTERM. Without the handler (`rreview show`) a
/// subprocess stays in rreview's group, so the terminal's Ctrl-C still ends it.
pub fn catching() -> bool {
    CATCHING.load(Ordering::SeqCst)
}

/// `Err(Interrupted)` once rreview has caught SIGINT or SIGTERM.
pub fn interrupted() -> Result<()> {
    match SIGNAL.load(Ordering::SeqCst) {
        0 => Ok(()),
        s => Err(Interrupted(s).into()),
    }
}

/// rreview was told to stop; the run fails (exit 2) after its cleanup.
#[derive(Debug)]
pub struct Interrupted(i32);

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let name = if self.0 == SIGINT {
            "SIGINT"
        } else {
            "SIGTERM"
        };
        write!(f, "interrupted by {name}")
    }
}

impl std::error::Error for Interrupted {}

/// `claude -p --safe-mode --settings <runroot>/claude-settings.json --output-format json`.
/// Writes the settings file: no edit/write/MCP tools, Bash sandboxed, writes denied in the
/// worktree and the git common dir, allowed only in `<runroot>/out`.
pub fn claude(worktree: &Path, git_common_dir: &Path, runroot: &Path) -> Result<Launch> {
    let (settings, cwd) = claude_settings(worktree, git_common_dir, runroot)?;
    Ok(Launch {
        program: "claude".into(),
        args: [
            "-p",
            "--safe-mode",
            "--settings",
            &settings,
            "--output-format",
            "json",
        ]
        .map(String::from)
        .into(),
        inherit: [INHERIT, CLAUDE_INHERIT].concat(),
        env: vec![zdotdir(runroot)?],
        cwd,
    })
}

/// The same isolation as [`claude`] as an interactive session (the orca runner). The reply file
/// is written by a sandboxed Bash redirect into `<runroot>/out`; Claude asks before such a write
/// outside its working dirs, so `out` is added as a working dir and `acceptEdits` takes it
/// without a dialog nobody would answer. Not `dontAsk`: on a live pass it refused the
/// reviewer's heredoc reply twice (*verified*, Claude Code 2.1.316). The deny rules still remove
/// Edit and Write, and the sandbox still refuses every other write.
pub fn claude_tui(worktree: &Path, git_common_dir: &Path, runroot: &Path) -> Result<Launch> {
    let (settings, cwd) = claude_settings(worktree, git_common_dir, runroot)?;
    let out = absolute(&runroot.join("out"))?;
    Ok(Launch {
        program: "claude".into(),
        args: [
            "--safe-mode",
            "--settings",
            &settings,
            "--permission-mode",
            "acceptEdits",
            "--add-dir",
            utf8(&out)?,
        ]
        .map(String::from)
        .into(),
        inherit: [INHERIT, CLAUDE_INHERIT].concat(),
        env: vec![zdotdir(runroot)?],
        cwd,
    })
}

/// Writes `<runroot>/claude-settings.json`; returns its path and the resolved worktree.
fn claude_settings(
    worktree: &Path,
    git_common_dir: &Path,
    runroot: &Path,
) -> Result<(String, PathBuf)> {
    let worktree = absolute(worktree)?;
    let git_common_dir = absolute(git_common_dir)?;
    fs::create_dir_all(runroot.join("out")).with_context(|| format!("{}", runroot.display()))?;
    let runroot = absolute(runroot)?;
    let out = runroot.join("out");
    let snapshots = user_claude_config_dir()?.join("shell-snapshots");
    let settings_path = runroot.join("claude-settings.json");
    let settings = json!({
        "permissions": { "deny": [
            "Edit", "Write", "NotebookEdit", "mcp__*",
            "Bash(gh *)", "Bash(git push*)", "Bash(curl *)", "Bash(wget *)",
            // Network tools; under the orca runner a WebSearch also stopped the pass at a
            // permission dialog until its timeout (*verified*).
            "WebSearch", "WebFetch"
        ] },
        "sandbox": {
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "filesystem": {
                "denyWrite": [utf8(&worktree)?, utf8(&git_common_dir)?],
                "allowWrite": [utf8(&out)?],
                // Claude Code builds a snapshot of the shell setup from the real home's
                // `.zshrc` or `.bashrc` (functions, aliases, options, PATH) and sources it before
                // each Bash command; unreadable, it is skipped (spec §3).
                "denyRead": [utf8(&snapshots)?]
            }
        },
        "disableBundledSkills": true
    });
    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)
        .with_context(|| format!("{}", settings_path.display()))?;
    Ok((utf8(&settings_path)?.into(), worktree))
}

/// `codex exec --json -s read-only -C <worktree> -o <runroot>/replies/<out_name> -` with
/// `CODEX_HOME=<runroot>/codex-home`, a fresh dir holding only a symlink to `user_auth`, so
/// Codex loads none of the user's config or AGENTS.md. The prompt goes on stdin; stdout is
/// Codex's JSONL event log (the commands it ran and their output), the last message goes to `-o`.
/// Not `--ephemeral`: the wrap-up resumes the session, which then lives in that run-scoped home.
/// The reply goes outside `out/`: a Claude reviewer may write there and could plant a symlink
/// that Codex, writing unsandboxed, would follow.
pub fn codex(worktree: &Path, runroot: &Path, out_name: &str, user_auth: &Path) -> Result<Launch> {
    let worktree = absolute(worktree)?;
    fs::create_dir_all(runroot.join("replies"))
        .with_context(|| format!("{}", runroot.display()))?;
    let (runroot, env) = codex_home(runroot, user_auth)?;
    let reply = runroot.join("replies").join(out_name);
    let args = ["exec", "--json", "-s", "read-only", "-C", utf8(&worktree)?];
    let args = [&args[..], &["-o", utf8(&reply)?], &CODEX_NO_STARTUP, &["-"]].concat();
    Ok(Launch {
        program: "codex".into(),
        args: args.into_iter().map(String::from).collect(),
        inherit: INHERIT.into(),
        env,
        cwd: worktree,
    })
}

/// The Codex TUI for the orca runner, in the same clean home. `-s read-only` would block the
/// reply file too, so it runs `workspace-write` rooted at `<runroot>/out` with `/tmp` and
/// `$TMPDIR` excluded: `out/` is its only writable dir; it reads the worktree by absolute path.
/// `-a never` returns a refused command to the model instead of asking the user.
/// Orca's `codex` shell function prepends `-c shell_environment_policy.set.ORCA_*=...` (its
/// terminal and workspace ids, its data dir); Codex applies `include_only` after `set`, so the
/// reviewer's commands get only the allowlist and the run's `ZDOTDIR` ([`zdotdir`]), whichever
/// `-c` comes first (spec §3).
pub fn codex_tui(worktree: &Path, runroot: &Path, user_auth: &Path) -> Result<Launch> {
    let worktree = absolute(worktree)?;
    let names: Vec<_> = INHERIT
        .iter()
        .chain(&["ZDOTDIR"])
        .map(|n| format!("\"{n}\""))
        .collect();
    let include_only = format!(
        "shell_environment_policy.include_only=[{}]",
        names.join(",")
    );
    fs::create_dir_all(runroot.join("out")).with_context(|| format!("{}", runroot.display()))?;
    let (runroot, env) = codex_home(runroot, user_auth)?;
    Ok(Launch {
        program: "codex".into(),
        args: [
            "-C",
            utf8(&runroot.join("out"))?,
            "-s",
            "workspace-write",
            "-a",
            "never",
            "-c",
            "sandbox_workspace_write.exclude_slash_tmp=true",
            "-c",
            "sandbox_workspace_write.exclude_tmpdir_env_var=true",
            "-c",
            "check_for_update_on_startup=false",
            "-c",
            &include_only,
        ]
        .into_iter()
        .chain(CODEX_NO_STARTUP)
        .map(String::from)
        .collect(),
        inherit: INHERIT.into(),
        env,
        cwd: worktree,
    })
}

/// `ZDOTDIR=<runroot>/zdotdir`, an empty dir no pass can write: zsh reads its startup files
/// (`.zshenv`, and `.zprofile` and `.zshrc` for Codex's login shell and shell snapshot) from
/// there instead of `$HOME`, so nothing the user's own ones export reaches a reviewer's commands
/// (spec §3, *verified* for Claude's Bash and Codex's shell tool).
fn zdotdir(runroot: &Path) -> Result<(String, String)> {
    let dir = runroot.join("zdotdir");
    fs::create_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
    Ok(("ZDOTDIR".into(), utf8(&absolute(&dir)?)?.into()))
}

/// Codex runs commands through the account's login shell (from the passwd entry, not
/// `$SHELL`) and its own shell snapshot of `.zshrc` or `.bashrc`; without both, `bash -c`
/// reads no startup file and `zsh -c` only `$ZDOTDIR/.zshenv` (spec §3).
const CODEX_NO_STARTUP: [&str; 4] = [
    "-c",
    "allow_login_shell=false",
    "-c",
    "features.shell_snapshot=false",
];

/// Creates `<runroot>/codex-home` with only the auth symlink; returns the resolved run root and
/// the `CODEX_HOME` and [`zdotdir`] env.
fn codex_home(runroot: &Path, user_auth: &Path) -> Result<(PathBuf, Vec<(String, String)>)> {
    let user_auth = absolute(user_auth).context("Codex login (auth.json) not found")?;
    fs::create_dir_all(runroot).with_context(|| format!("{}", runroot.display()))?;
    let runroot = absolute(runroot)?;
    let home = runroot.join("codex-home");
    fs::create_dir_all(&home).with_context(|| format!("{}", home.display()))?;
    let link = home.join("auth.json");
    // A wrap-up or retry turn reuses the pass's home; the link may already be there.
    match fs::read_link(&link) {
        Ok(target) if target == user_auth => {}
        Ok(target) => bail!(
            "{} points at {}, not {}",
            link.display(),
            target.display(),
            user_auth.display()
        ),
        Err(e) if e.kind() == ErrorKind::NotFound => std::os::unix::fs::symlink(&user_auth, &link)
            .with_context(|| format!("{}", link.display()))?,
        Err(e) => return Err(e).with_context(|| format!("{} is not a symlink", link.display())),
    }
    let env = vec![
        ("CODEX_HOME".into(), utf8(&home)?.into()),
        zdotdir(&runroot)?,
    ];
    Ok((runroot, env))
}

/// The user's Claude Code config dir, as Claude finds it: `$CLAUDE_CONFIG_DIR`, else
/// `~/.claude`; absolute, symlink-free when it exists.
pub fn user_claude_config_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".claude"),
    };
    Ok(fs::canonicalize(&dir).or_else(|_| std::path::absolute(&dir))?)
}

/// The user's Codex login: `$CODEX_HOME/auth.json`, else `~/.codex/auth.json`.
pub fn user_codex_auth() -> Result<PathBuf> {
    let home = match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".codex"),
    };
    Ok(home.join("auth.json"))
}

/// Absolute and symlink-free: the sandbox matches the real path (macOS `/var` is `/private/var`),
/// and a relative path in a settings file does not anchor at the worktree (spec §3).
fn absolute(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("{}", path.display()))
}

/// A path that is not UTF-8 would be written lossily and miss the sandbox rule; refuse it.
pub fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::fresh_temp_dir;
    use serde_json::Value;

    #[test]
    fn claude_launch_is_isolated_with_absolute_paths() {
        let root = fresh_temp_dir("rreview-test").unwrap();
        let (wt, git, run) = (root.join("wt"), root.join("git"), root.join("run"));
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(&git).unwrap();

        let launch = claude(&wt, &git, &run).unwrap();

        let settings_path = root.join("run/claude-settings.json");
        assert_eq!(launch.program, "claude");
        assert_eq!(
            launch.args,
            [
                "-p",
                "--safe-mode",
                "--settings",
                settings_path.to_str().unwrap(),
                "--output-format",
                "json"
            ]
        );
        assert_eq!(
            launch.env,
            [(
                "ZDOTDIR".to_string(),
                root.join("run/zdotdir").to_str().unwrap().to_string()
            )]
        );
        assert_eq!(fs::read_dir(root.join("run/zdotdir")).unwrap().count(), 0);
        assert_eq!(launch.cwd, wt);
        let s: Value = serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(
            s["permissions"]["deny"],
            json!([
                "Edit",
                "Write",
                "NotebookEdit",
                "mcp__*",
                "Bash(gh *)",
                "Bash(git push*)",
                "Bash(curl *)",
                "Bash(wget *)",
                "WebSearch",
                "WebFetch"
            ])
        );
        let sandbox = &s["sandbox"];
        assert_eq!(sandbox["enabled"], true);
        assert_eq!(sandbox["autoAllowBashIfSandboxed"], true);
        assert_eq!(sandbox["allowUnsandboxedCommands"], false);
        assert_eq!(sandbox["filesystem"]["denyWrite"], json!([wt, git]));
        assert_eq!(
            sandbox["filesystem"]["allowWrite"],
            json!([root.join("run/out")])
        );
        // Claude's shell snapshot, built from the user's `.zshrc`, never reaches a command.
        assert_eq!(
            sandbox["filesystem"]["denyRead"],
            json!([user_claude_config_dir().unwrap().join("shell-snapshots")])
        );
        assert_eq!(s["disableBundledSkills"], true);
        assert!(root.join("run/out").is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_launch_resolves_relative_paths() {
        let root = fresh_temp_dir("rreview-test").unwrap();
        let wt = root.join("wt");
        fs::create_dir_all(wt.join("sub")).unwrap();
        // `wt/sub/..` is wt; the settings must name the resolved, absolute path.
        let launch = claude(&wt.join("sub/.."), &wt, &root.join("run")).unwrap();
        let s: Value = serde_json::from_str(
            &fs::read_to_string(root.join("run/claude-settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(s["sandbox"]["filesystem"]["denyWrite"][0], json!(wt));
        assert_eq!(launch.cwd, wt);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_launch_uses_a_fresh_home_with_only_an_auth_symlink() {
        let root = fresh_temp_dir("rreview-test").unwrap();
        let (wt, run, auth) = (
            root.join("wt"),
            root.join("run"),
            root.join("user-codex/auth.json"),
        );
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(auth.parent().unwrap()).unwrap();
        fs::write(&auth, "{}").unwrap();

        let launch = codex(&wt, &run, "security.json", &auth).unwrap();
        // A second pass of the same run reuses the home.
        codex(&wt, &run, "intent.json", &auth).unwrap();

        let home = root.join("run/codex-home");
        assert_eq!(launch.program, "codex");
        assert_eq!(
            launch.args,
            [
                "exec",
                "--json",
                "-s",
                "read-only",
                "-C",
                wt.to_str().unwrap(),
                "-o",
                root.join("run/replies/security.json").to_str().unwrap(),
                "-c",
                "allow_login_shell=false",
                "-c",
                "features.shell_snapshot=false",
                "-"
            ]
        );
        assert_eq!(
            launch.env,
            [
                ("CODEX_HOME".to_string(), home.to_str().unwrap().to_string()),
                (
                    "ZDOTDIR".to_string(),
                    root.join("run/zdotdir").to_str().unwrap().to_string()
                )
            ]
        );
        let entries: Vec<_> = fs::read_dir(&home)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["auth.json"]);
        assert!(
            fs::symlink_metadata(home.join("auth.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(home.join("auth.json")).unwrap(), auth);
        fs::remove_dir_all(root).unwrap();
    }

    /// A `.zshenv` in `HOME` exports a canary; zsh run with a reviewer's env must not see it,
    /// while the same env without `ZDOTDIR` does, or the check proves nothing.
    #[test]
    fn a_reviewer_shell_skips_the_user_zsh_startup_files() {
        let Some(zsh) = ["/bin/zsh", "/usr/bin/zsh"]
            .into_iter()
            .find(|p| Path::new(p).exists())
        else {
            eprintln!("skipped: no zsh");
            return;
        };
        let root = fresh_temp_dir("rreview-test").unwrap();
        let (wt, home, auth) = (root.join("wt"), root.join("home"), root.join("auth.json"));
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(&auth, "{}").unwrap();
        for f in [".zshenv", ".zprofile", ".zshrc"] {
            fs::write(home.join(f), format!("export RREVIEW_STARTUP={f}\n")).unwrap();
        }
        let launches = [
            claude(&wt, &wt, &root.join("c")).unwrap(),
            claude_tui(&wt, &wt, &root.join("ct")).unwrap(),
            codex(&wt, &root.join("x"), "r.json", &auth).unwrap(),
            codex_tui(&wt, &root.join("xt"), &auth).unwrap(),
        ];
        for (i, mut launch) in launches.into_iter().enumerate() {
            launch.program = zsh.into();
            launch.args = ["-lic", "echo \"<$RREVIEW_STARTUP>\""]
                .map(String::from)
                .into();
            launch
                .env
                .push(("HOME".into(), home.to_str().unwrap().into()));
            let seen = |launch: &Launch, log: &str| {
                let log = root.join(log);
                launch.run("", &log, Duration::from_secs(20)).unwrap();
                fs::read_to_string(log.with_extension("stdout")).unwrap()
            };
            assert_eq!(seen(&launch, &format!("l{i}")), "<>\n", "{launch:?}");
            launch.env.retain(|(k, _)| k != "ZDOTDIR");
            assert_eq!(seen(&launch, &format!("c{i}")), "<.zshrc>\n", "control");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timeout_kills_the_whole_process_tree() {
        let root = fresh_temp_dir("rreview-test").unwrap();
        let pid_file = root.join("grandchild.pid");
        let launch = Launch {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                format!("sleep 60 & echo $! > '{}'; sleep 60", pid_file.display()),
            ],
            inherit: vec![],
            env: vec![],
            cwd: root.clone(),
        };
        let err = launch
            .run("", &root.join("log"), Duration::from_secs(1))
            .unwrap_err();
        assert!(err.is::<TimedOut>(), "{err:#}");
        assert_eq!(err.to_string(), "sh timed out after 1s");
        let pid = fs::read_to_string(&pid_file).unwrap().trim().to_string();
        let alive = || {
            Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "grandchild {pid} outlived the timeout");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exited_leaves_the_child_unreaped() {
        let mut child = Command::new("true").spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !exited(&child).unwrap() {
            assert!(Instant::now() < deadline, "true did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Still ours to reap: its pid, and so its group id, was reserved until now.
        assert!(child.try_wait().unwrap().unwrap().success());
    }

    #[test]
    fn codex_launch_needs_the_user_login() {
        let root = fresh_temp_dir("rreview-test").unwrap();
        let err = codex(
            &root,
            &root.join("run"),
            "x.json",
            &root.join("missing/auth.json"),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("Codex login (auth.json) not found"),
            "{err:#}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
