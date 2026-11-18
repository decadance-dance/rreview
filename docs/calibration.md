# Calibration: rreview against existing cross-model reviews

Status: first calibration, 2026-11-18, issue #7 (spec §8 milestone 6). Claude Code 2.1.316, codex-cli 0.167.0, default models and efforts, `--author claude`, headless runner.

## Verdict
- **Precision is high, recall is low.** Of the 20 findings rreview returned on 5 PRs, 19 are real defects (one false positive, restored by the judge). But it matched only 2 of the 15 must-fix defects the cross-model review found at the same heads, and both only below the gate.
- **The old gate (`p<=2,conf>=0.8`) let through nothing real.** The coordinator gave every P0-P2 finding a confidence of 0.72 or less, including defects it had confirmed. So the only finding to clear the bar was the judge's false positive.
- **Gate change, made in this PR:** the default becomes `p<=2,conf>=0.5`. On this set it passes 3 real findings and the same false positive, including one must-fix defect. `--all` stays opt-in: it would fail every run on P3s (20 findings on 5 of 5 PRs).
- **The real fix lies outside the gate:** the panel needs a procedure for the defect classes it missed (#13), and the coordinator needs honest confidence (#14). Until then, read `ungated_findings` too (#16).
- **Cost:** 95-326 s wall time per run, 20.5 min for all five, no degraded run.

## Dataset
Each PR's first cross-model review (Codex reviewing Claude's work, Comment mode) is the ground truth: its must-fix findings, each with a repro. Later rounds found defects only in the fixes, code that did not exist at the first head, so they add nothing to the ground truth.

| PR | Reviewed head | Size at that head | Ground truth (must-fix + suggestions) | Why |
|---|---|---|---|---|
| orca-plane #31 | `986b825` | 4 files, +26 -9 | 1 + 0 | small; one real UI defect against the issue's criterion |
| rreview #8 | `67e44ba` | 15 files, +1339 -2 | 2 + 1 | doctor self-test: false-pass checks |
| rreview #9 | `1664fd9` | 14 files, +1626 -20 | 4 + 2 | runner: strict decoding, cleanup, boundaries |
| rreview #10 | `f3ec99e` | 12 files, +2360 -212 | 5 + 3 | full pipeline: validation, retry, budget |
| orca-plane #22 | `9aa5fde` | 40 files, +4988 -7 | 3 + 0 | large TypeScript plugin; async races |

## Method
- **Head:** `rreview <pr> --repo <owner/name> --head <sha>` (new in this PR). `--head` reviews the commit the first review looked at, not the merged head.
- **No look-ahead:** each run used a fresh clone in a scratch dir. The clone held only two commits and their history: the reviewed head and the base branch before the merge (`mergeCommit^1`). So `git log --all` could not show the later fix commits.
- **PR text as the reviewer saw it:** the PR bodies of rreview #8 and orca-plane #22 were edited after the first review to describe the fixes. A `gh` wrapper therefore served the body version that was current at the first review, taken from GitHub's edit history. The other three bodies were last edited before their first review.
- **Task:** each PR's issue body, passed with `--context-file`, as the first reviewer had it.
- **Runs:** one per PR, one after another (none overlapped). Run dirs and `result.json` stayed outside the repo. The `gh` wrapper also kept the token out of rreview's environment (see #17).
- **Matching:**
  - *Matched*: same root cause and same failure.
  - *Partial*: same root cause, different symptom.
  - *Missed*: no candidate at all; dropped candidates count too.
  - Every finding outside the ground truth got a verdict from reading the code at that head, with a repro where it was cheap.

## Results per run
Candidates are listed per lens: intent / reliability / security. "Kept" are the coordinator's findings. Under the old gate `p<=2,conf>=0.8`, "gated" is the number of findings that cleared it, and "exit" is the exit code.

