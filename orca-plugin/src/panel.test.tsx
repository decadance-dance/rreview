// The panel over a mock host whose requests stand in for src/worker.ts.
import { assert, mockHost, mount, test } from 'orca/testing'
import { done, failed, running, target } from './fixtures'
import * as panel from './panel'
import type { Run } from './protocol'

function setup(run: Run | null = null) {
  const calls: { name: string; input: unknown }[] = []
  const host = mockHost({
    results: {
      'agents.list': { agents: [{ terminalId: 't1', title: 'claude author', agent: 'claude', state: 'waiting', stateStartedAt: 0 }] }
    },
    requests: {
      'target.get': (input) => (calls.push({ name: 'target.get', input }), target),
      'review.get': () => run,
      'review.start': (input) => (calls.push({ name: 'review.start', input }), running),
      'findings.send': (input) => (calls.push({ name: 'findings.send', input }), { sent: input.keys.length })
    }
  })
  return { host, calls }
}

test('shows the workspace PR and starts a review with the chosen runner', async () => {
  const { host, calls } = setup()
  const view = await mount(panel, { host })
  assert.match(view.summary().join('\n') + JSON.stringify(view.tree()), /Orca runner/)
  await view.select({ label: 'Runner' }, 'orca')
  await view.press({ text: 'Run review' })
  assert.deepEqual(calls.at(-1), { name: 'review.start', input: { workspaceId: 'ws1', pr: '', runner: 'orca' } })
  // The worker's posts drive the passes table while it runs.
  assert.equal(view.find({ text: 'Running…' })?.text, 'Running…')
  assert.deepEqual(view.rowsOf({ label: 'Passes' }), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  await view.post('review.changed', done)
  assert.deepEqual(view.rowsOf({ label: 'Passes' }), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  assert.deepEqual(view.errors, [])
  assert.deepEqual(view.refusals, [])
  await view.unmount()
})

test('a finished review lists findings by file and sends the picked ones', async () => {
  const { host, calls } = setup(done)
  const view = await mount(panel, { host })
  const tree = JSON.stringify(view.find({ label: 'Findings by file' }))
  assert.match(tree, /src/)
  assert.match(tree, /L120-124 \[P1\] Terminal leaks when close fails/)
  assert.ok(!/Unclear error text/.test(tree), 'below the gate stays hidden')
  assert.match(view.summary().join('\n'), /degraded/i)

  await view.toggle({ label: 'All 3, below the gate too' })
  assert.match(JSON.stringify(view.find({ label: 'Findings by file' })), /Unclear error text/)

  // Both gated findings start picked; unpick one through its detail.
  await view.selectItem({ label: 'Findings by file' }, 'src/review.rs#40#Budget skips setup')
  assert.match(JSON.stringify(view.tree()), /Budget skips setup\*\*: what fails/)
  await view.toggle({ label: 'Send to the author' })
  await view.press({ text: 'Send 1 to author' })
  assert.deepEqual(calls.at(-1), {
    name: 'findings.send',
    input: { terminalId: 't1', keys: ['src/orca.rs#120#Terminal leaks when close fails'] }
  })
  assert.match(JSON.stringify(view.tree()), /Sent 1 finding to the author/)
  assert.deepEqual(view.errors, [])
  await view.unmount()
})

test('a failed review is shown as failed with its error', async () => {
  const { host } = setup(failed)
  const view = await mount(panel, { host })
  const s = view.summary().join('\n')
  assert.match(s, /The review failed/)
  assert.match(JSON.stringify(view.tree()), /the reply is not JSON/)
  assert.equal(view.find({ label: 'Findings by file' }), null)
  await view.unmount()
})

test('the file tree folds single-child folders', () => {
  const items = panel.fileTree([
    { title: 'A', body: '', file: 'a/b/c/x.rs', start_line: 1, end_line: 1, priority: 0, confidence_score: 1, stage: 'judge' },
    { title: 'B', body: '', file: 'top.rs', start_line: 2, end_line: 3, priority: 2, confidence_score: 0.8, stage: 'coordinator' }
  ])
  assert.deepEqual(items.map((i) => i.label), ['a/b/c', 'top.rs'])
  assert.equal(items[0].children?.[0].children?.[0].label, 'L1 [P0] A')
  assert.equal(items[1].children?.[0].label, 'L2-3 [P2] B')
})
