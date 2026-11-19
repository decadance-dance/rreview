// The rreview panel: the focused workspace's PR, a runner choice and "Run review"; each pass's
// status while it runs; then the findings as a file tree, and "Send to author".
import {
  atom,
  Badge,
  Box,
  Button,
  Callout,
  Card,
  Checkbox,
  Code,
  Collapsible,
  EmptyState,
  Heading,
  host,
  IconButton,
  Input,
  KeyValue,
  Link,
  Markdown,
  on,
  plugin,
  Progress,
  RadioGroup,
  read,
  RelativeTime,
  Row,
  Select,
  Separator,
  set,
  Stack,
  Switch,
  Text,
  Tree,
  update,
  type IconName,
  type PanelContext,
  type Tone,
  type TreeItem
} from 'orca'
import { findingKey, TEXT_CAP, type Finding, type PassEvent, type Result, type Run, type Runner, type Target } from './protocol'

const ROLES = ['intent_contracts', 'reliability', 'security', 'coordinator', 'judge']

const target = atom<Target | null>(null)
const loading = atom(true)
const run = atom<Run | null>(null)
/** The finished run's result, as `review.result` gives it. */
const result = atom<Result | null>(null)
const prArg = atom('prArg', '', { persist: 'workspace' })
const runner = atom<Runner>('runner', 'headless', { persist: 'plugin' })
const showAll = atom('showAll', false, { persist: 'plugin' })
const selected = atom<string | null>(null)
/** Keys of the findings "Send to author" types. */
const picked = atom<string[]>([])
/** Agents in the reviewed run's workspace; `session` is what `findings.send` checks. */
const agents = atom<{ value: string; label: string; session: string | null }[]>([])
/** The terminal the person chose; never picked for them. */
const author = atom<{ terminalId: string; sessionId: string | null } | null>(null)
const notice = atom<{ tone: Tone; text: string } | null>(null)
/** The last "Send to author" of a run, kept so a reopened panel still shows this run's were sent. */
const sent = atom<{ token: string; sent: number; file: string; to: string } | null>('sent', null, { persist: 'plugin' })

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e))

async function loadTarget(): Promise<void> {
  set(loading, true)
  try {
    set(target, await plugin.request('target.get', { pr: read(prArg) }, { timeoutMs: 60_000 }))
  } catch (e) {
    set(notice, { tone: 'danger', text: errorText(e) })
  } finally {
    set(loading, false)
  }
}

async function loadAgents(): Promise<void> {
  const workspaceId = read(run)?.workspaceId
  try {
    const list = workspaceId ? (await host.call('agents.list', { workspaceId })).agents : []
    const options = list.flatMap((a) =>
      a.terminalId && a.agent
        ? [{ value: a.terminalId, label: `${a.title ?? a.agent} (${a.state})`, session: a.sessionId ?? null }]
        : []
    )
    set(agents, options)
  } catch {
    set(agents, [])
  }
  // The choice is a terminal and the agent session it ran when chosen: a change of either drops it.
  const a = read(author)
  if (!read(agents).some((o) => o.value === a?.terminalId && o.session === a.sessionId)) set(author, null)
}

async function showRun(r: Run | null): Promise<void> {
  const was = read(run)
  set(run, r)
  if (was?.status === 'running' && r?.status !== 'running') set(notice, null)
  const fresh = !was || !r || was.token !== r.token
  if (fresh) {
    set(result, null)
    set(author, null)
    void loadAgents()
  }
  // A newly finished review: fetch its result, with its gated findings picked.
  if (r?.status === 'done' && (fresh || was?.status !== 'done')) {
    const res = await plugin.request('review.result')
    if (read(run)?.token !== r.token) return
    set(result, res)
    set(picked, res ? res.findings.map(findingKey) : [])
    // The first finding shown (rreview lists the most urgent first) opens with its detail.
    const first = res && (read(showAll) ? res.ungated_findings : res.findings)[0]
    set(selected, first ? findingKey(first) : null)
  }
}

async function startReview(): Promise<void> {
  const t = read(target)
  if (!t) return
  set(notice, null)
  try {
    showRun(await plugin.request('review.start', { workspaceId: t.workspaceId, pr: read(prArg), runner: read(runner) }, { timeoutMs: 60_000 }))
  } catch (e) {
    set(notice, { tone: 'danger', text: errorText(e) })
  }
}

