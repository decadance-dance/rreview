//! The orca runner (spec §4, variant 5): every pass runs the same isolated agent (spec §3),
//! interactive, in a visible Orca terminal of an Orca-created review worktree. Like the headless
//! runner, each pass gets `<runroot>/scratch/<role>`; the reviewer delivers its reply by writing
//! `out/reply.txt` there, the one dir it may write, and the host validates and publishes it.
//! Success is decided from Orca's structured `--json` output and that file, never from what the
//! terminal shows.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::launch::{self, Launch, TimedOut};
use crate::runner::{Agent, Pass, REPLY, Runner, model_args};

/// A reply larger than this is not a panelist envelope; reading stops there.
const MAX_REPLY: u64 = 4 << 20;
/// Bounds for Orca calls outside a pass's budget: worktree setup (also within the review's
/// budget) and cleanup.
const SETUP: Duration = Duration::from_secs(300);
const CLOSE: Duration = Duration::from_secs(30);

/// The runner drives Orca through its CLI, which needs the app running with its runtime
/// reachable and this repository registered (spec §4); `orca status` and `orca repo show` prove
/// that before any work, from an Orca terminal or not.
pub fn check_env(git_common_dir: &Path, deadline: Option<Instant>) -> Result<()> {
    let bound = Instant::now()
        .checked_add(CLOSE)
        .into_iter()
        .chain(deadline)
        .min();
    let v = orca(&["status"], bound).context(
        "--runner orca needs a running Orca app; use the default headless runner elsewhere",
    )?;
    if v["app"]["running"] != true || v["runtime"]["reachable"] != true {
        bail!(
            "--runner orca needs a running Orca app, and `orca status` reports it is not reachable; use the default headless runner elsewhere"
        );
    }
    let repo = main_checkout(git_common_dir);
    let selector = format!("path:{}", launch::utf8(repo)?);
    orca(&["repo", "show", "--repo", &selector], bound).with_context(|| {
        format!(
            "--runner orca needs this repository in Orca, and Orca does not show {}; add it in Orca first",
            repo.display()
        )
    })?;
    Ok(())
}

/// Orca registers a repository by its main checkout, the parent of a non-bare common dir.
fn main_checkout(git_common_dir: &Path) -> &Path {
    match git_common_dir.file_name() {
        Some(n) if n == ".git" => git_common_dir.parent().unwrap_or(git_common_dir),
        _ => git_common_dir,
    }
}

/// Orca knows the worktree the caller's working dir is in; `rreview doctor` opens its
/// terminals there.
pub fn check_current_worktree() -> Result<()> {
    orca(
        &["worktree", "show", "--worktree", "current"],
        Instant::now().checked_add(CLOSE),
    )
    .context("run `rreview doctor --runner orca` from a directory of an Orca worktree")
    .map(drop)
}

