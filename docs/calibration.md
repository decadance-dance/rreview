# Calibration: rreview against existing cross-model reviews

Status: first calibration, 2026-11-18, issue #7 (spec §8 milestone 6). Claude Code 2.1.316, codex-cli 0.167.0, default models and efforts, `--author claude`, headless runner.

## Verdict
- **Recall is low.** rreview matched 3 of the 15 must-fix defects that the cross-model review found at the same heads. A lens found a fourth, but the coordinator dropped it.
- **What it reports is real.** Of the 11 findings it returned, 10 are real defects. The eleventh is a risk the spec accepts for v1.
- **The old gate (`p<=2,conf>=0.8`) let nothing through.** No finding reached 0.8: the maximum was 0.75, and the matched must-fix defects scored 0.5-0.6.
- **Gate change, made in this PR:** the default becomes `p<=2,conf>=0.5`. On this set it passes 4 findings:
  - one must-fix defect;
  - two other real defects;
  - the accepted risk.
- **0.5 is provisional.** Five PRs with one run each say that 0.8 is too high for today's coordinator. They do not show that 0.5 is the right bar for other PRs. Revisit it after #14.
- **The fix lies outside the gate.**
  - The panel misses whole defect classes (#13).
  - The coordinator scores confirmed defects low, and drops real ones by deferring to the change's own design (#14).
  - The judge restored none of 5 wrongly dropped real candidates (#15).
- **Cost:** 123-290 s wall time per run, 20 min for all five. No run degraded, retried or needed a wrap-up.

## Dataset
Each PR's first cross-model review (Codex reviewing Claude's work, Comment mode) is the ground truth: its must-fix findings and suggestions, each with a repro. Later rounds found defects only in the fixes, code that did not exist at the first head, so they add nothing to the ground truth.

| PR | Reviewed head | Size at that head | Ground truth (must-fix + suggestions) | Why |
|---|---|---|---|---|
| orca-plane #31 | `986b825` | 4 files, +26 -9 | 1 + 0 | small; one real UI defect against the issue's criterion |
| rreview #8 | `67e44ba` | 15 files, +1339 -2 | 2 + 1 | doctor self-test: false-pass checks |
| rreview #9 | `1664fd9` | 14 files, +1626 -20 | 4 + 2 | runner: strict decoding, cleanup, boundaries |
| rreview #10 | `f3ec99e` | 12 files, +2360 -212 | 5 + 3 | full pipeline: validation, retry, budget |
| orca-plane #22 | `9aa5fde` | 40 files, +4988 -7 | 3 + 0 | large TypeScript plugin; async races |

## Method
The setup is built so that no reviewer could reach a commit later than the reviewed head. The checks after each run prove it.

**The clone.** Each PR got a fresh clone holding exactly two commits and their history:
- the reviewed head;
- the pre-merge base, `mergeCommit^1`, read from the GitHub API rather than fetched.

The setup fetched only those two commits by SHA, so its own `FETCH_HEAD` names only them.

**The `gh` wrapper** (first on `PATH`) answers `gh pr view` as the PR looked at its first review:
- `body` is that time's version, from GitHub's edit history. The bodies of rreview #8 and orca-plane #22 were rewritten later to describe the fixes.
- `state` is `OPEN`, `mergeCommit` is null, and `baseRefOid` is the pre-merge base. rreview then resolves the base from a commit the clone already has. For a merged PR it would otherwise need the merge commit, and fetching that brings every later commit.
- The wrapper also sets the token for `gh` only, so it never enters rreview's environment (#17).

**The run:**

```
rreview <n> --repo <owner/name> --head <sha> --context-file <issue body>
```

- `--head` (new in this PR) reviews the reviewed head. It is checked against the PR's commits as `gh api` lists them.
- `--context-file` passes the PR's issue body, the task the first reviewer had.
- `GIT_ALLOW_PROTOCOL=file` is set, so any network fetch by rreview fails the run instead of quietly bringing in later commits.
- Runs went one after another; none overlapped.
- Run dirs and `result.json` files stayed outside the repo, under `XDG_STATE_HOME` in a scratch dir.

**The proof, after each run:**
- The object count of the clone was unchanged: 466, 49, 107, 173 and 401 objects.
- None of the PR's commits after the reviewed head was present (2, 3, 2, 5 and 3 commits per PR, checked with `git cat-file -e`).

**Matching:**
- *Matched:* same root cause and failure in a reported finding.
- *Found, dropped:* a lens reported it, the coordinator dropped it, and the judge did not restore it.
- *Missed:* no candidate at all.
- Every other finding got a verdict from reading the code at that head, with a repro where it was cheap.

The first setup (appendix) let later commits in. It was rerun as above, and only the rerun counts.

## Results per run
Each pass is listed with its duration in seconds. Candidates are listed per lens: intent / reliability / security. "Gated" counts findings under the old gate / the new default, and exit is the code under the new default.

| PR | Wall | Passes (s) | Candidates | Kept / dropped / restored | Gated (old / new), exit | Must-fix matched | Suggestions matched | Other findings |
|---|---|---|---|---|---|---|---|---|
| orca-plane #31 | 123 s | intent 46, rel 62, sec 29, coord 51, judge 7 | 1 / 2 / 0 | 2 / 1 / 0 | 0 / 0, 0 | 1 of 1 (P3, 0.5) | – | 1 real |
| rreview #8 | 254 s | 49, 168, 47, 73, 10 | 1 / 4 / 1 | 3 / 3 / 0 | 0 / 0, 0 | 1 of 2 (P3, 0.6) | 0 of 1 | 2 real |
| rreview #9 | 268 s | 59, 170, 70, 86, 8 | 1 / 5 / 1 | 5 / 1 / 0 | 0 / 3, 1 | 0 of 4 | 1 of 2 (P2, 0.7) | 3 real, 1 accepted risk |
| rreview #10 | 275 s | 55, 178, 56, 72, 23 | 1 / 5 / 0 | 0 / 6 / 0 | 0 / 0, 0 | 0 of 5, 1 found, dropped | 0 of 3, 1 found, dropped | – |
| orca-plane #22 | 290 s | 50, 220, 90, 51, 15 | 1 / 4 / 1 | 1 / 5 / 0 | 0 / 1, 1 | 1 of 3 (P2, 0.55) | – | – |
| **Total** | 1210 s | 25 passes, all completed | 28 | 11 / 16 / 0 | 0 / 4 | **3 of 15**, 1 found, dropped | **1 of 6**, 1 found, dropped | 6 real, 1 accepted risk |

The Reliability lens (Claude) is the slowest pass every time, at 62-220 s. The Codex lenses come back in about a minute.

### orca-plane #31 at `986b825`
- **Matched:** *P2: the fallback can still show only headings.* rreview reported the line-limit form of the repro: five leading headings fill the 10-line slice, so the preview is headings and `…`. The issue's acceptance criterion says a preview never ends on a heading.
- **Real:** the new `[ \t]+\S*$` no longer trims a word cut on a line without spaces; the old `\s+\S*$` did. A Node repro of both regexes confirms it. Cosmetic, P3.
- **Drop, sound:** a long token right after a heading is cut mid-token. The author did this on purpose, and a test asserts it.

### rreview #8 at `67e44ba`
- **Matched:** *the Codex write check passes when the reply merely contains `write=`* (`src/doctor.rs:392`). The intent lens rated it P2, 0.96; the coordinator lowered it to P3, 0.6.
- **Missed:** the Claude probe report is not checked for contradictions or a nonzero exit.
- **Missed suggestion:** a positive control for the planted-hook check.
- **Real, both P3:**
  - No test covers `Launch::run`'s timeout and kill path; one came in #9.
  - `Fixture::new` leaves its temp dir behind when a git step fails.
- **Drops:**
  - *Wrong:* "Prevent sandboxed Claude Bash from exfiltrating inherited secrets". This is real and is now #17. The coordinator answered with the spec's network non-goal, which says nothing about the environment.
  - *Sound:* timeouts for the version probes (speculative) and pinning the toolchain (the issue asked for `stable`).

### rreview #9 at `1664fd9`
- **Missed, must-fix:**
  - A failed checkout leaves the worktree registered.
  - Positional candidate arrays pass strict decoding.
  - A file name with a leading space is rejected.
  - The largest `--timeout` panics.
- **Matched suggestion:** *publishing follows a reviewer-planted `out/` symlink* (P2, 0.7). It is gated under the new default.
- **Missed suggestion:** weak resume assertions.
- **Real:**
  - An interrupted rreview leaves its reviewer running (P2, 0.65). The code notes it only as a `limit:` comment; now #18.
  - The wrap-up turn resumes with every tool, so a reviewer can keep investigating instead of answering (P3). The PR's own live check saw this.
  - A failed progress write hides the pass error (P3).
- **Accepted risk:** "Block network egress for Claude review passes" (P1, 0.75). This is true, but the spec lists network isolation beyond the deny rules as a v1 non-goal and an open item (§1, §9). It is not a defect against the agreed contract; #17 covers the part the spec does not.
- **Drop, sound:** the same-head run dir race, a documented limit with a narrow trigger.

### rreview #10 at `f3ec99e`
The coordinator dropped all 6 candidates, and the judge restored none.
- **Found, dropped:** *candidates are accounted for by count, not identity* (P1 in the reference review). The intent lens reported it at P2, 0.98. The coordinator dropped it because the count-only check "is the documented design", although the change itself introduced that design (#14).
- **Found, dropped suggestion:** *a duplicate that clears the gate can lose to one that does not.* The coordinator dropped it because the judge "is told not to restore duplicates".
- **Missed, must-fix:**
  - Judge restorations are not linked to drops.
  - The whole reply is not validated against its shape.
  - A Codex turn that exits 0 without a reply is not retried.
  - Preparation runs outside the budget.
- **Missed suggestions:**
  - The judge prompt substitutes template text inside evidence.
  - Published artifacts sit in reviewer-writable space.
- **Other drops:**
  - *Wrong:* the interrupt defect again, dropped as already documented.
  - *Sound, intended behavior:* no budget reserve for the wrap-up, `show` naming its head only by path, and exit 0 with every panelist failed (`degraded` reports that).

### orca-plane #22 at `9aa5fde`
- **Matched:** *a truncated workspace list permits a duplicate Start* (P2, 0.55). It is gated under the new default.
- **Missed:**
  - A late secret read restores a disconnected connection.
  - A queued Start completion writes into the new account after a switch.
- **Drops:**
  - *Wrong:* `void this.create(...)` has no catch, so a storage error leaves the item `pending`. This is real at P3, and the first setup kept it.
  - *Sound:*
    - A refresh after a connection switch: disproved by `running = null` before the new refresh.
    - A grace period for `unknown`: the spec says to offer Start again.
    - Refresh storage failures: speculative.
    - Ticket text reaching the agent: the feature itself.

## Gate
The gate is recomputed at each threshold from the kept and restored findings (`out/coordinator.json`, `out/judge.json`), with rreview's dedupe run at that same threshold. No run had two findings with the same dedupe key, so no threshold changes which duplicate wins. Exit 1 means at least one finding cleared the gate; all 5 PRs had must-fix defects.

| Gate | Gated | Real | Accepted risk | Must-fix | PRs that exit 1 |
|---|---|---|---|---|---|
| `p<=2,conf>=0.8` (old default) | 0 | 0 | 0 | 0 | 0 |
| `p<=2,conf>=0.7` | 2 | 1 | 1 | 0 | 1 |
| `p<=2,conf>=0.6` | 3 | 2 | 1 | 0 | 1 |
| **`p<=2,conf>=0.5` (new default)** | 4 | 3 | 1 | 1 | 2 |
| `p<=3,conf>=0.8` | 0 | 0 | 0 | 0 | 0 |
| `--all` | 11 | 10 | 1 | 3 | 4 |

- **0.5 is the coordinator's floor:** its prompt drops anything below it. So today the default filters on priority alone.
- **The first setup points the same way:**
  - Under 0.8, its only gated finding was a judge restoration whose priority was overstated (appendix).
  - Under 0.5, it gated 3 real findings and that restoration.
- **Why `--all` stays off by default:**
  - It would make 4 of 5 runs exit 1, mostly on P3 test gaps and edge cases.
  - #16 (open) is to make those findings visible in the summary without gating on them; today the summary prints the gated findings and the counts only.
- **Provisional:** five selected PRs with one clean run each cannot show that 0.5 is an optimum. The two setups also matched different defects (below), so a single run is noisy. Recalibrate once #14 makes confidence mean "this defect exists".

## Run-to-run variance
The two setups differ only in which objects the clone held, and no reviewer log shows a command touching a later commit. Comparing them is therefore a rough look at variance:

| Ground truth | First setup | Clean rerun |
|---|---|---|
| orca-plane #31 heading-only preview | matched | matched |
| orca-plane #22 truncated workspace list | matched | matched |
| rreview #8 `write=` check | missed | matched |
| rreview #8 hook positive control (suggestion) | matched | missed |
| rreview #9 `out/` symlink (suggestion) | missed | matched |
| rreview #10 accounting by identity | missed | found, dropped |
| rreview #10 reviewer-writable `out/` (suggestion) | matched | missed |

- **Findings returned:** 20 in the first setup, 11 in the rerun.
- **Must-fix matched:** 2 in the first setup, 3 in the rerun. Both first-setup matches recur in the rerun, so together the two runs matched 3 different must-fix defects. That is 4 detected if you count the rreview #10 defect that a lens found and the coordinator dropped.

## Follow-ups
- #13: the panel misses defects in validators, boundary inputs, failure paths and async ordering (12 of 15 must-fix missed).
- #14: the coordinator scores confirmed defects with low confidence and priority, and drops a defect because the change's own design describes it.
- #15: the judge restores drops without answering the coordinator's evidence (one overstated restoration; 0 of 5 wrongly dropped real candidates restored).
- #16: the summary hides findings below the gate.
- #17: reviewers inherit rreview's environment, including tokens.
- #18: an interrupted rreview leaves its reviewers running.
- #20: run dirs are readable by other local users.

## Limits of this calibration
- Five PRs, one clean run each, from two repos by the same author and reviewer pair. The counts are a direction, not a rate.
- The first reviewer ran repros for minutes per finding; rreview's panel passes took 29-220 s. The comparison measures rreview at its defaults, with `--role-effort` left unset.
- Cost in tokens is not recorded: both CLIs ran on subscriptions.

## Reproduce
Per PR, with `head` the reviewed head, `base` = `mergeCommit^1` from `gh api repos/<o>/<r>/commits/<mergeCommit> --jq '.parents[0].sha'`, and `body.md` the PR body at its first review (GraphQL `pullRequest.userContentEdits`):

```sh
git init -q clone && cd clone
git fetch -q --no-tags https://github.com/<o>/<r>.git "$head:refs/heads/reviewed" "$base:refs/heads/base"
git checkout -q base && git remote add origin https://github.com/<o>/<r>.git
before=$(git cat-file --batch-all-objects --batch-check | wc -l)

# gh wrapper first on PATH: `gh pr view` answers as the PR looked at its first review
cat > ../bin/gh <<'EOF'
#!/bin/sh
if [ "$1" = pr ] && [ "$2" = view ]; then
  /path/to/real/gh "$@" | jq -c --rawfile b "$RR_BODY" --arg base "$RR_BASE" \
    '.body = ($b | rtrimstr("\n")) | .state = "OPEN" | .mergeCommit = null | .baseRefOid = $base'
else
  exec /path/to/real/gh "$@"
fi
EOF
chmod +x ../bin/gh

PATH="$PWD/../bin:$PATH" XDG_STATE_HOME=<scratch>/state RR_BODY=body.md RR_BASE="$base" GIT_ALLOW_PROTOCOL=file \
  rreview <n> --repo <o>/<r> --head "$head" --context-file issue.md

# proof: nothing later entered the clone
test "$(git cat-file --batch-all-objects --batch-check | wc -l)" = "$before" || exit 1
for c in $(gh api --paginate repos/<o>/<r>/pulls/<n>/commits --jq '.[].sha' | sed -n "/^$head\$/,\$p" | tail -n +2); do
  git cat-file -e "$c" 2>/dev/null && { echo "later commit $c is present"; exit 1; }
done
```

The gate table comes from the five run dirs: for each threshold, take the findings of `out/coordinator.json` plus `out/judge.json`, dedupe them by file, start line and title, and keep the winner that ranks highest under that gate.

## Appendix: the first setup (superseded)
- **What went wrong:** the first five runs used the same clones, but `gh pr view` reported each PR as merged. Resolving `mergeCommit^1` needs the merge commit, so preparation fetched the current `refs/pull/<n>/head` and the base branch at the start of every run. Each clone's `FETCH_HEAD` named the final PR head and today's base tip, so the later fix commits were in the object store while the reviewers ran.
- **Did a reviewer use them?** The Codex event logs and the Claude session transcripts show git commands only between the base and the reviewed head: `diff`, `show`, `grep`, `ls-files`, and `log` of that range. No `FETCH_HEAD`, `--all`, `fsck` or `reflog` appears. The only SHAs outside that range are heads of earlier PRs, which sit in the base history or are quoted in the reviewed PR's own text. The runs are still not valid evidence, so they were redone as above.
- **Results:**
  - 1232 s in all, 24 passes, one retry after malformed JSON.
  - 20 findings, of which 2 matched must-fix defects and 2 matched suggestions; one partial match.
  - Under 0.8, one gated finding: the judge's restoration of "Restrict permissions on review run directories" (rreview #9, P2, 0.82). The coordinator had dropped it because the worktree is a copy of the user's clone under the same umask.
- **That restoration, rechecked:** it is real but overstated. The run dir also holds files the clone does not: prompts with the PR and task text, Codex event logs with command output, and run-scoped Codex sessions. All are 0644 or 0755 under umask 022, so other local users can read them where the state dir is traversable: a 0755 home on many Linux systems, though the default macOS home is 0750. That makes it P3 at most, filed as #20. The coordinator's evidence missed the generated files, and the judge did not answer that evidence (#15).
- **The orca-plane #22 "connect" finding, rescoped:** at `9aa5fde` the panel that sends `connect` loads its data from the reply (`src/panel.tsx:177`), and every panel gets items through the worker's `items.changed` (`src/items.ts:27`).
  - The affected panel is a second panel that is already open, or the connecting panel when the 60 s connect reply times out. On `connection.changed` such a panel sets only `status` (`src/panel.tsx:142`).
  - So it keeps `starts` empty and never initializes the Project and Agent pickers. Start then shows instead of Open workspace for a linked item, and Confirm stays disabled until the panel reloads.
  - The finding is real at P3.
