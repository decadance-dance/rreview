// The rreview panel: the focused workspace's PR, a runner choice and "Run review" in two rows;
// each pass's status while it runs; then the verdict, the findings and "Send to author", with the
// passes folded at the bottom.
import {
  atom,
  Badge,
  Box,
  Button,
  Callout,
  Checkbox,
  Code,
  Collapsible,
  EmptyState,
  Heading,
  host,
  IconButton,
  Input,
  Link,
  List,
  ListItem,
  Markdown,
  on,
  plugin,
  Progress,
  read,
  RelativeTime,
  Row,
  Select,
  Separator,
  set,
  Stack,
  Switch,
  Text,
  update,
  Toolbar,
  type IconName,
  type OrcaNode,
  type PanelContext,
  type Tone
} from 'orca'
import { findingKey, TEXT_CAP, type Finding, type PassEvent, type Result, type Run, type Runner, type Target } from './protocol'

const ROLES = ['intent_contracts', 'reliability', 'security', 'coordinator', 'judge']
const LENSES = ['intent_contracts', 'reliability', 'security']
/** The passes every run has; the judge runs only when the coordinator dropped findings. */
const FIXED = ROLES.filter((x) => x !== 'judge')

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
/** "Passes" as the person left it for the shown run; null: folded. */
const passesOpen = atom<boolean | null>(null)
/** "Other PR…" as the person left it; null: open only when the workspace has no PR. */
const otherPr = atom<boolean | null>(null)
/** The last "Send to author" of a run, kept so a reopened panel still shows this run's were sent. */
const sent = atom<{ token: string; sent: number; file: string; to: string } | null>('sent', null, { persist: 'plugin' })

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e))

async function loadTarget(): Promise<void> {
  set(loading, true)
  try {
    const pr = read(prArg)
    set(target, await plugin.request('target.get', { pr }, { timeoutMs: 60_000 }))
    set(otherPr, null)
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
    set(passesOpen, null)
    set(author, null)
    void loadAgents()
  }
  // A newly finished review: fetch its result, with its gated findings picked.
  if (r?.status === 'done' && (fresh || was?.status !== 'done')) {
    const res = await plugin.request('review.result')
    if (read(run)?.token !== r.token) return
    set(result, res)
    set(picked, res ? res.findings.map(findingKey) : [])
    // The most urgent finding shown opens with its detail.
    const first = res && mostUrgent(read(showAll) ? res.ungated_findings : res.findings)
    set(selected, first ? findingKey(first) : null)
  }
}