/// The Codex TUI starts in `<runroot>/out`; inside a git checkout it stops at its trust prompt,
/// which rreview does not answer for Codex. Such a run dir is refused before anything starts.
pub fn check_codex_run_dir(runroot: &Path) -> Result<()> {
    let out = Command::new("git")
        .args(["-C", launch::utf8(runroot)?, "rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("cannot run git")?;
    if out.status.success() {
        bail!(
            "the run dir {} is inside the git checkout {}, where the Codex TUI stops at its trust prompt; set XDG_STATE_HOME to a directory outside any checkout",
            runroot.display(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(())
}

/// The branch `orca worktree create` is asked to make for a review worktree.
pub fn branch_for(name: &str) -> String {
    format!("rreview/{name}")
}

/// Refuses a name whose branch exists: a review worktree's branch must be new, so whatever
/// carries it after `create_worktree` is ours, and `remove_worktree` may delete it by name.
pub fn check_branch_free(name: &str) -> Result<()> {
    let branch = branch_for(name);
    if git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])
    .is_ok()
    {
        bail!(
            "branch {branch} is left from an earlier run (--keep-worktree?); remove its worktree with `orca worktree rm`, then `git branch -D {branch}`"
        );
    }
    Ok(())
}

/// `orca worktree create` of `head` on the new branch `branch_for(name)` (see
/// `check_branch_free`), in the repository of `git_common_dir`, so Orca tracks the reviewers'
/// terminals in it (spec §4). Repo setup hooks are skipped: they would run the reviewed PR's
/// code. Returns the worktree path.
/// `deadline` is the review's budget; it is capped at `SETUP`.
pub fn create_worktree(
    git_common_dir: &Path,
    name: &str,
    head: &str,
    deadline: Option<Instant>,
) -> Result<PathBuf> {
    let branch = branch_for(name);
    let repo = main_checkout(git_common_dir);
    let v = orca(
        &[
            "worktree",
            "create",
            "--repo",
            &format!("path:{}", launch::utf8(repo)?),
            "--name",
            name,
            "--branch",
            &branch,
            "--base-branch",
            head,
            "--setup",
            "skip",
        ],
        Instant::now()
            .checked_add(SETUP)
            .into_iter()
            .chain(deadline)
            .min(),
    )?;
    let wt = &v["worktree"];
    let made = wt["branch"].as_str().unwrap_or_default();
    if made.strip_prefix("refs/heads/") != Some(&*branch) {
        bail!("orca worktree create made branch {made:?}, not {branch}");
    }
    Ok(wt["path"]
        .as_str()
        .context("orca worktree create: no path")?
        .into())
}

/// Removes the worktree on `branch_for(name)` and that branch, found through git rather than
/// Orca's reply, so a create whose reply was lost is cleaned up as well.
pub fn remove_worktree(name: &str) -> Result<()> {
    let branch = branch_for(name);
    let list = git(&["worktree", "list", "--porcelain"])?;
    let path = list.split("\n\n").find_map(|entry| {
        let mut lines = entry.lines();
        let path = lines.next()?.strip_prefix("worktree ")?;
        lines
            .any(|l| l == format!("branch refs/heads/{branch}"))
            .then_some(path)
    });
    if let Some(path) = path {
        let removed = orca(
            &[
                "worktree",
                "rm",
                "--worktree",
                &format!("path:{path}"),
                "--force",
            ],
            Instant::now().checked_add(SETUP),
        );
        // A worktree Orca never registered (its reply was lost) is git's to remove.
        if let Err(e) = removed {
            git(&["worktree", "remove", "--force", path])
                .with_context(|| format!("{e:#}; git worktree remove"))?;
        }
    }
    if git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])
    .is_ok()
    {
        git(&["branch", "-D", "--", &branch])?;
    }
    Ok(())
}

/// Git in the current repository; its stdout.
fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
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
    Ok(String::from_utf8_lossy(&out.stdout).into())
}

/// One open terminal per pass: `wrap_up` types into it after a timeout or an invalid reply.
#[derive(Clone)]
struct Term {
    handle: String,
    /// The agent came up; only then may a message be typed.
    ready: bool,
}

pub struct Orca {
    worktree: PathBuf,
    git_common_dir: PathBuf,
    runroot: PathBuf,
    codex_auth: PathBuf,
    /// The Orca worktree selector the terminals open in.
    terminal_worktree: String,
    /// Appended to every terminal title, `rreview:<role>:<tag>`; empty for a review.
    title_tag: String,
    /// Added to every launch's env.
    env: Vec<(String, String)>,
    /// What the reviewer writes to its reply file, as the delivery instruction names it.
    reply_as: String,
    terminals: Mutex<HashMap<String, Term>>,
}

impl Orca {
    pub fn new(
        worktree: PathBuf,
        git_common_dir: PathBuf,
        runroot: PathBuf,
        codex_auth: PathBuf,
    ) -> Self {
        Orca {
            terminal_worktree: format!("path:{}", worktree.display()),
            title_tag: String::new(),
            worktree,
            git_common_dir,
            runroot,
            codex_auth,
            env: vec![],
            reply_as: "the JSON object".into(),
            terminals: Mutex::default(),
        }
    }

