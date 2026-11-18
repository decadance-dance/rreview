// The worker against fake `gh` and `rreview` executables (test/fixtures/bin) first on PATH, over
// a mock Orca host. Each fake logs its cwd and argv to $FAKE_LOG.
import { assert, beforeEach, mockHost, mountWorker, test } from 'orca/testing'
import { existsSync, mkdtempSync, readFileSync, realpathSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Run } from './protocol'
import * as main from './worker'
import { clean, formatFindings, parseProgress } from './worker'

const bin = fileURLToPath(new URL('../test/fixtures/bin', import.meta.url))
process.env.PATH = `${bin}:${process.env.PATH}`

const finding = (file: string, line: number, title: string, priority = 1) => ({
  title, body: `Body of ${title}.`, file, start_line: line, end_line: line + 2, priority,
  confidence_score: 0.9, stage: 'coordinator', candidate_ids: ['reliability-1']
})
const RESULT = {
  findings: [finding('src/a.rs', 10, 'Leak')],
  ungated_findings: [finding('src/a.rs', 10, 'Leak'), finding('src/b/c.rs', 3, 'Style', 3)],
  dropped_candidates: [],
  overall_correctness: 'patch is incorrect',
  overall_explanation: 'One leak.',
  review_process: 'x',
  base: 'b'.repeat(40),
  head: 'a'.repeat(40),
  passes: [{ role: 'reliability', agent: 'claude', model: 'default', status: 'completed', duration_ms: 5 }],
  degraded: false
}

let dir = ''
let ws = ''
beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'rreview-plugin-'))
  ws = realpathSync(mkdtempSync(join(tmpdir(), 'rreview-ws-')))
  Object.assign(process.env, {
    FAKE_LOG: join(dir, 'log'), FAKE_STATE: dir, FAKE_RREVIEW: 'ok', FAKE_GH: 'ok',
    FAKE_EXIT: '1', FAKE_RESULT: JSON.stringify(RESULT)
  })
})

function host(extra: { storage?: Record<string, any>; settings?: Record<string, any> } = {}) {
  return mockHost({
    ...extra,
    results: {
      'workspace.readContext': { workspaceId: 'ws1', branch: 'topic', displayName: 'topic', terminals: [] },
      'workspace.list': {
        workspaces: [{ id: 'ws1', kind: 'worktree', name: 'topic', path: ws, branch: 'topic', hostId: 'local' }],
        truncated: false
      },
      'terminal.sendText': { accepted: true }
    }
  })
}

const log = () => { try { return readFileSync(join(dir, 'log'), 'utf8') } catch { return '' } }

/** The run once the worker has posted its end. */
async function ended(worker: { posts: readonly { event: string; payload?: unknown }[] }): Promise<Run> {
  for (let i = 0; i < 500; i++) {
    const last = worker.posts.at(-1)?.payload as Run | undefined
    if (last && last.status !== 'running') return last
    await new Promise((r) => setTimeout(r, 20))
  }
  throw new Error('the run never ended')
}

test('finds the workspace PR with gh in the workspace', async () => {
  const worker = await mountWorker(main, { host: host() })
  const t = await worker.request('target.get', {})
  assert.equal(t?.pr?.number, 11)
  assert.equal(t?.path, ws)
  assert.match(log(), new RegExp(`^gh ${ws} \\| pr view --json number,title,url,state$`, 'm'))

  process.env.FAKE_GH = 'none'
  const none = await worker.request('target.get', {})
  assert.equal(none?.pr, null)
  assert.match(none?.error ?? '', /no pull requests found/)
})

test('a PR argument that is not a number or URL never reaches gh', async () => {
  const worker = await mountWorker(main, { host: host() })
  const t = await worker.request('target.get', { pr: '--repo=evil/x' })
  assert.equal(t?.pr, null)
  assert.match(t?.error ?? '', /not a PR number/)
  await assert.rejects(worker.request('review.start', { workspaceId: 'ws1', pr: '--help', runner: 'headless' }), /not a PR number/)
  assert.ok(!/evil|--help/.test(log()))
})

