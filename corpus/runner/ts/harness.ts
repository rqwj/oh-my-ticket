/**
 * Behavioral-corpus harness (U2): executes one self-describing
 * `{meta, setup, operations, invariants}` JSON document against a REAL
 * OmtCore/OmtStore/OmtCorePool in a fresh temporary home.
 *
 * Design rules (plan U2):
 *  - Plain and deterministic. No extensible runner framework: the operation
 *    and invariant vocabularies below are CLOSED and documented in
 *    corpus/README.md; adding an op means editing this file.
 *  - Cross-language well-definedness comes from (a) volatile-field masking —
 *    wall-clock stamps are replaced by "__MASKED__" before comparison, so a
 *    Rust leg with a real injected clock produces identical projections —
 *    and (b) assertions keyed on problem codes + structured details, never
 *    message text (R5).
 *  - Every operation result (or `{error:{code,message,details}}`) is stored
 *    in order; invariants reference operations by index or label.
 */
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { OmtCore } from '../../../src/host/core.ts'
import { OmtStore } from '../../../src/host/store.ts'
import { OmtCorePool } from '../../../src/host/pool.ts'
import {
  DEFAULT_RUN_CONFIG,
  NUDGE_BUDGET,
  OmtError,
  isRunItemStalled,
  type RunConfig,
} from '../../../src/host/types.ts'

/** Placeholder written over volatile fields before comparison. */
export const MASK_PLACEHOLDER = '__MASKED__'

/** Volatile wall-clock stamps masked by default (envelope may override). */
const DEFAULT_MASK_KEYS = ['created_at', 'updated_at', 'nudged_at', 'started_at', 'finished_at']

/** Fixed nudge stamp used by the `nudge` op (masked anyway; keeps runs deterministic). */
const FIXED_NUDGE_AT = '2026-08-19T00:00:00.000Z'

export interface ScenarioOp {
  readonly op: string
  readonly params?: Record<string, unknown>
  readonly label?: string
}

export interface ScenarioInvariant {
  readonly expect: string
  readonly op?: number | string
  readonly path?: string
  readonly value?: unknown
  /** Alternative to `value`: compare against the (masked) result of another operation. */
  readonly valueFrom?: number | string
  readonly file?: string
  readonly text?: string
}

export interface ScenarioDoc {
  readonly meta?: { name?: string; source?: string | string[]; description?: string; recordEvents?: boolean }
  readonly setup?: {
    readonly mask?: string[]
    readonly nodes?: Record<string, unknown>[]
    readonly activeSessionIds?: string[]
    readonly pool?: {
      readonly globalDirName?: string
      readonly workspaces: { name: string; omt: boolean }[]
    }
  }
  readonly operations?: ScenarioOp[]
  readonly invariants?: ScenarioInvariant[]
}

export interface ScenarioSummary {
  readonly ok: boolean
  readonly name: string
  readonly checks: number
  readonly failures: string[]
  /** Masked operation results — present only when invariants failed (diagnostics). */
  readonly debugResults?: unknown[]
}

// ── comparison helpers ───────────────────────────────────────────────────

function maskValue(value: unknown, keys: ReadonlySet<string>): unknown {
  if (Array.isArray(value)) return value.map(entry => maskValue(entry, keys))
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {}
    for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
      out[key] = keys.has(key) ? MASK_PLACEHOLDER : maskValue(entry, keys)
    }
    return out
  }
  return value
}

/** Dotted path lookup; numeric segments index arrays. Missing → undefined. */
function getPath(root: unknown, path: string | undefined): unknown {
  if (path === undefined || path === '') return root
  let current: unknown = root
  for (const segment of path.split('.')) {
    if (current === null || typeof current !== 'object') return undefined
    current = (current as Record<string, unknown>)[segment]
  }
  return current
}

