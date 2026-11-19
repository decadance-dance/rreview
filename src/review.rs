//! `rreview <pr>`: prepare the review worktree and run dir (spec §2.1), run the panel, the
//! coordinator and the judge through a runner (spec §2, §4), gate the findings and leave
//! `meta.json`, `progress.jsonl`, `out/<role>.json` and `result.json` (spec §5).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::coordinator::{self, PanelCandidate, PanelResult, Scope, Tagged};
use crate::launch;
use crate::orca::{self, Orca};
use crate::panelist::{self, PrText, ROLES, Role};
use crate::result::{self, Gate, PassReport, ReviewResult};
use crate::runner::{Agent, Headless, Pass, Runner};

const WRAP_UP: &str = "Time is up. Stop reviewing now and reply with only the JSON object described in the first message, covering what you have checked so far.";
const RETRY: &str = "Your reply was rejected. Reply again with only the JSON described in the first message, fixed; nothing else.";
const WRAP_UP_TIMEOUT: Duration = Duration::from_secs(180);
/// The passes after the panel, in order.
const PIPELINE: [&str; 2] = ["coordinator", "judge"];

pub struct Args {
    pub pr: String,
    pub repo: Option<String>,
    /// Review this commit instead of the PR's current head (a full commit id).
    pub head: Option<String>,
    /// Run this panelist alone, without coordinator and judge (a development flag).
    pub only: Option<String>,
    /// The family that wrote the change; the defaults give most discovery and the judge to
    /// the other one.
    pub author: Agent,
    pub role_agents: Vec<(String, Agent)>,
    pub role_models: Vec<(String, String)>,
    pub role_efforts: Vec<(String, String)>,
    pub context_file: Option<PathBuf>,
    pub gate: Gate,
    pub json: bool,
    pub keep_worktree: bool,
    pub timeout: Duration,
    /// The whole review's budget.
    pub budget: Duration,
    /// `--runner orca`: passes in visible Orca terminals (spec §4).
    pub orca: bool,
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

/// Agent, model and effort of every role (the three lenses, coordinator, judge).
struct Settings {
    agents: BTreeMap<&'static str, Agent>,
    models: BTreeMap<&'static str, String>,
    efforts: BTreeMap<&'static str, String>,
}

impl Settings {
    fn new(args: &Args) -> Result<Settings> {
        let all: Vec<&'static str> = ROLES.iter().map(|r| r.id).chain(PIPELINE).collect();
        let id = |r: &str| {
            all.iter()
                .copied()
                .find(|x| *x == r)
                .with_context(|| format!("unknown role {r:?}; known: {}", all.join(", ")))
        };
        let flip = |a: Agent| match (args.author, a) {
            (Agent::Claude, a) => a,
            (Agent::Codex, Agent::Claude) => Agent::Codex,
            (Agent::Codex, Agent::Codex) => Agent::Claude,
        };
        let mut agents: BTreeMap<_, _> = ROLES
            .iter()
            .map(|r| (r.id, flip(r.default_agent)))
            .chain([
                ("coordinator", flip(Agent::Claude)),
                ("judge", flip(Agent::Codex)),
            ])
            .collect();
        for (r, a) in &args.role_agents {
            agents.insert(id(r)?, *a);
        }
        if agents["coordinator"] == agents["judge"] {
            bail!(
                "the coordinator and the judge must not share a model family, but both are {}; change one with --role-agent",
                agents["judge"]
            );
        }
        let mut models = BTreeMap::new();
        for (r, m) in &args.role_models {
            models.insert(id(r)?, m.clone());
        }
        let mut efforts = BTreeMap::new();
        for (r, e) in &args.role_efforts {
            let r = id(r)?;
            let known: &[&str] = match agents[r] {
                Agent::Claude => &["low", "medium", "high", "xhigh", "max"],
                Agent::Codex => &["minimal", "low", "medium", "high", "xhigh"],
            };
            if !known.contains(&e.as_str()) {
                bail!(
                    "effort {e:?} for {r} ({}): known: {}",
                    agents[r],
                    known.join(", ")
                );
            }
            efforts.insert(r, e.clone());
        }
        Ok(Settings {
            agents,
            models,
            efforts,
        })
    }
}

/// Runs the review; returns the exit code: 0 no gated findings, 1 gated findings, 2 failed.
pub fn run(args: &Args) -> Result<u8> {
    let settings = Settings::new(args)?;
    let panel: Vec<&Role> = match &args.only {
        Some(id) => vec![panelist::role(id)?],
        None => ROLES.iter().collect(),
    };
    let task = match &args.context_file {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("--context-file {}", path.display()))?,
        None => String::new(),
    };
    // None: a budget too large for the clock is no limit.
    let deadline = Instant::now().checked_add(args.budget);
    let git_common_dir: PathBuf = git(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        deadline,
    )?
    .into();
    if args.orca {
        orca::check_env(&git_common_dir, deadline)?;
    }

