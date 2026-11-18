// `orca plugin check` renders the panel once per scenario. The requests stand in for the worker,
// which would run gh and rreview.
import { defineCheck } from 'orca/testing'
import { done, doneResult, failed, running, target } from './fixtures'
import type { Run } from './protocol'

const agents = { agents: [{ terminalId: 't1', title: 'claude', agent: 'claude', state: 'waiting', stateStartedAt: 0 }] }
const scenario = (run: Run | null) => ({
  results: { 'agents.list': agents },
  requests: { 'target.get': () => target, 'review.get': () => run, 'review.result': () => (run === done ? doneResult : null) }
})

export default defineCheck({
  idle: scenario(null),
  running: scenario(running),
  done: scenario(done),
  failed: scenario(failed),
  noPr: {
    requests: { 'target.get': () => ({ ...target, pr: null, error: 'no pull requests found for branch "topic"' }), 'review.get': () => null }
  }
})