async function startReview(): Promise<void> {
  const t = read(target)
  if (!t) return
  set(notice, null)
  try {
    // The shown PR's URL: an empty arg would let the worker's own lookup pick another PR (a new one
    // for the branch), and a typed but unsubmitted Other PR… value is not what the header shows.
    showRun(await plugin.request('review.start', { workspaceId: t.workspaceId, pr: t.pr?.url ?? '', runner: read(runner) }, { timeoutMs: 60_000 }))
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

const lines = (f: Finding) => `${f.start_line}${f.end_line > f.start_line ? `-${f.end_line}` : ''}`
/** Severity is told by the `P<n>` text and the icon's shape, never by color alone. */
const severityIcon = (priority: number): IconName => (priority <= 1 ? 'error' : priority === 2 ? 'warning' : 'info')

/** `95s` as `1m 35s`. */
export function duration(ms: number): string {
  const s = Math.round(ms / 1000)
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`
}

/** The lowest `P<n>` first; among equals, rreview's order. */
const mostUrgent = (list: Finding[]): Finding | undefined =>
  list.reduce<Finding | undefined>((best, f) => (!best || f.priority < best.priority ? f : best), undefined)

/** Each pass's last event, in pipeline order; the judge only once it ran. A finished run's
 * result.json reports win: rreview records a pass's outcome there even when its last progress
 * line was lost. */
function passes(r: Run): { role: string; e?: PassEvent }[] {
  // The `run` line records a stop; it is no pass.
  const last = new Map(r.events.filter((e) => e.role !== 'run').map((e) => [e.role, e]))
  const res = r.status === 'done' ? read(result) : null
  for (const p of res?.passes ?? []) {
    const e = last.get(p.role)
    last.set(p.role, { ts: e?.ts ?? r.startedAt, model: e?.model ?? '', ...p })
  }
  const roles = [...ROLES.filter((x) => x !== 'judge' || last.has('judge')), ...[...last.keys()].filter((x) => !ROLES.includes(x))]
  return roles.map((role) => ({ role, e: last.get(role) }))
}

/** The time of the run's last progress line. */
const runEnd = (r: Run) => Math.max(r.startedAt, ...r.events.map((e) => e.ts))
const finished = (e?: PassEvent) => e?.status === 'completed' || e?.status === 'failed'
/** A glyph per status, told by its shape as well as its color. */
const GLYPH: Record<string, string> = { completed: '✓', failed: '✗', started: '●', wrapup: '●' }
/** Why a pass failed, without rreview's `pass <role> (<agent>): ` prefix the callout already shows. */
const reason = (e?: PassEvent) => (e?.error ?? 'no reason recorded').replace(/^pass \S+ \(\S+\): /, '').slice(0, TEXT_CAP)

const STATUS_TONE: Record<string, Tone> = { completed: 'success', failed: 'danger', started: 'accent', wrapup: 'accent' }

/** A pass line's glyph, its tone and what follows the role on the right: the time, plus a word
 * where the glyph alone would be unclear. */
function passState(r: Run, e?: PassEvent): { glyph: string; tone?: Tone; right: OrcaNode } {
  if (!e) return r.status === 'running' ? { glyph: '○', right: 'waiting' } : { glyph: '–', right: 'not run' }
  // The run ended without this pass's last line: its clock stops with the run.
  if (!finished(e) && r.status !== 'running')
    return r.status === 'done'
      ? { glyph: '?', right: `unknown · ${duration(runEnd(r) - (e.ts - e.duration_ms))}` }
      : { glyph: '■', right: `interrupted · ${duration(runEnd(r) - (e.ts - e.duration_ms))}` }
  const tone = STATUS_TONE[e.status]
  if (e.status === 'completed') return { glyph: '✓', tone, right: duration(e.duration_ms) }
  if (e.status === 'failed') return { glyph: '✗', tone, right: `failed · ${duration(e.duration_ms)}` }
  const ticking = <RelativeTime at={e.ts - e.duration_ms} format="duration" tone="accent" />
  return { glyph: GLYPH[e.status] ?? '●', tone, right: e.status === 'wrapup' ? ['wrap-up · ', ticking] : ticking }
}

/** One line per pass: the glyph, the role and its agent, then the time on the right. A pass not
 * started yet is muted. */
function PassRows({ r }: { r: Run }) {
  return (
    <Stack gap="xs">
      {passes(r).map(({ role, e }) => {
        const { glyph, tone, right } = passState(r, e)
        return (
          <Row key={`pass:${role}`} justify="between" align="center" gap="xs" wrap>
            <Text size="sm" muted={!e}>
              <Text size="sm" tone={tone} muted={!tone}>
                {glyph}
              </Text>{' '}
              {role} <Text size="sm" muted>{e?.agent ?? ''}</Text>
            </Text>
            <Text size="sm" muted={!tone || e?.status === 'completed'} tone={e?.status === 'failed' ? 'danger' : undefined}>
              {right}
            </Text>
          </Row>
        )
      })}
    </Stack>
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

/** An ended run puts "Other PR…" under its passes, so the verdict comes right after the
 * controls; with no PR it stays on top, open. */
const otherPrBelow = () => {
  const r = read(run)
  return !!r && r.status !== 'running' && !!read(target)?.pr
}

function OtherPr() {
  return (
    <Collapsible
      title="Other PR…"
      description={read(prArg) || undefined}
      open={read(otherPr) ?? !read(target)?.pr}
      onChange={(open) => set(otherPr, open)}
    >
      <Input
        label="PR number or URL"
        value={read(prArg)}
        placeholder="empty: the workspace's PR"
        onChange={(v) => set(prArg, v)}
        onSubmit={() => void loadTarget()}
      />
    </Collapsible>
  )
}

/** Whether `r` reviewed the PR the header shows, which Run review would start. */
const isShown = (r: Run) => r.pr.url === read(target)?.pr?.url

function Header() {
  const t = read(target)
  const r = read(run)
  const running = r?.status === 'running'
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
        // One line: the title is cut, the number and the state stay.
        <Row gap="xs" align="center">
          <Box grow shrink minWidth={0}>
            <Text size="sm" truncate>
              <Link href={t.pr.url}>#{t.pr.number}</Link> {t.pr.title}
            </Text>
          </Box>
          <Badge>{t.pr.state}</Badge>
        </Row>
      ) : (
        <Callout tone="warning" title={`No PR for ${t.name}`}>
          <Text size="sm">{t.error ?? ''}</Text>
          <Text size="sm">Open a PR for this branch, or type a PR number or URL under Other PR, then press Refresh.</Text>
        </Callout>
      )}
      {/* Orca has no segmented control: two xs buttons in a labelled Toolbar stand in for one, the
          chosen one solid and checked, so the choice reads without color. Buttons have no pressed
          state, so the toolbar's name and the chosen button's tooltip say which one is chosen. */}
      <Row justify="between" align="center" gap="xs" wrap>
        <Toolbar label={`Runner: ${read(runner)}`} gap="none">
          {(['headless', 'orca'] as const).map((v) => (
            <Button
              key={`runner:${v}`}
              size="xs"
              variant={read(runner) === v ? 'solid' : 'ghost'}
              icon={read(runner) === v ? 'check' : undefined}
              tooltip={`${read(runner) === v ? 'Chosen: runs' : 'Run'} the reviewers ${v === 'orca' ? 'in Orca terminals' : 'headless'}`}
              onPress={() => set(runner, v)}
            >
              {v}
            </Button>
          ))}
        </Toolbar>
        <Row gap="xs" align="center">
          <Button
            size="xs"
            variant="solid"
            tone="accent"
            icon={running ? undefined : 'play'}
            disabled={running || read(loading) || !t?.pr}
            onPress={startReview}
          >
            {running ? 'Running…' : r && isShown(r) ? 'Run again' : 'Run review'}
          </Button>
          {running ? <IconButton icon="stop" label="Cancel" tooltip="Cancel the review" size="xs" onPress={cancelReview} /> : null}
        </Row>
      </Row>
      {!otherPrBelow() && (t || !read(loading)) ? <OtherPr /> : null}
    </Stack>
  )
}

/** The first sentence or line of `text`, and the rest. A leading fenced block is the whole lead,
 * so the preview never renders half a fence. */
export function lead(text: string): [string, string] {
  const t = text.trim()
  const head =
    t.match(/^(`{3,}|~{3,})[^\n]*\n(?:[\s\S]*?\n\1[^\n]*(?=\n|$)|[\s\S]*$)/)?.[0] ??
    t.match(/^[^\n]*?[.!?](?=\s)/)?.[0] ??
    t.split('\n')[0]
  return [head, t.slice(head.length).trim()]
}

