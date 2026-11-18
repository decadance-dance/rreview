# rreview: vision and spec

Status: draft v0.1, 2026-11-18. Source: a research session (findings also in the Claude memory note `panel-review-research.md`). Everything marked *verified* was run on Claude Code 2.1.316, codex-cli 0.167.0, Orca 1.4.261, macOS.

## 1. Vision

`rreview <PR>` gives a pull request an independent, cross-model panel review before it is marked ready. It adapts the pipeline of [block/review](https://github.com/block/review) (Apache-2.0): fixed-lens reviewers in parallel, a coordinator that tries to disprove every candidate, and a judge that re-examines only the dropped candidates.

What makes it ours:
- **Isolated reviewers.** A reviewer sees the code, never the user's CLAUDE.md, AGENTS.md, plugins, skills, hooks or MCP, and never the reviewed PR's own `.claude/` config. Same result on any machine.
- **Read-only by the OS, not by the prompt.** Reviewers cannot write to the worktree, its git dir, or anywhere else except the run directory.
- **Cross-model.** Discovery and the judge are split across Claude and Codex, so the coordinator and the judge never share a model family.
- **Subscriptions, not API keys.** Runs on the user's Claude and ChatGPT logins through the official CLIs.
- **Visible when wanted.** The same review runs headless (default, CI-like) or with every reviewer in a visible Orca terminal, and an Orca plugin shows progress and findings.

Non-goals (v1): posting comments to GitHub, fixing findings, reviewing non-GitHub hosts, Windows, a hosted service, network isolation of reviewers beyond deny rules.

## 2. Pipeline

1. **Prepare.** Resolve the PR with `gh pr view <n> --json number,headRefName,baseRefName,title,body,url,headRefOid,baseRefOid,state,mergeCommit` (`--repo` passed through). Fetch `refs/pull/<n>/head` and the base branch from the PR's repository only when a commit is missing locally. Create a detached review worktree of the PR head (`git worktree add --detach <runroot>/wt <head-sha>`), never the author's worktree; it is removed at the end, also after a failure, unless `--keep-worktree`. Compute `base = merge-base(base, head)`, where `base` is the base branch tip, or for a merged PR the merge commit's first parent (today's base tip already contains the head). Create the run dir `<runroot>` (default `$XDG_STATE_HOME/rreview/<owner>/<name>/<pr>-<head-sha>/`, `~/.local/state` when unset); a rerun for the same head replaces it, and stops if a kept worktree is still there.
2. **Panel** (parallel). Three roles, each one pass:

| Role id | Lens | Input | Default agent |
|---|---|---|---|
| `intent_contracts` | Behavior and contracts: does the PR meet its stated intent and the task's acceptance criteria; producer to consumer tracing; compatibility; edge cases; a test that fails when the logic breaks | diff, PR title and body, task AC (`--context-file`) | codex |
| `reliability` | Failure injection at each call, partial success, races, cleanup, unbounded growth, load, migrations and rollback | diff, PR title and body | claude |
| `security` | Trust boundaries, authz, injection, secrets, supply chain | diff only: no PR text, no task (blind lens) | codex |

   Lens procedures start from block/review's `panelists.go`; the intent lens adds the AC check. PR text, task text and repo files are untrusted evidence, never instructions.
