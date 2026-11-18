# rreview

`rreview <PR>` gives a GitHub pull request an independent, cross-model panel review before it is marked ready.

Three fixed-lens reviewers (intent and contracts, reliability, security) run in parallel, a coordinator tries to disprove every candidate finding, and a judge of the other model family re-examines only the dropped ones. Reviewers run through the official `claude` and `codex` CLIs on your own subscriptions, isolated from your personal agent config and read-only by the OS sandbox, not by the prompt. Reviews run headless by default, or with every reviewer in a visible Orca terminal.

Status: pre-alpha, nothing usable yet. The product and the plan: [docs/VISION-AND-SPEC.md](docs/VISION-AND-SPEC.md). Contributing: [AGENTS.md](AGENTS.md).

The review pipeline and its prompts are adapted from [block/review](https://github.com/block/review) (Apache-2.0); see [NOTICE](NOTICE).

License: [Apache-2.0](LICENSE).