function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true
  if (typeof a === 'number' && typeof b === 'number') return Object.is(a, b)
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((entry, index) => deepEqual(entry, b[index]))
  }
  if (a !== null && b !== null && typeof a === 'object' && typeof b === 'object') {
    const left = a as Record<string, unknown>
    const right = b as Record<string, unknown>
    const leftKeys = Object.keys(left).filter(key => left[key] !== undefined)
    const rightKeys = Object.keys(right).filter(key => right[key] !== undefined)
    if (leftKeys.length !== rightKeys.length) return false
    return leftKeys.every(key => key in right && deepEqual(left[key], right[key]))
  }
  // Treat undefined and null as equivalent absent-values (row mappers omit null columns).
  if ((a === undefined || a === null) && (b === undefined || b === null)) return true
  return false
}

function subsetMatch(actual: unknown, expected: unknown): boolean {
  if (Array.isArray(expected)) {
    return Array.isArray(actual)
      && expected.length === actual.length
      && expected.every((entry, index) => subsetMatch(actual[index], entry))
  }
  if (expected !== null && typeof expected === 'object') {
    if (actual === null || typeof actual !== 'object' || Array.isArray(actual)) return false
    return Object.entries(expected as Record<string, unknown>).every(([key, value]) =>
      subsetMatch((actual as Record<string, unknown>)[key], value))
  }
  return deepEqual(actual, expected)
}

function stringify(value: unknown): string {
  return JSON.stringify(value, null, 2) ?? String(value)
}

// ── scenario execution ───────────────────────────────────────────────────

interface PoolFixture {
  readonly pool: OmtCorePool
  readonly vars: Map<string, string>
}

interface RunContext {
  home: string
  core: OmtCore
  fixture?: PoolFixture
  results: unknown[]
  aliases: Map<string, unknown>
  events: [string, string | undefined, string][]
  recording: boolean
  detach: () => void
}

function collectItemEvents(context: RunContext): void {
  context.detach()
  context.events = []
  context.detach = context.core.onRunEvent(event => {
    if (event.kind === 'item' && event.item !== undefined) {
      context.events.push([event.item.node_id, event.fromItemState, event.item.state])
    }
  })
}

/** Substitute `$var` references inside expected strings. */
function resolveExpected(value: unknown, vars: Map<string, string>): unknown {
  if (typeof value === 'string' && value.startsWith('$')) {
    const resolved = vars.get(value.slice(1))
    return resolved === undefined ? value : resolved
  }
  if (Array.isArray(value)) return value.map(entry => resolveExpected(entry, vars))
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {}
    for (const [key, entry] of Object.entries(value as Record<string, unknown>)) out[key] = resolveExpected(entry, vars)
    return out
  }
  return value
}

function fail(failures: string[], doc: ScenarioDoc, index: number, invariant: ScenarioInvariant, detail: string): void {
  failures.push(`invariant #${index} (${invariant.expect}${invariant.path !== undefined ? ` @${invariant.path}` : ''}) of "${doc.meta?.name ?? '?'}": ${detail}`)
}

function resultOf(context: RunContext, ref: number | string | undefined): unknown {
  if (ref === undefined) return undefined
  if (typeof ref === 'string') return context.aliases.get(ref)
  return context.results[ref]
}

/** One closed operation vocabulary. Each handler receives parsed params and returns the stored result. */
type OpHandler = (params: Record<string, unknown>, context: RunContext) => Promise<unknown> | unknown

function errOf(error: unknown): unknown {
  if (error instanceof OmtError) {
    return { error: { code: error.code, message: error.message, ...(error.details !== undefined ? { details: error.details } : {}) } }
  }
  return { error: { code: 'UNKNOWN', message: String((error as Error)?.message ?? error) } }
}

function str(params: Record<string, unknown>, key: string): string {
  return params[key] as string
}

