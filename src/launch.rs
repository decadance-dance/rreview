//! Builders for isolated reviewer launches (spec §3). The review runner and `rreview doctor`
//! both launch reviewers through these, so the doctor's self-test checks the real flags.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::json;

/// One reviewer process: program, argument vector, extra env and working directory.
#[derive(Debug)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

impl Launch {
    /// Runs with `prompt` on stdin. Stdout and stderr go to `<log>.stdout` and `<log>.stderr`.
    /// The child leads its own process group, so a timeout kills everything it started.
    // limit: in its own group the child misses the terminal's Ctrl-C; an interrupted rreview
    // leaves a running reviewer behind.
    pub fn run(&self, prompt: &str, log: &Path, timeout: Duration) -> Result<ExitStatus> {
        let stdout = log.with_extension("stdout");
        let stderr = log.with_extension("stderr");
        let mut child = Command::new(&self.program)
            .args(&self.args)
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
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if started.elapsed() >= timeout {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                let _ = child.wait();
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

/// A launch killed at its timeout; callers tell it from other failures to give a wrap-up turn.
#[derive(Debug)]
pub struct TimedOut(String);

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TimedOut {}

/// `claude -p --safe-mode --settings <runroot>/claude-settings.json --output-format json`.
/// Writes the settings file: no edit/write/MCP tools, Bash sandboxed, writes denied in the
/// worktree and the git common dir, allowed only in `<runroot>/out`.
pub fn claude(worktree: &Path, git_common_dir: &Path, runroot: &Path) -> Result<Launch> {
    let worktree = absolute(worktree)?;
    let git_common_dir = absolute(git_common_dir)?;
    fs::create_dir_all(runroot.join("out")).with_context(|| format!("{}", runroot.display()))?;
    let runroot = absolute(runroot)?;
    let out = runroot.join("out");
    let settings_path = runroot.join("claude-settings.json");
    let settings = json!({
        "permissions": { "deny": [
            "Edit", "Write", "NotebookEdit", "mcp__*",
            "Bash(gh *)", "Bash(git push*)", "Bash(curl *)", "Bash(wget *)"
        ] },
        "sandbox": {
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "filesystem": {
                "denyWrite": [utf8(&worktree)?, utf8(&git_common_dir)?],
                "allowWrite": [utf8(&out)?]
            }
        },
        "disableBundledSkills": true
    });
    // Parallel passes of one run rewrite the same file; a rename never shows a half-written one.
    let tmp = runroot.join(format!(
        ".claude-settings.{:?}.tmp",
        std::thread::current().id()
    ));
    fs::write(&tmp, serde_json::to_string_pretty(&settings)?)
        .with_context(|| format!("{}", tmp.display()))?;
    fs::rename(&tmp, &settings_path).with_context(|| format!("{}", settings_path.display()))?;
    Ok(Launch {
        program: "claude".into(),
        args: [
            "-p",
            "--safe-mode",
            "--settings",
            utf8(&settings_path)?,
            "--output-format",
            "json",
        ]
        .map(String::from)
        .into(),
        env: vec![],
        cwd: worktree,
    })
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
    let user_auth = absolute(user_auth).context("Codex login (auth.json) not found")?;
    fs::create_dir_all(runroot.join("replies"))
        .with_context(|| format!("{}", runroot.display()))?;
    let runroot = absolute(runroot)?;
    let home = runroot.join("codex-home");
    fs::create_dir_all(&home).with_context(|| format!("{}", home.display()))?;
    let link = home.join("auth.json");
    // Several passes of one run share the home, in parallel: the link may already be there.
    match std::os::unix::fs::symlink(&user_auth, &link) {
        Err(e) if e.kind() == ErrorKind::AlreadyExists => match fs::read_link(&link) {
            Ok(target) if target == user_auth => {}
            Ok(target) => bail!(
                "{} points at {}, not {}",
                link.display(),
                target.display(),
                user_auth.display()
            ),
            Err(e) => {
                return Err(e).with_context(|| format!("{} is not a symlink", link.display()));
            }
        },
        r => r.with_context(|| format!("{}", link.display()))?,
    }
    Ok(Launch {
        program: "codex".into(),
        args: [
            "exec",
            "--json",
            "-s",
            "read-only",
            "-C",
            utf8(&worktree)?,
            "-o",
            utf8(&runroot.join("replies").join(out_name))?,
            "-",
        ]
        .map(String::from)
        .into(),
        env: vec![("CODEX_HOME".into(), utf8(&home)?.into())],
        cwd: worktree,
    })
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
        assert!(launch.env.is_empty());
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
                "Bash(wget *)"
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
                "-"
            ]
        );
        assert_eq!(
            launch.env,
            [("CODEX_HOME".to_string(), home.to_str().unwrap().to_string())]
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
