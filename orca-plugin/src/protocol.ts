// The panel <-> worker contract, declared once: src/panel.tsx and src/worker.ts both read it.
// Shapes follow rreview's data contract (docs/VISION-AND-SPEC.md §5).

export type Runner = 'headless' | 'orca'

export type Pr = { number: number; title: string; url: string; state: string }

/** The focused workspace and the PR the panel would review there. */
export type Target = {
  workspaceId: string
  name: string
  path: string
  pr: Pr | null
  /** Why there is no PR (gh's own message), when there is none. */
  error: string | null
}

/** One line of `<runroot>/progress.jsonl`. */
export type PassEvent = {
  ts: number
  role: string
  status: string
  agent: string
  model: string
  duration_ms: number
  error?: string
}

/** A finding of `result.json`. */
export type Finding = {
  title: string
  body: string
  file: string
  start_line: number
  end_line: number
  priority: number
  confidence_score: number
  stage: string
}

export type Pass = { role: string; agent: string; status: string; duration_ms: number; error?: string }

/** The parts of `result.json` the panel shows. */
export type Result = {
  findings: Finding[]
  ungated_findings: Finding[]
  overall_correctness: string
  overall_explanation: string
  passes: Pass[]
  head: string
  degraded: boolean
  /** Findings left out of what the panel got, to keep it within Orca's message limits. */
  omitted?: number
}

/** A finished run's counts, small enough for every post and the plugin's storage. */
export type Summary = { gated: number; all: number; degraded: boolean }

export type Run = {
  status: 'running' | 'done' | 'failed'
  pr: Pr
  workspaceId: string
  runner: Runner
  startedAt: number
  runDir: string | null
  /** `dev:ino` of the run dir when the run named it: a later run for the same PR head replaces
   * the dir, and the worker then stops reading it. */
  runDirId?: string
  events: PassEvent[]
  /** Set when the run is done; the result itself comes from `review.result`. */
  summary: Summary | null
  /** rreview's exit code: 0 no gated findings, 1 gated findings, 2 failed. */
  exitCode: number | null
  error: string | null
}

declare module 'orca' {
  interface PluginRequests {
    /** The focused workspace and its PR, or the PR `pr` (number or URL) names. Null: none focused. */
    'target.get': { input: { pr?: string }; output: Target | null }
    /** Starts `rreview <pr> --runner <runner> --json` in the workspace; one run at a time. */
    'review.start': { input: { workspaceId: string; pr?: string; runner: Runner }; output: Run }
    /** The current or last run. */
    'review.get': { output: Run | null }
    /** The finished run's result, bodies and length cut to Orca's limits (the full text stays in
     * result.json); null when no finished run or its run dir is gone or replaced. */
    'review.result': { output: Result | null }
    /** Writes the findings `keys` name (see findingKey) to a file in the run dir and asks the
     * agent in `terminalId` to read it. Refused unless that terminal still runs the agent session
     * `sessionId` in the reviewed run's workspace. */
    'findings.send': {
      input: { terminalId: string; sessionId: string | null; keys: string[] }
      output: { sent: number; file: string }
    }
  }
  interface PluginPanelEvents {
    /** Posted to every open panel when the run changes. */
    'review.changed': Run
  }
}

/** A finding's key: rreview dedupes on file, start line and title, so this is unique in a result. */
export function findingKey(f: Finding): string {
  return `${f.file}#${f.start_line}#${f.title}`
}