    /// Terminals open in the Orca worktree `selector` instead of the review worktree, which
    /// Orca need not track (`rreview doctor`'s throwaway repo); the agent still starts in the
    /// review worktree (the shim enters it). Their titles end in `:<tag>`, unique to the run, so
    /// a terminal whose create reply was lost can be found and closed: no teardown removes the
    /// caller's worktree.
    pub fn terminals_in(mut self, selector: &str, tag: &str) -> Self {
        self.terminal_worktree = selector.into();
        self.title_tag = format!(":{tag}");
        self
    }

    fn title(&self, role: &str) -> String {
        format!("rreview:{role}{}", self.title_tag)
    }

    /// Closes the terminals titled `title` in the terminals' worktree: a create whose reply was
    /// lost may still have opened one. Best effort; a failure is a warning.
    // limit: a create that opens its terminal only after this listing still leaves it open.
    fn close_titled(&self, title: &str) {
        let listed = orca(
            &["terminal", "list", "--worktree", &self.terminal_worktree],
            Instant::now().checked_add(CLOSE),
        );
        match listed {
            Ok(v) => {
                for t in v["terminals"].as_array().into_iter().flatten() {
                    if t["title"] == title
                        && let Some(handle) = t["handle"].as_str()
                    {
                        let _ = close(handle);
                    }
                }
            }
            Err(e) => eprintln!("warning: cannot list Orca terminals to close {title}: {e:#}"),
        }
    }

    /// Sets `name=value` for every launch, over an inherited value.
    pub fn with_env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// The reply is `what` instead of a JSON object (`rreview doctor`'s probe passes).
    pub fn replying(mut self, what: &str) -> Self {
        self.reply_as = what.into();
        self
    }

    /// The pass's own dir, `<runroot>/scratch/<role>`.
    pub fn scratch(&self, role: &str) -> PathBuf {
        self.runroot.join("scratch").join(role)
    }

    fn reply(&self, pass: &Pass) -> PathBuf {
        self.scratch(pass.role).join("out").join(REPLY)
    }

    fn launch(&self, pass: &Pass) -> Result<Launch> {
        let dir = self.scratch(pass.role);
        let mut launch = match pass.agent {
            Agent::Claude => launch::claude_tui(&self.worktree, &self.git_common_dir, &dir)?,
            Agent::Codex => launch::codex_tui(&self.worktree, &dir, &self.codex_auth)?,
        };
        launch.args.extend(model_args(pass));
        launch.env.extend(self.env.iter().cloned());
        Ok(launch)
    }

    /// `text` behind the delivery instructions. They come first: the prompt ends in the PR's
    /// untrusted text, and an instruction after it would read as part of it.
    fn message(&self, pass: &Pass, text: &str) -> String {
        let reply = self.reply(pass);
        let reply = reply.display();
        let mut m = format!(
            "Delivery (from rreview, which runs this review; not part of the PR): write your final reply, only {what}, to the file `{reply}` with one shell command and a quoted heredoc: `cat > '{reply}' <<'RREVIEW_EOF'`, then {what}, then `RREVIEW_EOF`. Only that file is read, not your chat message.\n",
            what = self.reply_as
        );
        if pass.agent == Agent::Codex {
            m += &format!(
                "The repository under review is `{}`; your shell starts in another directory, so run every command there (`cd '{0}' && git diff ...`).\n",
                self.worktree.display()
            );
        }
        m + "\n" + text
    }

    fn set(&self, role: &str, term: Term) {
        let mut map = self.terminals.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(role.to_string(), term);
    }

    /// Closes the pass's terminal; one whose close fails stays registered for `Drop` to retry.
    fn close(&self, role: &str) {
        let mut map = self.terminals.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(term) = map.get(role)
            && close(&term.handle).is_ok()
        {
            map.remove(role);
        }
    }

