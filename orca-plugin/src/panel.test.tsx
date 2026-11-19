// The panel over a mock host whose requests stand in for src/worker.ts.
import type { TableRow, TreeItem } from 'orca'
import { assert, mockHost, mount, test, type TestPanel } from 'orca/testing'
import { cancelled, done, doneResult, failed, running, target } from './fixtures'
import * as panel from './panel'
import type { Run } from './protocol'

/** Agents per workspace: ws1 is the reviewed run's, ws2 another one. */
const AGENTS: Record<string, unknown[]> = {
  ws1: [
    { terminalId: 't1', title: 'claude author', agent: 'claude', state: 'waiting', stateStartedAt: 0, sessionId: 's1' },
    { terminalId: 't0', title: 'a shell', agent: null, state: 'done', stateStartedAt: 0 }
  ],
  ws2: [{ terminalId: 't9', title: 'other', agent: 'codex', state: 'waiting', stateStartedAt: 0, sessionId: 's9' }]
}

/** The passes table's cells per row; a live duration shows as `relative`. */
const cells = (view: TestPanel) =>
  (view.find({ label: 'Passes' })?.props?.rows as TableRow[]).map((r) =>
    ['pass', 'status', 'time'].map((k) => {
      const c = r.cells[k]
      return typeof c === 'object' && c ? c.type : c
    })
  )

/** The findings tree's leaves as label and icon. */
const leaves = (view: TestPanel) => {
  const walk = (items: readonly TreeItem[]): TreeItem[] => items.flatMap((i) => (i.children ? walk(i.children) : [i]))
  return walk(view.find({ label: 'Findings by file' })?.props?.items as TreeItem[]).map((i) => [i.label, i.icon])
}

function setup(run: Run | null = null) {
  const calls: { name: string; input: unknown }[] = []
  const host = mockHost({
    results: {
      'agents.list': (p: { workspaceId?: string }) => ({ agents: AGENTS[p?.workspaceId ?? 'ws2'] ?? [] })
    },
    requests: {
      'target.get': (input) => (calls.push({ name: 'target.get', input }), target),
      'review.get': () => run,
      'review.result': () => (calls.push({ name: 'review.result', input: null }), doneResult),
      'review.start': (input) => (calls.push({ name: 'review.start', input }), running),
      'review.cancel': (input) => (calls.push({ name: 'review.cancel', input }), running),
      'findings.send': (input) => (calls.push({ name: 'findings.send', input }), { sent: input.keys.length, file: '/state/x/author-1.md' })
    }
  })
  return { host, calls }
}