async function cancelReview(): Promise<void> {
  const r = read(run)
  if (!r) return
  try {
    await plugin.request('review.cancel', { token: r.token })
    set(notice, { tone: 'neutral', text: 'Cancelling: rreview is stopping its reviewers and cleaning up.' })
  } catch (e) {
    set(notice, { tone: 'danger', text: errorText(e) })
  }
}

async function sendToAuthor(): Promise<void> {
  const a = read(author)
  if (!a) return
  const { terminalId, sessionId } = a
  const token = read(run)?.token
  try {
    const { sent: n, file } = await plugin.request('findings.send', { terminalId, sessionId, keys: read(picked) })
    const to = read(agents).find((o) => o.value === terminalId)?.label ?? 'the author'
    if (token) set(sent, { token, sent: n, file, to })
  } catch (e) {
    set(notice, { tone: 'danger', text: errorText(e) })
  }
}

export function setup(ctx: PanelContext): void {
  ctx.subscriptions.push(plugin.on('review.changed', (r) => void showRun(r)))
  let workspaceId: string | undefined
  ctx.subscriptions.push(
    on('workspace.changed', ({ context }) => {
      if (context?.workspaceId !== workspaceId) {
        workspaceId = context?.workspaceId
        void loadTarget()
      }
    })
  )
  ctx.subscriptions.push(on('agent.status.changed', () => void loadAgents()))
  void plugin.request('review.get').then(showRun)
  void loadTarget()
}

// The directory tree with single-child folders folded together follows buildTree() in
// orca-pr-review's ui/app.js (https://github.com/Geromendez135/orca-pr-review),
// Copyright (c) 2026 Geromendez135, MIT License; adapted to Orca's Tree items and findings.
const lines = (f: Finding) => `${f.start_line}${f.end_line > f.start_line ? `-${f.end_line}` : ''}`
/** Severity is told by the `P<n>` text and the icon's shape, never by color alone. */
const severityIcon = (priority: number): IconName => (priority <= 1 ? 'error' : priority === 2 ? 'warning' : 'info')

type Dir = { name: string; path: string; dirs: Map<string, Dir>; files: Map<string, Finding[]> }

export function fileTree(findings: Finding[]): TreeItem[] {
  const root: Dir = { name: '', path: '', dirs: new Map(), files: new Map() }
  for (const f of findings) {
    const parts = f.file.split('/')
    let node = root
    for (const part of parts.slice(0, -1)) {
      const path = node.path ? `${node.path}/${part}` : part
      if (!node.dirs.has(part)) node.dirs.set(part, { name: part, path, dirs: new Map(), files: new Map() })
      node = node.dirs.get(part)!
    }
    node.files.set(f.file, [...(node.files.get(f.file) ?? []), f])
  }
  const items = (node: Dir): TreeItem[] => [
    ...[...node.dirs.values()]
      .sort((a, b) => a.name.localeCompare(b.name))
      .map((d) => {
        let c = d
        let name = d.name
        while (c.files.size === 0 && c.dirs.size === 1) {
          c = [...c.dirs.values()][0]
          name = `${name}/${c.name}`
        }
        return { key: `dir:${c.path}`, label: name, icon: 'folder' as const, children: items(c) }
      }),
    ...[...node.files.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([file, fs]) => ({
        key: `file:${file}`,
        label: file.split('/').pop() ?? file,
        description: `${fs.length}`,
        icon: 'file' as const,
        children: fs
          .sort((a, b) => a.start_line - b.start_line)
          .map((f) => ({
            key: findingKey(f),
            label: `P${f.priority} · L${lines(f)} · ${f.title}`,
            icon: severityIcon(f.priority)
          }))
      }))
  ]
  return items(root)
}

const allKeys = (items: readonly TreeItem[]): string[] =>
  items.flatMap((i) => (i.children ? [i.key, ...allKeys(i.children)] : []))