const OPS: Record<string, OpHandler> = {
  // ── nodes ──
  create: (p, c) => c.core.create(p as never),
  update: (p, c) => c.core.update(p as never),
  move: (p, c) => c.core.move(str(p, 'id'), str(p, 'newParentId')),
  show: (p, c) => c.core.show(str(p, 'id')),
  list: (p, c) => c.core.list(p as never),
  tree: (p, c) => c.core.tree(p?.rootId as string | undefined),
  getNode: (p, c) => c.core.getNode(str(p, 'id')) ?? null,
  reindex: (_p, c) => c.core.reindex(),
  readFile: async p => {
    try {
      return await readFile(join(String(p.home), String(p.path)), 'utf8')
    } catch (error) {
      return errOf(error)
    }
  },
  /** Hand-edit simulation: replace the whole node file (reindex scenarios). */
  writeFile: async p => {
    try {
      const { writeFile } = await import('node:fs/promises')
      await writeFile(join(String(p.home), String(p.path)), String(p.text), 'utf8')
      return { written: p.path }
    } catch (error) {
      return errOf(error)
    }
  },
  deleteFile: async p => {
    try {
      await rm(join(String(p.home), String(p.path)), { force: false })
      return { deleted: p.path }
    } catch (error) {
      return errOf(error)
    }
  },

  // ── runs ──
  createRun: (p, c) => c.core.createRun(p as never),
  getRun: (p, c) => c.core.getRun(str(p, 'id')) ?? null,
  listRuns: (p, c) => c.core.listRuns(p as never),
  runItems: (p, c) => c.core.runItems(str(p, 'runId')),
  getRunItem: (p, c) => c.core.getRunItem(str(p, 'runId'), str(p, 'nodeId')) ?? null,
  runItemStateCounts: (p, c) => c.core.runItemStateCounts(str(p, 'runId')),
  addRunMembers: (p, c) => c.core.addRunMembers(str(p, 'runId'), p.members as never),
  runsOfNode: (p, c) => c.core.runsOfNode(str(p, 'nodeId')),
  startRun: (p, c) => c.core.startRun(str(p, 'id')),
  pauseRun: (p, c) => c.core.pauseRun(str(p, 'id')),
  resumeRun: (p, c) => c.core.resumeRun(str(p, 'id')),
  cancelRun: (p, c) => c.core.cancelRun(str(p, 'id')),
  transitionItem: (p, c) => c.core.transitionItem(str(p, 'runId'), str(p, 'nodeId'), p.to as never, {
    ...(p.executorSessionId !== undefined ? { executorSessionId: p.executorSessionId as string } : {}),
    ...(p.error !== undefined ? { error: p.error as string } : {}),
  }),
  retryItem: (p, c) => c.core.retryItem(str(p, 'runId'), str(p, 'nodeId')),
  replayItem: (p, c) => c.core.replayItem(str(p, 'runId'), str(p, 'nodeId')),
  claimRunItem: async (p, c) => (await c.core.claimRunItem(str(p, 'runId'), str(p, 'executorSessionId'))) ?? null,
  reportRunItem: (p, c) => c.core.reportRunItem(str(p, 'runId'), str(p, 'nodeId'), p.outcome as never, p.note as string | undefined),
  removeRunItem: async (p, c) => {
    await c.core.removeRunItem(str(p, 'runId'), str(p, 'nodeId'))
    return { removed: p.nodeId }
  },
  /** Explicit seed primitive (replaces direct OmtStore writes in old specs). */
  seedRun: async (p, c) => {
    const store = await OmtStore.open(join(c.home, 'omt.db'))
    try {
      const config: RunConfig = { ...DEFAULT_RUN_CONFIG, ...(p.config as Partial<RunConfig> | undefined) }
      const id = (p.id as string | undefined) ?? store.nextRunId()
      store.insertRun({
        id,
        ...((p.title as string | undefined) !== undefined ? { title: p.title as string } : {}),
        status: p.status as never,
        config,
        created_at: (p.createdAt as string | undefined) ?? FIXED_NUDGE_AT,
      })
      ;(p.items as Record<string, unknown>[] | undefined ?? []).forEach(item => {
        store.insertRunItem({
          run_id: id,
          node_id: String(item.nodeId),
          position: Number(item.position),
          state: item.state as never,
          attempts: Number(item.attempts ?? 0),
          nudge_count: Number(item.nudgeCount ?? 0),
          ...(item.executorSessionId !== undefined ? { executor_session_id: String(item.executorSessionId) } : {}),
          ...(item.lastError !== undefined ? { last_error: String(item.lastError) } : {}),
          ...(item.startedAt !== undefined ? { started_at: String(item.startedAt) } : {}),
          ...(item.finishedAt !== undefined ? { finished_at: String(item.finishedAt) } : {}),
        } as never)
      })
      return { id }
    } finally {
      store.close()
    }
  },
  /** Record `count` continuation nudges (TICKET-0062 bookkeeping) on one item. */
  nudge: (p, c) => {
    const count = Number(p.count ?? 1)
    let last: unknown = null
    for (let index = 0; index < count; index += 1) last = c.core.recordItemNudge(str(p, 'runId'), str(p, 'nodeId'), FIXED_NUDGE_AT)
    return last
  },
  stallCheck: async (p, c) => ({ stalled: isRunItemStalled(c.core.getRunItem(str(p, 'runId'), str(p, 'nodeId'))!) }),
  continuationCandidates: (p, c) => c.core.continuationCandidates(str(p, 'sessionId')),
  executorItems: (p, c) => c.core.executorItems(str(p, 'sessionId')),
  /** Startup-janitor sweep driven by injected session liveness. */
  sweep: (p, c) => {
    const live = new Set((p.activeSessions as string[] | undefined) ?? [])
    return c.core.janitorSweep(sessionId => live.has(sessionId))
  },
  /**
   * Crash/reopen boundary: close without settling, optionally delete the
   * database files (freshDb), then reopen with the given live sessions.
   */
  reopen: async (p, c) => {
    c.detach()
    c.core.close()
    if (p.freshDb === true) {
      for (const suffix of ['', '-wal', '-shm']) await rm(join(c.home, `omt.db${suffix}`), { force: true })
    }
    c.core = await OmtCore.open(c.home, { activeSessionIds: (p.activeSessionIds as string[] | undefined) ?? [] })
    if (c.recording) collectItemEvents(c)
    return { reopened: true }
  },

  // ── pool workspace routing (the half that moves into the daemon) ──
  resolveHome: (p, c) => {
    if (c.fixture === undefined) return errOf(new Error('resolveHome requires setup.pool'))
    const raw = p.cwd as string | null | undefined
    const cwd = typeof raw === 'string' ? (c.fixture.vars.get(raw.slice(1)) ?? raw) : undefined
    return c.fixture.pool.homeFor(cwd)
  },
  /** Route the ACTIVE core through the pool for this cwd (null = global home). */
  openRouted: async (p, c) => {
    if (c.fixture === undefined) return errOf(new Error('openRouted requires setup.pool'))
    const raw = p.cwd as string | null | undefined
    const cwd = typeof raw === 'string' ? (c.fixture.vars.get(raw.slice(1)) ?? raw) : undefined
    c.detach()
    c.core = await c.fixture.pool.coreFor(cwd)
    c.home = c.core.home
    if (c.recording) collectItemEvents(c)
    return { home: c.home }
  },
}