/** Which Leads the person opened, by id. */
const moreOpen = atom<string[]>([])

/** A long text as its first sentence, the whole text behind "More": the verdict and the detail stay
 * short. Opened, the text renders as one document, never as a lead plus a rest that could split
 * Markdown (a fence, a list) in two. */
function Lead({ id, text, markdown }: { id: string; text: string; markdown?: boolean }) {
  const [head, rest] = lead(text)
  const open = read(moreOpen).includes(id)
  const show = (s: string) => (markdown ? <Markdown content={s} /> : <Text size="sm" muted>{s}</Text>)
  return (
    <Stack gap="xs">
      {open && rest ? null : show(head)}
      {rest ? (
        <Collapsible
          title={open ? 'Less' : 'More'}
          open={open}
          onChange={(o) => update(moreOpen, (ids) => (o ? [...ids, id] : ids.filter((x) => x !== id)))}
        >
          {show(text.trim())}
        </Collapsible>
      ) : null}
    </Stack>
  )
}

/** `17m` from 17m 6s; seconds under a minute. Short, so the counts stay on one line. */
const minutes = (ms: number) => (ms < 60_000 ? duration(ms) : `${Math.round(ms / 60_000)}m`)

/** One line of counts at 280 px: findings at and below the gate, passes completed of all, the
 * run's time, degraded. The Passes fold under the findings spells the pass counts out. */
