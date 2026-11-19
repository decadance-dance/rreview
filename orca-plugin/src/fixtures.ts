// Runs and targets the panel's tests and `orca plugin check` scenarios share.
import type { Finding, Result, Run, Target } from './protocol'

export const target: Target = {
  workspaceId: 'ws1',
  name: 'feature/5-orca-runner',
  path: '/repo',
  pr: { number: 11, title: 'Orca runner', url: 'https://github.com/acme/repo/pull/11', state: 'MERGED' },
  error: null
}

const f = (file: string, line: number, title: string, priority: number, confidence_score: number): Finding => ({
  title, body: `**${title}**: what fails and how.`, file, start_line: line, end_line: line + 4,
  priority, confidence_score, stage: 'coordinator'
})

const ev = (role: string, status: string, agent: string, duration_ms: number) => ({
  ts: 1_700_000_000_000 + duration_ms, role, status, agent, model: 'default', duration_ms
})

const base = { pr: target.pr!, workspaceId: 'ws1', runner: 'orca' as const, startedAt: 1_700_000_000_000, runDir: '/state/rreview/acme/repo/11-abc' }

export const running: Run = {
  ...base, status: 'running', summary: null, exitCode: null, error: null,
  events: [
    ev('intent_contracts', 'started', 'codex', 0),
    ev('reliability', 'started', 'claude', 0),
    ev('security', 'started', 'codex', 0),
    ev('intent_contracts', 'completed', 'codex', 61_000)
  ]
}

const gated = [f('src/orca.rs', 120, 'Terminal leaks when close fails', 1, 0.9), f('src/review.rs', 40, 'Budget skips setup', 2, 0.85)]
const below = [f('src/orca.rs', 300, 'Unclear error text', 3, 0.6)]

export const doneResult: Result = {
  findings: gated,
  ungated_findings: [...gated, ...below],
  overall_correctness: 'patch is incorrect',
  overall_explanation: 'A failed close leaves a terminal open.',
  passes: [
    { role: 'security', agent: 'codex', status: 'failed', duration_ms: 40_000, error: 'no reply' },
    { role: 'coordinator', agent: 'claude', status: 'completed', duration_ms: 120_000 }
  ],
  head: 'a'.repeat(40),
  degraded: true
}

export const done: Run = {
  ...base, status: 'done', exitCode: 1, error: null,
  events: [
    ...running.events,
    ev('reliability', 'completed', 'claude', 95_000),
    ev('security', 'failed', 'codex', 40_000),
    ev('coordinator', 'started', 'claude', 0),
    ev('coordinator', 'completed', 'claude', 120_000)
  ],
  summary: { gated: 2, all: 3, degraded: true }
}

export const failed: Run = {
  ...base, status: 'failed', exitCode: 2, summary: null,
  error: 'error: coordinator: the reply is not JSON',
  events: [...done.events.slice(0, 7), ev('coordinator', 'failed', 'claude', 30_000)]
}
