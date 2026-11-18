//! `rreview <pr>`: prepare the review worktree and run dir (spec §2.1), run the panel through
//! a runner (spec §4), and leave `meta.json`, `progress.jsonl` and `out/<role>.json` (spec §5).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use crate::launch;
use crate::panelist::{self, PrText};
use crate::runner::{Agent, Headless, Pass, Runner};

const WRAP_UP: &str = "Time is up. Stop reviewing now and reply with only the JSON object described in the first message, covering what you have checked so far.";
const WRAP_UP_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Args {
    pub pr: String,
    pub repo: Option<String>,
    pub only: String,
    pub role_agents: Vec<(String, Agent)>,
    pub keep_worktree: bool,
    pub timeout: Duration,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    title: String,
    body: String,
    url: String,
    head_ref_name: String,
    base_ref_name: String,
    head_ref_oid: String,
    base_ref_oid: String,
    state: String,
    merge_commit: Option<Oid>,
}

#[derive(Deserialize)]
struct Oid {
    oid: String,
}

/// Runs the review; true when every pass completed.
pub fn run(args: &Args) -> Result<bool> {
    let role = panelist::role(&args.only)?;
    let mut agent = role.default_agent;
    for (r, a) in &args.role_agents {
        panelist::role(r)?;
        if r == role.id {
            agent = *a;
        }
    }

    let pr = view_pr(args)?;
    let (owner, name) = owner_and_name(&pr.url)?;
    let head = commit(&pr.head_ref_oid, "headRefOid")?;
    // A merged PR's base branch now contains its head; its diff is against the base before the merge.
    let base_tip = match (&*pr.state, &pr.merge_commit) {
        ("MERGED", Some(m)) => format!("{}^1", commit(&m.oid, "mergeCommit")?),
        _ => commit(&pr.base_ref_oid, "baseRefOid")?.to_string(),
    };
    ensure_commits(&pr, &[head, &base_tip])?;
    let base = git(&["merge-base", &base_tip, head])?;
    // Raw bytes: names may start or end with spaces. A non-UTF-8 name cannot be named in JSON.
    let diff_files: BTreeSet<String> = git_raw(&["diff", "--name-only", "-z", &base, head])?
        .split(|b| *b == 0)
        .filter_map(|f| String::from_utf8(f.to_vec()).ok())
        .filter(|f| !f.is_empty())
        .collect();

    let runroot = state_home()?
        .join("rreview")
        .join(owner)
        .join(name)
        .join(format!("{}-{head}", pr.number));
    let wt_path = runroot.join("wt");
    if wt_path.exists() {
        bail!(
            "{} is left from an earlier run (--keep-worktree?); remove it with `git worktree remove --force {0}`",
            wt_path.display()
        );
    }
    // limit: two runs for the same PR head at once share this dir; the later one wipes it.
    if runroot.exists() {
        fs::remove_dir_all(&runroot).with_context(|| format!("{}", runroot.display()))?;
    }
    fs::create_dir_all(&runroot).with_context(|| format!("{}", runroot.display()))?;
    let runroot = fs::canonicalize(&runroot)?;
    // The guard exists before the checkout: a failed one (a post-checkout hook) can leave a
    // registered, half-made worktree behind.
    let mut wt = Worktree {
        path: runroot.join("wt"),
        keep: false,
    };
    git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        launch::utf8(&wt.path)?,
        head,
    ])
    .context("cannot create the review worktree")?;
    wt.keep = args.keep_worktree;

    let roles = BTreeMap::from([(role.id, agent.to_string())]);
    let meta = json!({
        "pr": { "number": pr.number, "title": pr.title, "url": pr.url, "repo": format!("{owner}/{name}"),
                "head_ref": pr.head_ref_name, "base_ref": pr.base_ref_name },
        "base": base, "head": head, "runner": "headless", "roles": roles,
        "versions": { "claude": version("claude"), "codex": version("codex") },
    });
    fs::write(
        runroot.join("meta.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;

    let runner = Headless {
        git_common_dir: git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?.into(),
        worktree: wt.path.clone(),
        runroot: runroot.clone(),
        codex_auth: launch::user_codex_auth()?,
    };
    let pass = Pass {
        role: role.id,
        agent,
        prompt: panelist::prompt(
            role,
            &base,
            head,
            &PrText {
                title: &pr.title,
                body: &pr.body,
            },
        ),
        timeout: args.timeout,
    };
    let progress = Progress(runroot.join("progress.jsonl"));
    let out = runroot.join("out").join(format!("{}.json", role.id));
    let started = Instant::now();
    progress.event(&pass, "started", started, None)?;
    let result = run_pass(&runner, &pass, &progress, started).and_then(|raw| {
        let envelope = panelist::parse(&raw, role, &diff_files)?;
        // The reviewer can write into out/ (a symlink, say); a rename from the run root, which
        // it cannot write, replaces whatever stands there without following it.
        let tmp = runroot.join(format!(".{}.json.tmp", role.id));
        fs::write(&tmp, serde_json::to_string_pretty(&envelope)?)?;
        fs::rename(&tmp, &out).with_context(|| format!("{}", out.display()))?;
        Ok(envelope)
    });
    let ok = match result {
        Ok(envelope) => {
            progress.event(&pass, "completed", started, None)?;
            println!(
                "{} ({agent}): {} candidates in {}",
                role.id,
                envelope.candidates.len(),
                out.display()
            );
            true
        }
        Err(e) => {
            // The reviewer may write into out/; only a validated envelope may stand there.
            let _ = fs::remove_file(&out);
            let error = format!("pass {} ({agent}): {e:#}", role.id);
            progress.event(&pass, "failed", started, Some(&error))?;
            eprintln!("error: {error}");
            false
        }
    };
    drop(wt);
    println!("run dir: {}", runroot.display());
    Ok(ok)
}

/// One pass; a pass that times out gets one wrap-up turn (spec §4).
fn run_pass(
    runner: &impl Runner,
    pass: &Pass,
    progress: &Progress,
    started: Instant,
) -> Result<String> {
    match runner.run(pass) {
        Err(e) if e.is::<launch::TimedOut>() => {
            progress.event(pass, "wrapup", started, None)?;
            runner
                .wrap_up(pass, WRAP_UP, WRAP_UP_TIMEOUT.min(pass.timeout))
                .with_context(|| format!("{e:#}; wrap-up"))
        }
        r => r,
    }
}

/// The review worktree; removed when dropped (also on an error path) unless kept.
// limit: a killed rreview (SIGKILL, Ctrl-C) leaves it; the next run for the same head says so.
struct Worktree {
    path: PathBuf,
    keep: bool,
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.keep {
            println!("review worktree kept: {}", self.path.display());
            return;
        }
        let removed =
            launch::utf8(&self.path).and_then(|p| git(&["worktree", "remove", "--force", p]));
        // Not registered (a checkout that failed early): only a directory may be left.
        if let Err(e) = removed
            && self.path.exists()
            && fs::remove_dir_all(&self.path).is_err()
        {
            eprintln!("warning: cannot remove the review worktree: {e:#}");
        }
    }
}

