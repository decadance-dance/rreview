// The panel over a mock host whose requests stand in for src/worker.ts.
import { assert, mockHost, mount, test, type TestPanel } from 'orca/testing'
import { cancelled, done, doneResult, failed, running, target } from './fixtures'
import * as panel from './panel'
import type { Result, Run, Target } from './protocol'

/** Agents per workspace: ws1 is the reviewed run's, ws2 another one. */
const AGENTS: Record<string, unknown[]> = {
  ws1: [
    { terminalId: 't1', title: 'claude author', agent: 'claude', state: 'waiting', stateStartedAt: 0, sessionId: 's1' },
    { terminalId: 't0', title: 'a shell', agent: null, state: 'done', stateStartedAt: 0 }
  ],
  ws2: [{ terminalId: 't9', title: 'other', agent: 'codex', state: 'waiting', stateStartedAt: 0, sessionId: 's9' }]
}

/** Each pass's line as Orca draws its text, in order. */
const passRows = (view: TestPanel) => view.findAll({ type: 'Row' }).filter((n) => String(n.key).startsWith('pass:'))
const cells = (view: TestPanel) => passRows(view).map((n) => n.text)
const roles = (view: TestPanel) => passRows(view).map((n) => String(n.key).slice('pass:'.length))

/** Opens the folded passes of an ended run, so finders see its lines and failures. */
const openPasses = (view: TestPanel) => view.toggle({ label: /Passes · / })

/** The location chip of the open finding. */
const location = (view: TestPanel) => view.find({ type: 'Code', text: /:\d/ })?.text

/** The findings list's items as title, the line under it, and icon. */
const leaves = (view: TestPanel) => view.findAll({ type: 'ListItem' }).map((n) => [n.text, n.props?.icon])
const current = (view: TestPanel) => view.findAll({ type: 'ListItem' }).find((n) => n.props?.current)?.text

const pr12: Target = { ...target, pr: { ...target.pr!, number: 12, title: 'Other', url: 'https://github.com/acme/repo/pull/12' } }

