# Release notes
Read when: drafting `RELEASE-<ver>.md` (local, never committed) for a release or hotfix.
TL;DR: The agent writes the whole release body: a short version for readers who stop after six lines, upgrade notes, then every PR grouped by area. It becomes the message of the annotated tag (`git tag -a <ver> --cleanup=verbatim -F RELEASE-<ver>.md`): the first line `rreview <ver>` is the tag's subject, the rest is carried verbatim to the GitHub Release, whose title is the version alone; nothing is generated.

Model: the Orca releases (https://github.com/stablyai/orca/releases).

## Template
```
rreview <ver>

## The short version

- **<Benefit in a few words>:** <what changed for the user, one or two sentences>.
- ...

## Compatibility & upgrade notes

**<Area>:** <what a user must do or know: a changed CLI flag or exit code, a new `result.json` field, a newer minimum Claude Code, Codex or Orca version>.

---

## Changes

### <Area>

*<One sentence: what this area gained in this release.>*

* **<label>** <PR title> by @<author> in https://github.com/decadance-dance/rreview/pull/<N>
* ...

**Full Changelog**: https://github.com/decadance-dance/rreview/compare/<prev>...<ver>
```
For the first release there is no `<prev>`: drop the Full Changelog line.

## Do
- Collect the input from the branch, not from dates: the PRs merged on the release branch since the last tag and the issues they closed ([releasing.md](releasing.md), Release branch); for a hotfix, the hotfix PR into `main` and its issue. For a release, cross-check against the milestone in all states (`gh issue list -R decadance-dance/rreview --milestone <ver> --state all --limit 500`): leaving out the release issue itself, which closes only with the PR into `develop` after publishing, the milestone must hold exactly the shipped issues; an issue in only one of the two lists is a mistake to resolve before publishing. Exactly 500 results means the list is cut: page through `gh api --paginate "repos/decadance-dance/rreview/issues?milestone=<number>&state=all" --jq '.[] | select(.pull_request | not) | .number'` instead (this endpoint also returns PRs, hence the filter). A hotfix's milestone holds only the hotfix issue, so it has nothing to cross-check. Labels of a PR: `gh pr view <pr> -R decadance-dance/rreview --json labels --jq '.labels[].name'`.
- Use the issues for "The short version" and the area sentences; the PR list stays the source for the per-PR lines.
- Write "The short version" as 3-6 bullets, each starting with a bold benefit. Say what a user gets, not which files changed.
- Start every PR line with the PR's labels in bold: `enhancement`, `bug`, `documentation` (`* **bug** <PR title> ...`). A PR without one of these labels gets no prefix.
- Put every merged PR in exactly one area: for example Panel & prompts, Coordinator & judge, Runners, CLI & doctor, Orca plugin, Docs & workflow, Tests & maintenance. Drop an area that has no PRs; add one when a release needs it.
- Write "Compatibility & upgrade notes" only when there is something to do or know; otherwise drop the section.

## Don't
- Don't list commits or file paths in the short version.
- Don't write a title after the version: the Release title is the version alone, and the tag subject is `rreview <ver>`.