test('shows the workspace PR and starts a review with the chosen runner', async () => {
  const { host, calls } = setup()
  const view = await mount(panel, { host })
  assert.equal(view.find({ text: 'Rreview' })?.text, 'Rreview')
  assert.match(view.summary().join('\n') + JSON.stringify(view.tree()), /Orca runner/)
  // Ready: what a run does, before the first one.
  assert.match(view.summary().join('\n'), /No review yet/)
  await view.select({ label: 'Runner' }, 'orca')
  await view.press({ text: 'Run review' })
  assert.deepEqual(calls.at(-1), { name: 'review.start', input: { workspaceId: 'ws1', pr: '', runner: 'orca' } })
  // The worker's posts drive the passes table while it runs.
  assert.equal(view.find({ text: 'Running…' })?.text, 'Running…')
  assert.deepEqual(view.rowsOf({ label: 'Passes' }), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  // Each pass with its agent, a status that reads without color, and its own running time.
  assert.deepEqual(cells(view), [
    ['intent_contracts · codex', '✓ completed', '1m 1s'],
    ['reliability · claude', '● running', 'relative'],
    ['security · codex', '● running', 'relative'],
    ['coordinator', '○ waiting', '']
  ])
  // Progress over the whole run and the time since it started.
  assert.equal(view.find({ type: 'Progress' })?.props?.value, 25)
  assert.match(JSON.stringify(view.tree()), /1 of 4 passes done · elapsed /)
  assert.equal(view.find({ type: 'RelativeTime' })?.props?.at, running.startedAt)
  assert.ok(!/No review yet/.test(view.summary().join('\n')))
  // The post that says done is small; the panel then asks for the result itself.
  await view.post('review.changed', done)
  assert.deepEqual(view.rowsOf({ label: 'Passes' }), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  assert.equal(calls.at(-1)?.name, 'review.result')
  assert.match(JSON.stringify(view.find({ label: 'Findings by file' })), /Terminal leaks when close fails/)
  assert.equal(view.find({ type: 'Progress' }), null)
  assert.match(JSON.stringify(view.tree()), /4 of 4 passes finished in 2m 0s/)
  assert.deepEqual(view.errors, [])
  assert.deepEqual(view.refusals, [])
  await view.unmount()
})

test('a finished review lists findings by file and sends the picked ones', async () => {
  const { host, calls } = setup(done)
  const view = await mount(panel, { host })
  const tree = JSON.stringify(view.find({ label: 'Findings by file' }))
  assert.match(tree, /src/)
  // Severity first, as text, and an icon per severity: never color alone.
  assert.deepEqual(leaves(view), [
    ['P1 · L120-124 · Terminal leaks when close fails', 'error'],
    ['P2 · L40-44 · Budget skips setup', 'warning']
  ])
  assert.ok(!/Unclear error text/.test(tree), 'below the gate stays hidden')
  assert.ok(view.find({ type: 'Badge', text: 'degraded' }))
  // The failed pass is named with its reason, rreview's own prefix cut.
  const failures = JSON.stringify(view.find({ type: 'Callout', text: /1 pass failed/ }))
  assert.match(failures, /security \(codex\)/)
  assert.match(failures, /no reply/)
  assert.ok(!/pass security/.test(failures))
  assert.match(failures, /The coordinator covered the failed lens/)
  assert.match(view.summary().join('\n'), /Callout warning "Patch is incorrect"/)
  assert.deepEqual(view.warnings, [])

  // The most urgent finding opens with its detail: full title, priority as text, a copyable
  // location and a link to its lines at the reviewed head.
  assert.equal(view.find({ label: 'Findings by file' })?.props?.selected, 'src/orca.rs#120#Terminal leaks when close fails')
  assert.equal(view.valueOf({ label: 'File and line (select to copy)' }), 'src/orca.rs:120-124')
  assert.equal(view.find({ label: 'File and line (select to copy)' })?.props?.onChange, undefined)
  assert.equal(
    view.find({ type: 'Link', text: 'Open on GitHub at the reviewed head' })?.props?.href,
    `https://github.com/acme/repo/blob/${'a'.repeat(40)}/src/orca.rs#L120-L124`
  )
  assert.match(JSON.stringify(view.find({ type: 'KeyValue' })), /P1 \(P0 most urgent\)/)

  await view.toggle({ label: 'All 3, below the gate too' })
  assert.deepEqual(leaves(view)[1], ['P3 · L300-304 · Unclear error text', 'info'])

  // Both gated findings start picked; unpick one through its detail.
  await view.selectItem({ label: 'Findings by file' }, 'src/review.rs#40#Budget skips setup')
  assert.match(JSON.stringify(view.tree()), /Budget skips setup\*\*: what fails/)
  await view.toggle({ label: 'Send to the author' })
  // No recipient is chosen for the person: the agents are the reviewed run's (ws1), shells left out.
  const picker = { label: 'Author agent (in the reviewed workspace)' }
  assert.equal(view.valueOf(picker), '')
  assert.equal(view.find({ text: 'Send 1 to author' })?.props?.disabled, true)
  assert.match(JSON.stringify(view.find(picker)), /claude author/)
  assert.ok(!/a shell|other/.test(JSON.stringify(view.find(picker))))
  assert.ok(host.callsOf('agents.list').every((p) => (p as { workspaceId: string }).workspaceId === 'ws1'))
  await view.select(picker, 't1')
  await view.press({ text: 'Send 1 to author' })
  assert.deepEqual(calls.at(-1), {
    name: 'findings.send',
    input: { terminalId: 't1', sessionId: 's1', keys: ['src/orca.rs#120#Terminal leaks when close fails'] }
  })
  // The confirmation sits by the button, names the agent and the file, and outlives the panel.
  assert.match(view.summary().join('\n'), /Callout success "Sent 1 finding to claude author \(waiting\)"/)
  assert.match(JSON.stringify(view.tree()), /author-1\.md/)
  const kept = view.persisted.plugin.sent
  assert.deepEqual(kept, { token: 'run-1', sent: 1, file: '/state/x/author-1.md', to: 'claude author (waiting)' })
  assert.deepEqual(view.errors, [])
  await view.unmount()

  const again = await mount(panel, { host: setup(done).host, persisted: { plugin: { sent: kept } } })
  assert.match(again.summary().join('\n'), /Sent 1 finding/)
  await again.unmount()
  // Another run's send is not this one's.
  const other = await mount(panel, { host: setup(done).host, persisted: { plugin: { sent: { ...(kept as object), token: 'run-0' } } } })
  assert.ok(!/Sent 1 finding/.test(other.summary().join('\n')))
  await other.unmount()
})

test('the author choice is dropped when its terminal runs another agent session', async () => {
  const { host, calls } = setup(done)
  const view = await mount(panel, { host })
  const picker = { label: 'Author agent (in the reviewed workspace)' }
  await view.select(picker, 't1')
  const t1 = AGENTS.ws1[0] as { sessionId: string }
  t1.sessionId = 's2'
  try {
    await view.emit('agent.status.changed', { workspaceId: 'ws1', paneKey: 'p1', terminalId: 't1', state: 'waiting', receivedAt: 1, sessionId: 's2' })
    // The person chose session s1: the replacement is not chosen for them.
    assert.equal(view.find(picker)?.props?.value, undefined)
    assert.equal(view.find({ text: 'Send 2 to author' })?.props?.disabled, true)
    await view.select(picker, 't1')
    await view.press({ text: 'Send 2 to author' })
    assert.equal((calls.at(-1)?.input as { sessionId: string }).sessionId, 's2')
  } finally {
    t1.sessionId = 's1'
  }
  await view.unmount()
})

test('a failed review is shown as failed with its error', async () => {
  const { host } = setup(failed)
  const view = await mount(panel, { host })
  const s = view.summary().join('\n')
  assert.match(s, /The review failed/)
  const callout = JSON.stringify(view.find({ type: 'Callout', text: /The review failed/ }))
  assert.match(callout, /the reply is not JSON/)
  assert.match(callout, /Fix the cause, then press Run review again/)
  assert.match(callout, /11-abc/)
  // Both failed passes, each with its reason.
  const failures = JSON.stringify(view.find({ type: 'Callout', text: /2 passes failed/ }))
  assert.match(failures, /security \(codex\).*no reply/)
  assert.match(failures, /coordinator \(claude\).*the reply is not JSON/)
  assert.deepEqual(cells(view).at(-1), ['coordinator · claude', '✗ failed', '30s'])
  assert.equal(view.find({ label: 'Findings by file' }), null)
  await view.unmount()
})

test('cancel stops the running review, which then shows as cancelled', async () => {
  const { host, calls } = setup(running)
  const view = await mount(panel, { host })
  await view.press({ text: 'Cancel' })
  // The run it shows, so a stale Cancel cannot stop a later one.
  assert.deepEqual(calls.at(-1), { name: 'review.cancel', input: { token: running.token } })
  assert.match(JSON.stringify(view.tree()), /Cancelling: rreview is stopping its reviewers/)
  await view.post('review.changed', cancelled)
  const s = view.summary().join('\n') + JSON.stringify(view.tree())
  assert.match(s, /The review was cancelled/)
  assert.match(s, /Run review starts a new run/)
  assert.match(s, /reliability \(claude\).*no reason recorded/)
  assert.ok(!/Cancelling/.test(s), 'the notice goes with the run')
  assert.equal(view.find({ text: 'Cancel' }), null)
  // The stop line is no pass.
  assert.deepEqual(view.rowsOf({ label: 'Passes' }), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  assert.deepEqual(view.errors, [])
  await view.unmount()
})

test('no findings above the gate says how many are below it and how to see them', async () => {
  const below = { ...doneResult, findings: [], ungated_findings: doneResult.ungated_findings.slice(2) }
  const host = mockHost({
    results: { 'agents.list': { agents: [] } },
    requests: {
      'target.get': () => target,
      'review.get': () => ({ ...done, summary: { gated: 0, all: 1, degraded: false } }),
      'review.result': () => below
    }
  })
  const view = await mount(panel, { host })
  const s = view.summary().join('\n') + JSON.stringify(view.tree())
  assert.match(s, /EmptyState "No findings above the gate"/)
  assert.match(s, /1 below the gate: turn on the switch above to see them/)
  assert.match(s, /no gated findings/)
  await view.toggle({ label: 'All 1, below the gate too' })
  // The one below the gate opens at once.
  assert.equal(view.valueOf({ label: 'File and line (select to copy)' }), 'src/orca.rs:300-304')
  await view.unmount()
})

test('no PR and no workspace each say what to do next', async () => {
  const noPr = await mount(panel, {
    host: mockHost({ requests: { 'target.get': () => ({ ...target, pr: null, error: 'no pull requests found' }), 'review.get': () => null } })
  })
  const callout = JSON.stringify(noPr.find({ type: 'Callout' }))
  assert.match(callout, /no pull requests found/)
  assert.match(callout, /type a PR number or URL below, then press Refresh/)
  assert.equal(noPr.find({ text: 'Run review' })?.props?.disabled, true)
  await noPr.unmount()
  const none = await mount(panel, { host: mockHost({ requests: { 'target.get': () => null, 'review.get': () => null } }) })
  assert.match(JSON.stringify(none.tree()), /Focus a workspace in Orca, then press Refresh/)
  assert.ok(!/No review yet/.test(none.summary().join('\n')))
  await none.unmount()
})

test('the passes table keeps every column in a narrow panel', async () => {
  const view = await mount(panel, { host: setup(done).host })
  assert.deepEqual(view.columnsOf({ label: 'Passes' }, { width: 240 }), ['pass', 'status', 'time'])
  await view.unmount()
})

test('durations read as minutes and seconds', () => {
  assert.equal(panel.duration(9_400), '9s')
  assert.equal(panel.duration(901_917), '15m 2s')
})

test('the file tree folds single-child folders', () => {
  const items = panel.fileTree([
    { title: 'A', body: '', file: 'a/b/c/x.rs', start_line: 1, end_line: 1, priority: 0, confidence_score: 1, stage: 'judge' },
    { title: 'B', body: '', file: 'top.rs', start_line: 2, end_line: 3, priority: 2, confidence_score: 0.8, stage: 'coordinator' }
  ])
  assert.deepEqual(items.map((i) => i.label), ['a/b/c', 'top.rs'])
  assert.equal(items[0].children?.[0].children?.[0].label, 'P0 · L1 · A')
  assert.equal(items[1].children?.[0].label, 'P2 · L2-3 · B')
})
