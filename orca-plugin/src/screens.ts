// The real review of this repo's PR #1 (orca runner; the run the plugin's screenshots show),
// as its progress.jsonl and result.json gave it: `orca plugin shot --scenario pr1*` draws it.
import type { PassEvent, Result } from './protocol'

export const pr1 = {
  number: 1,
  title: 'Bootstrap docs: spec, README, NOTICE, AGENTS.md, git-flow',
  url: 'https://github.com/decadance-dance/rreview/pull/1',
  state: 'MERGED'
}

export const pr1Events: PassEvent[] = [
  {
    agent: 'codex',
    duration_ms: 0,
    model: 'default',
    role: 'intent_contracts',
    status: 'started',
    ts: 1795042381790
  },
  {
    agent: 'codex',
    duration_ms: 0,
    model: 'default',
    role: 'security',
    status: 'started',
    ts: 1795042381790
  },
  {
    agent: 'claude',
    duration_ms: 0,
    model: 'default',
    role: 'reliability',
    status: 'started',
    ts: 1795042381790
  },
  {
    agent: 'codex',
    duration_ms: 58145,
    model: 'default',
    role: 'security',
    status: 'completed',
    ts: 1795042439936
  },
  {
    agent: 'codex',
    duration_ms: 86877,
    model: 'default',
    role: 'intent_contracts',
    status: 'completed',
    ts: 1795042468668
  },
  {
    agent: 'claude',
    duration_ms: 900275,
    model: 'default',
    role: 'reliability',
    status: 'wrapup',
    ts: 1795043282095
  },
  {
    agent: 'claude',
    duration_ms: 901916,
    error:
      'pass reliability (claude): pass reliability timed out after 900s; wrap-up: orca terminal send: agent_prompt_blocked: agent_prompt_blocked The agent is showing a permission or trust prompt; to answer it, send --text and --enter as separate commands. Terminal prompt request ID: e7a4133b-3037-402d-b3db-9aaef862be94. Re-issue the exact command with --retry-request e7a4133b-3037-402d-b3db-9aaef862be94 --wait-submit <seconds>; do not retry it without that ID.',
    model: 'default',
    role: 'reliability',
    status: 'failed',
    ts: 1795043283737
  },
  {
    agent: 'claude',
    duration_ms: 0,
    model: 'default',
    role: 'coordinator',
    status: 'started',
    ts: 1795043283738
  },
  {
    agent: 'claude',
    duration_ms: 93637,
    model: 'default',
    role: 'coordinator',
    status: 'completed',
    ts: 1795043377376
  },
  {
    agent: 'codex',
    duration_ms: 0,
    model: 'default',
    role: 'judge',
    status: 'started',
    ts: 1795043377376
  },
  {
    agent: 'codex',
    duration_ms: 30410,
    model: 'default',
    role: 'judge',
    status: 'completed',
    ts: 1795043407787
  }
]

export const pr1Result: Result = {
  findings: [],
  ungated_findings: [
    {
      title: 'Open the finishing PR as a draft so cross-model review precedes ready',
      body: 'The finish step opens a ready PR, but cross-model review must happen while the PR is still a draft.\n1. `docs/workflow/git-flow.md:22` and `:65` run `gh pr create` without `--draft`.\n2. `:36` requires review before `gh pr ready` on a draft; `:17` makes an early draft optional.\nAdd `--draft` to both commands.',
      file: 'docs/workflow/git-flow.md',
      start_line: 19,
      end_line: 22,
      priority: 3,
      confidence_score: 0.7,
      stage: 'coordinator'
    },
    {
      title: 'Fix the cheat-sheet cleanup so it runs after the merge',
      body: 'The cheat-sheet cleanup fails as written.\n1. `docs/workflow/git-flow.md:63` checks out the feature branch in the current checkout.\n2. After `--delete-branch`, `git pull` (`:68`) on that branch has no upstream ref; `git branch -d` also refuses a branch checked out here or in a worktree (`:15`).\nSwitch to `develop` and remove the worktree before deleting.',
      file: 'docs/workflow/git-flow.md',
      start_line: 66,
      end_line: 68,
      priority: 3,
      confidence_score: 0.65,
      stage: 'coordinator'
    }
  ],
  overall_correctness: 'patch is correct',
  overall_explanation:
    'This change adds documentation only, with no code, tests or CI to break. The remaining findings are P3 inconsistencies in the git-flow commands and do not block the change.',
  passes: [
    {
      role: 'intent_contracts',
      agent: 'codex',
      status: 'completed',
      duration_ms: 86879
    },
    {
      role: 'reliability',
      agent: 'claude',
      status: 'failed',
      duration_ms: 901917,
      error:
        'pass reliability (claude): pass reliability timed out after 900s; wrap-up: orca terminal send: agent_prompt_blocked: agent_prompt_blocked The agent is showing a permission or trust prompt; to answer it, send --text and --enter as separate commands. Terminal prompt request ID: e7a4133b-3037-402d-b3db-9aaef862be94. Re-issue the exact command with --retry-request e7a4133b-3037-402d-b3db-9aaef862be94 --wait-submit <seconds>; do not retry it without that ID.'
    },
    {
      role: 'security',
      agent: 'codex',
      status: 'completed',
      duration_ms: 58145
    },
    {
      role: 'coordinator',
      agent: 'claude',
      status: 'completed',
      duration_ms: 93638
    },
    {
      role: 'judge',
      agent: 'codex',
      status: 'completed',
      duration_ms: 30410
    }
  ],
  head: '6f0ba497adeaa3858630194e7874f7cabad1f1cd',
  degraded: true
}