/** `95s` as `1m 35s`. */
export function duration(ms: number): string {
  const s = Math.round(ms / 1000)
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`
}

/** Each pass's last event, in pipeline order; the judge only once it ran. */
function passes(r: Run): { role: string; e?: PassEvent }[] {
  // The `run` line records a stop; it is no pass.
  const last = new Map(r.events.filter((e) => e.role !== 'run').map((e) => [e.role, e]))
  const roles = [...ROLES.filter((x) => x !== 'judge' || last.has('judge')), ...[...last.keys()].filter((x) => !ROLES.includes(x))]
  return roles.map((role) => ({ role, e: last.get(role) }))
}

const finished = (e?: PassEvent) => e?.status === 'completed' || e?.status === 'failed'
/** A glyph and a word: the status reads without color. */
const STATUS: Record<string, string> = { completed: '✓ completed', failed: '✗ failed', started: '● running', wrapup: '● wrapping up' }
/** Why a pass failed, without rreview's `pass <role> (<agent>): ` prefix the callout already shows. */
const reason = (e?: PassEvent) => (e?.error ?? 'no reason recorded').replace(/^pass \S+ \(\S+\): /, '').slice(0, TEXT_CAP)

const STATUS_TONE: Record<string, Tone> = { completed: 'success', failed: 'danger', started: 'accent', wrapup: 'accent' }

/** One line per pass, wrapping rather than cutting at any panel width: role and agent, then
 * the status and the time (a running pass's ticks). */
function PassRows({ r }: { r: Run }) {
  return (
    <Box border radius="md" paddingX="sm" paddingY="xs">
      <Stack gap="xs">
        {passes(r).map(({ role, e }) => (
          <Row key={`pass:${role}`} justify="between" align="center" gap="xs" wrap>
            <Text size="sm">
              {role} <Text size="sm" muted>{e?.agent ?? ''}</Text>
            </Text>
            {e ? (
              <Text size="sm" tone={STATUS_TONE[e.status]}>
                {`${STATUS[e.status] ?? e.status} · `}
                {finished(e) ? duration(e.duration_ms) : <RelativeTime at={e.ts - e.duration_ms} format="duration" />}
              </Text>
            ) : (
              <Text size="sm" muted>{r.status === 'running' ? '○ waiting' : '– not run'}</Text>
            )}
          </Row>
        ))}
      </Stack>
    </Box>
  )
}

/** A failed pass's reason: the start in the callout, all of it behind "Full reason" when long. */
function Failure({ role, e }: { role: string; e?: PassEvent }) {
  const text = reason(e)
  const long = text.length > 160
  return (
    <Stack gap="xs">
      <Text size="sm">
        <Code>{`${role} (${e?.agent})`}</Code> {long ? `${text.slice(0, 160)}…` : text}
      </Text>
      {long ? (
        <Collapsible title="Full reason">
          <Text size="sm">{text}</Text>
        </Collapsible>
      ) : null}
    </Stack>
  )
}

function Header() {
  const t = read(target)
  return (
    <Stack gap="sm">
      <Row justify="between" align="center">
        <Heading>Rreview</Heading>
        <IconButton icon="refresh" label="Refresh the PR" variant="ghost" size="sm" onPress={loadTarget} />
      </Row>
      {read(loading) ? (
        <Text muted>Looking up the PR…</Text>
      ) : !t ? (
        <EmptyState title="No workspace focused" icon="folder" description="Focus a workspace in Orca, then press Refresh." />
      ) : t.pr ? (
        <Text weight="semibold">
          <Link href={t.pr.url}>#{t.pr.number}</Link> {t.pr.title} <Badge>{t.pr.state}</Badge>
        </Text>
      ) : (
        <Callout tone="warning" title={`No PR for ${t.name}`}>
          <Text size="sm">{t.error ?? ''}</Text>
          <Text size="sm">Open a PR for this branch, or type a PR number or URL below, then press Refresh.</Text>
        </Callout>
      )}
      <Input
        label="PR number or URL"
        value={read(prArg)}
        placeholder="empty: the workspace's PR"
        onChange={(v) => set(prArg, v)}
        onSubmit={() => void loadTarget()}
      />
    </Stack>
  )
}

function Controls() {
  const r = read(run)
  const running = r?.status === 'running'
  return (
    <Row gap="sm" align="center" wrap>
      <RadioGroup
        label="Runner"
        orientation="horizontal"
        value={read(runner)}
        options={[
          { value: 'headless', label: 'headless' },
          { value: 'orca', label: 'orca terminals' }
        ]}
        onChange={(v) => set(runner, v as Runner)}
      />
      <Button icon="play" tone="accent" disabled={running || !read(target)?.pr} onPress={startReview}>
        {running ? 'Running…' : 'Run review'}
      </Button>
      {running ? (
        <Button icon="stop" onPress={cancelReview}>
          Cancel
        </Button>
      ) : null}
    </Row>
  )
}

function StatusBadge({ r }: { r: Run }) {
  if (r.status === 'running') return <Badge tone="accent">running</Badge>
  if (r.status === 'failed') return <Badge tone="danger">failed</Badge>
  if (r.status === 'cancelled') return <Badge tone="neutral">cancelled</Badge>
  const gated = r.summary?.gated ?? 0
  return <Badge tone={gated ? 'warning' : 'success'}>{gated ? `${gated} gated finding${gated === 1 ? '' : 's'}` : 'no gated findings'}</Badge>
}

function Status({ r }: { r: Run }) {
  const res = read(result)
  const list = passes(r)
  const done = list.filter(({ e }) => finished(e)).length
  const failures = list.filter(({ e }) => e?.status === 'failed')
  const running = r.status === 'running'
  const last = Math.max(r.startedAt, ...r.events.map((e) => e.ts))
  return (
    <Stack gap="sm">
      <Separator />
      <Row gap="xs" align="center" wrap>
        <Heading level={3}>{`PR #${r.pr.number} · ${r.runner === 'orca' ? 'orca terminals' : 'headless'}`}</Heading>
        <StatusBadge r={r} />
        {r.summary?.degraded ? <Badge tone="warning">degraded</Badge> : null}
      </Row>
      {running ? <Progress label="Passes done" labelHidden value={Math.round((100 * done) / list.length)} /> : null}
      {running ? (
        <Text size="sm" muted>
          {`${done} of ${list.length} passes done · elapsed `}
          <RelativeTime at={r.startedAt} format="duration" />
        </Text>
      ) : r.events.length ? (
        <Text size="sm" muted>{`${done} of ${list.length} passes finished in ${duration(last - r.startedAt)}`}</Text>
      ) : null}
      <PassRows r={r} />
      {failures.length ? (
        <Callout tone="warning" title={`${failures.length} pass${failures.length === 1 ? '' : 'es'} failed`}>
          {failures.map(({ role, e }) => (
            <Failure key={`failure:${role}`} role={role} e={e} />
          ))}
          {res?.degraded ? <Text size="sm">The coordinator covered the failed lens, so the findings still stand.</Text> : null}
        </Callout>
      ) : null}
      {r.status === 'failed' ? (
        <Callout tone="danger" title="The review failed">
          <Text size="sm">{r.error ?? ''}</Text>
          <Text size="sm">Fix the cause, then press Run review again.</Text>
          {r.runDir ? (
            <Text size="xs" muted>
              Run dir: <Code>{r.runDir}</Code>
            </Text>
          ) : null}
        </Callout>
      ) : null}
      {r.status === 'cancelled' ? (
        <Callout tone="neutral" title="The review was cancelled">
          <Text size="sm">rreview stopped its reviewers and ran its cleanup before it exited. Run review starts a new run.</Text>
          {r.runDir ? (
            <Text size="xs" muted>
              Run dir: <Code>{r.runDir}</Code>
            </Text>
          ) : null}
        </Callout>
      ) : null}
      {res ? (
        <Callout
          tone={res.overall_correctness === 'patch is correct' ? 'success' : 'warning'}
          icon={res.overall_correctness === 'patch is correct' ? 'check' : 'warning'}
          title={res.overall_correctness.charAt(0).toUpperCase() + res.overall_correctness.slice(1)}
        >
          <Text size="sm">{res.overall_explanation}</Text>
        </Callout>
      ) : null}
    </Stack>
  )
}