function Counts({ r }: { r: Run }) {
  const res = read(result)
  const list = passes(r)
  const gated = r.summary?.gated ?? res?.findings.length ?? 0
  const all = r.summary?.all ?? res?.ungated_findings.length ?? 0
  const ok = list.filter(({ e }) => e?.status === 'completed').length
  const degraded = r.summary?.degraded ?? res?.degraded
  return (
    <Text size="xs" muted>
      {`${gated} gated · ${all - gated} below · ${ok}/${list.length} · ${minutes(runEnd(r) - r.startedAt)}`}
      {degraded ? ' · ' : null}
      {degraded ? (
        <Text size="xs" tone="warning">
          degraded
        </Text>
      ) : null}
    </Text>
  )
}

/** How to retry a run of another PR than the one shown: Run review would start the shown one, and
 * once that PR is shown again the button reads Run again. */
const retryElsewhere = (r: Run) =>
  `To review #${r.pr.number} again, focus its workspace or enter ${r.pr.url} under Other PR…, then press Run again.`

function Status({ r }: { r: Run }) {
  const res = read(result)
  const list = passes(r)
  const fixedDone = list.filter(({ role, e }) => FIXED.includes(role) && finished(e)).length
  const judge = list.find(({ role }) => role === 'judge')?.e
  const ok = res?.overall_correctness === 'patch is correct'
  if (r.status === 'running')
    return (
      <Stack gap="sm">
        <Row justify="between" align="center" gap="xs">
          <Text size="sm">
            {`${fixedDone} of ${FIXED.length} passes${judge ? `, judge ${finished(judge) ? judge.status : 'running'}` : ''} · `}
            <RelativeTime at={r.startedAt} format="duration" muted />
          </Text>
          <Badge tone="accent">running</Badge>
        </Row>
        {/* Once the fixed passes are done, whether the judge runs is unknown: no value, no 100 %. */}
        <Progress label="Passes done" labelHidden value={fixedDone < FIXED.length ? Math.round((100 * fixedDone) / FIXED.length) : undefined} />
      </Stack>
    )
  if (r.status === 'failed')
    return (
      <Callout tone="danger" title="The review failed">
        <Text size="sm">{r.error ?? ''}</Text>
        <Text size="sm">{isShown(r) ? 'Fix the cause, then press Run again.' : retryElsewhere(r)}</Text>
        {r.runDir ? (
          <Text size="xs" muted>
            Run dir: <Code>{r.runDir}</Code>
          </Text>
        ) : null}
      </Callout>
    )
  if (r.status === 'cancelled')
    return (
      <Callout tone="neutral" title="The review was cancelled">
        <Text size="sm">
          {`rreview stopped its reviewers and ran its cleanup before it exited. ${isShown(r) ? 'Run again starts a new run.' : retryElsewhere(r)}`}
        </Text>
        {r.runDir ? (
          <Text size="xs" muted>
            Run dir: <Code>{r.runDir}</Code>
          </Text>
        ) : null}
      </Callout>
    )
  return res ? (
    <Callout
      tone={ok ? 'success' : 'warning'}
      icon={ok ? 'check' : 'warning'}
      title={res.overall_correctness.charAt(0).toUpperCase() + res.overall_correctness.slice(1)}
    >
      <Lead id={`verdict:${r.token}`} text={res.overall_explanation} />
      <Counts r={r} />
    </Callout>
  ) : (
    <Counts r={r} />
  )
}