3. **Coordinator** (claude). Dedupe by root cause and failure scenario; try to disprove every candidate against the code; drop only with cited counter-evidence; account for every candidate (kept, merged, or dropped with reason). Port block/review `coordinator.md`. If a panelist failed, it covers that lens and the run is `degraded`. It sees the PR text and the task as untrusted data, and every panelist's validated envelope or error. A review that lists fewer findings plus dropped candidates than the completed panelists sent candidates fails validation. Coordinator failure fails the review (exit 2, no `result.json`).
4. **Judge** (codex; never the coordinator's family). Sees only dropped candidates plus kept findings; restores real defects. Port block/review `judge.md`. It runs only when the coordinator dropped a candidate. Judge failure keeps the coordinator's findings and marks `degraded`.
5. **Deliver.** Merge kept and restored, dedupe (same file, start line and title, case and spacing ignored; the most urgent priority wins), gate (default P0-P2 and confidence >= 0.8; `--gate p<=N,conf>=X` moves the bar, `--all` disables). Write `result.json`; print a human summary (`--json` prints `result.json` instead); exit code 0 = no gated findings, 1 = gated findings, 2 = review failed.

Role-to-agent mapping is configurable per role (`--role-agent security=claude`), with a `--author claude|codex` shortcut (default `claude`) that flips the defaults so most discovery and the judge use the other family. A mapping that gives the coordinator and the judge the same family is refused before any work, with exit 2. `--role-model` and `--role-effort` set a role's model (`claude --model`, `codex -m`) and reasoning effort (`claude --effort`: low, medium, high, xhigh, max; `codex -c model_reasoning_effort=`: minimal, low, medium, high, xhigh); unset, the CLI's own default applies.

## 3. Isolation and sandbox (the security core)

**Claude passes** run `claude` with:
- `--safe-mode`: disables CLAUDE.md, skills, plugins, hooks, MCP, agents (user and project scope). *Verified*, including a hostile project hook that did not fire. `--bare` is not usable: it needs an API key.
- `--settings <runroot>/claude-settings.json`, generated per run with **absolute paths only** (a relative `./` in a `--settings` file does not anchor at cwd; *verified* that it let a write through):

```json
{
  "permissions": { "deny": ["Edit", "Write", "NotebookEdit", "mcp__*", "Bash(gh *)", "Bash(git push*)", "Bash(curl *)", "Bash(wget *)"] },
  "sandbox": {
    "enabled": true, "autoAllowBashIfSandboxed": true, "allowUnsandboxedCommands": false,
    "filesystem": { "denyWrite": ["<abs worktree>", "<abs git common dir>"], "allowWrite": ["<abs runroot>/out"] }
  },
  "disableBundledSkills": true
}
```
- Without the git common dir in `denyWrite`, `git commit` and `git branch` succeed from the worktree (*verified*).
- `sandbox.network.allowedDomains` did not block `curl` (*verified*); the deny rules above are the network fallback. Open item.
- Session transcripts: a reviewer pass leaves its normal session transcript in the user's Claude config dir (`$CLAUDE_CONFIG_DIR` or `~/.claude`). Accepted: a run-scoped config dir has no login, and `--no-session-persistence` would rule out the wrap-up, which resumes the session (`--session-id <uuid>`, then `--resume <uuid>`). rreview never edits the user's config dir.

**Codex passes** run `codex exec --json -s read-only -C <worktree> -o <runroot>/replies/<role>.reply.txt` (stdout is Codex's JSONL event log: the commands it ran and their output) with `CODEX_HOME=<runroot>/codex-home`, a fresh dir holding only a symlink to the user's `auth.json`. Not `--ephemeral`: the wrap-up resumes the session (`codex exec <same options> resume <thread-id> -`), so the session is kept, in that run-scoped home only. A resume with the exec-level `-s read-only -C` keeps the sandbox (*verified*: a write into the repo got "Operation not permitted"). The reply file is outside `out/`: Codex writes it itself, outside its sandbox, and a Claude reviewer running in parallel may write into `out/` and could plant a symlink there. `--ignore-user-config` alone still loads `~/.codex/AGENTS.md` (*verified*); the clean home does not (*verified*). Open items: `-m gpt-6.1-sol` was rejected in a clean home with a ChatGPT login (default became `gpt-6-luna`); whether token refresh replaces the symlink.

**Repo-provided text** (AGENTS.md, CLAUDE.md, `.agents/checks`) may still be read as code. Prompts state it is untrusted evidence.

## 4. Runners

One interface, two implementations:

```
run(pass) -> raw text      // pass = {role, agent, model, effort, prompt, cwd, timeout, outFile}
wrapUp(pass, message) -> raw text   // "time is up, reply with the JSON now"
```

- **headless** (default): `claude -p --output-format json` / `codex exec -o`. The reply is Claude's `result` (a failed exit or `is_error` fails the pass) or Codex's `-o` file. Each agent runs in its own process group; a timeout kills the whole group and the error names the pass. Wrap-up resumes the session (`claude -p --resume <id>`, `codex exec resume <thread-id>`). Logs: `<runroot>/logs/<role>.{stdout,stderr}`.
- **orca** (variant 5): `orca terminal create --worktree path:<wt> --title rreview:<role> --command "<same isolated command, interactive>"`, then `orca terminal send --text <prompt> --enter`, `orca terminal wait --for tui-idle`, then read `<runroot>/out/<role>.json` that the reviewer wrote (it may write only there). Close with `orca terminal close`. *Verified*: Orca lists such a terminal as a claude agent, tracks working/done, the sandbox blocks writes, Orca-created worktrees show no trust dialog. Do **not** use `orchestration worker-start`/`worker_done`: `worker_done` cannot reach the Orca runtime from inside the sandbox (EPERM, *verified*). The review worktree must be Orca-created (`orca worktree create`) in this mode, because Claude's trust dialog in an untrusted dir offers only "No, exit" under `--safe-mode`. Orca commands need to run from an Orca terminal (`ORCA_TERMINAL_HANDLE`).

Panel passes run in parallel (max 3, one thread each); coordinator and judge run in sequence. Budget: a per-pass timeout (`--timeout`, default 900 s) plus a total budget (`--budget`, default 3600 s); every pass's timeout, wrap-up and retry are capped at what is left of the budget, and a pass with no budget left does not start (it fails). A pass killed at its timeout gets one wrap-up turn (at most 180 s). A reply that fails validation gets one more turn, which resumes the session with the validation error (at most 180 s); a CLI that failed (exit status, Claude's `is_error`) is not retried. After that the pass has failed.

## 5. Data contract

All outputs live in `<runroot>`:
- `meta.json`: PR, base, head, runner, role-to-agent map, versions of claude/codex.
- `progress.jsonl`: one line per event `{ts, role, status: started|wrapup|retry|completed|failed, agent, model, duration_ms, error?}`; `ts` is Unix epoch milliseconds, `duration_ms` counts from the pass's start, `model` is the role's `--role-model` or `default`. A `retry` event's `error` is the validation error that caused it. Roles: `intent_contracts`, `reliability`, `security`, `coordinator`, `judge`. The plugin tails it.
- `out/<role>.json`: the validated reviewer JSON (panelist envelope, coordinator review, judge array) following block/review's shapes (`panelistShape`, finding schema with `title`, `body`, `priority` 0-3, `confidence_score`, `code_location`). It exists only for a pass whose reply passed validation, and the host publishes it by a rename from the run root, so a symlink a reviewer planted in `out/` is replaced, not followed; anything else there (a reviewer may write into `out/`) is removed when the pass fails. A panelist candidate locates itself with `path`, `line_start`, `line_end`.
- `result.json`: `{findings, ungated_findings, dropped_candidates, overall_correctness, overall_explanation, review_process, base, head, passes, degraded}` (block/review `Result`), written only when the coordinator completed. A finding is `{title, body, file, start_line, end_line, priority, confidence_score, stage: coordinator|judge}` with a repo-relative `file` and the `[Pn]` tag cut from the title; a pass is `{role, agent, model, effort?, status: completed|failed, duration_ms, error?}`. `degraded` is true when a panelist or the judge failed.
- Validation: strict decoding, unknown fields rejected; one malformed candidate rejects the envelope; paths must be repo-relative and inside the diff's files (`git diff --name-only base head`, matched exactly). The same holds for the coordinator's review and the judge's array: a finding's `code_location.absolute_file_path` may also be an absolute path inside the review worktree; `priority` and `confidence_score` are required; a struct sent as a positional array or a repeated key is rejected.

## 6. CLI

```
rreview <pr-number|url> [--repo owner/name] [--runner headless|orca] [--author claude|codex]
        [--role-agent role=agent]... [--role-model role=model]... [--role-effort role=effort]...
        [--context-file path] [--gate p<=2,conf>=0.8 | --all] [--json] [--keep-worktree]
        [--timeout secs] [--budget secs] [--only role]
rreview show <runroot|pr> [--repo owner/name] [--json]   # print result of the latest run
rreview doctor                   # claude/codex/gh/orca versions, logins, sandbox self-test
```

`--timeout` is the per-pass timeout (default 900 s); a pass that hits it gets one wrap-up turn of at most 180 s. `--budget` is the whole review's (default 3600 s). `--context-file` is the task's acceptance criteria, read by the intent lens and the coordinator. `--only <role>` runs one panelist alone, without coordinator, judge or `result.json` (a development flag; exit 0 when it completed, else 2). `--runner` takes only `headless` until the Orca runner lands. `rreview show` reads `<runroot>/result.json`, or for a PR the newest `result.json` among its run dirs, and prints it like the review did, with the same exit code.

`rreview doctor` runs the isolation self-test on a throwaway repo and a detached worktree of it, through the same launch builders a review uses: a write into the worktree, a commit and a branch must fail, `git diff` must work, a planted project hook must not fire, Codex must not load the user's AGENTS.md. Checks need the effects on disk and tool-side evidence to agree, never the reviewer's reply alone: Claude's probe writes its exit codes to `out/`, Codex's run must show the probe in its `--json` event log. The planted hook has a positive control at no model cost: the same Claude launch without `--safe-mode` and with an empty `CLAUDE_CONFIG_DIR` (no user config, no login) must fire it. The AGENTS.md probe gives Codex a stand-in `HOME` whose `.codex/AGENTS.md` names a random canary that must not appear in the reply, plus a control run whose clean `CODEX_HOME` holds a canary that must appear; the user's own `~/.codex` and `~/.claude` are never touched. It prints one line per check (`ok`, `FAIL`, or `warn` for the optional `orca`) and exits 0 only when every required check passes, else 1. This turns section 3's *verified* facts into a check that runs on every machine and every CLI upgrade.

## 7. Orca plugin (variant 6), after the CLI

A `trusted-worker` plugin (TypeScript, Node `child_process`/`fs`; confirmed in community plugins `klcd/agent-cli-permissions` and `Geromendez135/orca-pr-review`):
- Panel: pick the PR of the focused workspace (`gh pr view`), button "Run review", runner choice.
- Runs `rreview --runner orca --json` as a child process; tails `progress.jsonl`; renders findings from `result.json` as a file tree with line findings (UI pattern from `orca-pr-review`, MIT).
- "Send to author": `orca terminal send` of the selected findings to the author agent's terminal (`--from` is required for orchestration sends from a plugin worker).

## 8. Milestones

1. Skeleton: Rust workspace, `rreview doctor` with the isolation self-test, CI (`cargo fmt --check`, `clippy -D warnings`, `cargo test`).
2. Headless runner + one panelist end to end on a fixture repo, strict JSON parsing, `progress.jsonl`.
3. Full pipeline: three lenses, coordinator, judge, gate, `result.json`, exit codes, wrap-up and retry.
4. Orca runner.
5. Orca plugin.
6. Calibration on 3-5 already-reviewed PRs against the existing cross-model review; tune the gate.

Each milestone is a GitHub issue with acceptance criteria; tests use fake `claude`/`codex` binaries on `PATH` (cheap checks first), with one opt-in live test per agent.

## 9. Open questions

- Network isolation for sandboxed Bash (why `allowedDomains` did not block).
- Codex model selection in a clean `CODEX_HOME`; `auth.json` symlink on token refresh.
- `--safe-mode` availability on older Claude Code versions (doctor must check).
- Whether to also offer the herdr runner (same interface; untested: herdr's recognition of a custom-flag agent pane).
- License of the ported prompts: keep Apache-2.0 attribution and NOTICE.