struct Progress(PathBuf);

impl Progress {
    fn event(
        &self,
        pass: &Pass,
        status: &str,
        started: Instant,
        error: Option<&str>,
    ) -> Result<()> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let mut event = json!({
            "ts": ts as u64, "role": pass.role, "status": status, "agent": pass.agent.to_string(),
            // limit: no --role-model yet (#4); the CLI picks its default model.
            "model": "default", "duration_ms": started.elapsed().as_millis() as u64,
        });
        if let Some(error) = error {
            event["error"] = error.into();
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.0)?;
        writeln!(file, "{event}").with_context(|| format!("{}", self.0.display()))
    }
}

fn view_pr(args: &Args) -> Result<Pr> {
    let mut cmd = Command::new("gh");
    cmd.args(["pr", "view", &args.pr, "--json"]).arg(
        "number,title,body,url,headRefName,baseRefName,headRefOid,baseRefOid,state,mergeCommit",
    );
    if let Some(repo) = &args.repo {
        cmd.args(["--repo", repo]);
    }
    let out = cmd.stdin(Stdio::null()).output().context("cannot run gh")?;
    if !out.status.success() {
        bail!(
            "gh pr view {}: {}",
            args.pr,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).context("unexpected `gh pr view` output")
}

/// `<owner>/<name>` from the PR URL (`.../<owner>/<name>/pull/<n>`), checked to be plain
/// directory names before they become part of the run dir path.
fn owner_and_name(url: &str) -> Result<(&str, &str)> {
    let repo_url = repo_url(url)?;
    let mut parts = repo_url.rsplit('/');
    let (name, owner) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    for part in [owner, name] {
        if !matches!(
            Path::new(part).components().collect::<Vec<_>>()[..],
            [Component::Normal(_)]
        ) {
            bail!("cannot tell the repository from the PR URL {url:?}");
        }
    }
    Ok((owner, name))
}

fn repo_url(url: &str) -> Result<&str> {
    url.rsplit_once("/pull/")
        .map(|(repo, _)| repo)
        .with_context(|| format!("not a PR URL: {url:?}"))
}

/// A full commit id; anything else from gh is refused before it reaches git's argv.
fn commit<'a>(oid: &'a str, field: &str) -> Result<&'a str> {
    if matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(oid)
    } else {
        bail!("gh returned a bad {field}: {oid:?}")
    }
}

/// Fetches the PR head and its base branch from the PR's repository when a commit is missing.
fn ensure_commits(pr: &Pr, revs: &[&str]) -> Result<()> {
    let missing = |rev: &str| git(&["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_err();
    if !revs.iter().any(|r| missing(r)) {
        return Ok(());
    }
    git(&[
        "fetch",
        "--quiet",
        "--no-tags",
        repo_url(&pr.url)?,
        &format!("refs/pull/{}/head", pr.number),
        &format!("refs/heads/{}", pr.base_ref_name),
    ])
    .context("cannot fetch the PR")?;
    match revs.iter().find(|r| missing(r)) {
        Some(rev) => bail!(
            "commit {rev} is not in this repository even after a fetch; run rreview in a clone of the PR's repository"
        ),
        None => Ok(()),
    }
}

fn state_home() -> Result<PathBuf> {
    match std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        _ => Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                .join(".local/state"),
        ),
    }
}

fn version(bin: &str) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

/// Git in the current repository.
fn git(args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_raw(args)?).trim().into())
}

/// Git's stdout as it is.
fn git_raw(args: &[&str]) -> Result<Vec<u8>> {
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
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_and_name_come_from_the_pr_url() {
        assert_eq!(
            owner_and_name("https://github.com/acme/widget/pull/7").unwrap(),
            ("acme", "widget")
        );
        for bad in [
            "https://github.com/acme/widget",
            "https://x/../pull/7",
            "/pull/7",
            "https://github.com/a/./pull/1",
        ] {
            assert!(owner_and_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn commit_takes_only_full_hex_ids() {
        assert!(commit(&"a".repeat(40), "f").is_ok());
        for bad in ["--upload-pack=x", "HEAD", &"a".repeat(39), &"g".repeat(40)] {
            assert!(commit(bad, "f").is_err(), "{bad}");
        }
    }
}