/** The finding's lines on GitHub at the reviewed head, from the PR's URL. */
function githubUrl(prUrl: string, head: string, f: Finding): string | null {
  const repo = prUrl.match(/^(https:\/\/[^/]+\/[^/]+\/[^/]+)\/pull\/\d+/)?.[1]
  if (!repo || !/^[0-9a-f]{40}$/.test(head)) return null
  const path = f.file.split('/').map(encodeURIComponent).join('/')
  return `${repo}/blob/${head}/${path}#L${f.start_line}${f.end_line > f.start_line ? `-L${f.end_line}` : ''}`
}

function Detail({ f, r, res }: { f: Finding; r: Run; res: Result }) {
  const key = findingKey(f)
  const url = githubUrl(r.pr.url, res.head, f)
  return (
    <Card>
      <Stack gap="sm">
        <Text weight="semibold">{f.title}</Text>
        <KeyValue
          label="Finding"
          items={[
            { label: 'Priority', value: `P${f.priority} (P0 most urgent)`, tone: f.priority <= 1 ? 'danger' : 'warning' },
            { label: 'Confidence', value: f.confidence_score },
            { label: 'Stage', value: f.stage }
          ]}
        />
        <Input label="File and line (select to copy)" value={`${f.file}:${lines(f)}`} />
        {url ? (
          <Text size="sm">
            <Link href={url}>Open on GitHub at the reviewed head</Link>
          </Text>
        ) : null}
        <Markdown content={f.body} />
        <Checkbox
          label="Send to the author"
          checked={read(picked).includes(key)}
          onChange={(on) => update(picked, (k) => (on ? [...k, key] : k.filter((x) => x !== key)))}
        />
      </Stack>
    </Card>
  )
}