    // The budget covers preparation too: gh, a fetch or a checkout hook can stall.
    let pr = view_pr(&args.pr, args.repo.as_deref(), deadline)?;
    let (owner, name) = owner_and_name(&pr.url)?;
    let head = match &args.head {
        Some(h) => pr_commit(h, &pr, owner, name, deadline)?,
        None => commit(&pr.head_ref_oid, "headRefOid")?,
    };
    // A merged PR's base branch now contains its head; its diff is against the base before the merge.
    let base_tip = match (&*pr.state, &pr.merge_commit) {
        ("MERGED", Some(m)) => format!("{}^1", commit(&m.oid, "mergeCommit")?),
        _ => commit(&pr.base_ref_oid, "baseRefOid")?.to_string(),
    };
    ensure_commits(&pr, &[head, &base_tip], deadline)?;
    let base = git(&["merge-base", &base_tip, head], deadline)?;
    // Raw bytes: names may start or end with spaces. A non-UTF-8 name cannot be named in JSON.
    let diff_files: BTreeSet<String> =
        git_raw(&["diff", "--name-only", "-z", &base, head], deadline)?
            .split(|b| *b == 0)
            .filter_map(|f| String::from_utf8(f.to_vec()).ok())
            .filter(|f| !f.is_empty())
            .collect();

