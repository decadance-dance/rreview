# rreview Orca plugin

A sidebar panel in [Orca](https://github.com/stablyai/orca) that runs an rreview panel review of the
focused workspace's pull request, shows each pass while it runs, and lists the findings by file
(spec §7, `docs/VISION-AND-SPEC.md`).

- **PR**: the focused workspace's (`gh pr view` in its folder), or a PR number or URL typed into the
  panel, reviewed in that workspace's repository.
- **Run review** starts `rreview <pr-url> --runner headless|orca --json` as a child process of the
  plugin's worker. One run at a time.
- **Progress**: the worker learns the run dir from rreview's first `run dir:` line on stderr and
  tails `<runroot>/progress.jsonl`, one row per pass.
- **Result**: from rreview's exit code and its JSON on stdout (`result.json`). Exit 2, a crash or
  output that is not a valid result shows as a failed review with rreview's `error:` lines and the
  run dir; a review where a panelist or the judge failed shows as degraded.
- **Findings**: a file tree (folders, files, then one item per finding with its lines, priority and
  confidence); select one to read its body. Gated findings by default, all with the switch.
- **Send to author** types the picked findings (gated ones start picked) into the chosen agent of
  the focused workspace, through the worker's `terminal.sendText` (capability `terminal:send`). The
  text says the findings are review evidence, not instructions, and loses its control bytes.

## Install for development

Needs Orca 1.4.248 or later, `gh`, and an `rreview` binary (`cargo build --release` in this repo).

```sh
orca plugin dev load orca-plugin     # prints the access it asks for; approve with --approve <fingerprint>
orca plugin open local.rreview --panel rreview
```

Settings (Settings > Plugins > rreview):

- **rreview executable**: `rreview` on the worker's PATH, or an absolute path such as
  `<repo>/target/release/rreview`. The worker's PATH is Orca's, which may lack `~/.cargo/bin`.
- **GitHub account**: a `gh` account whose token gh and rreview get, for a repository gh's active
  account cannot see. The worker fetches it with `gh auth token -u <account>` for each call and
  passes it as `GH_TOKEN` to those two child processes only; it is never stored or logged.

## Limits

- The worker runs with Orca's environment, not a terminal's: `CLAUDE_CONFIG_DIR` and `CODEX_HOME`
  are unset, so the headless runner uses `~/.claude` and `~/.codex`, not an Orca per-account home.
  The orca runner's reviewers run in Orca terminals with Orca's own environment.
- Until [#17](https://github.com/decadance-dance/rreview/issues/17) lands, rreview passes its whole
  environment to headless reviewers, so with the GitHub account setting the token is in the claude
  and codex processes' environment, as it is when you run `GH_TOKEN=... rreview` in a shell. The orca
  runner's reviewers do not get it.
- A worker restart (Orca stops an idle worker; a save in dev mode) loses the child it was watching:
  that run shows as failed, with its run dir. `rreview show <run dir>` prints its result once it
  finishes.
- The worker stops a run that is still going 70 minutes after it started (rreview's default budget
  is 60). That kill skips rreview's cleanup.

## Develop

```sh
orca plugin types orca-plugin   # .orca/types, for tsc
orca plugin test orca-plugin    # src/*.test.ts(x); fake gh and rreview in test/fixtures/bin
orca plugin check orca-plugin   # renders each scenario of src/panel.check.ts
```

`bin/check` at the repo root runs both, and CI installs the Orca CLI from the Linux AppImage for them.

The file tree's folding of single-child folders follows `buildTree()` of
[orca-pr-review](https://github.com/Geromendez135/orca-pr-review) (MIT, Copyright (c) 2026
Geromendez135); see the comment in `src/panel.tsx`.