function Findings({ r }: { r: Run }) {
  const res = read(result)
  if (r.status !== 'done') return null
  if (!res) return <Text muted>The result is not available: its run dir is gone or was replaced by a later run.</Text>
  const all = read(showAll)
  const list = all ? res.ungated_findings : res.findings
  const items = fileTree(list)
  const sel = list.find((f) => findingKey(f) === read(selected))
  const keys = read(picked)
  const below = res.ungated_findings.length - res.findings.length
  const s = read(sent)
  const toggle = (v: boolean) => {
    set(showAll, v)
    const next = v ? res.ungated_findings : res.findings
    if (!next.some((f) => findingKey(f) === read(selected))) set(selected, next[0] ? findingKey(next[0]) : null)
  }
  return (
    <Stack gap="sm">
      <Separator />
      <Row justify="between" align="center" wrap>
        <Heading level={3}>
          Findings <Badge>{`${list.length}`}</Badge>
        </Heading>
        <Switch label={`All ${res.ungated_findings.length}, below the gate too`} checked={all} onChange={toggle} />
      </Row>
      {res.omitted ? (
        <Text size="xs" muted>
          {`${res.omitted} more findings are not shown here (Orca's message limits); see result.json in the run dir.`}
        </Text>
      ) : null}
      {list.length ? (
        <Tree
          label="Findings by file"
          items={items}
          defaultExpanded={allKeys(items)}
          selected={read(selected) ?? undefined}
          maxHeight="md"
          onSelect={({ key }) => set(selected, key)}
        />
      ) : (
        <EmptyState
          title={all ? 'No findings' : 'No findings above the gate'}
          icon="check"
          description={!all && below ? `${below} below the gate: turn on the switch above to see them.` : 'The reviewers reported nothing to fix.'}
        />
      )}
      {sel ? <Detail f={sel} r={r} res={res} /> : null}
      <Row gap="sm" align="end" wrap>
        <Select
          label="Author agent (in the reviewed workspace)"
          placeholder={read(agents).length ? 'Choose the author agent' : 'No agent in the reviewed workspace'}
          value={read(author)?.terminalId ?? undefined}
          options={read(agents).map(({ value, label }) => ({ value, label }))}
          onChange={(v) => set(author, { terminalId: v, sessionId: read(agents).find((o) => o.value === v)?.session ?? null })}
        />
        <Button icon="next" disabled={!keys.length || !read(author)} onPress={sendToAuthor}>
          {`Send ${keys.length} to author`}
        </Button>
      </Row>
      {s?.token === r.token ? (
        <Callout tone="success" icon="check" title={`Sent ${s.sent} finding${s.sent === 1 ? '' : 's'} to ${s.to}`}>
          <Text size="xs" muted>
            The agent was asked to read <Code>{s.file}</Code>
          </Text>
        </Callout>
      ) : null}
    </Stack>
  )
}

export default function Panel() {
  const r = read(run)
  const n = read(notice)
  return (
    <Stack gap="md">
      <Header />
      <Controls />
      {n ? <Callout tone={n.tone}>{n.text}</Callout> : null}
      {r ? (
        <Status r={r} />
      ) : read(target)?.pr ? (
        <EmptyState
          icon="search"
          title="No review yet"
          description="Run review starts three lenses, a coordinator and, when it drops findings, a judge on this PR. Each pass shows here while it runs, then the findings by file."
        />
      ) : null}
      {r ? <Findings r={r} /> : null}
    </Stack>
  )
}