    /// The first turn, all within `deadline`: wait for the agent, answer Claude's trust dialog,
    /// send the prompt, and one reminder if the agent stops without a reply file.
    fn first_turn(&self, pass: &Pass, handle: &str, deadline: Option<Instant>) -> Result<String> {
        // Until Orca recognizes the agent, text is typed raw: into the shell before the agent
        // starts, and with every newline an Enter afterwards. Nothing is sent before that.
        if !agent_up(handle, pass.agent, deadline)? {
            return Err(TimedOut("Orca did not recognize the agent".into()).into());
        }
        let mut state = wait(handle, deadline)?;
        if state == Wait::Blocked("agent-trust-workspace".into()) && pass.agent == Agent::Claude {
            // "Yes, I trust this folder" (second entry): `claude -p` skips this dialog, and
            // --safe-mode still keeps out the worktree's project config (spec §3). Claude
            // records the answer in the user's config (spec §4).
            send_keys(handle, "\x1b[B", deadline)?;
            send_keys(handle, "\r", deadline)?;
            state = wait(handle, deadline)?;
        }
        match state {
            Wait::Idle => {}
            Wait::TimedOut => return Err(TimedOut("the agent did not come up".into()).into()),
            Wait::Blocked(why) => bail!("the agent did not start: {why}"),
        }
        self.set(
            pass.role,
            Term {
                handle: handle.into(),
                ready: true,
            },
        );
        let reply = self.reply(pass);
        // Sent when the reviewer went idle without writing its reply file.
        let nudge = format!(
            "You stopped without writing the reply file. Write {} described in the first message to the reply file now.",
            self.reply_as
        );
        for text in [&*pass.prompt, &nudge] {
            send(handle, pass.agent, &self.message(pass, text), deadline)?;
            match wait(handle, deadline)? {
                Wait::Idle => {}
                Wait::TimedOut => return Err(TimedOut("no idle agent".into()).into()),
                Wait::Blocked(why) => bail!("the agent stopped: {why}"),
            }
            if let Some(r) = read_reply(&reply)? {
                return Ok(r);
            }
        }
        bail!(
            "the agent went idle twice without writing {}",
            reply.display()
        )
    }
}

impl Runner for Orca {
    fn run(&self, pass: &Pass) -> Result<String> {
        // limit: a timeout past Instant's range means no deadline.
        let deadline = Instant::now().checked_add(pass.timeout);
        let reply = self.reply(pass);
        match fs::remove_file(&reply) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("{}", reply.display()));
            }
            _ => {}
        }
        // limit: the shim runs this executable by path; replacing it mid-run (a rebuild) breaks
        // the later launches of the run.
        let exe = std::env::current_exe().context("cannot find the rreview executable")?;
        let command = shell_line(
            &self.launch(pass)?,
            &self.scratch(pass.role).join("bin"),
            &exe,
        )?;
        let title = self.title(pass.role);
        let v = orca(
            &[
                "terminal",
                "create",
                "--worktree",
                &self.terminal_worktree,
                "--title",
                &title,
                "--command",
                &command,
            ],
            deadline,
        )
        .inspect_err(|_| self.close_titled(&title))?;
        let handle = v["terminal"]["handle"]
            .as_str()
            .context("orca terminal create: no handle")?
            .to_string();
        // Registered before anything else can fail, so `Drop` closes it at the latest.
        self.set(
            pass.role,
            Term {
                handle: handle.clone(),
                ready: false,
            },
        );
        let result = self.first_turn(pass, &handle, deadline).map_err(|e| {
            if e.is::<TimedOut>() {
                TimedOut(format!(
                    "pass {} timed out after {}s",
                    pass.role,
                    pass.timeout.as_secs()
                ))
                .into()
            } else {
                e
            }
        });
        // The terminal stays open for a wrap-up turn (after a timeout or an invalid reply) and
        // closes with the runner; a pass that failed otherwise has no use for it.
        if matches!(&result, Err(e) if !e.is::<TimedOut>()) {
            self.close(pass.role);
        }
        result
    }

    fn wrap_up(&self, pass: &Pass, message: &str, timeout: Duration) -> Result<String> {
        let deadline = Instant::now().checked_add(timeout);
        let term = self
            .terminals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(pass.role)
            .cloned()
            .with_context(|| format!("no open terminal for pass {}", pass.role))?;
        let result = (|| {
            if !term.ready {
                bail!("the agent never became ready for the prompt");
            }
            send(
                &term.handle,
                pass.agent,
                &self.message(pass, message),
                deadline,
            )?;
            match wait(&term.handle, deadline)? {
                Wait::Idle => {}
                Wait::TimedOut => bail!(TimedOut("no idle agent".into())),
                Wait::Blocked(why) => bail!("the agent stopped: {why}"),
            }
            read_reply(&self.reply(pass))?.with_context(|| {
                format!(
                    "the agent went idle without writing {}",
                    self.reply(pass).display()
                )
            })
        })()
        .map_err(|e| {
            if e.is::<TimedOut>() {
                anyhow::anyhow!("the wrap-up timed out after {}s", timeout.as_secs())
            } else {
                e
            }
        });
        // Past its wrap-up the pass has failed; its terminal goes now.
        if result.is_err() {
            self.close(pass.role);
        }
        result
    }
}

