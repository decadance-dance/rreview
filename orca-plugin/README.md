# Rreview Orca plugin

A sidebar panel in [Orca](https://github.com/stablyai/orca) that runs an rreview panel review of the
focused workspace's pull request, shows each pass while it runs, then leads with the verdict and
the findings (spec §7, `docs/VISION-AND-SPEC.md`). The PR line, the runner and Run review take two
rows at the top.

- **PR**: the focused workspace's (`gh pr view` in its folder), or a PR number or URL typed under
  **Other PR…** (folded, open on its own when the workspace has no PR, and below **Passes** once a
  run ended), reviewed in that workspace's repository.
- **Run review** (**Run again** once the PR was reviewed) starts `rreview <pr-url> --runner headless|orca --json` as a child process of the
  plugin's worker. One run at a time.
- **Progress**: the worker learns the run id and dir from rreview's first `run id:` and `run dir:`
  lines on stderr and tails `<runroot>/progress.jsonl`, one row per pass: one bounded read at a
  time, and only while the dir's `meta.json` names this run's id (another rreview run for the same
  PR head replaces the dir, possibly with the same path and inode).
  The panel shows how many passes are done, the time since the run started and a progress bar,
  each pass with its agent, a status that reads without color (`✓ done`, `✗ failed`,
  `● running`, `○ waiting`) and its time, and each failed pass with its reason (a long one
  folded). Once the run ends they fold into **Passes** at the bottom, folded by default; its title
  counts the passes done and failed.
- **Cancel** (the stop button beside **Running…**) sends rreview SIGTERM; see Limits.
- **Result**: from rreview's exit code and its JSON on stdout (`result.json`). Exit 2, a crash or
  output that is not a valid result shows as a failed review with rreview's `error:` lines and the
  run dir; a review where a panelist or the judge failed shows as degraded.
- **Findings**: the verdict's first sentence (the rest under **More**) with one line of counts (gated, below the gate, passes done, the
  run's time, degraded), then a list, most urgent first: each finding's full title, and under it
  its priority as text, `P0`-`P3`, with an icon per severity, its file and lines, and the
  confidence. The most urgent finding opens with its detail: its body's first sentence (the rest
  under **More**), the `file:lines` as code, a link to those lines on GitHub at the reviewed head,
  and the send checkbox. Gated findings by default, all with the **below gate** switch; with none above the gate the panel says how many are
  below it. Posts
  and stored state carry only the run's status and counts; the panel fetches the result itself,
  with long texts cut and findings past about 200 KB left out (it says how many), so a large
  review stays within Orca's 256 KiB post and storage limits. The full text stays in `result.json`.
- **Send to author**: you choose the recipient among the agents of the reviewed run's workspace
  (never preselected). The worker writes the picked findings (gated ones start picked), marked as
  review evidence, to `<runroot>/author-<time>.md`, checks that the chosen terminal still runs the
  same agent session in that workspace, and types one fixed line naming that file through
  `terminal.sendText` (capability `terminal:send`). Reviewer text is never typed, so it cannot reach
  a shell even if the agent has just exited; the line holds only characters a shell reads as plain
  words. The run's status then says what was sent to which agent, and keeps saying it for that
  run across panel reloads, under the Send row.

## Install for development

Needs Orca 1.4.248 or later and an `rreview` binary (`cargo build --release` in this repo), with
rreview's [prerequisites](../README.md#-quick-start).

```sh
orca plugin dev load orca-plugin     # prints the access it asks for; approve with --approve <fingerprint>
orca plugin open local.rreview --panel rreview
```

Settings (Settings > Plugins > Rreview):

- **rreview executable**: `rreview` on the worker's PATH, or an absolute path such as
  `<repo>/target/release/rreview`. The worker's PATH is Orca's, which may lack `~/.cargo/bin`.
- **GitHub account**: a `gh` account whose token gh and rreview get, for a repository gh's active
  account cannot see. The worker fetches it with `gh auth token -u <account>` for each call and
  passes it as `GH_TOKEN` to those two child processes only; it is never stored or logged.
  Reviewers never get it: rreview gives them only an allowlist of variables, with no `GH_TOKEN`,
  in both runners (spec §3, [#17](https://github.com/decadance-dance/rreview/issues/17)).

## Limits

- Reviewers use the Claude config and Codex login of the rreview process, which is the worker's,
  with either runner: `$CLAUDE_CONFIG_DIR` and `$CODEX_HOME` when set, else `~/.claude` and
  `~/.codex`. The worker runs with Orca's environment, not a terminal's, where those two are
  normally unset, so a login made only in an Orca per-account terminal home does not reach the
  reviewers, even when the orca runner opens their terminals.
- A worker restart (Orca stops an idle worker; a save in dev mode) loses the child it was watching:
  that run shows as failed, with its run dir. `rreview show <run dir>` prints its result once it
  finishes.
- Closing the panel does not stop a run; **Cancel** does. It sends rreview SIGTERM, on which rreview
  stops its reviewers, removes its review worktree (with the orca runner also its branch and the
  reviewers' terminals) and exits 2; the run shows as cancelled once rreview reports that stop (a
  run that failed on its own meanwhile shows its error). Cancel names the run the panel shows by a
  token unique to it, so a late one never stops a later run. The worker sends the same SIGTERM to a run still going 70
  minutes after it started (rreview's default budget is 60), which then shows as failed, unless a
  Cancel came first. An rreview still running 10 minutes after a SIGTERM is killed, and that
  kill skips what is left of its cleanup.

## Develop

```sh
orca plugin types orca-plugin   # .orca/types, for tsc
orca plugin test orca-plugin    # src/*.test.ts(x); fake gh and rreview in test/fixtures/bin
orca plugin check orca-plugin   # renders each scenario of src/panel.check.ts
```

`docs/screenshots/orca-plugin-*.png` are the `pr1*` scenarios of `src/panel.check.ts`, which replay
the real review of this repo's PR #1 (`src/screens.ts`), drawn by Orca with Orca's window
minimized, without loading the plugin:

```sh
orca plugin shot orca-plugin --scenario pr1Running --theme dark --width 320 --out docs/screenshots/orca-plugin-running.png
```

`bin/check` at the repo root runs both, and CI installs the Orca CLI from the Linux AppImage for them.