    let runroot = state_home()?
        .join("rreview")
        .join(owner)
        .join(name)
        .join(format!("{}-{head}", pr.number));
    // Held until this function returns, past the last write into the run dir.
    let _lock = lock_head(&runroot)?;
    let wt_path = runroot.join("wt");
    if wt_path.exists() {
        bail!(
            "{} is left from an earlier run (--keep-worktree?); remove it with `git worktree remove --force {0}`",
            wt_path.display()
        );
    }
    if runroot.exists() {
        fs::remove_dir_all(&runroot).with_context(|| format!("{}", runroot.display()))?;
    }
    // Private from its creation, whatever the umask: it holds prompts with the PR and task text,
    // reviewer logs and run-scoped Codex sessions. Its parent exists: lock_head made it.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&runroot)
        .and_then(|()| fs::create_dir(runroot.join("out")))
        .with_context(|| format!("{}", runroot.display()))?;
    let runroot = fs::canonicalize(&runroot)?;
    // The run's own id, also in meta.json: a later run that replaces this dir may get the same
    // path and inode, so the Orca plugin compares this id before it trusts the dir's files.
    let run_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    // Printed now, not only at the end: the Orca plugin tails progress.jsonl here during the run.
    eprintln!("run id: {run_id}\nrun dir: {}", runroot.display());
    // The guard exists before the checkout: a failed one (a post-checkout hook) can leave a
    // registered, half-made worktree behind.
    let ran = |r: &str| panel.iter().any(|p| p.id == r) || args.only.is_none();
    if args.orca
        && settings
            .agents
            .iter()
            .any(|(r, a)| ran(r) && *a == Agent::Codex)
    {
        orca::check_codex_run_dir(&runroot)?;
    }
    let mut wt = Worktree {
        path: runroot.join("wt"),
        keep: false,
        orca_name: None,
    };
    if args.orca {
        // The pid and start time make the name the run's own: a concurrent run for the same
        // PR head gets another one, so neither run's cleanup can reach the other's worktree.
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let name = format!(
            "rreview-{}-{}-{}-{started}",
            pr.number,
            &head[..12],
            std::process::id()
        );
        orca::check_branch_free(&name)?;
        // Set before the create: a create whose reply is lost is still found by its name.
        wt.orca_name = Some(name.clone());
        wt.path = orca::create_worktree(&git_common_dir, &name, head, deadline)
            .context("cannot create the review worktree")?;
        let at = git(
            &["-C", launch::utf8(&wt.path)?, "rev-parse", "HEAD"],
            deadline,
        )?;
        if at != head {
            bail!(
                "the Orca worktree {} is at {at}, not {head}",
                wt.path.display()
            );
        }
    } else {
        git(
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                launch::utf8(&wt.path)?,
                head,
            ],
            deadline,
        )
        .context("cannot create the review worktree")?;
    }
    wt.keep = args.keep_worktree;

    let roles: BTreeMap<_, _> = settings
        .agents
        .iter()
        .filter(|(r, _)| ran(r))
        .map(|(r, a)| (*r, a.to_string()))
        .collect();
    let meta = json!({
        "run_id": run_id,
        "pr": { "number": pr.number, "title": pr.title, "url": pr.url, "repo": format!("{owner}/{name}"),
                "head_ref": pr.head_ref_name, "base_ref": pr.base_ref_name },
        "base": base, "head": head, "runner": if args.orca { "orca" } else { "headless" }, "roles": roles,
        "versions": { "claude": version("claude", deadline), "codex": version("codex", deadline) },
    });
    fs::write(
        runroot.join("meta.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;

    let runner: Box<dyn Runner + Sync> = if args.orca {
        Box::new(Orca::new(
            wt.path.clone(),
            git_common_dir,
            runroot.clone(),
            launch::user_codex_auth()?,
        ))
    } else {
        Box::new(Headless {
            git_common_dir,
            worktree: wt.path.clone(),
            runroot: runroot.clone(),
            codex_auth: launch::user_codex_auth()?,
        })
    };
    let ctx = Ctx {
        runner: &runner,
        progress: Progress(runroot.join("progress.jsonl")),
        runroot: &runroot,
        deadline,
        timeout: args.timeout,
        settings: &settings,
    };
    let pr_text = PrText {
        title: &pr.title,
        body: &pr.body,
        task: &task,
    };

    // Panel: the lenses in parallel threads.
    let outcomes = std::thread::scope(|s| {
        let threads: Vec<_> = panel
            .iter()
            .map(|role| {
                let (ctx, diff_files) = (&ctx, &diff_files);
                let prompt = panelist::prompt(role, &base, head, &pr_text);
                s.spawn(move || {
                    ctx.review_pass(role.id, prompt, |raw| {
                        panelist::parse(raw, role, diff_files)
                    })
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|t| t.join().map_err(|_| anyhow!("a panel thread panicked")))
            .collect::<Result<Vec<_>>>()
    })?;
    // Every candidate gets a stable id, `<role>-<n>`, which the coordinator accounts for.
    let ids: Vec<Vec<String>> = panel
        .iter()
        .zip(&outcomes)
        .map(|(role, (_, outcome))| match outcome {
            Ok(env) => (1..=env.candidates.len())
                .map(|n| format!("{}-{n}", role.id))
                .collect(),
            Err(_) => vec![],
        })
        .collect();
    let mut results = vec![];
    let mut candidates = vec![];
    for ((role, (report, outcome)), ids) in panel.iter().zip(&outcomes).zip(&ids) {
        match outcome {
            Ok(env) => {
                for (id, c) in ids.iter().zip(&env.candidates) {
                    candidates.push(PanelCandidate {
                        id: id.clone(),
                        panelist: role.name,
                        candidate: c,
                    });
                }
                eprintln!(
                    "{} ({}): {} candidates",
                    role.id,
                    report.agent,
                    env.candidates.len()
                );
            }
            Err(e) if args.only.is_some() => eprintln!("error: {e:#}"),
            Err(e) => eprintln!("warning: {e:#}; the coordinator covers this lens"),
        }
        results.push(PanelResult {
            panelist: role.name,
            status: if outcome.is_ok() {
                "completed"
            } else {
                "failed"
            },
            coverage_summary: outcome.as_ref().ok().map(|e| e.coverage_summary.as_str()),
            candidates: outcome.as_ref().ok().map(|env| {
                ids.iter()
                    .zip(&env.candidates)
                    .map(|(id, candidate)| Tagged { id, candidate })
                    .collect()
            }),
            error: outcome.as_ref().err().map(|e| format!("{e:#}")),
        });
    }
    let mut degraded = results.iter().any(|r| r.status != "completed");
    if args.only.is_some() {
        // Terminals close before their worktree goes.
        drop(ctx);
        drop(runner);
        drop(wt);
        eprintln!("run dir: {}", runroot.display());
        return Ok(if degraded { 2 } else { 0 });
    }

    let wt_str = launch::utf8(&wt.path)?.to_string();
    let scope = Scope {
        worktree: &wt_str,
        diff_files: &diff_files,
    };
    let roster: Vec<_> = panel.iter().map(|r| r.name).collect();
    let prompt = coordinator::coordinator_prompt(&roster, &base, head, &pr_text, &results)?;
    let (report, review) = ctx.review_pass("coordinator", prompt, |raw| {
        coordinator::parse_review(raw, &scope, &candidates)
    });
    let mut passes: Vec<PassReport> = outcomes
        .into_iter()
        .map(|(r, _)| r)
        .chain([report])
        .collect();
    let review = match review {
        Ok(review) => review,
        Err(e) => {
            // Terminals close before their worktree goes.
            drop(ctx);
            drop(runner);
            drop(wt);
            eprintln!("error: {e:#}\nrun dir: {}", runroot.display());
            return Ok(2);
        }
    };

    // The judge is a recovery pass: when it fails, the coordinator's findings stand.
    let mut findings = review.findings.clone();
    if !review.dropped_candidates.is_empty() {
        let prompt =
            coordinator::judge_prompt(&base, head, &review.findings, &review.dropped_candidates)?;
        let (report, restored) = ctx.review_pass("judge", prompt, |raw| {
            coordinator::parse_judge(raw, &scope, &review.dropped_candidates)
        });
        passes.push(report);
        match restored {
            Ok(restored) => findings.extend(restored),
            Err(e) => {
                degraded = true;
                eprintln!("warning: {e:#}; the coordinator's findings stand");
            }
        }
    }
    // Terminals close before their worktree goes.
    drop(ctx);
    drop(runner);
    drop(wt);

    let ungated = result::dedupe(findings, args.gate);
    let result = ReviewResult {
        findings: ungated
            .iter()
            .filter(|f| args.gate.keep(f))
            .cloned()
            .collect(),
        ungated_findings: ungated,
        dropped_candidates: review.dropped_candidates,
        overall_correctness: review.overall_correctness,
        overall_explanation: review.overall_explanation,
        review_process: review.review_process,
        base,
        head: head.into(),
        passes,
        degraded,
    };
    fs::write(
        runroot.join("result.json"),
        serde_json::to_string_pretty(&result)?,
    )?;
    eprintln!("run dir: {}", runroot.display());
    print_result(&result, args.json)
}

/// Locks `<runroot>.lock`, beside the run dir, which a rerun removes and recreates: two runs of
/// one PR head would share the dir and mix their progress and results. The OS drops the lock
/// when the process ends, a crash too.
fn lock_head(runroot: &Path) -> Result<fs::File> {
    let mut path = runroot.as_os_str().to_owned();
    path.push(".lock");
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("{}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("{}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => bail!(
            "another review of this PR head is running (it holds {}); wait for it to end",
            path.display()
        ),
        Err(fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

/// `rreview show`: the result in a run dir, or of the latest finished review of a PR.
pub fn show(target: &str, repo: Option<&str>, json: bool) -> Result<u8> {
    let file = if Path::new(target).is_dir() {
        Path::new(target).join("result.json")
    } else {
        latest_result(target, repo)?
    };
    let text = fs::read_to_string(&file).with_context(|| format!("{}", file.display()))?;
    let result: ReviewResult =
        serde_json::from_str(&text).with_context(|| format!("{}", file.display()))?;
    eprintln!("result: {}", file.display());
    print_result(&result, json)
}

fn latest_result(pr: &str, repo: Option<&str>) -> Result<PathBuf> {
    let pr = view_pr(pr, repo, None)?;
    let (owner, name) = owner_and_name(&pr.url)?;
    let dir = state_home()?.join("rreview").join(owner).join(name);
    let prefix = format!("{}-", pr.number);
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&dir).with_context(|| format!("no reviews in {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let file = entry.path().join("result.json");
        if let Ok(time) = fs::metadata(&file).and_then(|m| m.modified())
            && latest.as_ref().is_none_or(|(t, _)| time > *t)
        {
            latest = Some((time, file));
        }
    }
    latest.map(|(_, f)| f).with_context(|| {
        format!(
            "no finished review of PR #{} in {}",
            pr.number,
            dir.display()
        )
    })
}

/// Prints `result.json` (`--json`) or the human summary; returns the exit code.
fn print_result(result: &ReviewResult, json: bool) -> Result<u8> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        print!("{}", result::summary(result));
    }
    Ok(if result.findings.is_empty() { 0 } else { 1 })
}

/// What every pass of one run shares.
struct Ctx<'a, R> {
    runner: &'a R,
    progress: Progress,
    runroot: &'a Path,
    /// The end of the budget; None when it is too far away to represent.
    deadline: Option<Instant>,
    timeout: Duration,
    settings: &'a Settings,
}

impl<R: Runner> Ctx<'_, R> {
    fn left(&self) -> Duration {
        self.deadline.map_or(Duration::MAX, |d| {
            d.saturating_duration_since(Instant::now())
        })
    }

    /// One pass, its progress events and its report. A valid reply is published as
    /// `out/<role>.json`; a failed pass leaves nothing there.
    fn review_pass<T: Serialize>(
        &self,
        role: &str,
        prompt: String,
        parse: impl Fn(&str) -> Result<T>,
    ) -> (PassReport, Result<T>) {
        let agent = self.settings.agents[role];
        let pass = Pass {
            role,
            agent,
            model: self.settings.models.get(role).map(String::as_str),
            effort: self.settings.efforts.get(role).map(String::as_str),
            prompt,
            timeout: self.timeout.min(self.left()),
        };
        let started = Instant::now();
        let out = self.runroot.join("out").join(format!("{role}.json"));
        let result = (|| {
            if pass.timeout.is_zero() {
                bail!("the review's budget is spent");
            }
            self.progress.event(&pass, "started", started, None)?;
            let value = self.turns(&pass, &parse, started)?;
            // out/ is the host's: no reviewer can write there (reviewers write only into their
            // own `scratch/<role>/`).
            fs::write(&out, serde_json::to_string_pretty(&value)?)
                .with_context(|| format!("{}", out.display()))?;
            Ok(value)
        })()
        .with_context(|| format!("pass {role} ({agent})"));
        let error = result.as_ref().err().map(|e| format!("{e:#}"));
        let status = if error.is_some() {
            "failed"
        } else {
            "completed"
        };
        // A lost progress line must not turn the pass's own outcome around.
        if let Err(e) = self
            .progress
            .event(&pass, status, started, error.as_deref())
        {
            eprintln!("warning: {e:#}");
        }
        let report = PassReport {
            role: role.into(),
            agent: agent.to_string(),
            model: pass.model.unwrap_or("default").into(),
            effort: pass.effort.map(String::from),
            status: status.into(),
            duration_ms: started.elapsed().as_millis() as u64,
            error,
        };
        (report, result)
    }

    /// The pass's turns (spec §4): a pass killed at its timeout gets one wrap-up turn; a reply
    /// that does not validate gets one more turn with the reason. Then it has failed.
    fn turns<T>(
        &self,
        pass: &Pass,
        parse: &impl Fn(&str) -> Result<T>,
        started: Instant,
    ) -> Result<T> {
        let raw = match self.runner.run(pass) {
            Err(e) if e.is::<launch::TimedOut>() => {
                let timeout = self.turn_timeout(pass);
                if timeout.is_zero() {
                    bail!("{e:#}; no budget left for a wrap-up");
                }
                self.progress.event(pass, "wrapup", started, None)?;
                self.runner
                    .wrap_up(pass, WRAP_UP, timeout)
                    .with_context(|| format!("{e:#}; wrap-up"))?
            }
            r => r?,
        };
        let e = match parse(&raw) {
            Ok(value) => return Ok(value),
            Err(e) => e,
        };
        let timeout = self.turn_timeout(pass);
        if timeout.is_zero() {
            return Err(e);
        }
        self.progress
            .event(pass, "retry", started, Some(&format!("{e:#}")))?;
        let raw = self
            .runner
            .wrap_up(pass, &format!("{RETRY}\n\nThe reason: {e:#}"), timeout)
            .with_context(|| format!("{e:#}; retry"))?;
        parse(&raw).with_context(|| format!("{e:#}; retry"))
    }

    fn turn_timeout(&self, pass: &Pass) -> Duration {
        WRAP_UP_TIMEOUT.min(pass.timeout).min(self.left())
    }
}

/// The review worktree; removed when dropped (also on an error path) unless kept.
// limit: a killed rreview (SIGKILL, Ctrl-C) leaves it; the next run for the same head says so.
struct Worktree {
    path: PathBuf,
    keep: bool,
    /// Set for an Orca-created worktree: it and its branch are found by this name.
    orca_name: Option<String>,
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.keep {
            eprintln!("review worktree kept: {}", self.path.display());
            return;
        }
        if let Some(name) = &self.orca_name {
            if let Err(e) = orca::remove_worktree(name) {
                eprintln!("warning: cannot remove the review worktree: {e:#}");
            }
            return;
        }
        let removed =
            launch::utf8(&self.path).and_then(|p| git(&["worktree", "remove", "--force", p], None));
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
            "model": pass.model.unwrap_or("default"), "duration_ms": started.elapsed().as_millis() as u64,
        });
        if let Some(error) = error {
            event["error"] = error.into();
        }
        // One write per line: passes run in parallel threads and append to the same file.
        let mut file = OpenOptions::new().create(true).append(true).open(&self.0)?;
        file.write_all(format!("{event}\n").as_bytes())
            .with_context(|| format!("{}", self.0.display()))
    }
}