function setup(run: Run | null = null) {
  const calls: { name: string; input: unknown }[] = []
  const host = mockHost({
    results: {
      'agents.list': (p: { workspaceId?: string }) => ({ agents: AGENTS[p?.workspaceId ?? 'ws2'] ?? [] })
    },
    requests: {
      'target.get': (input) => (calls.push({ name: 'target.get', input }), (input as { pr?: string })?.pr === '12' ? pr12 : target),
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
  // The workspace has a PR: Other PR… stays folded, so the header keeps its two rows.
  assert.equal(view.find({ type: 'Collapsible', text: /Other PR/ })?.props?.open, false)
  // Buttons have no pressed state: the toolbar's name says which runner Run review uses.
  assert.equal(view.find({ type: 'Toolbar' })?.props?.label, 'Runner: headless')
  await view.press({ text: 'orca' })
  assert.equal(view.find({ type: 'Toolbar' })?.props?.label, 'Runner: orca')
  assert.match(String(view.find({ type: 'Button', text: 'orca' })?.props?.tooltip), /^Chosen: /)
  assert.doesNotMatch(String(view.find({ type: 'Button', text: 'headless' })?.props?.tooltip), /Chosen/)
  // The chosen runner is solid and checked, so it reads without color.
  assert.equal(view.find({ type: 'Button', text: 'orca' })?.props?.icon, 'check')
  assert.equal(view.find({ type: 'Button', text: 'headless' })?.props?.variant, 'ghost')
  await view.press({ text: 'Run review' })
  assert.deepEqual(calls.at(-1), { name: 'review.start', input: { workspaceId: 'ws1', pr: 'https://github.com/acme/repo/pull/11', runner: 'orca' } })
  // The worker's posts drive the passes table while it runs.
  assert.equal(view.find({ text: 'Running…' })?.text, 'Running…')
  assert.deepEqual(roles(view), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  // Each pass with its agent, a status that reads without color, and its own running time.
  const lines = cells(view)
  assert.equal(lines[0], '✓ intent_contracts codex 1m 1s')
  assert.match(lines[1], /^● reliability claude \S+$/)
  assert.equal(lines[3], '○ coordinator waiting')
  // A running pass's time ticks from its start.
  const ticking = view.findAll({ type: 'RelativeTime' }).map((n) => n.props?.at)
  assert.ok(ticking.includes(running.events[1].ts))
  // Progress over the whole run and the time since it started.
  assert.equal(view.find({ type: 'Progress' })?.props?.value, 25)
  assert.match(JSON.stringify(view.tree()), /1 of 4 passes · /)
  assert.ok(view.find({ type: 'Badge', text: 'running' }))
  assert.ok(ticking.includes(running.startedAt))
  assert.ok(!/No review yet/.test(view.summary().join('\n')))
  // The post that says done is small; the panel then asks for the result itself.
  await view.post('review.changed', done)
  await openPasses(view)
  assert.deepEqual(roles(view), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  assert.equal(calls.at(-1)?.name, 'review.result')
  assert.match(String(leaves(view)[0][0]), /^Terminal leaks when close fails /)
  assert.equal(view.find({ type: 'Progress' }), null)
  // The verdict's one line of counts replaces the status heading.
  assert.match(JSON.stringify(view.find({ type: 'Callout', text: /Patch is incorrect/ })), /2 gated · 1 below · 3\/4 · 2m · .*degraded/)
  assert.equal(view.find({ type: 'Heading', text: /PR #11/ }), null)
  // Run again, for the PR it just reviewed.
  assert.ok(view.find({ text: 'Run again' }))
  assert.deepEqual(view.errors, [])
  assert.deepEqual(view.refusals, [])
  await view.unmount()
})

test('a finished review lists the findings and sends the picked ones', async () => {
  const { host, calls } = setup(done)
  const view = await mount(panel, { host })
  // Full titles; severity as `P<n>` text and an icon per severity, never color alone.
  assert.deepEqual(leaves(view), [
    ['Terminal leaks when close fails P1 · src/orca.rs:120-124 · conf 0.9', 'error'],
    ['Budget skips setup P2 · src/review.rs:40-44 · conf 0.85', 'warning']
  ])
  assert.ok(view.find({ type: 'Text', text: 'degraded' }))
  // Folded by default, as in the mockup; its title counts the failed pass.
  assert.equal(view.find({ type: 'Collapsible', text: /Passes · 3\/4 done, 1 failed/ })?.props?.open, false)
  await openPasses(view)
  // The failed pass is named with its reason, rreview's own prefix cut.
  const failures = JSON.stringify(view.find({ type: 'Callout', text: /1 pass failed/ }))
  assert.match(failures, /security \(codex\)/)
  assert.match(failures, /no reply/)
  assert.ok(!/pass security/.test(failures))
  assert.match(failures, /The coordinator covered the failed lens/)
  assert.match(view.summary().join('\n'), /Callout warning "Patch is incorrect"/)
  assert.deepEqual(view.warnings, [])

  // The most urgent finding opens with its detail: its body, a copyable location and a link to
  // its lines at the reviewed head.
  assert.match(current(view) ?? '', /^Terminal leaks when close fails /)
  assert.equal(location(view), 'src/orca.rs:120-124')
  assert.equal(
    view.find({ type: 'Link', text: 'GitHub ↗' })?.props?.href,
    `https://github.com/acme/repo/blob/${'a'.repeat(40)}/src/orca.rs#L120-L124`
  )
  assert.match(JSON.stringify(view.find({ type: 'Markdown' })), /Terminal leaks when close fails\*\*: what fails/)

  await view.toggle({ label: 'below gate' })
  assert.deepEqual(leaves(view)[2], ['Unclear error text P3 · src/orca.rs:300-304 · conf 0.6', 'info'])

  // Both gated findings start picked; unpick one through its detail.
  await view.press({ type: 'ListItem', text: /Budget skips setup/ })
  assert.match(current(view) ?? '', /^Budget skips setup /)
  assert.match(JSON.stringify(view.tree()), /Budget skips setup\*\*: what fails/)
  await view.toggle({ label: 'Send to the author' })
  // No recipient is chosen for the person: the agents are the reviewed run's (ws1), shells left out.
  const picker = { label: 'Author agent (in the reviewed workspace)' }
  assert.equal(view.valueOf(picker), '')
  assert.equal(view.find({ text: 'Send 1' })?.props?.disabled, true)
  assert.match(JSON.stringify(view.find(picker)), /claude author/)
  assert.ok(!/a shell|other/.test(JSON.stringify(view.find(picker))))
  assert.ok(host.callsOf('agents.list').every((p) => (p as { workspaceId: string }).workspaceId === 'ws1'))
  await view.select(picker, 't1')
  await view.press({ text: 'Send 1' })
  assert.deepEqual(calls.at(-1), {
    name: 'findings.send',
    input: { terminalId: 't1', sessionId: 's1', keys: ['src/orca.rs#120#Terminal leaks when close fails'] }
  })
  // The confirmation, under the Send row, names the agent and the file, and outlives the panel.
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
    assert.equal(view.find({ text: 'Send 2' })?.props?.disabled, true)
    await view.select(picker, 't1')
    await view.press({ text: 'Send 2' })
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
  assert.match(callout, /Fix the cause, then press Run again/)
  assert.match(callout, /11-abc/)
  // Both failed passes, each with its reason.
  await openPasses(view)
  const failures = JSON.stringify(view.find({ type: 'Callout', text: /2 passes failed/ }))
  assert.match(failures, /security \(codex\).*no reply/)
  assert.match(failures, /coordinator \(claude\).*the reply is not JSON/)
  assert.equal(cells(view).at(-1), '✗ coordinator claude failed · 30s')
  assert.equal(view.find({ type: 'List' }), null)
  assert.match(String(view.find({ type: 'Collapsible', text: /Passes · / })?.text), /Passes · 2\/4 done, 2 failed/)
  await view.unmount()
})

test('cancel stops the running review, which then shows as cancelled', async () => {
  const { host, calls } = setup(running)
  const view = await mount(panel, { host })
  await view.press({ label: 'Cancel' })
  // The run it shows, so a stale Cancel cannot stop a later one.
  assert.deepEqual(calls.at(-1), { name: 'review.cancel', input: { token: running.token } })
  assert.match(JSON.stringify(view.tree()), /Cancelling: rreview is stopping its reviewers/)
  await view.post('review.changed', cancelled)
  await openPasses(view)
  const s = view.summary().join('\n') + JSON.stringify(view.tree())
  assert.match(s, /The review was cancelled/)
  assert.match(s, /Run again starts a new run/)
  assert.match(s, /reliability \(claude\).*no reason recorded/)
  assert.ok(!/Cancelling/.test(s), 'the notice goes with the run')
  assert.equal(view.find({ label: 'Cancel' }), null)
  // The stop line is no pass.
  assert.deepEqual(roles(view), ['intent_contracts', 'reliability', 'security', 'coordinator'])
  assert.deepEqual(view.errors, [])
  await view.unmount()
})

test('no findings above the gate says how many are below it and how to see them', async () => {
  const below = { ...doneResult, findings: [], ungated_findings: doneResult.ungated_findings.slice(2), passes: [] }
  const events = done.events.map(({ error: _, ...e }) => (e.status === 'failed' ? { ...e, status: 'completed' } : e))
  const host = mockHost({
    results: { 'agents.list': { agents: [] } },
    requests: {
      'target.get': () => target,
      'review.get': () => ({ ...done, events, summary: { gated: 0, all: 1, degraded: false } }),
      'review.result': () => below
    }
  })
  const view = await mount(panel, { host })
  const s = view.summary().join('\n') + JSON.stringify(view.tree())
  assert.match(s, /EmptyState "No findings above the gate"/)
  assert.match(s, /1 below the gate: turn on the switch above to see them/)
  assert.match(s, /0 gated · 1 below · /)
  // No pass failed: the passes stay folded.
  assert.equal(view.find({ type: 'Collapsible', text: /Passes · / })?.props?.open, false)
  await view.toggle({ label: 'below gate' })
  // The one below the gate opens at once.
  assert.equal(location(view), 'src/orca.rs:300-304')
  await view.unmount()
})

test('a run that failed before any pass says so without a time', async () => {
  const view = await mount(panel, { host: setup({ ...failed, events: [], runDir: null, error: 'cannot run rreview: ENOENT' }).host })
  const s = JSON.stringify(view.tree())
  assert.match(s, /cannot run rreview: ENOENT/)
  assert.ok(!/passes finished/.test(s))
  // No pass failed: the passes stay folded until opened.
  assert.equal(view.find({ type: 'Collapsible', text: /Passes · 0\/4 done/ })?.props?.open, false)
  await view.toggle({ label: /Passes/ })
  assert.equal(cells(view)[0], '– intent_contracts not run')
  await view.unmount()
})

test('a long failure reason shows its start and the rest behind Full reason', async () => {
  const long = `pass reliability (claude): pass reliability timed out after 900s; ${'wrap-up failed. '.repeat(20)}the end`
  const events = running.events.map((e) => (e.role === 'reliability' ? { ...e, status: 'failed', error: long } : e))
  const view = await mount(panel, { host: setup({ ...running, events }).host })
  const callout = view.find({ type: 'Callout', text: /1 pass failed/ })
  const shown = view.findAll({ type: 'Text' }).find((n) => /^reliability \(claude\)/.test(n.text))?.text ?? ''
  assert.match(shown, /^reliability \(claude\) pass reliability timed out after 900s;.*…$/)
  assert.ok(shown.length < 200)
  assert.match(JSON.stringify(view.find({ type: 'Collapsible', text: /Full reason/ })), /Full reason.*the end/)
  assert.ok(callout)
  await view.unmount()
})

/** The panel on a run whose result is `res`. */
const mountRun = (run: Run, res: Result | null = null) =>
  mount(panel, {
    host: mockHost({
      results: { 'agents.list': { agents: [] } },
      requests: { 'target.get': () => target, 'review.get': () => run, 'review.result': () => res }
    })
  })

test('a finished run takes a pass outcome from result.json when its last progress line was lost', async () => {
  // security's `failed` line never reached progress.jsonl; result.json still reports the failure.
  const lost = { ...done, events: done.events.filter((e) => !(e.role === 'security' && e.status === 'failed')) }
  const view = await mountRun(lost, doneResult)
  await openPasses(view)
  assert.equal(cells(view)[2], '✗ security codex failed · 40s')
  assert.match(JSON.stringify(view.find({ type: 'Callout', text: /1 pass failed/ })), /security \(codex\).*no reply/)
  assert.equal(view.findAll({ type: 'RelativeTime' }).length, 0)
  await view.unmount()
})

test('a stopped run shows a pass that never finished as interrupted, with a stopped clock', async () => {
  // In the cancelled fixture security started and has no later line.
  const view = await mountRun(cancelled)
  await openPasses(view)
  assert.equal(cells(view)[2], '■ security codex interrupted · 1m 11s')
  assert.equal(view.findAll({ type: 'RelativeTime' }).length, 0)
  // A done run that lost a pass's last line and has no report for it: unknown, not running.
  const lost = { ...done, events: done.events.filter((e) => !(e.role === 'reliability' && e.status === 'completed')) }
  const again = await mountRun(lost, { ...doneResult, passes: [] })
  await openPasses(again)
  assert.match(cells(again)[1], /^\? reliability claude unknown · /)
  await again.unmount()
})

test('overall progress never reaches 100 % before the judge decision, and never goes back', async () => {
  const { host } = setup(running)
  const view = await mount(panel, { host })
  const progress = () => view.find({ type: 'Progress' })?.props?.value
  const at = (ms: number) => 1_700_000_000_000 + ms
  const ev = (role: string, status: string, agent: string, ms: number) => ({ ts: at(ms), role, status, agent, model: 'default', duration_ms: ms })
  const lensesDone = [...running.events, ev('reliability', 'completed', 'claude', 90_000), ev('security', 'completed', 'codex', 91_000)]
  await view.post('review.changed', { ...running, events: [...lensesDone, ev('coordinator', 'started', 'claude', 0)] })
  assert.equal(progress(), 75)
  const coordinatorDone = [...lensesDone, ev('coordinator', 'completed', 'claude', 100_000)]
  await view.post('review.changed', { ...running, events: coordinatorDone })
  // The judge may or may not run: the bar has no value rather than 100 %.
  assert.equal(progress(), undefined)
  assert.match(JSON.stringify(view.tree()), /4 of 4 passes · /)
  await view.post('review.changed', { ...running, events: [...coordinatorDone, ev('judge', 'started', 'codex', 0)] })
  assert.equal(progress(), undefined)
  assert.match(JSON.stringify(view.tree()), /4 of 4 passes, judge running · /)
  await view.unmount()
})

test('a judge-only failure says the dropped findings were not re-examined, not that a lens was covered', async () => {
  const ev = (role: string, status: string, agent: string, ms: number, error?: string) => ({
    ts: 1_700_000_000_000 + ms, role, status, agent, model: 'default', duration_ms: ms, ...(error ? { error } : {})
  })
  const events = [
    ...['intent_contracts', 'reliability', 'security', 'coordinator'].map((role) => ev(role, 'completed', 'claude', 60_000)),
    ev('judge', 'failed', 'codex', 20_000, 'pass judge (codex): no reply')
  ]
  const res: Result = {
    ...doneResult, degraded: true,
    passes: [{ role: 'judge', agent: 'codex', status: 'failed', duration_ms: 20_000, error: 'pass judge (codex): no reply' }]
  }
  const view = await mountRun({ ...done, events }, res)
  await openPasses(view)
  const callout = JSON.stringify(view.find({ type: 'Callout', text: /1 pass failed/ }))
  assert.match(callout, /judge \(codex\).*no reply/)
  assert.match(callout, /the findings it dropped could not be re-examined/)
  assert.ok(!/covered the failed lens/.test(callout))
  await view.unmount()
})

test('the most urgent finding opens first, whatever order rreview listed them in', async () => {
  // P2 first in the result, P1 second.
  const res: Result = { ...doneResult, findings: [...doneResult.findings].reverse(), ungated_findings: [...doneResult.ungated_findings].reverse() }
  const view = await mountRun(done, res)
  assert.equal(location(view), 'src/orca.rs:120-124')
  await view.unmount()
  // A gate toggle that drops the open finding opens the most urgent one left.
  const below = await mountRun(done, res)
  await below.toggle({ label: 'below gate' })
  // The list is most urgent first too.
  assert.deepEqual(leaves(below).map((l) => String(l[0]).match(/ (P\d) · /)?.[1]), ['P1', 'P2', 'P3'])
  await below.press({ type: 'ListItem', text: /Unclear error text/ })
  await below.toggle({ label: 'below gate' })
  assert.equal(location(below), 'src/orca.rs:120-124')
  await below.unmount()
})

test('no PR and no workspace each say what to do next', async () => {
  const noPr = await mount(panel, {
    host: mockHost({ requests: { 'target.get': () => ({ ...target, pr: null, error: 'no pull requests found' }), 'review.get': () => null } })
  })
  const callout = JSON.stringify(noPr.find({ type: 'Callout' }))
  assert.match(callout, /no pull requests found/)
  assert.match(callout, /type a PR number or URL under Other PR, then press Refresh/)
  assert.equal(noPr.find({ text: 'Run review' })?.props?.disabled, true)
  // Other PR… opens on its own only when the workspace has no PR.
  assert.equal(noPr.find({ type: 'Collapsible', text: /Other PR/ })?.props?.open, true)
  await noPr.unmount()
  const none = await mount(panel, { host: mockHost({ requests: { 'target.get': () => null, 'review.get': () => null } }) })
  assert.match(JSON.stringify(none.tree()), /Focus a workspace in Orca, then press Refresh/)
  assert.ok(!/No review yet/.test(none.summary().join('\n')))
  await none.unmount()
})

test('a run of another PR than the one shown says which PR it reviewed', async () => {
  const other = { ...target, pr: { ...target.pr!, number: 12, title: 'Other', url: 'https://github.com/acme/repo/pull/12' } }
  const view = await mount(panel, { host: mockHost({ results: { 'agents.list': { agents: [] } }, requests: { 'target.get': () => other, 'review.get': () => done, 'review.result': () => doneResult } }) })
  assert.equal(view.findAll({ type: 'Link' }).map((l) => l.props?.href).find((h) => String(h).endsWith('/pull/11')), 'https://github.com/acme/repo/pull/11')
  assert.match(JSON.stringify(view.tree()), /Review of /)
  assert.ok(view.find({ text: 'Run review' }), 'not Run again: the button reviews #12')
  await view.unmount()
  const same = await mountRun(done, doneResult)
  assert.ok(!/Review of /.test(JSON.stringify(same.tree())))
  await same.unmount()
})

test('Run again starts the PR the header shows, not one typed under Other PR and never submitted', async () => {
  const { host, calls } = setup(done)
  const view = await mount(panel, { host })
  await view.toggle({ label: /Other PR/ })
  await view.type({ label: 'PR number or URL' }, '12')
  await view.toggle({ label: /Other PR/ })
  await view.press({ text: 'Run again' })
  assert.deepEqual(calls.at(-1), { name: 'review.start', input: { workspaceId: 'ws1', pr: 'https://github.com/acme/repo/pull/11', runner: 'headless' } })
  // Submitted, the typed PR becomes the shown one, and the one Run review starts.
  await view.post('review.changed', done)
  await view.toggle({ label: /Other PR/ })
  await view.type({ label: 'PR number or URL' }, '', { submit: true })
  await view.press({ text: /Run/ })
  assert.deepEqual(calls.at(-1)?.input, { workspaceId: 'ws1', pr: pr12.pr!.url, runner: 'headless' })
  await view.unmount()
})

test('Passes counts a failure the result reports after the fold appeared, and stays as the person left it', async () => {
  // security's `failed` line never reached progress.jsonl, and the result comes late.
  const lost = { ...done, events: done.events.filter((e) => !(e.role === 'security' && e.status === 'failed')) }
  let deliver: (r: Result) => void = () => {}
  const pending = new Promise<Result>((resolve) => (deliver = resolve))
  const view = await mount(panel, {
    host: mockHost({
      results: { 'agents.list': { agents: [] } },
      requests: { 'target.get': () => target, 'review.get': () => running, 'review.result': () => pending }
    })
  })
  // The fold appears with the done run, folded, before its result reports security's failure.
  // Acts resolve once the panel settles, which waits for the result: none is awaited until it lands.
  const posted = view.post('review.changed', lost)
  await new Promise((r) => setTimeout(r, 20))
  const fold = () => view.find({ type: 'Collapsible', text: /Passes · / })
  assert.equal(fold()?.props?.open, false)
  assert.doesNotMatch(String(fold()?.label), /failed/)
  // Opened by the person while the result is on its way, it stays open when the result lands.
  const opened = openPasses(view)
  await new Promise((r) => setTimeout(r, 20))
  assert.equal(fold()?.props?.open, true)
  deliver(doneResult)
  await Promise.all([posted, opened])
  assert.match(String(fold()?.label), /Passes · 3\/4 done, 1 failed/)
  assert.equal(fold()?.props?.open, true)
  await view.unmount()
})

test('a failed or cancelled run of another PR says how to retry that PR, not Run again', async () => {
  let shown: Target = target
  const pr12: Target = { ...target, workspaceId: 'ws2', pr: { ...target.pr!, number: 12, url: 'https://github.com/acme/repo/pull/12' } }
  for (const run of [failed, cancelled]) {
    shown = target
    const view = await mount(panel, {
      host: mockHost({ results: { 'agents.list': { agents: [] } }, requests: { 'target.get': () => shown, 'review.get': () => run } })
    })
    assert.match(JSON.stringify(view.tree()), /Run again/)
    shown = pr12
    await view.emit('workspace.changed', { context: { workspaceId: 'ws2' } } as never)
    const s = JSON.stringify(view.find({ type: 'Callout', text: /The review (failed|was cancelled)/ }))
    assert.match(s, /To review #11 again, focus its workspace or enter https:\/\/github\.com\/acme\/repo\/pull\/11 under Other PR…, then press Run again\./)
    assert.ok(view.find({ type: 'Button', text: 'Run review' }), 'the button starts #12 for now')
    // Back on #11, the button the text named is there, and it starts #11.
    shown = target
    await view.emit('workspace.changed', { context: { workspaceId: 'ws1' } } as never)
    assert.ok(view.find({ type: 'Button', text: 'Run again' }), run.status)
    assert.doesNotMatch(JSON.stringify(view.find({ type: 'Callout', text: /The review (failed|was cancelled)/ })), /To review #11/)
    await view.unmount()
  }
})

test('findings in files with the same name show their whole path', async () => {
  const twin = (file: string) => ({ ...doneResult.findings[0], file })
  const res: Result = { ...doneResult, findings: [twin('src/index.ts'), twin('tests/index.ts')], ungated_findings: [twin('src/index.ts'), twin('tests/index.ts')] }
  const view = await mountRun(done, res)
  const [a, b] = leaves(view).map((l) => l[0])
  assert.notEqual(a, b)
  assert.match(String(b), / tests\/index\.ts:120-124 /)
  await view.unmount()
})

test('a done run leads with its verdict: a short explanation, Other PR under the passes', async () => {
  const res = { ...doneResult, overall_explanation: 'A failed close leaves a terminal open. The rest says why.' }
  const view = await mountRun(done, res)
  const callout = JSON.stringify(view.find({ type: 'Callout', text: /Patch is incorrect/ }))
  assert.match(callout, /A failed close leaves a terminal open\./)
  assert.equal(view.find({ type: 'Text', text: /The rest says why/ }), null, 'the rest sits behind More')
  await view.toggle({ label: 'More' })
  assert.ok(view.find({ type: 'Text', text: /The rest says why/ }))
  const folds = view.findAll({ type: 'Collapsible' }).map((n) => String(n.label))
  assert.ok(folds.findIndex((l) => l.startsWith('Passes')) < folds.findIndex((l) => l.startsWith('Other PR')), folds.join(' | '))
  await view.unmount()
  // While a run runs, Other PR stays under the PR line, before the run's status.
  const live = await mountRun(running)
  assert.match(String(live.findAll({ type: 'Collapsible' })[0]?.label), /^Other PR/)
  await live.unmount()
})

test('a lead is the first sentence or line', () => {
  assert.deepEqual(panel.lead('One. Two.\nThree'), ['One.', 'Two.\nThree'])
  assert.deepEqual(panel.lead('**Budget skips setup**: what fails and how.'), ['**Budget skips setup**: what fails and how.', ''])
  assert.deepEqual(panel.lead('no stop\n1. a list'), ['no stop', '1. a list'])
  // A leading fence is the whole lead, closed or not, so the preview never shows half of one.
  assert.deepEqual(panel.lead('```rust\nlet a = 1;\n```\nThe call leaks. More.'), ['```rust\nlet a = 1;\n```', 'The call leaks. More.'])
  assert.deepEqual(panel.lead('```\nno close. here'), ['```\nno close. here', ''])
})

test('durations read as minutes and seconds', () => {
  assert.equal(panel.duration(9_400), '9s')
  assert.equal(panel.duration(901_917), '15m 2s')
})

test('More shows a finding body as one Markdown document, so a fenced block stays whole', async () => {
  const body = '```rust\nfn close(t: Terminal) {\n    t.kill();\n}\n```\nThe kill above can fail. Then the terminal leaks.'
  const res: Result = { ...doneResult, findings: [{ ...doneResult.findings[0], body }] }
  const view = await mountRun(done, res)
  const docs = () => view.findAll({ type: 'Markdown' }).map((n) => String(n.props?.content))
  // Folded: the preview is the whole fenced block, never its opening line alone.
  assert.deepEqual(docs(), ['```rust\nfn close(t: Terminal) {\n    t.kill();\n}\n```'])
  await view.toggle({ label: 'More' })
  assert.deepEqual(docs(), [body], 'one document: no rest that starts after the opening fence')
  await view.toggle({ label: 'Less' })
  assert.equal(docs().length, 1)
  await view.unmount()
})

test('a location longer than the chip fits goes in a read-only field to select and copy; GitHub sits under it', async () => {
  const file = `src/${'a'.repeat(80)}.rs`
  const res: Result = { ...doneResult, findings: [{ ...doneResult.findings[0], file }] }
  const view = await mountRun(done, res)
  assert.equal(location(view), undefined, 'no chip: Orca\'s inline Code never wraps')
  assert.equal(view.valueOf({ label: 'File and lines (select to copy)' }), `${file}:120-124`)
  await view.unmount()
  // A short one stays a chip, with GitHub on its own line under it.
  const short = await mountRun(done, doneResult)
  assert.equal(location(short), 'src/orca.rs:120-124')
  assert.equal(short.find({ label: 'File and lines (select to copy)' }), null)
  const detail = short.findAll({ type: 'Row' }).find((n) => /GitHub/.test(String(n.text)) && /orca\.rs:/.test(String(n.text)))
  assert.equal(detail, undefined, 'the chip and the link do not share a row')
  await short.unmount()
})

test('Run again starts the PR the header shows, even after the branch gets another PR', async () => {
  // The worker resolves an empty PR arg itself: the branch's PR at that moment.
  let branchPr = target
  const started: string[] = []
  const view = await mount(panel, {
    host: mockHost({
      results: { 'agents.list': { agents: [] } },
      requests: {
        'target.get': () => branchPr,
        'review.get': () => done,
        'review.result': () => doneResult,
        'review.start': (input: { pr: string }) => {
          const t = input.pr ? [target, pr12].find((x) => x.pr!.url === input.pr || String(x.pr!.number) === input.pr) : branchPr
          started.push(t!.pr!.url)
          return { ...running, pr: t!.pr! }
        }
      }
    })
  })
  assert.ok(view.find({ type: 'Button', text: 'Run again' }))
  branchPr = pr12
  await view.press({ text: 'Run again' })
  assert.deepEqual(started, [target.pr!.url])
  await view.unmount()
})
