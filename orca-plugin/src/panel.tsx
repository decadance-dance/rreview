// The rreview panel: the focused workspace's PR, a runner choice and "Run review"; each pass's
// status while it runs; then the findings as a file tree, and "Send to author".
import {
  atom,
  Badge,
  Button,
  Callout,
  Checkbox,
  EmptyState,
  Heading,
  host,
  Input,
  Link,
  Markdown,
  on,
  plugin,
  RadioGroup,
  read,
  Row,
  Select,
  set,
  Stack,
  Switch,
  Table,
  Text,
  Tree,
  update,
  type PanelContext,
  type TableRow,
  type Tone,
  type TreeItem
} from 'orca'
import { findingKey, type Finding, type Run, type Runner, type Target } from './protocol'

const ROLES = ['intent_contracts', 'reliability', 'security', 'coordinator', 'judge']

const target = atom<Target | null>(null)
const loading = atom(true)
const run = atom<Run | null>(null)
const prArg = atom('prArg', '', { persist: 'workspace' })
const runner = atom<Runner>('runner', 'headless', { persist: 'plugin' })
const showAll = atom('showAll', false, { persist: 'plugin' })
const selected = atom<string | null>(null)
/** Keys of the findings "Send to author" types. */
const picked = atom<string[]>([])
const agents = atom<{ value: string; label: string }[]>([])
const author = atom<string | null>(null)
const notice = atom<{ tone: Tone; text: string } | null>(null)

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
  try {
    const list = await host.call('agents.list')
    const options = list.agents.flatMap((a) =>
      a.terminalId ? [{ value: a.terminalId, label: `${a.title ?? a.agent ?? 'agent'} (${a.state})` }] : []
    )
    set(agents, options)
    if (!options.some((o) => o.value === read(author))) set(author, options[0]?.value ?? null)
  } catch {
    set(agents, [])
  }
}

function showRun(r: Run | null): void {
  const was = read(run)
  set(run, r)
  // A newly finished review starts with its gated findings picked.
  if (r?.status === 'done' && r.result && (was?.status !== 'done' || was.startedAt !== r.startedAt)) {
    set(picked, r.result.findings.map(findingKey))
    set(selected, null)
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

async function sendToAuthor(): Promise<void> {
  const terminalId = read(author)
  if (!terminalId) return
  try {
    const { sent } = await plugin.request('findings.send', { terminalId, keys: read(picked) })
    set(notice, { tone: 'success', text: `Sent ${sent} finding${sent === 1 ? '' : 's'} to the author.` })
  } catch (e) {
    set(notice, { tone: 'danger', text: errorText(e) })
  }
}

export function setup(ctx: PanelContext): void {
  ctx.subscriptions.push(plugin.on('review.changed', showRun))
  let workspaceId: string | undefined
  ctx.subscriptions.push(
    on('workspace.changed', ({ context }) => {
      void loadAgents()
      if (context?.workspaceId !== workspaceId) {
        workspaceId = context?.workspaceId
        void loadTarget()
      }
    })
  )
  ctx.subscriptions.push(on('agent.status.changed', () => void loadAgents()))
  void plugin.request('review.get').then(showRun)
  void loadTarget()
  void loadAgents()
}

// The directory tree with single-child folders folded together follows buildTree() in
// orca-pr-review's ui/app.js (https://github.com/Geromendez135/orca-pr-review),
// Copyright (c) 2026 Geromendez135, MIT License; adapted to Orca's Tree items and findings.
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
            label: `L${f.start_line}${f.end_line > f.start_line ? `-${f.end_line}` : ''} [P${f.priority}] ${f.title}`,
            description: `conf ${f.confidence_score}`,
            icon: f.priority <= 1 ? ('error' as const) : ('warning' as const)
          }))
      }))
  ]
  return items(root)
}

const allKeys = (items: readonly TreeItem[]): string[] =>
  items.flatMap((i) => (i.children ? [i.key, ...allKeys(i.children)] : []))

function passRows(r: Run): TableRow[] {
  const last = new Map(r.events.map((e) => [e.role, e]))
  const roles = [...ROLES.filter((x) => x !== 'judge' || last.has('judge')), ...[...last.keys()].filter((x) => !ROLES.includes(x))]
  return roles.map((role) => {
    const e = last.get(role)
    const done = e && (e.status === 'completed' || e.status === 'failed')
    return {
      key: role,
      cells: {
        role,
        status: e?.status ?? (r.status === 'running' ? 'waiting' : 'not run'),
        agent: e?.agent ?? '',
        time: done ? `${Math.round(e.duration_ms / 1000)} s` : e ? { type: 'relative', at: e.ts - e.duration_ms, format: 'duration' } : ''
      }
    }
  })
}

