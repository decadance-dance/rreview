// `orca plugin check` renders the panel once per scenario. The requests stand in for the worker,
// which would run gh and rreview. The `pr1*` scenarios replay the real review of PR #1 for
// `orca plugin shot orca-plugin --scenario <name>`, which draws docs/screenshots/orca-plugin-*.png.
import { defineCheck } from 'orca/testing'
import { cancelled, done, doneResult, failed, running, target } from './fixtures'
import type { PassEvent, Result, Run, Target } from './protocol'
import { pr1, pr1Events, pr1Result } from './screens'

const agents = { agents: [{ terminalId: 't1', title: 'claude', agent: 'claude', state: 'waiting', stateStartedAt: 0 }] }
const scenario = (run: Run | null, res: Result | null = run === done ? doneResult : null, t: Target | null = target) => ({
  results: { 'agents.list': agents },
  requests: { 'target.get': () => t, 'review.get': () => run, 'review.result': () => res }
})

// PR #1's run, its clock moved so a still frame shows a plausible elapsed time.
const pr1Target: Target = { ...target, name: 'feature/bootstrap-docs', pr: pr1 }
const pr1Run = (events: PassEvent[], status: Run['status']): Run => {
  const shift = Date.now() - events.at(-1)!.ts - 15_000
  return {
    pr: pr1, workspaceId: 'ws1', runner: 'orca', token: 'pr1', startedAt: pr1Events[0].ts + shift,
    runDir: null, events: events.map((e) => ({ ...e, ts: e.ts + shift })), status,
    summary: status === 'done' ? { gated: 0, all: 2, degraded: true } : null, exitCode: status === 'done' ? 0 : null, error: null
  }
}
const pr1Done = pr1Run(pr1Events, 'done')

export default defineCheck({
  idle: scenario(null),
  running: scenario(running),
  done: scenario(done),
  failed: scenario(failed),
  cancelled: scenario(cancelled),
  noFindingsAboveGate: scenario({ ...done, summary: { gated: 0, all: 1, degraded: false } }, { ...doneResult, findings: [], degraded: false }),
  noPr: scenario(null, null, { ...target, pr: null, error: 'no pull requests found for branch "topic"' }),
  noWorkspace: scenario(null, null, null),
  pr1Ready: scenario(null, null, pr1Target),
  // Both codex lenses done, the reliability lens still at work: the PR #1 run about 100 s in.
  pr1Running: scenario(pr1Run(pr1Events.slice(0, 5), 'running'), null, pr1Target),
  pr1Findings: { ...scenario(pr1Done, pr1Result, pr1Target), persisted: { plugin: { showAll: true } } },
  pr1Sent: {
    ...scenario(pr1Done, pr1Result, pr1Target),
    persisted: { plugin: { showAll: true, sent: { token: 'pr1', sent: 1, file: 'author-1795043500000.md', to: 'claude (waiting)' } } }
  }
})