| PR | Wall | Passes (s) | Candidates | Kept / dropped / restored | Gated, exit | Must-fix matched | New findings (real / FP) |
|---|---|---|---|---|---|---|---|
| orca-plane #31 | 95 s | intent 47, rel 51, sec 31, coord 40; no judge (nothing dropped) | 1 / 1 / 0 | 2 / 0 / 0 | 0, 0 | 1 of 1, ungated (P3, 0.55) | 1 / 0 |
| rreview #8 | 256 s | 54, 176, 57, 66, judge 10 | 1 / 6 / 1 | 3 / 5 / 0 | 0, 0 | 0 of 2 (suggestion 1 of 1) | 2 / 0 |
| rreview #9 | 265 s | 102 (one retry: malformed JSON), 187, 76, 62, judge 11 | 2 / 4 / 1 | 4 / 2 / 1 | 1, 1 | 0 of 4 (suggestions 0 of 2) | 4 / 1 |
| rreview #10 | 326 s | 109, 212, 89, 97, judge 13 | 5 / 4 / 1 | 3 / 6 / 0 | 0, 0 | 0 of 5, 1 partial (suggestions 1 of 3) | 2 / 0 |
| orca-plane #22 | 290 s | 74, 196, 69, 78, judge 12 | 1 / 7 / 0 | 7 / 1 / 0 | 0, 0 | 1 of 3, ungated (P2, 0.55) | 6 / 0 |
| **Total** | 1232 s | 24 passes, 24 completed, 1 retry, 0 wrap-ups | 35 | 19 / 14 / 1 | 1 | **2 of 15**, 1 partial (suggestions 2 of 6) | **15 / 1** |

No run was degraded. The Reliability lens (Claude) is the slowest pass every time, and the Codex lenses come back in about a minute. The judge took 10-13 s.

### orca-plane #31 at `986b825`
- **Matched:** *P2: the fallback can still show only headings.* rreview reported it as "Make sure leading headings can't use up the whole fallback preview" (intent lens, P3, 0.87). The coordinator kept it at P3, 0.55, after writing "The behavior is real". The issue's own acceptance criterion says a preview never ends on a heading.
- **New, real:** "Keep the word-boundary cut working after a line break" (P3, 0.7). The new `[ \t]+\S*$` no longer trims a word cut right after a `<br>` (`"  \n"`); the old `\s+\S*$` did. A Node repro of both regexes confirms it. Cosmetic.

### rreview #8 at `67e44ba`
- **Missed, must-fix:**
  - The Codex write check passes when the reply merely contains `write=` (`src/doctor.rs`, `reply.contains("write=")`).
  - The Claude probe report is not checked for contradictions, and a nonzero exit is not checked either.
- **Matched suggestion:** add a positive control for the planted-hook check (P3, 0.55).
- **New, real (both P3):**
  - `Launch::run`'s timeout and kill path had no test (one came in #9).
  - `Fixture::new` leaves its temp dir behind when a git step fails: `?` returns before any cleanup.
- **Drops, all sound:**
  - A stale Codex home: no caller reuses one.
  - The `auth.json` token-refresh question: an open item in spec §9.
  - A spec wording nit.
  - Toolchain pinning: a deterministic CI matter.
  - Network access through other shell clients: a documented non-goal and open item.

### rreview #9 at `1664fd9`
- **Missed, must-fix:**
  - A failed checkout leaves the worktree registered.
  - Positional candidate arrays pass strict decoding.
  - A file name with a leading space is rejected.
  - The largest `--timeout` panics.
- **Missed suggestions:**
  - Publishing through an `out/` symlink.
  - Weak resume assertions.
- **New, real:**
  - An interrupted rreview leaves the reviewer running (P2, 0.72). The code already notes this as a `limit:` comment, and no issue tracked it until #18.
  - The envelope slice from the first `{` to the last `}` breaks on braces in prose (P3). This is the root cause that #10's review later hit.
  - A failed progress write hides the pass error (P3).
  - Spec §6 and §3 disagree on "`~/.claude` is never touched" (P3).
- **False positive, gated:** "Restrict permissions on review run directories" (judge, P2, 0.82). The coordinator had dropped it with the right evidence: the worktree is a copy of the user's own clone under the same umask, so it exposes nothing new. The judge restored it without answering that (#15). It was the only gated finding of the whole set.

### rreview #10 at `f3ec99e`
- **Missed, must-fix:**
  - Candidates are accounted for by count, not identity (P1).
  - Judge restorations are not linked to drops.
  - A Codex turn that exits 0 without a reply is not retried.
  - Preparation runs outside the budget.
- **Partial:** *the whole reply is not validated against its shape.* The intent lens found the root cause, the `{`..`}` delimiter scan in `coordinator.rs` and `panelist.rs`, but described the false-reject symptom instead of the false accept. The coordinator dropped both candidates as speculative, and the judge left them dropped.
- **Missed suggestions:**
  - A duplicate that clears the gate can lose to one that does not.
  - The judge prompt substitutes template text inside evidence.
- **Matched suggestion:** *published artifacts sit in reviewer-writable space*, reported as "Keep one pass's reviewer from breaking another pass's output" (P2, 0.6).
- **New, real (both P3):**
  - A directory planted at `out/<role>.json` survives a failed pass. Same root cause as the matched suggestion.
  - The interrupt issue again.