/** Each pass and why any failed: in full while the run runs, then folded at the bottom; its title
 * counts the failed ones. */
function Passes({ r }: { r: Run }) {
  const list = passes(r)
  const failures = list.filter(({ e }) => e?.status === 'failed')
  const done = r.status === 'done'
  const body = (
    <Stack gap="sm">
      <PassRows r={r} />
      {failures.length ? (
        <Callout tone="warning" title={`${failures.length} pass${failures.length === 1 ? '' : 'es'} failed`}>
          {failures.map(({ role, e }) => (
            <Failure key={`failure:${role}`} role={role} e={e} />
          ))}
          {done && failures.some(({ role }) => LENSES.includes(role)) ? (
            <Text size="sm">The coordinator covered the failed lens, so the findings still stand.</Text>
          ) : null}
          {done && failures.some(({ role }) => role === 'judge') ? (
            <Text size="sm">The coordinator's findings stand, but the findings it dropped could not be re-examined.</Text>
          ) : null}
        </Callout>
      ) : null}
    </Stack>
  )
  if (r.status === 'running') return body
  const ok = list.filter(({ e }) => e?.status === 'completed').length
  return (
    <Collapsible
      title={`Passes · ${ok}/${list.length} done${failures.length ? `, ${failures.length} failed` : ''}`}
      open={read(passesOpen) ?? false}
      onChange={(open) => set(passesOpen, open)}
    >
      {body}
    </Collapsible>
  )
}

/** This run's last "Send to author", kept across panel reloads. */
function Sent() {
  const s = read(sent)
  if (!s) return null
  return (
    <Callout tone="success" icon="check" title={`Sent ${s.sent} finding${s.sent === 1 ? '' : 's'} to ${s.to}`}>
      <Text size="xs" muted>
        The agent was asked to read <Code>{s.file}</Code>
      </Text>
    </Callout>
  )
}

/** The finding's lines on GitHub at the reviewed head, from the PR's URL. */
function githubUrl(prUrl: string, head: string, f: Finding): string | null {
  const repo = prUrl.match(/^(https:\/\/[^/]+\/[^/]+\/[^/]+)\/pull\/\d+/)?.[1]
  if (!repo || !/^[0-9a-f]{40}$/.test(head)) return null
  const path = f.file.split('/').map(encodeURIComponent).join('/')
  return `${repo}/blob/${head}/${path}#L${f.start_line}${f.end_line > f.start_line ? `-L${f.end_line}` : ''}`
}

/** The longest `file:lines` the Code chip shows: what fits the detail at 280 px (31 at 7.4 px). Orca's inline Code
 * never wraps and cannot take focus, so a longer one goes in a read-only field, which a keyboard
 * can focus, select and copy from. */
const CHIP_MAX = 31

function Detail({ f, r, res }: { f: Finding; r: Run; res: Result }) {
  const key = findingKey(f)
  const url = githubUrl(r.pr.url, res.head, f)
  const loc = `${f.file}:${lines(f)}`
  return (
    <Box border radius="md" padding="sm">
      <Stack gap="xs">
        <Lead id={`body:${r.token}:${key}`} text={f.body} markdown />
        {loc.length <= CHIP_MAX ? (
          <Code>{loc}</Code>
        ) : (
          <Input label="File and lines (select to copy)" labelHidden value={loc} />
        )}
        {url ? (
          <Text size="xs">
            <Link href={url}>GitHub ↗</Link>
          </Text>
        ) : null}
        <Checkbox
          label="Send to the author"
          checked={read(picked).includes(key)}
          onChange={(on) => update(picked, (k) => (on ? [...k, key] : k.filter((x) => x !== key)))}
        />
      </Stack>
    </Box>
  )
}

/** Most urgent first; among equals, rreview's order (the sort is stable). */
const byUrgency = (list: Finding[]) => [...list].sort((a, b) => a.priority - b.priority)

