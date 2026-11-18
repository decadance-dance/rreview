# Releases and hotfixes
Read when: cutting a release, shipping a hotfix, bumping the version, or tagging `main`.
TL;DR: A `release-<ver>` branch comes off `develop`, a `hotfix-<ver>` branch off `main`. Each gets its own release issue, a PR into `main`, a plain `X.Y.Z` annotated tag on `main` whose message is the release notes and whose push publishes the GitHub Release, and a PR back into `develop`. Every release and hotfix bumps `version` in `Cargo.toml` (and so `Cargo.lock`). Branch basics and the task-branch rules: [git-flow.md](git-flow.md). A closed issue stays closed after a release: new work gets a new issue.

## Release branch (`release-*`)
### Do
- Cut it when `develop` has collected enough, usually a finished milestone: `git checkout -b release-1.2.0 origin/develop`.
- Limit it to small bug fixes, the version bump and the release notes. New work keeps going to `develop` in parallel.
- Give the release its own issue (`Release 1.2.0`). Its checklist holds only what is done before the PR into `main` merges: version bumped in `Cargo.toml` and `Cargo.lock`; shipped issues marked; notes ready; `ci-ok` green and the cross-model review done on the draft PR. The steps after the merge (tag, GitHub Release, PR into `develop`, deleting the branch) go in the issue as notes.
- Bump the version in its own commit: edit `version` in `Cargo.toml`, then run `bin/check`, which rewrites `Cargo.lock` with the same number; commit both.
- List what ships by the branch's contents, not by dates: work merged into `develop` after the cut does not ship. The PRs are the merge commits on the branch since the last tag (for the first release, all of them: drop `<last tag>..`):
  ```
  git log --first-parent --merges --format=%H <last tag>..release-1.2.0 |
    while read c; do gh api repos/decadance-dance/rreview/commits/$c/pulls --jq '.[] | "#\(.number) \(.title) @\(.user.login)"'; done
  ```
  The shipped issues are the ones those PRs closed (`gh pr view <pr> -R decadance-dance/rreview --json closingIssuesReferences --jq '.closingIssuesReferences[].number'`). This PR list and these issues are the notes' input.
- Mark the shipped issues: each of them, and the release issue itself, get the milestone `<ver>` (`gh issue edit <n> --milestone 1.2.0`; create it once with `gh api repos/decadance-dance/rreview/milestones -f title=1.2.0`).
- Draft the release notes: write the whole body into a local file outside the repo, `RELEASE-<ver>.md`, by the template in [release-notes.md](release-notes.md), and keep its path in `NOTES` (`NOTES=<dir outside the repo>/RELEASE-1.2.0.md`). The maintainer edits it. Never commit it.
- Open a draft PR into `main` (`gh pr create --draft --base main --title "Release 1.2.0" --body "Refs #<release issue>"`). When `ci-ok` is green and the cross-model review is done, tick the last item and mark it ready (`gh pr ready`).
- After that PR merges, the maintainer tags `main` and pushes the tag; the push publishes the GitHub Release (`.github/workflows/release.yml`: title = the version, body = the tag message body as written; `--cleanup=verbatim` keeps its `##` headings):
  ```
  git checkout main && git pull
  git tag -a 1.2.0 --cleanup=verbatim -F "$NOTES"
  git push origin 1.2.0
  ```
  Check the published Release (`gh release view 1.2.0`); a failed workflow run leaves none.
- Then open a PR from the same branch into `develop` (`gh pr create --base develop --head release-1.2.0 --title "Release 1.2.0 into develop" --body "Closes #<release issue>"`). If it conflicts, branch `merge/1.2.0` off `develop`, merge `release-1.2.0` into it, resolve, and PR `merge/1.2.0` into `develop` with the same title.
- Delete the release branch after both PRs have merged.

### Don't
- Don't add new features in a release branch.
- Don't skip the PR back into `develop`, or the fixes made during the release are lost in the next one.
- Don't tag with a `v` prefix, a suffix (`1.2.0-rc.1`) or a lightweight tag: plain annotated `X.Y.Z` tags only. The release workflow fails on a lightweight tag.
- Don't publish the GitHub Release by hand: the tag push does it, and a hand-made one makes the workflow fail.

## Hotfix branch (`hotfix-*`)
### Do
- Use it only when something broke for users and the next release cannot wait. It is the only branch that comes off `main`: `git checkout -b hotfix-1.2.1 origin/main`.
- Give it its own issue by the release issue template above (checklist before the merge, post-merge steps as notes) and mark only the hotfix issue with milestone `1.2.1`: the closed issues waiting on `develop` ship with the next release. The notes' input is the hotfix issue and the hotfix PR into `main`, not `develop`'s history.
- Finish it like a release: version bump, a PR into `main`, the tag and the GitHub Release on `main`, then a PR from the same branch into `develop`.

### Don't
- Don't use a hotfix for planned work or features.
- Don't forget the PR into `develop`.

## Release numbers
### Do
- Use `MAJOR.MINOR.PATCH` (for example `1.4.2`):
  - patch +0.0.1 when a `hotfix-*` ships, or a `release-*` ships without new features (only fixes or docs);
  - minor +0.1.0 when a `release-*` ships with features;
  - major +1.0.0 when a release breaks users: a CLI flag or exit code that no longer works as before, or a `result.json` or run-dir layout that tools reading it can no longer parse.
- Stay on `0.x` until rreview is a review people rely on; `1.0.0` is that release. While on `0.x`, a breaking change bumps the minor, as Cargo reads `0.x` versions.
- The first release is the version already in `Cargo.toml`, `0.1.0`.
- Keep the tag and `version` in `Cargo.toml` equal.

## Cheat sheet
```
# release a version
git checkout -b release-1.2.0 origin/develop
# release issue #<r>; milestone 1.2.0 on it and on every issue closed by a PR merged on the branch since the last tag
# version bump in Cargo.toml + bin/check (Cargo.lock); NOTES=<outside the repo>/RELEASE-1.2.0.md drafted and edited (never committed)
git push -u origin release-1.2.0 && gh pr create --draft --base main --title "Release 1.2.0" --body "Refs #<r>"
# ci-ok green, cross-model review done: tick the checklist, then gh pr ready
# maintainer:
gh pr merge <pr> -R decadance-dance/rreview --merge
git checkout main && git pull && git tag -a 1.2.0 --cleanup=verbatim -F "$NOTES" && git push origin 1.2.0   # the push publishes the Release
gh pr create --base develop --head release-1.2.0 --title "Release 1.2.0 into develop" --body "Closes #<r>"   # then merge it
git push origin --delete release-1.2.0 && git branch -d release-1.2.0

# urgently fix a release
git checkout -b hotfix-1.2.1 origin/main
# hotfix issue, milestone 1.2.1 alone (develop's closed issues wait for the next release); notes from the hotfix PR into main
# same finish as a release: version bump, PR into main, tag and GitHub Release on main, PR into develop, delete the branch
```