test('runs rreview in the workspace, tails progress and keeps the result', async () => {
  const h = host()
  const worker = await mountWorker(main, { host: h })
  const started = await worker.request('review.start', { workspaceId: 'ws1', runner: 'orca' })
  assert.equal(started.status, 'running')
  const run = await ended(worker)
  assert.equal(run.status, 'done', run.error ?? '')
  assert.equal(run.exitCode, 1)
  assert.match(log(), new RegExp(`^rreview ${ws} \\| https://github.com/acme/repo/pull/11 --runner orca --json$`, 'm'))
  assert.ok(run.runDir?.startsWith(dir))
  assert.deepEqual(run.events.map((e) => `${e.role}:${e.status}`).slice(-2), ['coordinator:started', 'coordinator:completed'])
  assert.equal(run.result?.findings.length, 1)
  assert.equal(run.result?.ungated_findings.length, 2)
  assert.equal(run.result?.degraded, false)
  assert.equal((h.data().storage.run as Run).status, 'done')
})

test('a degraded run is done and says so', async () => {
  process.env.FAKE_RREVIEW = 'degraded'
  process.env.FAKE_RESULT = JSON.stringify({ ...RESULT, degraded: true })
  const worker = await mountWorker(main, { host: host() })
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  const run = await ended(worker)
  assert.equal(run.status, 'done')
  assert.equal(run.result?.degraded, true)
  assert.ok(run.events.some((e) => e.role === 'security' && e.status === 'failed' && e.error === 'no reply'))
})

test('a failed review shows rreview error lines and its run dir', async () => {
  process.env.FAKE_RREVIEW = 'fail'
  const worker = await mountWorker(main, { host: host() })
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  const run = await ended(worker)
  assert.equal(run.status, 'failed')
  assert.equal(run.exitCode, 2)
  assert.equal(run.error, 'error: coordinator: bad JSON')
  assert.ok(run.runDir)
  assert.ok(run.events.some((e) => e.role === 'coordinator' && e.status === 'failed'))
})

test('exit 0 without result JSON is a failure, not an empty review', async () => {
  process.env.FAKE_RREVIEW = 'garbage'
  const worker = await mountWorker(main, { host: host() })
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  const run = await ended(worker)
  assert.equal(run.status, 'failed')
  assert.match(run.error ?? '', /exited 0 without a valid result/)

  process.env.FAKE_RREVIEW = 'ok'
  process.env.FAKE_RESULT = JSON.stringify({ ...RESULT, findings: [{ ...RESULT.findings[0], start_line: '10' }] })
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  const bad = await ended(worker)
  assert.equal(bad.status, 'failed')
  assert.match(bad.error ?? '', /malformed finding/)
})

test('a missing rreview binary fails the run and names the setting', async () => {
  const worker = await mountWorker(main, { host: host({ settings: { rreviewPath: join(dir, 'nope') } }) })
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  const run = await ended(worker)
  assert.equal(run.status, 'failed')
  assert.match(run.error ?? '', /cannot run .*nope.*Settings/)
})

