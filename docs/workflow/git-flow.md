# Git workflow: git-flow
Read when: starting a task, committing, opening or merging a PR, releasing, or fixing a release.
TL;DR: Two permanent branches (`main` = released, `develop` = next release) and three temporary ones (`feature/*`, `release-*`, `hotfix-*`). Every branch works on a GitHub issue of this repo, named by its number (`#12`). Every merge into a permanent branch is a GitHub pull request, merged with a merge commit after the `ci-ok` check is green and a reviewer of the other model family has reviewed it. Agents commit only on temporary branches, may push them and open PRs while working on an issue, and merge, tag or release only when the maintainer asks.

Source: Vincent Driessen, "A successful Git branching model", https://nvie.com/posts/a-successful-git-branching-model/. Deviations: our `master` is named `main`, and merges happen through GitHub pull requests instead of local `git merge`.

## Branches at a glance
| Type | Branches off | Merges into (by PR) | Name |
| --- | --- | --- | --- |
| task | `develop` | `develop` | `feature/<n>-<slug>` |
| release | `develop` | `main` **and** `develop` | `release-<version>` |
| urgent fix | `main` | `main` **and** `develop` | `hotfix-<version>` |

`develop` is the default branch on GitHub, so `Closes #<n>` in a PR into `develop` closes the issue when it merges. Delete each temporary branch right after its last PR is merged. GitHub's "Automatically delete head branches" is off on purpose: a `release-*` or `hotfix-*` branch needs two PRs, into `main` and then into `develop`, and auto-delete would remove it after the first.

## Permanent branches
### Do
- Treat `main` as released code: every commit on it is a release with its own tag.
- Treat `develop` as "the next release not yet out". Finished issues land here.
- Keep both branches forever.

### Don't
- Don't commit to `main` by hand. Code reaches it only through a PR from `release-*` or `hotfix-*`.
- Don't commit work directly on `develop`. It arrives through PRs: from `feature/*`, and back from `release-*` and `hotfix-*` after they reach `main`.
- Don't push, rebase, reset, force-push or delete `main` or `develop`. GitHub offers no ruleset or branch protection for a private repo on the free plan, so nothing on GitHub stops it: these rules are the only guard.
- Don't wait for an approving review: a sole maintainer cannot approve their own PR. The green `ci-ok` check and the cross-model review are the gate.

## Task branch (`feature/*`)
### Do
- One issue, one branch off `develop`: `feature/<n>-<slug>` (`feature/12-strict-json`). No issue yet? Even a small fix gets one first (`gh issue create`).
- Several agent sessions at once: each issue gets its own git worktree; the primary checkout is for reading.
- Commit as often as useful; small, unfinished commits are fine. Commit only this issue's files: name paths in `git add`.
- A draft PR may go up early: `gh pr create --draft --base develop`.
- Before a ready PR: `bin/check` passes, the author reviewed its own diff, the cross-model review ([below](#cross-model-review)) is done, and the PR description says what changed, why, and how it was verified. A PR that only touches an issue it does not finish says `Refs #<n>`, not `Closes #<n>`.
- Finish with a PR into `develop`, title and body passed explicitly (not `--fill`):
  ```
  git push -u origin feature/<n>-<slug>
  gh pr create --base develop --title "<summary>" --body "Closes #<n>. <what and why; how verified>"
  gh pr checks <pr> --watch
  ```
- The maintainer merges: `gh pr merge <pr> -R decadance-dance/rreview --merge --delete-branch`. With `-R`, gh deletes only the remote branch and leaves local checkouts alone. Then, from the primary checkout: `git pull`, and remove the issue's worktree and local branch.

### Don't
- Don't branch a task off `main` or another feature branch unless the maintainer asks for stacked work.
- Don't mix unrelated changes in one branch or sweep others' files into your commit (no blind `git add -A`).
- Don't merge with a red `ci-ok` or open must-fix review findings.
- Don't pass `--no-verify`. Don't delete a branch before its PR has merged.

## Cross-model review
A PR written by a coding agent merges only after a reviewer of the other model family has reviewed its head commit.

- When: after `ci-ok` is green and before `gh pr ready`, on the pushed branch (the PR is a draft).
- Who: the other model family in a fresh session. Codex reviews Claude's code, Claude reviews Codex's. A subagent of the authoring session does not count: it shares the author's model and framing.
- Input: the PR's own diff against its base (`gh pr diff <pr>`: `develop` for a task, `main` for a release or hotfix), the issue (`gh issue view <n>`), the spec sections it cites and this guide, not the author's summary. The reviewer runs `bin/check`, and for a change to reviewer launch or isolation also `cargo run -- doctor` where `claude` and `codex` are logged in.
- Output: a GitHub review in Comment mode (`gh pr review <pr> --comment --body-file <file>`). A finding is must-fix only with a failing test or a repro; the author fixes or answers each finding in the PR.
- Rounds: at most two. After the second, the maintainer decides.
- A push after the review is a new commit: the reviewer looks at the change again.
- Once rreview works end to end, `rreview <pr>` is part of this review, not a replacement for it.

## Merge commits on every merge
Merge every PR into `develop` or `main` with "Create a merge commit" (`gh pr merge --merge`); squash and rebase merges are disabled in the repo settings. An issue's work then shows as one block in history and reverts with one command. Don't fast-forward, squash or rebase into a permanent branch.

## Agents in this repo
### Do
- Check `git status` and the current branch before the first edit and before every commit.
- Write commit messages that say what changed and why.
- Report every commit you make (hash and one line).
- While working on an issue, push its `feature/*`, `release-*` or `hotfix-*` branch and open its PRs.
- Merge PRs, push tags, create releases and change repo settings only when the maintainer asks.
- Ask before any other action that leaves the machine or is hard to undo.

### Don't
- Don't push `main` or `develop` directly.
- Don't use `--force`, `reset --hard`, or `checkout -- .` on work you didn't create.
- Don't amend or rewrite commits that are already pushed. Amending your own unpushed commit is fine.

## Releases and hotfixes
Cutting a release or shipping a hotfix: read [releasing.md](releasing.md) first.

## Cheat sheet
```
# start an issue (#<n>)
git checkout -b feature/<n>-<slug> origin/develop
# ... work, bin/check, self-review, cross-model review ...
git push -u origin feature/<n>-<slug> && gh pr create --base develop --title "<summary>" --body "Closes #<n>. ..."
# maintainer:
gh pr merge <pr> -R decadance-dance/rreview --merge --delete-branch
git pull && git branch -d feature/<n>-<slug>
```