fn view_pr(pr: &str, repo: Option<&str>, deadline: Option<Instant>) -> Result<Pr> {
    let mut cmd = Command::new("gh");
    cmd.args(["pr", "view", pr, "--json"]).arg(
        "number,title,body,url,headRefName,baseRefName,headRefOid,baseRefOid,state,mergeCommit",
    );
    if let Some(repo) = repo {
        cmd.args(["--repo", repo]);
    }
    let out = output(&mut cmd, deadline).context("cannot run gh")?;
    if !out.status.success() {
        bail!(
            "gh pr view {pr}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).context("unexpected `gh pr view` output")
}

/// `--head` must be one of the PR's commits as GitHub lists them now, so a run filed under PR
/// `<n>` reviews that PR. A head that a force-push removed is no longer listed.
fn pr_commit<'a>(
    sha: &'a str,
    pr: &Pr,
    owner: &str,
    name: &str,
    deadline: Option<Instant>,
) -> Result<&'a str> {
    let path = format!("repos/{owner}/{name}/pulls/{}/commits", pr.number);
    let out = output(
        Command::new("gh").args(["api", "--paginate", "--jq", ".[].sha", &path]),
        deadline,
    )
    .context("cannot run gh")?;
    if !out.status.success() {
        bail!(
            "gh api {path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // limit: GitHub lists at most 250 commits of a PR; a later commit of a longer PR is refused.
    if !String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == sha)
    {
        bail!(
            "--head {sha} is not a commit of PR #{} (a head that a force-push removed from the PR cannot be reviewed as that PR)",
            pr.number
        );
    }
    Ok(sha)
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
fn ensure_commits(pr: &Pr, revs: &[&str], deadline: Option<Instant>) -> Result<()> {
    let missing =
        |rev: &str| git(&["cat-file", "-e", &format!("{rev}^{{commit}}")], deadline).is_err();
    if !revs.iter().any(|r| missing(r)) {
        return Ok(());
    }
    git(
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            repo_url(&pr.url)?,
            &format!("refs/pull/{}/head", pr.number),
            &format!("refs/heads/{}", pr.base_ref_name),
        ],
        deadline,
    )
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

fn version(bin: &str, deadline: Option<Instant>) -> Option<String> {
    let out = output(Command::new(bin).arg("--version"), deadline).ok()?;
    let v = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

/// Git in the current repository, stopped at `deadline`.
fn git(args: &[&str], deadline: Option<Instant>) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_raw(args, deadline)?)
        .trim()
        .into())
}