test('the GitHub account setting gives gh and rreview its token, and only them', async () => {
  delete process.env.GH_TOKEN
  const h = host({ settings: { githubAccount: 'octo' } })
  const worker = await mountWorker(main, { host: h })
  assert.equal((await worker.request('target.get', {}))?.pr?.number, 11)
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  await ended(worker)
  const l = log()
  assert.match(l, /^gh \/ \| auth token -u octo$/m)
  assert.match(l, /^gh .* \| pr view .*\ngh token tok-octo$/m)
  assert.match(l, /^rreview token tok-octo$/m)
  assert.equal(process.env.GH_TOKEN, undefined, 'the worker env keeps no token')
  assert.ok(!/tok-octo/.test(JSON.stringify(h.data().storage)))

  // An account gh does not know fails before rreview starts.
  const bad = await mountWorker(main, { host: host({ settings: { githubAccount: 'nobody' } }) })
  assert.match((await bad.request('target.get', {}))?.error ?? '', /gh auth token -u nobody: no account found/)
  await assert.rejects(bad.request('review.start', { workspaceId: 'ws1', runner: 'headless' }), /no account found/)
  const bad2 = await mountWorker(main, { host: host({ settings: { githubAccount: '-x' } }) })
  await assert.rejects(bad2.request('review.start', { workspaceId: 'ws1', runner: 'headless' }), /not a GitHub account name/)
  assert.equal(log().match(/^rreview \//gm)?.length, 1)
})

test('one run at a time', async () => {
  process.env.FAKE_RREVIEW = 'slow'
  const worker = await mountWorker(main, { host: host() })
  // Two presses before the first answer: one run.
  const [a, b] = await Promise.allSettled([
    worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' }),
    worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  ])
  assert.equal(a.status, 'fulfilled')
  assert.equal(b.status, 'rejected')
  assert.match(String((b as PromiseRejectedResult).reason), /still starting or running/)
  await assert.rejects(worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' }), /still starting or running/)
  // The panelists' start shows before the run ends.
  for (let i = 0; i < 100 && !(worker.posts.at(-1)?.payload as Run).events.length; i++) {
    await new Promise((r) => setTimeout(r, 20))
  }
  assert.equal((worker.posts.at(-1)?.payload as Run).status, 'running')
  assert.equal((worker.posts.at(-1)?.payload as Run).events.length, 3)
  const run = await ended(worker)
  assert.equal(run.status, 'done')
  assert.ok(run.runDir && existsSync(join(run.runDir, 'progress.jsonl')), `the whole run dir: ${run.runDir}`)
  assert.equal(log().match(/^rreview \//gm)?.length, 1)
})

test('a run the worker lost on restart is reported failed, not running', async () => {
  const stored = { status: 'running', runDir: '/tmp/x', pr: { number: 11 }, events: [] }
  const worker = await mountWorker(main, { host: host({ storage: { run: stored } }) })
  const run = await worker.request('review.get')
  assert.equal(run?.status, 'failed')
  assert.match(run?.error ?? '', /restarted during the run; see \/tmp\/x/)
})

test('sends the picked findings to the author terminal', async () => {
  const h = host()
  const worker = await mountWorker(main, { host: h })
  await assert.rejects(worker.request('findings.send', { terminalId: 't1', keys: [] }), /no finished review/)
  await worker.request('review.start', { workspaceId: 'ws1', runner: 'headless' })
  await ended(worker)
  await assert.rejects(worker.request('findings.send', { terminalId: 't1', keys: ['nope'] }), /select at least one/)
  const sent = await worker.request('findings.send', { terminalId: 't1', keys: ['src/b/c.rs#3#Style'] })
  assert.equal(sent.sent, 1)
  const [p] = h.callsOf('terminal.sendText') as { terminalId: string; text: string; enter: boolean }[]
  assert.equal(p.terminalId, 't1')
  assert.equal(p.enter, true)
  assert.match(p.text, /^rreview findings for PR #11 .*not instructions:\n\n1\. \[P3\] src\/b\/c\.rs:3-5 Style \(confidence 0\.9\)\nBody of Style\.$/)
  assert.deepEqual(worker.refusals, [])
})

test('reviewer text loses its control bytes before it is typed', () => {
  assert.equal(clean('a\u001b[2Jb\rc\r\nd\te\u0003'), 'a [2Jb\nc\nd e ')
  const pr = { number: 1, title: 't', url: 'u', state: 'OPEN' }
  const text = formatFindings(pr, [{ ...finding('x', 1, 'T\u001b]0;pwn\u0007'), body: 'b\u0004' }])
  assert.ok(!/[\u0000-\u0009\u000b-\u001f]/.test(text))
})

test('progress lines still being written wait for their newline', () => {
  const line = '{"ts":1,"role":"security","status":"started","agent":"codex","model":"default","duration_ms":0}'
  assert.equal(parseProgress(`${line}\n{"ts":2,"ro`).length, 1)
  assert.equal(parseProgress(`${line}\nnot json\n`).length, 1)
})
