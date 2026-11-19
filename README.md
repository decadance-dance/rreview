# rreview

`rreview <PR>` gives a GitHub pull request an independent, cross-model panel review before it is marked ready.

Three fixed-lens reviewers (intent and contracts, reliability, security) run in parallel, a coordinator tries to disprove every candidate finding, and a judge of the other model family re-examines only the dropped ones. Reviewers run through the official `claude` and `codex` CLIs on your own subscriptions, isolated from your personal agent config and read-only by the OS sandbox, not by the prompt. Reviews run headless by default, or with every reviewer in a visible Orca terminal.

Status: pre-alpha. `rreview doctor` checks the `claude`, `codex` and `gh` logins and that reviewer isolation holds on this machine (`cargo run -- doctor`); from an Orca terminal, `--runner orca` checks the Orca launch mode too. `rreview <pr> --only reliability` runs one panelist on a PR, headless, and leaves its validated findings in the run dir. `--runner orca`, with the Orca app running, runs every reviewer in a visible Orca terminal instead; known limit: answering Claude's workspace trust dialog there is recorded in your Claude config (`.claude.json`), as accepting it by hand would be. The Orca plugin in [orca-plugin/](orca-plugin/README.md) runs a review of the focused workspace's PR from a sidebar panel and lists the findings by file. The product and the plan: [docs/VISION-AND-SPEC.md](docs/VISION-AND-SPEC.md). Contributing: [AGENTS.md](AGENTS.md).

The review pipeline and its prompts are adapted from [block/review](https://github.com/block/review) (Apache-2.0); see [NOTICE](NOTICE).

License: [Apache-2.0](LICENSE).