export async function runScenario(doc: ScenarioDoc): Promise<ScenarioSummary> {
  const name = doc.meta?.name ?? '<unnamed>'
  const failures: string[] = []
  let checks = 0
  const maskKeys = new Set(doc.setup?.mask ?? DEFAULT_MASK_KEYS)

  const homeRoot = await mkdtemp(join(tmpdir(), 'omt-corpus-'))
  let context: RunContext | undefined
  const cleanups: (() => Promise<void>)[] = []
  try {
    const fixture = doc.setup?.pool
    const vars = new Map<string, string>()
    let poolInstance: OmtCorePool | undefined
    let home: string
    let core: OmtCore

    if (fixture !== undefined) {
      const globalDirName = fixture.globalDirName ?? 'global'
      const globalHome = join(homeRoot, globalDirName)
      vars.set('global', globalHome)
      for (const workspace of fixture.workspaces) {
        const dir = join(homeRoot, workspace.name)
        if (workspace.omt) {
          const { mkdir } = await import('node:fs/promises')
          await mkdir(join(dir, '.omt'), { recursive: true })
        }
        vars.set(workspace.name, dir)
      }
      poolInstance = new OmtCorePool(globalHome)
      cleanups.push(async () => { await poolInstance?.closeAll() })
      // No core until openRouted resolves one.
      const firstRouting = (doc.operations ?? []).find(operation => operation.op === 'openRouted')
      if (firstRouting === undefined) {
        return { ok: false, name, checks: 0, failures: ['pool scenario without an openRouted operation'] }
      }
      const cwdRaw = firstRouting.params?.cwd as string | null | undefined
      const cwd = typeof cwdRaw === 'string' ? (vars.get(cwdRaw.slice(1)) ?? cwdRaw) : undefined
      core = await poolInstance.coreFor(cwd)
      home = core.home
      context = {
        home,
        core,
        fixture: { pool: poolInstance, vars },
        results: [],
        aliases: new Map(),
        events: [],
        recording: doc.meta?.recordEvents === true,
        detach: () => {},
      }
    } else {
      home = homeRoot
      core = await OmtCore.open(home, { activeSessionIds: doc.setup?.activeSessionIds ?? [] })
      context = {
        home,
        core,
        results: [],
        aliases: new Map(),
        events: [],
        recording: doc.meta?.recordEvents === true,
        detach: () => {},
      }
    }
    cleanups.push(async () => { context?.core.close() })

    if (context.recording) collectItemEvents(context)

    // setup.nodes: sequential core.create calls (pre-operation fixture).
    for (const input of doc.setup?.nodes ?? []) {
      try {
        await context.core.create(input as never)
      } catch (error) {
        failures.push(`setup.nodes ${stringify(input)} failed: ${String((error as Error)?.message ?? error)}`)
      }
    }

    // operations: every result (or structured error) stored in order.
    for (const [index, operation] of (doc.operations ?? []).entries()) {
      const handler = OPS[operation.op]
      if (handler === undefined) {
        failures.push(`operation #${index}: unknown op "${operation.op}"`)
        context.results.push(null)
        continue
      }
      try {
        const result = await handler({ ...(operation.params ?? {}), home: context.home }, context)
        context.results.push(result === undefined ? null : result)
      } catch (error) {
        context.results.push(errOf(error))
      }
      if (operation.label !== undefined) context.aliases.set(operation.label, context.results[context.results.length - 1])
    }

    // Pre-flight file-content reads once per referenced file so invariant
    // evaluation stays synchronous.
    const files = new Map<string, string | Error>()
    for (const invariant of doc.invariants ?? []) {
      if ((invariant.expect === 'fileContains' || invariant.expect === 'fileNotContains')
        && invariant.file !== undefined && !files.has(invariant.file)) {
        try {
          files.set(invariant.file, await readFile(join(context.home, invariant.file), 'utf8'))
        } catch (error) {
          files.set(invariant.file, error as Error)
        }
      }
    }

    // invariants: evaluated against masked projections.
    (doc.invariants ?? []).forEach((invariant, index) => {
      checks += 1
      // Path semantics: `path` addresses the ACTUAL operation result. A
      // literal `value` is already written relative to that path; a
      // `valueFrom` reference points at another whole operation result, so
      // the same path is applied to it before comparison.
      const expectedSource = invariant.value !== undefined
        ? resolveExpected(invariant.value, vars)
        : getPath(resultOf(context, invariant.valueFrom), invariant.path)
      const expected = maskValue(expectedSource, maskKeys)
      const expectedRaw = expectedSource
      const rawActual = resultOf(context, invariant.op)
      const actual = maskValue(rawActual, maskKeys)
      const at = getPath(actual, invariant.path)
      const describeActual = `actual=${stringify(invariant.path !== undefined ? getPath(rawActual, invariant.path) : rawActual)}`
      const describeExpected = `expected=${stringify(invariant.path !== undefined ? expectedRaw : expected)}`
      switch (invariant.expect) {
        case 'equals':
          if (!deepEqual(at, expected)) fail(failures, doc, index, invariant, `${describeExpected}, ${describeActual}`)
          break
        case 'matches':
          if (!subsetMatch(at, expected)) fail(failures, doc, index, invariant, `expected subset ${stringify(expected)}; ${describeActual}`)
          break
        case 'contains': {
          if (typeof at === 'string' && typeof expected === 'string') {
            if (!at.includes(expected)) fail(failures, doc, index, invariant, `"${expected}" not found in string`)
            break
          }
          if (Array.isArray(at)) {
            if (!at.some(entry => subsetMatch(entry, expected))) {
              fail(failures, doc, index, invariant, `no element matches ${stringify(expected)} in ${stringify(at)}`)
            }
            break
          }
          fail(failures, doc, index, invariant, 'contains requires a string or array at path')
          break
        }
        case 'length':
          if (!Array.isArray(at) || at.length !== expected) {
            fail(failures, doc, index, invariant, `expected length ${String(expected)}, got ${Array.isArray(at) ? String(at.length) : 'non-array'}`)
          }
          break
        case 'gte':
          if (typeof at !== 'number' || typeof expected !== 'number' || at < expected) {
            fail(failures, doc, index, invariant, `expected >= ${String(expected)}, got ${stringify(at)}`)
          }
          break
        case 'defined':
          if (at === undefined) fail(failures, doc, index, invariant, 'expected a defined value')
          break
        case 'notDefined':
          if (at !== undefined) fail(failures, doc, index, invariant, `expected absence, got ${stringify(at)}`)
          break
        case 'fileContains':
        case 'fileNotContains': {
          const cached = invariant.file === undefined ? undefined : files.get(invariant.file)
          if (cached === undefined) {
            fail(failures, doc, index, invariant, `file "${invariant.file}" was not pre-read (runner bug)`)
            break
          }
          if (cached instanceof Error) {
            fail(failures, doc, index, invariant, `file "${invariant.file}" unreadable: ${cached.message}`)
            break
          }
          const has = cached.includes(invariant.text ?? '')
          if (invariant.expect === 'fileContains' && !has) fail(failures, doc, index, invariant, `"${invariant.text}" not in ${invariant.file}`)
          if (invariant.expect === 'fileNotContains' && has) fail(failures, doc, index, invariant, `"${invariant.text}" unexpectedly in ${invariant.file}`)
          break
        }
        case 'itemEvents': {
          const observed = context.events.map(event => event)
          if (!deepEqual(observed, expected)) {
            fail(failures, doc, index, invariant, `item events ${stringify(observed)} != ${stringify(expected)}`)
          }
          break
        }
        default:
          fail(failures, doc, index, invariant, `unknown expect kind`)
      }
    })
  } catch (error) {
    failures.push(`harness error: ${String((error as Error)?.stack ?? error)}`)
  } finally {
    for (const cleanup of cleanups.reverse()) {
      try {
        await cleanup()
      } catch { /* best-effort teardown */ }
    }
    await rm(homeRoot, { recursive: true, force: true }).catch(() => {})
  }
  if (failures.length > 0 && context !== undefined) {
    return { ok: false, name, checks, failures, debugResults: context.results.map(result => maskValue(result, maskKeys)) }
  }
  return { ok: failures.length === 0, name, checks, failures }
}

export { NUDGE_BUDGET }