- **Drops, sound:**
  - A concurrent run on the same head: a documented limit.
  - Exit 0 with every panelist failed: by design, since `degraded` reports it.
  - The shared Codex home: intended, per spec §3.

### orca-plane #22 at `9aa5fde`
- **Missed, must-fix:**
  - A late secret read restores a disconnected connection. rreview flagged a different defect in the same function: a transient `secrets.get` failure cached as disconnected.
  - A queued Start completion writes into the new account after a switch.
- **Matched:** *a truncated workspace list permits a duplicate Start*, reported as "Don't unlink Start states when workspace.list is truncated" (P2, 0.55).
- **New, real (each checked in the code at that head):**
  - The `defaultAgent` enum lists 5 of the 9 agents that design D8 and `KNOWN_AGENTS` support (P3, 0.85).
  - The `connection.changed` handler never loads data on connect (P3).
  - An `unknown` Start stays a disabled "Checking…" until reload, because `starts.get` runs only from `loadConnected` (P3).
  - `void this.create(...)` has no catch, so a storage error leaves the item `pending` (P3).
  - `load()` caches a thrown `secrets.get` as disconnected (P3).
  - A throw in `dropStorage` or `notify` after `switchTo` leaves connect half-applied (P3).
- **Drop, sound:** the grace period for `unknown`. The spec says that state goes back to idle.

## Gate
These numbers come from the 20 findings in `ungated_findings` of the five `result.json` files. "Must-fix" is a ground-truth must-fix defect, and "FP" is the judge's false positive.

| Gate | Gated | Real | FP | Must-fix | PRs that exit 1 (all 5 had must-fix defects) |
|---|---|---|---|---|---|
| `p<=2,conf>=0.8` (old default) | 1 | 0 | 1 | 0 | 1 (#9, on the FP) |
| `p<=2,conf>=0.7` | 2 | 1 | 1 | 0 | 1 |
| `p<=2,conf>=0.6` | 3 | 2 | 1 | 0 | 2 |
| **`p<=2,conf>=0.5` (new default)** | 4 | 3 | 1 | 1 | 3 |
| `p<=3,conf>=0.8` | 2 | 1 | 1 | 0 | 2 |
| `--all` | 20 | 19 | 1 | 2 | 5 |

- **Confidences:** from 0.5 to 0.85. Only 2 findings reach 0.8: the judge's false positive and a P3. Every P0-P2 finding from the coordinator scored 0.72 or less, and both matched must-fix defects scored 0.55.
- **Conclusion:** on this data, confidence does not separate real findings from false ones (19 of 20 are real), and priority does only weakly. A bar of 0.5 equals the coordinator's own floor (it drops anything below 0.5), so the default gate now filters on priority alone.
- **Next step:** once #14 makes confidence mean "this defect exists", rerun this set and consider raising the bar again.
- **Why `--all` stays off by default:**
  - It turns the exit code into "the change has a P3 somewhere", which held for all 5 runs.
  - The 15 new real findings are mostly test gaps and edge cases, worth reading but not blocking.
  - #16 makes them visible in the summary without gating on them.

## Follow-ups
- #13: the panel misses defects in validators, boundary inputs and failure paths (13 of 15 must-fix missed).
- #14: the coordinator scores confirmed defects with low confidence and priority.
- #15: the judge restores drops without rebutting the coordinator's evidence (the only gated finding was a false positive).
- #16: the summary hides findings below the gate.
- #17: reviewers inherit rreview's environment, including tokens (found while setting up the runs).
- #18: an interrupted rreview leaves its reviewers running (reported by rreview on #9 and #10).

## Limits of this calibration
- Five PRs, one run each, from two repos by the same author and reviewer pair. The counts are a direction, not a rate.
- The first reviewer ran repros for minutes per finding; rreview's lenses read for 30 s to 3.5 minutes. The comparison measures what rreview finds by default, not what it could find with more time or effort (`--role-effort` was left at the CLI defaults).
- Cost in tokens is not recorded: both CLIs ran on subscriptions.

## Reproduce
For each PR:
1. Make an empty clone holding only the two commits: `git init`, then `git fetch origin <head>:refs/heads/reviewed <mergeCommit^1>:refs/heads/base`.
2. From inside that clone, run `rreview <pr> --repo <owner/name> --head <head> --context-file <issue body>`. Set `XDG_STATE_HOME` to a scratch dir.
3. Optionally put a `gh` wrapper first on `PATH` that replaces `.body` in `gh pr view` output with the body version from the PR's edit history (GraphQL `userContentEdits`).

`jq` over `ungated_findings[]` with a `priority`/`confidence_score` filter reproduces the gate table.