/// Git's stdout as it is.
fn git_raw(args: &[&str], deadline: Option<Instant>) -> Result<Vec<u8>> {
    let out = output(Command::new("git").args(args), deadline).context("cannot run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Runs `cmd` to its end with no stdin. With a deadline (the review's budget), it runs in its
/// own process group, which is killed at the deadline; past the deadline it does not start.
fn output(cmd: &mut Command, deadline: Option<Instant>) -> Result<Output> {
    cmd.stdin(Stdio::null());
    let Some(deadline) = deadline else {
        return Ok(cmd.output()?);
    };
    let program = cmd.get_program().to_string_lossy().into_owned();
    let spent = || anyhow!("the review's budget ran out while preparing ({program})");
    if Instant::now() >= deadline {
        return Err(spent());
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    // Reader threads, so a full pipe cannot stall the child while we wait on it.
    fn read(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = vec![];
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let (stdout, stderr) = (read(child.stdout.take()), read(child.stderr.take()));
    loop {
        // A descendant can hold the pipes after the leader exits, so the drain is under the
        // deadline too: done only when the leader has exited and both pipes reached EOF.
        if let Some(status) = child.try_wait()?
            && stdout.is_finished()
            && stderr.is_finished()
        {
            return Ok(Output {
                status,
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            });
        }
        if Instant::now() >= deadline {
            launch::kill_group(&mut child);
            return Err(spent());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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