impl Drop for Orca {
    fn drop(&mut self) {
        let map = self.terminals.get_mut().unwrap_or_else(|e| e.into_inner());
        for (_, term) in map.drain() {
            // One retry: a close can fail while Orca's runtime is briefly unavailable.
            if close(&term.handle).is_err() && close(&term.handle).is_err() {
                eprintln!("warning: Orca terminal {} is still open", term.handle);
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Wait {
    Idle,
    TimedOut,
    /// Orca's `blockedReason`: the agent waits on a dialog.
    Blocked(String),
}

/// `orca terminal wait --for tui-idle` until `deadline`: the agent finished its turn and waits
/// for input.
fn wait(handle: &str, deadline: Option<Instant>) -> Result<Wait> {
    // limit: Node timers overflow above 2^31-1 ms (~24 days); a longer wait ends there as a timeout.
    let ms = left(deadline).as_millis().min(i32::MAX as u128);
    if ms == 0 {
        return Ok(Wait::TimedOut);
    }
    // Orca's own timeout answers first; the kill is the backstop for an orca that hangs.
    let backstop = deadline.and_then(|d| d.checked_add(Duration::from_secs(5)));
    let (out, success) = run_orca(
        &[
            "terminal",
            "wait",
            "--terminal",
            handle,
            "--for",
            "tui-idle",
            "--timeout-ms",
            &ms.to_string(),
        ],
        backstop,
    )?;
    // An answer that arrives past the deadline (inside the backstop) is not accepted.
    if left(deadline).is_zero() {
        return Ok(Wait::TimedOut);
    }
    if out["ok"] != true {
        return match out["error"]["code"].as_str() {
            Some("timeout") => Ok(Wait::TimedOut),
            _ => bail!("orca terminal wait: {}", error(&out)),
        };
    }
    let w = &out["result"]["wait"];
    if w["satisfied"] == true && success {
        return Ok(Wait::Idle);
    }
    Ok(Wait::Blocked(w["blockedReason"].as_str().map_or_else(
        || format!("terminal {}", w["status"]),
        String::from,
    )))
}

/// A prompt to the agent; Orca bracket-pastes it and submits it with Enter. Watching for the
/// turn to start takes at most 10 s and never past `deadline`.
fn send(handle: &str, agent: Agent, text: &str, deadline: Option<Instant>) -> Result<()> {
    let watch = left(deadline).as_secs().min(10).to_string();
    let text = printable(text);
    let mut args = vec![
        "terminal",
        "send",
        "--terminal",
        handle,
        "--text",
        &text,
        "--enter",
    ];
    if watch != "0" {
        args.extend(["--wait-submit", &watch]);
    }
    let v = orca(&args, deadline)?;
    if v["send"]["accepted"] != true {
        bail!("orca terminal send: the prompt was not accepted");
    }
    // Orca names the agent it pasted to; another provider means the text went in raw.
    if let Some(p) = v["send"]["prompt"]["provider"].as_str()
        && p != agent.to_string()
    {
        bail!("orca terminal send: typed as raw input (provider {p:?}), not pasted to {agent}");
    }
    Ok(())
}

/// Polls `orca terminal show` until Orca identifies the terminal's agent as `agent`; false when
/// `deadline` passes first.
fn agent_up(handle: &str, agent: Agent, deadline: Option<Instant>) -> Result<bool> {
    loop {
        let v = orca(&["terminal", "show", "--terminal", handle], deadline)?;
        if v["terminal"]["agentIdentity"].as_str() == Some(&agent.to_string()) {
            return Ok(true);
        }
        if left(deadline) < Duration::from_millis(250) {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Raw key bytes, as typed.
fn send_keys(handle: &str, keys: &str, deadline: Option<Instant>) -> Result<()> {
    orca(
        &["terminal", "send", "--terminal", handle, "--text", keys],
        deadline,
    )
    .map(drop)
}

fn close(handle: &str) -> Result<()> {
    orca(
        &["terminal", "close", "--terminal", handle],
        Instant::now().checked_add(CLOSE),
    )
    .map(drop)
    .inspect_err(|e| eprintln!("warning: cannot close Orca terminal {handle}: {e:#}"))
}

/// What is left until `deadline`; none means unbounded.
fn left(deadline: Option<Instant>) -> Duration {
    deadline.map_or(Duration::MAX, |d| {
        d.saturating_duration_since(Instant::now())
    })
}

/// The PR's text rides in the prompt: control bytes (Escape, a carriage return that would
/// submit early) are dropped so it stays pasted text.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Orca types `--command` into the terminal's login shell, whose environment is Orca's and the
/// shell's own, not rreview's. So the agent starts through a shim, `<bin>/<program>`, that hands
/// it to `<exe> exec-reviewer` (this binary): that builds the environment from nothing at the
/// last exec (spec §3), each inherited variable that is set plus the launch's own env, in the
/// launch's working dir (the terminal starts in the Orca worktree it opened in). Nothing
/// the shell holds can slip past it: a read-only export, a name that is no identifier, an
/// exported function. Only names go into an argv, never an inherited value. The line is
/// `PATH='<bin>':"$PATH" exec 'program' 'arg' ...`, every word single-quoted, no value on it
/// (the shell may keep it in its history), and the shim takes `<bin>` off `PATH` again. `exec`
/// leaves no shell behind the agent to receive later input. The program must be `exec`'s first
/// word: with `exec env ...` or `exec '<path>/program'` Orca still names the agent, but its
/// `send` reports provider "unsupported" and types the prompt raw (*verified*, Orca 1.4.262).
// limit: a POSIX-family login shell (sh, bash, zsh) types the line.
fn shell_line(launch: &Launch, bin: &Path, exe: &Path) -> Result<String> {
    let quote = |w: &str| -> Result<String> {
        if w.chars().any(char::is_control) {
            bail!("cannot put {w:?} on a shell line");
        }
        Ok(format!("'{}'", w.replace('\'', r"'\''")))
    };
    let name = |k: &str| !k.is_empty() && k.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
    if launch.program.contains('/') || launch.program.starts_with('.') || launch.program.is_empty()
    {
        bail!("cannot shim the program {:?}", launch.program);
    }
    fs::create_dir_all(bin).with_context(|| format!("{}", bin.display()))?;
    let bin = fs::canonicalize(bin).with_context(|| format!("{}", bin.display()))?;
    // A `:` splits a PATH entry, and the shell would then find the real program, unfiltered.
    if launch::utf8(&bin)?.contains(':') {
        bail!(
            "the run dir {} holds a ':', which a PATH entry cannot; \
             set XDG_STATE_HOME to a path without one",
            bin.display()
        );
    }
    let bin_q = quote(launch::utf8(&bin)?)?;
    let mut shim = format!(
        "#!/bin/sh\n# rreview: {} with only the variables it inherits (spec §3).\n\
         PATH=${{PATH#{bin_q}:}}\ncd {} || exit 1\nexec {} exec-reviewer",
        launch.program,
        quote(launch::utf8(&launch.cwd)?)?,
        quote(launch::utf8(exe)?)?
    );
    for k in &launch.inherit {
        if !name(k) {
            bail!("cannot inherit the variable {k:?}");
        }
        shim += &format!(" --inherit {k}");
    }
    for (k, v) in &launch.env {
        if !name(k) {
            bail!("cannot put the variable {k:?} on a shell line");
        }
        shim += &format!(" --set {}", quote(&format!("{k}={v}"))?);
    }
    shim += &format!(" -- {} \"$@\"\n", quote(&launch.program)?);
    let path = bin.join(&launch.program);
    // A fresh file: whatever stands at that path is replaced, never written through.
    match fs::remove_file(&path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("{}", path.display()));
        }
        _ => {}
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)
        .and_then(|mut f| f.write_all(shim.as_bytes()))
        .with_context(|| format!("{}", path.display()))?;
    let mut line = format!("PATH={bin_q}:\"$PATH\" exec");
    for w in [&launch.program].into_iter().chain(&launch.args) {
        line += &format!(" {}", quote(w)?);
    }
    Ok(line)
}

/// The reviewer's reply file, if it wrote one. It can plant anything in `out/`: a symlink or a
/// FIFO is refused, not followed or read, and reading stops at `MAX_REPLY`.
fn read_reply(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("{}", path.display())),
        Ok(m) if !m.is_file() => bail!("{} is not a regular file", path.display()),
        Ok(_) => {
            let mut s = String::new();
            File::open(path)
                .and_then(|f| f.take(MAX_REPLY).read_to_string(&mut s))
                .with_context(|| format!("{}", path.display()))?;
            Ok(Some(s))
        }
    }
}

/// `orca <args> --json` within `deadline`; the `result` of a successful call.
fn orca(args: &[&str], deadline: Option<Instant>) -> Result<Value> {
    let (mut out, success) = run_orca(args, deadline)?;
    if out["ok"] != true || !success {
        bail!(
            "orca {}: {}",
            args[..args.len().min(2)].join(" "),
            error(&out)
        );
    }
    Ok(out["result"].take())
}

/// `orca <args> --json`: its JSON whether the call succeeded or not, and whether it exited 0.
/// (`terminal wait` exits 1 with `"ok": true` when the agent is blocked on a dialog.) A call
/// still running at `deadline` is killed with its process group and returns `TimedOut`.
fn run_orca(args: &[&str], deadline: Option<Instant>) -> Result<(Value, bool)> {
    use std::os::unix::process::CommandExt;
    let child = Command::new("orca")
        .args(args)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("cannot run orca")?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    // The output is read on a thread, so a full pipe cannot stall the deadline below.
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    let out = match rx.recv_timeout(left(deadline)) {
        Ok(out) => out.context("cannot run orca")?,
        Err(_) => {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stderr(Stdio::null())
                .status();
            return Err(TimedOut(format!(
                "orca {} did not answer in time",
                args[..args.len().min(2)].join(" ")
            ))
            .into());
        }
    };
    let v = serde_json::from_slice(&out.stdout).with_context(|| {
        format!(
            "orca {}: no JSON reply ({}): {}",
            args[..args.len().min(2)].join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })?;
    Ok((v, out.status.success()))
}

fn error(v: &Value) -> String {
    let e = &v["error"];
    match (e["code"].as_str(), e["message"].as_str()) {
        (Some(c), Some(m)) if c != m => format!("{c}: {m}"),
        (Some(c), _) => c.into(),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_line_quotes_every_word() {
        let root = crate::doctor::fresh_temp_dir("rreview-test").unwrap();
        let bin = root.join("it's bin");
        let launch = Launch {
            program: "codex".into(),
            args: vec!["-C".into(), "/a b/it's".into()],
            inherit: vec!["PATH", "LC_ALL"],
            env: vec![("CODEX_HOME".into(), "/h $x".into())],
            cwd: PathBuf::from("/"),
        };
        let exe = Path::new("/opt/it's/rreview");
        let q = r"'\''";
        assert_eq!(
            shell_line(&launch, &bin, exe).unwrap(),
            format!(
                "PATH='{}/it{q}s bin':\"$PATH\" exec 'codex' '-C' '/a b/it{q}s'",
                root.display()
            )
        );
        // Names only: no inherited value is expanded into the exec-reviewer argv.
        assert_eq!(
            fs::read_to_string(bin.join("codex")).unwrap(),
            format!(
                "#!/bin/sh\n# rreview: codex with only the variables it inherits (spec §3).\n\
                 PATH=${{PATH#'{}/it{q}s bin':}}\n\
                 cd '/' || exit 1\n\
                 exec '/opt/it{q}s/rreview' exec-reviewer --inherit PATH --inherit LC_ALL \
                 --set 'CODEX_HOME=/h $x' -- 'codex' \"$@\"\n",
                root.display()
            )
        );
        for bad in [
            Launch {
                args: vec!["a\nrm -rf x".into()],
                ..launch.clone()
            },
            Launch {
                inherit: vec!["X) ;; *) rm -rf x"],
                ..launch.clone()
            },
            Launch {
                program: "../codex".into(),
                ..launch.clone()
            },
        ] {
            assert!(shell_line(&bad, &bin, exe).is_err(), "{bad:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// A `:` in the run dir would split the shim's PATH entry, and the shell would start the real
    /// program with the terminal's whole environment (review of PR #24, round 2): refuse it.
    #[test]
    fn shell_line_refuses_a_run_dir_with_a_colon() {
        let root = crate::doctor::fresh_temp_dir("rreview-test").unwrap();
        let launch = Launch {
            program: "claude".into(),
            args: vec![],
            inherit: vec!["PATH"],
            env: vec![],
            cwd: PathBuf::from("/"),
        };
        let err = shell_line(&launch, &root.join("a:b/bin"), Path::new("/rreview")).unwrap_err();
        assert!(
            format!("{err:#}").contains("holds a ':', which a PATH entry cannot"),
            "{err:#}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn printable_drops_control_bytes_but_keeps_lines() {
        assert_eq!(printable("a\x1b[201~b\r\nc\td\x03"), "a[201~b\nc\td");
    }

    #[test]
    fn read_reply_refuses_what_is_not_a_regular_file() {
        let root = crate::doctor::fresh_temp_dir("rreview-test").unwrap();
        assert_eq!(read_reply(&root.join("none")).unwrap(), None);
        fs::write(root.join("victim"), "secret").unwrap();
        std::os::unix::fs::symlink(root.join("victim"), root.join("link")).unwrap();
        assert!(read_reply(&root.join("link")).is_err());
        let fifo = root.join("fifo");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(read_reply(&fifo).is_err());
        fs::write(root.join("big"), vec![b'x'; MAX_REPLY as usize + 10]).unwrap();
        assert_eq!(
            read_reply(&root.join("big")).unwrap().unwrap().len(),
            MAX_REPLY as usize
        );
        fs::remove_dir_all(root).unwrap();
    }
}
