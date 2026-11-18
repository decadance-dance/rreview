//! Runners (spec §4): one interface for a reviewer pass; the headless one runs the agent CLIs
//! as child processes through the launch builders.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::launch::{self, Launch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        })
    }
}

impl std::str::FromStr for Agent {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "claude" => Ok(Agent::Claude),
            "codex" => Ok(Agent::Codex),
            _ => Err(format!("unknown agent {s:?}; known: claude, codex")),
        }
    }
}

/// One reviewer pass. The runner owns where it runs (worktree, run dir).
pub struct Pass<'a> {
    pub role: &'a str,
    pub agent: Agent,
    pub prompt: String,
    pub timeout: Duration,
}

pub trait Runner {
    /// Runs the pass; returns the reviewer's final message.
    fn run(&self, pass: &Pass) -> Result<String>;
    /// Resumes the pass's session with `message` ("time is up, reply now").
    fn wrap_up(&self, pass: &Pass, message: &str, timeout: Duration) -> Result<String>;
}

/// `claude -p` / `codex exec` in the review worktree. Logs go to `<runroot>/logs/<role>.*`.
pub struct Headless {
    pub worktree: PathBuf,
    pub git_common_dir: PathBuf,
    pub runroot: PathBuf,
    pub codex_auth: PathBuf,
}

impl Headless {
    fn launch(&self, pass: &Pass) -> Result<Launch> {
        match pass.agent {
            Agent::Claude => launch::claude(&self.worktree, &self.git_common_dir, &self.runroot),
            Agent::Codex => launch::codex(
                &self.worktree,
                &self.runroot,
                &reply_name(pass.role),
                &self.codex_auth,
            ),
        }
    }

    fn log(&self, pass: &Pass, step: &str) -> Result<PathBuf> {
        let dir = self.runroot.join("logs");
        fs::create_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
        Ok(dir.join(format!("{}{step}", pass.role)))
    }

    /// Runs `launch` and returns the final message: Claude's `result` from its JSON output,
    /// Codex's `-o` file. Exit status and the CLI's own error flag decide failure.
    fn finish(
        &self,
        pass: &Pass,
        launch: &Launch,
        prompt: &str,
        log: &Path,
        timeout: Duration,
    ) -> Result<String> {
        let reply = self.runroot.join("out").join(reply_name(pass.role));
        let _ = fs::remove_file(&reply);
        let status = launch.run(prompt, log, timeout)?;
        let stderr = log.with_extension("stderr");
        match pass.agent {
            Agent::Claude => {
                let stdout = fs::read_to_string(log.with_extension("stdout"))?;
                let out: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
                    anyhow!("claude {status}, no JSON result; see {}", stderr.display())
                })?;
                let result = out["result"].as_str().unwrap_or_default();
                if !status.success() || out["is_error"] != false {
                    bail!("claude {status}, reported an error: {}", first_line(result));
                }
                Ok(result.to_string())
            }
            Agent::Codex => {
                if !status.success() {
                    bail!("codex {status}; see {}", stderr.display());
                }
                fs::read_to_string(&reply)
                    .with_context(|| format!("codex wrote no final message to {}", reply.display()))
            }
        }
    }
}

impl Runner for Headless {
    fn run(&self, pass: &Pass) -> Result<String> {
        let mut launch = self.launch(pass)?;
        let log = self.log(pass, "")?;
        if pass.agent == Agent::Claude {
            // A known id, so a wrap-up can resume a session that was killed before it printed one.
            let id = uuid_v4();
            fs::write(log.with_extension("session"), &id)?;
            launch.args.extend(["--session-id".into(), id]);
        }
        self.finish(pass, &launch, &pass.prompt, &log, pass.timeout)
    }

    fn wrap_up(&self, pass: &Pass, message: &str, timeout: Duration) -> Result<String> {
        let mut launch = self.launch(pass)?;
        let first = self.log(pass, "")?;
        match pass.agent {
            Agent::Claude => {
                let id = fs::read_to_string(first.with_extension("session"))
                    .context("no claude session to resume")?;
                launch.args.extend(["--resume".into(), id]);
            }
            Agent::Codex => {
                // `exec [options] resume <id> -`: the exec options (sandbox, -C, -o) still apply.
                let id = codex_thread(&fs::read_to_string(first.with_extension("stdout"))?)
                    .context("no codex thread to resume")?;
                let stdin = launch.args.pop();
                launch.args.extend(["resume".into(), id]);
                launch.args.extend(stdin);
            }
        }
        self.finish(pass, &launch, message, &self.log(pass, "-wrapup")?, timeout)
    }
}

/// Codex's `-o` file for a pass; `out/<role>.json` is only ever the validated envelope.
fn reply_name(role: &str) -> String {
    format!("{role}.reply.txt")
}

/// The thread id from Codex's `thread.started` event.
fn codex_thread(events: &str) -> Option<String> {
    events
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "thread.started")
        .and_then(|e| e["thread_id"].as_str().map(String::from))
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn uuid_v4() -> String {
    let random = || RandomState::new().build_hasher().finish();
    let (a, b) = (
        random() & !0xf000 | 0x4000,
        random() & !(0xc << 60) | (0x8 << 60),
    );
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xffff,
        b >> 48,
        b & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_is_a_v4_shape_and_unique() {
        let (a, b) = (uuid_v4(), uuid_v4());
        assert_ne!(a, b);
        let parts: Vec<_> = a.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12], "{a}");
        assert_eq!(&a[14..15], "4", "{a}");
        assert!("89ab".contains(&a[19..20]), "{a}");
    }

    #[test]
    fn codex_thread_comes_from_the_thread_started_event() {
        let log = "noise\n{\"type\":\"turn.started\"}\n{\"type\":\"thread.started\",\"thread_id\":\"t-1\"}\n";
        assert_eq!(codex_thread(log).as_deref(), Some("t-1"));
        assert_eq!(codex_thread("{\"type\":\"turn.started\"}"), None);
    }
}