function Findings({ r }: { r: Run }) {
  const res = read(result)
  if (r.status !== 'done') return null
  if (!res) return <Text muted>The result is not available: its run dir is gone or was replaced by a later run.</Text>
  const all = read(showAll)
  const list = byUrgency(all ? res.ungated_findings : res.findings)
  const sel = list.find((f) => findingKey(f) === read(selected))
  const keys = read(picked)
  const below = res.ungated_findings.length - res.findings.length
  const toggle = (v: boolean) => {
    set(showAll, v)
    const next = v ? res.ungated_findings : res.findings
    if (!next.some((f) => findingKey(f) === read(selected))) {
      const first = mostUrgent(next)
      set(selected, first ? findingKey(first) : null)
    }
  }
  return (
    <Stack gap="sm">
      <Row justify="between" align="center" gap="xs" wrap>
        <Heading level={3}>
          Findings <Badge>{`${list.length}`}</Badge>
        </Heading>
        <Switch label="below gate" checked={all} onChange={toggle} />
      </Row>
      {res.omitted ? (
        <Text size="xs" muted>
          {`${res.omitted} more findings are not shown here (Orca's message limits); see result.json in the run dir.`}
        </Text>
      ) : null}
      {list.length ? (
        <List label="Findings">
          {list.map((f) => {
            const key = findingKey(f)
            return (
              // The title as a child, not `title`, which Orca cuts with an ellipsis.
              <ListItem key={key} icon={severityIcon(f.priority)} current={key === read(selected)} onPress={() => set(selected, key)}>
                <Text size="sm">{f.title}</Text>
                <Text size="xs" muted>{`P${f.priority} · ${f.file}:${lines(f)} · conf ${f.confidence_score}`}</Text>
              </ListItem>
            )
          })}
        </List>
      ) : (
        <EmptyState
          title={all ? 'No findings' : 'No findings above the gate'}
          icon="check"
          description={!all && below ? `${below} below the gate: turn on the switch above to see them.` : 'The reviewers reported nothing to fix.'}
        />
      )}
      {sel ? <Detail f={sel} r={r} res={res} /> : null}
      {/* One row: the picker takes what the button leaves. */}
      <Row gap="xs" align="center">
        <Box grow shrink minWidth={0}>
          <Select
            label="Author agent (in the reviewed workspace)"
            labelHidden
            placeholder={read(agents).length ? 'Author agent' : 'No agent in the reviewed workspace'}
            value={read(author)?.terminalId ?? undefined}
            options={read(agents).map(({ value, label }) => ({ value, label }))}
            onChange={(v) => set(author, { terminalId: v, sessionId: read(agents).find((o) => o.value === v)?.session ?? null })}
          />
        </Box>
        <Button icon="next" tooltip="Send the checked findings to the author agent" disabled={!keys.length || !read(author)} onPress={sendToAuthor}>
          {`Send ${keys.length}`}
        </Button>
      </Row>
      {read(sent)?.token === r.token ? <Sent /> : null}
    </Stack>
  )
}

export default function Panel() {
  const r = read(run)
  const n = read(notice)
  return (
    <Stack gap="sm">
      <Header />
      {n ? <Callout tone={n.tone}>{n.text}</Callout> : null}
      {r || read(target)?.pr ? <Separator /> : null}
      {/* The run shown is another PR's (the workspace changed, or another PR was typed). */}
      {r && !isShown(r) ? (
        <Text size="sm" muted>
          Review of <Link href={r.pr.url}>#{r.pr.number}</Link> {r.pr.title}
        </Text>
      ) : null}
      {r ? (
        <Status r={r} />
      ) : read(target)?.pr ? (
        <EmptyState
          icon="search"
          title="No review yet"
          description="Run review starts three lenses, a coordinator and, when it drops findings, a judge on this PR. Each pass shows here while it runs, then the verdict and the findings."
        />
      ) : null}
      {r ? <Findings r={r} /> : null}
      {r ? <Passes r={r} /> : null}
      {otherPrBelow() ? <OtherPr /> : null}
    </Stack>
  )
}