function Header() {
  const t = read(target)
  return (
    <Stack gap="sm">
      <Row justify="between">
        <Heading>rreview</Heading>
        <Button variant="ghost" size="sm" icon="refresh" onPress={loadTarget}>
          Refresh
        </Button>
      </Row>
      <Input
        label="PR (number or URL; empty: the workspace's)"
        value={read(prArg)}
        placeholder="the workspace's PR"
        onChange={(v) => set(prArg, v)}
        onSubmit={() => void loadTarget()}
      />
      {read(loading) ? (
        <Text muted>Looking up the PR…</Text>
      ) : !t ? (
        <EmptyState title="No workspace focused" icon="folder" />
      ) : t.pr ? (
        <Text>
          <Link href={t.pr.url}>#{t.pr.number}</Link> {t.pr.title} <Badge>{t.pr.state}</Badge>
        </Text>
      ) : (
        <Callout tone="warning" title={`No PR for ${t.name}`}>
          <Text size="sm">{t.error ?? ''}</Text>
        </Callout>
      )}
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
    </Row>
  )
}

function Status({ r }: { r: Run }) {
  const res = r.result
  return (
    <Stack gap="sm">
      <Row gap="xs" align="center">
        <Text weight="semibold">
          PR #{r.pr.number} · {r.runner}
        </Text>
        {r.status === 'running' ? (
          <Badge tone="accent">running</Badge>
        ) : r.status === 'failed' ? (
          <Badge tone="danger">failed</Badge>
        ) : (
          <Badge tone={r.exitCode === 0 ? 'success' : 'warning'}>
            {res && res.findings.length ? `${res.findings.length} gated` : 'no gated findings'}
          </Badge>
        )}
        {res?.degraded ? <Badge tone="warning">degraded</Badge> : null}
      </Row>
      <Table
        label="Passes"
        columns={[
          { key: 'role', label: 'Pass' },
          { key: 'status', label: 'Status', width: 'sm' },
          { key: 'agent', label: 'Agent', width: 'xs', hideBelow: 'sm' },
          { key: 'time', label: 'Time', width: 'xs', align: 'end' }
        ]}
        rows={passRows(r)}
      />
      {r.status === 'failed' ? (
        <Callout tone="danger" title="The review failed">
          <Text size="sm">{r.error ?? ''}</Text>
          {r.runDir ? <Text size="xs" muted>Run dir: {r.runDir}</Text> : null}
        </Callout>
      ) : null}
      {res?.degraded ? (
        <Callout tone="warning" title="Degraded">
          <Text size="sm">
            A panelist or the judge failed: {res.passes.filter((p) => p.status !== 'completed').map((p) => p.role).join(', ')}. The coordinator covered its lens.
          </Text>
        </Callout>
      ) : null}
      {res ? (
        <Text size="sm">
          <Badge tone={res.overall_correctness === 'patch is correct' ? 'success' : 'warning'}>{res.overall_correctness}</Badge>{' '}
          {res.overall_explanation}
        </Text>
      ) : null}
    </Stack>
  )
}

function Findings({ r }: { r: Run }) {
  const res = r.result
  if (!res) return null
  const list = read(showAll) ? res.ungated_findings : res.findings
  const items = fileTree(list)
  const sel = list.find((f) => findingKey(f) === read(selected))
  const keys = read(picked)
  return (
    <Stack gap="sm">
      <Row justify="between" align="center">
        <Heading level={3}>Findings</Heading>
        <Switch label={`All ${res.ungated_findings.length}, below the gate too`} checked={read(showAll)} onChange={(v) => set(showAll, v)} />
      </Row>
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
        <EmptyState title="No findings" icon="check" />
      )}
      {sel ? (
        <Stack gap="xs">
          <Text weight="semibold">
            {sel.file}:{sel.start_line} [P{sel.priority}] conf {sel.confidence_score} · {sel.stage}
          </Text>
          <Markdown content={sel.body} />
          <Checkbox
            label="Send to the author"
            checked={keys.includes(findingKey(sel))}
            onChange={(on) => update(picked, (k) => (on ? [...k, findingKey(sel)] : k.filter((x) => x !== findingKey(sel))))}
          />
        </Stack>
      ) : null}
      <Row gap="sm" align="center" wrap>
        <Select
          label="Author agent"
          placeholder="No agent in this workspace"
          value={read(author) ?? undefined}
          options={read(agents)}
          onChange={(v) => set(author, v)}
        />
        <Button icon="next" disabled={!keys.length || !read(author)} onPress={sendToAuthor}>
          {`Send ${keys.length} to author`}
        </Button>
      </Row>
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
      {r ? <Status r={r} /> : null}
      {r ? <Findings r={r} /> : null}
    </Stack>
  )
}
