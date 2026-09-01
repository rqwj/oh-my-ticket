/**
 * OmtService (plan U7a): the thin DSH adapter's runtime boundary. Owns one
 * @omt/client-ts client lifecycle — discover-or-spawn of `omt-daemon` (via
 * the shared resolveDaemonBinary precedence: explicit option / OMT_DAEMON
 * env, then PATH + known prefixes, then npm platform package — U13/KTD7),
 * handshake as kind:"dsh", automatic reconnection
 * with capped backoff, and disposal — and translates every adapter data op
 * into typed protocol calls (packages/client-ts/src/generated). No module in
 * the host half opens SQLite or reads Markdown directly any more (R1): bare
 * ids are resolved HERE, against the workspace context + the home registry
 * learned at connect, then every wire reference is qualified with a homeId
 * (R4).
 *
 * Adapter-owned state with no dedicated protocol channel in this daemon
 * build:
 *  - executor map: nodeId → DSH session id for items THIS adapter claimed or
 *    dispatched (the daemon records only its own connection actor namespace),
 *    feeding the idle/disposed/notify hooks' session attribution;
 *  - nudge ledger: continuation-nudge bookkeeping (TICKET-0062), kept
 *    in-memory because no nudge-record RPC exists yet (U7b open item).
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { OmtClient, OmtProtocolError } from '@omt/client-ts'
import type {
  ClaimContext,
  ClaimRunResult,
  HandshakeOutcome,
  NodeFilter,
  NodeSummary as ProtoNodeSummary,
  NodeView,
  Progress,
  RunItemView,
  RunStatus as ProtoRunStatus,
  RunView,
  SavedFilters,
  TreeNode,
} from '@omt/client-ts'
import {
  DEFAULT_RUN_CONFIG,
  isRunActive,
  NUDGE_BUDGET,
  OmtError,
  type LineageContent,
  type OmtNode,
  type OmtRun,
  type OmtRunEvent,
  type OmtRunItem,
  type OmtTreeNode,
  type ProblemCode,
  type ProblemDetails,
  type ReindexResult,
  type ReportResult,
  type RunConfig,
  type RunReportOutcome,
  type RunStatus,
  type ShowResult,
} from './types.ts'
import { savedFiltersSchema } from './ui-state.ts'
import { DAEMON_INSTALL_HINT, runtimeUnavailableFromResolution } from './util/daemon-resolve.ts'

/** Minimum spacing between generation-change heals (TICKET-0132): bounds the
 *  cost of a pathological stale-id loop while keeping recovery prompt. */
const SERVICE_HEAL_COOLDOWN_MS = 30_000

/**
 * UI bag key scoping (U4 / R3-R5, KD3): every surface writes its filters bag
 * under its own prefix (DSH → `dsh:ui`, desktop → `tauri:ui`); the server
 * does not enforce the prefix this generation. `recent` is the ONE shared
 * cross-surface key (R4) — legacy per-session recent keys become orphans by
 * design (no delete RPC).
 */
export const DSH_FILTERS_KEY = 'dsh:ui'
export const LEGACY_FILTERS_KEY = 'ui'
export const RECENT_SHARED_KEY = 'recent'

/**
 * One-time import of a pre-U7a `<home>/ui-filters.json` bag into
 * daemon-owned storage (TICKET-0123): parse → `ui/filters-set` under the
 * surface-prefixed bag key (`dsh:ui`, U4), then rename the file `.imported`
 * so the adapter never re-imports and never again writes preference data
 * into a daemon-owned home. Missing/corrupt/unparsable files are skipped
 * silently; a failed daemon push leaves the file in place for the next
 * connect.
 * @internal exported for tests; production callers go through OmtService.
 */
export async function importLegacyUiFiltersFile(
  homePath: string,
  homeId: string,
  setFilters: (homeId: string, key: string, filters: Record<string, unknown>) => Promise<void>,
): Promise<boolean> {
  const { readFile, rename } = await import('node:fs/promises')
  const file = join(homePath, 'ui-filters.json')
  let raw: string
  try {
    raw = await readFile(file, 'utf8')
  } catch {
    return false
  }
  let bag: unknown
  try {
    bag = JSON.parse(raw)
  } catch {
    return false
  }
  const parsed = savedFiltersSchema.safeParse(bag)
  if (!parsed.success) return false
  await setFilters(homeId, DSH_FILTERS_KEY, parsed.data)
  await rename(file, `${file}.imported`)
  return true
}

// Re-exported so adapter surfaces keep importing these shapes from types.ts.
export type { LineageContent, OmtRunEvent }

/** One daemon-opened home as exposed by the handshake. */
export interface HomeRef {
  readonly homeId: string
  readonly name?: string
  readonly kind?: 'workspace' | 'global'
  readonly path?: string
}

export interface OmtServiceOptions {
  /** Explicit runtime dir (tests); default OMT_RUNTIME_DIR then ~/.omt/run. */
  readonly runtimeDir?: string
  /** Daemon binary for spawning; default resolveDaemonBinary precedence
   *  (U13/KTD7): this option / OMT_DAEMON env, then PATH + known prefixes,
   *  then the installed npm platform package. */
  readonly daemonPath?: string
  /** Extra spawn args (e.g. ['--home', path]). */
  readonly daemonArgs?: readonly string[]
  /** Handshake client name. */
  readonly name?: string
  /** Per-request timeout ms forwarded to the transport. */
  readonly requestTimeoutMs?: number
}

// ── protocol view → adapter domain mapping ───────────────────────────────

function nodeOf(view: NodeView): OmtNode {
  return {
    id: view.nodeId,
    type: view.type,
    title: view.title,
    status: view.status,
    archived: view.archived,
    priority: view.priority,
    path: view.path,
    created_at: view.createdAt,
    updated_at: view.updatedAt,
  }
}

/** List/get summaries carry metadata only; placeholders keep tool schemas stable. */
function summaryAsNode(summary: ProtoNodeSummary): OmtNode {
  return {
    id: summary.nodeId,
    type: summary.type,
    title: summary.title,
    status: summary.status,
    archived: summary.archived,
    priority: summary.priority,
    path: '',
    created_at: '',
    updated_at: '',
  }
}

/** Tree nodes carry no path/timestamps on the wire; placeholders keep the shape. */
const WIRE_PLACEHOLDER = ''

function treeNodeOf(view: TreeNode): OmtTreeNode {
  return {
    id: view.nodeId,
    type: view.type,
    title: view.title,
    status: view.status,
    archived: view.archived,
    priority: view.priority,
    path: WIRE_PLACEHOLDER,
    created_at: WIRE_PLACEHOLDER,
    updated_at: WIRE_PLACEHOLDER,
    children: (view.children ?? []).map(treeNodeOf),
  }
}

function runConfigOf(config: RunView['config']): RunConfig {
  return {
    stopOnFailure: config.stopOnFailure ?? DEFAULT_RUN_CONFIG.stopOnFailure,
    autoContinue: config.autoContinue ?? DEFAULT_RUN_CONFIG.autoContinue,
    autoVerify: config.autoVerify ?? DEFAULT_RUN_CONFIG.autoVerify,
    concurrency: config.concurrency ?? DEFAULT_RUN_CONFIG.concurrency,
  }
}

function runOf(view: RunView): OmtRun {
  return {
    id: view.runId,
    ...(view.title !== undefined ? { title: view.title } : {}),
    status: view.status,
    config: runConfigOf(view.config),
    created_at: view.createdAt,
    ...(view.finishedAt !== undefined ? { finished_at: view.finishedAt } : {}),
  }
}

/** camelCase Progress → the adapter's snake_case per-state counts. */
function progressOf(progress: Progress): Record<string, number> {
  return {
    total: typeof progress.total === 'number' ? progress.total : 0,
    pending: progress.pending ?? 0,
    running: progress.running ?? 0,
    done: progress.done ?? 0,
    failed: progress.failed ?? 0,
    blocked: progress.blocked ?? 0,
    skipped: progress.skipped ?? 0,
    interrupted: progress.interrupted ?? 0,
    awaiting_confirmation: progress.awaitingConfirmation ?? 0,
  }
}

/** Convert one protocol run item; executor sessions come from the local map. */
function itemOf(view: RunItemView, actorNamespace: string | undefined, executors: Map<string, string>): OmtRunItem {
  const key = `${view.homeId}:${view.nodeId}`
  const mappedSession = view.executorActor !== undefined && view.executorActor === actorNamespace
    ? executors.get(key) ?? executors.get(view.nodeId)
    : undefined
  return {
    run_id: view.runId,
    node_id: view.nodeId,
    position: view.position,
    state: view.state,
    ...(mappedSession !== undefined || view.executorActor !== undefined
      ? { executor_session_id: mappedSession ?? view.executorActor }
      : {}),
    attempts: view.attempts,
    nudge_count: 0,
    ...(view.lastError !== undefined ? { last_error: view.lastError } : {}),
    ...(view.startedAt !== undefined ? { started_at: view.startedAt } : {}),
    ...(view.finishedAt !== undefined ? { finished_at: view.finishedAt } : {}),
    ...(typeof (view as { title?: unknown }).title === 'string'
      ? { title: (view as { title: string }).title }
      : {}),
  }
}

/** Overlay the adapter-side nudge ledger onto a converted item. The count
 * is historical bookkeeping: it stays visible across state transitions
 * (pre-daemon rows kept it too); only an explicit retry/report clears it. */
function applyLedger(item: OmtRunItem, ledger: Map<string, { count: number; at?: string }>): OmtRunItem {
  const entry = ledger.get(`${item.run_id}:${item.node_id}`)
  if (entry === undefined || entry.count === 0) return item
  return {
    ...item,
    nudge_count: entry.count,
    ...(entry.at !== undefined ? { nudged_at: entry.at } : {}),
  }
}

/**
 * camelCase protocol ClaimContext → the snake_case value the omt_run_claim
 * renderer renders (identical output bytes to the pre-daemon tool).
 */
/**
 * Managed-children block marker pair written into every parent file.
 * Claim-context ancestor bodies strip it (read-only background must not
 * leak the bookkeeping section).
 */
const CHILDREN_BLOCK_RE = /<!-- omt:children -->[\s\S]*?<!-- \/omt:children -->\n?/

function claimContextOf(context: ClaimContext) {
  // Daemon ancestors arrive nearest-parent-first; the pre-daemon contract
  // (and the rendered 背景 order) is root-first, so reverse here. Bodies
  // lose the managed-children block for the same reason.
  const ancestors = [...(context.ancestors ?? [])].reverse()
  return {
    ancestor_budget_bytes: context.ancestorBudgetBytes,
    ancestor_used_bytes: context.ancestorUsedBytes,
    truncated: context.truncated,
    ancestors: ancestors.map(entry => ({
      node: summaryAsNode(entry.node),
      body: entry.body.replace(CHILDREN_BLOCK_RE, '').trim(),
      truncated: entry.truncated,
      original_bytes: entry.originalBytes,
      included_bytes: entry.includedBytes,
    })),
    read_errors: (context.readErrors ?? []).map(entry => ({
      node: summaryAsNode(entry.node),
      error: entry.error,
    })),
    current: {
      node: summaryAsNode(context.current.node),
      body: context.current.body,
    },
  }
}

export type ClaimContextValue = ReturnType<typeof claimContextOf>

// ── change hub (formerly changes.ts; U7a moved here) ────────────────────

/** Run-dimension hint on a change event (which run / item moved). */
export interface OmtRunChangeHint {
  readonly id: string
  readonly kind: 'run' | 'item'
  /** Item-level changes: the member node that transitioned. */
  readonly nodeId?: string
}

export interface OmtChangeEvent {
  readonly version: number
  /** homeId whose data changed (informational; clients refetch their view). */
  readonly home: string
  readonly run?: OmtRunChangeHint
}

/**
 * In-process broadcast of OMT data changes, fed by daemon event envelopes
 * instead of local mutation call sites (U7a). Payloads stay minimal — the
 * browser refetches; this is a notification, not a data channel.
 */
export class ChangeHub {
  private readonly listeners = new Set<(event: OmtChangeEvent) => void>()
  private version = 0

  bump(home: string, run?: OmtRunChangeHint): void {
    this.version += 1
    const event: OmtChangeEvent = { version: this.version, home, ...(run !== undefined ? { run } : {}) }
    for (const listener of this.listeners) {
      try {
        listener(event)
      } catch {
        // A broken subscriber must not break the mutating call.
      }
    }
  }

  subscribe(listener: (event: OmtChangeEvent) => void): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }
}

// ── the service ──────────────────────────────────────────────────────────

interface RunSnapshot {
  run: OmtRun
  items: OmtRunItem[]
}

export class OmtService {
  private readonly client: OmtClient
  /** Change broadcast (SSE route); fed by daemon event envelopes. */
  readonly hub = new ChangeHub()
  private readonly runListeners = new Set<(event: OmtRunEvent) => void>()
  /** nodeId → DSH session attribution for items this adapter dispatched. */
  private readonly executors = new Map<string, string>()
  /** `${runId}:${nodeId}` → nudge bookkeeping (TICKET-0062), adapter-side. */
  private readonly nudges = new Map<string, { count: number; at?: string }>()
  /** Lookup by homeId AND by absolute path (workspace routing). */
  private homeRegistry = new Map<string, HomeRef>()
  private globalHomeRef: HomeRef | undefined
  private actorNamespace: string | undefined
  private connecting: Promise<void> | undefined
  private closed = false
  /** Live events() disposers keyed by homeId; reconciled on every handshake. */
  private readonly eventDisposers = new Map<string, () => void>()
  /** Home ids from PREVIOUS daemon generations — the stale-home guardrail's
   *  whitelist (TICKET-0132): only "our own past" heals, never strangers. */
  private readonly knownHomeIds = new Set<string>()
  private lastHealAttempt = 0
  private healInFlight: Promise<void> | undefined
  private initialLearnDone = false

  constructor(private readonly options: OmtServiceOptions = {}) {
    this.client = new OmtClient({
      runtimeDir: options.runtimeDir,
      daemonPath: options.daemonPath,
      daemonArgs: options.daemonArgs !== undefined ? [...options.daemonArgs] : undefined,
      requestTimeoutMs: options.requestTimeoutMs,
      reconnect: { initialDelayMs: 100, maxDelayMs: 5_000 },
      // TICKET-0131: a daemon generation change mints new home ids; rebuild
      // registry + subscriptions from the fresh handshake automatically.
      onReconnected: handshake => this.learnRuntimeState(handshake),
    })
  }

  /** Change-hub subscription (SSE route). */
  onChange(listener: (event: OmtChangeEvent) => void): () => void {
    return this.hub.subscribe(listener)
  }

  /**
   * Run/item transition stream for the notification hooks. Events are
   * synthesized from daemon envelopes; `run` snapshots are fetched fresh so
   * listeners always see post-change state (core.onRunEvent semantics).
   */
  onRunEvent(listener: (event: OmtRunEvent) => void): () => void {
    this.runListeners.add(listener)
    return () => {
      this.runListeners.delete(listener)
    }
  }

  get connected(): boolean {
    return this.client.connected
  }

  /** Actor namespace of the enrolled credential (`dsh:<pid>`). */
  get actor(): string | undefined {
    return this.actorNamespace
  }

  /** Credential principal id (`dsh:<pid>`; admin-grant bookkeeping). */
  get principalId(): string | undefined {
    return this.client.credential?.principalId
  }

  /** Homes visible to the credential (handshake projection). */
  homes(): HomeRef[] {
    const seen = new Set<string>()
    const result: HomeRef[] = []
    for (const ref of this.homeRegistry.values()) {
      if (seen.has(ref.homeId)) continue
      seen.add(ref.homeId)
      result.push(ref)
    }
    return result
  }

  /**
   * Discover-or-spawn + handshake + registry + subscriptions. Idempotent;
   * concurrent callers share one attempt.
   */
  async ready(): Promise<void> {
    if (this.closed) throw new Error('[omt] service already disposed')
    if (this.client.connected) return
    this.connecting ??= this.connect().finally(() => {
      this.connecting = undefined
    })
    await this.connecting
  }

  private async connect(): Promise<void> {
    try {
      const handshake = await this.client.connect('dsh', {}, this.options.name ?? 'oh-my-ticket-dsh')
      await this.learnRuntimeState(handshake)
    } catch (error) {
      // Graceful degradation: surface an actionable problem instead of a raw
      // stack — the plugin keeps loading without data ops (plan §System-Wide
      // Impact: daemon-absent degradation is an adapter responsibility).
      if (error instanceof OmtProtocolError) throw toOmtError(error)
      // U13/KTD7: an exhausted daemon search (explicit option, OMT_DAEMON,
      // PATH + known prefixes, npm platform package) carries the updated
      // install guidance — product channels (brew tap / install.sh) first,
      // platform packages as fallback.
      const resolutionMiss = runtimeUnavailableFromResolution(error)
      if (resolutionMiss !== undefined) throw resolutionMiss
      throw new OmtError('IO', `OMT runtime unavailable: ${String((error as Error)?.message ?? error)}`, {
        reason: 'runtime-unavailable',
        hint: DAEMON_INSTALL_HINT,
      })
    }
  }

  /**
   * Rebuild ALL handshake-derived state — home registry, actor namespace,
   * admin grant, event subscriptions — from one handshake outcome. Runs on
   * initial connect AND on every automatic reconnect (onReconnected), so a
   * daemon generation change heals the adapter without an instance restart
   * (TICKET-0131). Idempotent; subscription-safe (no double delivery).
   */
  private async learnRuntimeState(handshake: HandshakeOutcome): Promise<void> {
    this.actorNamespace = handshake.credential?.actorNamespace
    const registry = new Map<string, HomeRef>()
    let first: HomeRef | undefined
    let global: HomeRef | undefined
    for (const home of handshake.homes ?? []) {
      const ref: HomeRef = {
        homeId: home.homeId,
        name: home.name,
        kind: home.kind === 'global' ? 'global' : 'workspace',
        path: home.path,
      }
      first ??= ref
      if (ref.kind === 'global') global = ref
      registry.set(ref.homeId, ref)
      if (ref.path !== undefined) registry.set(ref.path, ref)
    }
    // Archive outgoing ids BEFORE replacing the registry: the stale-home
    // guardrail (TICKET-0132) heals only ids this service once trusted.
    for (const prev of this.homeRegistry.values()) this.knownHomeIds.add(prev.homeId)
    if (first !== undefined) this.globalHomeRef = global ?? first
    this.homeRegistry = registry
    this.ensureAdminGrant()
    // Subscription reconcile: dispose subscriptions whose home vanished;
    // never re-subscribe a surviving id — the client's cursor-based replay
    // already covers it (a second events() would double-deliver).
    for (const [id, dispose] of this.eventDisposers) {
      if (!registry.has(id)) {
        dispose()
        this.eventDisposers.delete(id)
      }
    }
    for (const home of handshake.homes ?? []) {
      if (!this.eventDisposers.has(home.homeId)) this.subscribeEvents(home.homeId)
    }
    // TICKET-0123: one-time import of pre-U7a preference files so the
    // adapter never needs to write into a daemon-owned home again. The
    // imported files are renamed on success, so this is naturally once-only;
    // the flag additionally keeps reconnects free of filesystem probing.
    if (!this.initialLearnDone) {
      this.initialLearnDone = true
      await this.migrateLegacyUiFilters(registry.values())
    }
  }

  /**
   * `home/reindex` stays a model-facing tool (R13 name preservation) although
   * the parity matrix classes it human_administrative. The local OS account
   * is the trust boundary (KTD9), so the adapter grants ITS OWN principal —
   * never another principal's — via the out-of-band grants file, which the
   * daemon re-reads fresh on every check.
   */
  private ensureAdminGrant(): void {
    const principalId = this.principalId
    if (principalId === undefined) return
    try {
      const dir = OmtClient.resolveRuntimeDir(this.options.runtimeDir)
      const file = join(dir, 'admin-grants.json')
      let ids: string[] = []
      try {
        const parsed: unknown = JSON.parse(readFileSync(file, 'utf8'))
        if (parsed !== null && typeof parsed === 'object' && Array.isArray((parsed as { principalIds?: unknown }).principalIds)) {
          ids = (parsed as { principalIds: unknown[] }).principalIds.filter((id): id is string => typeof id === 'string')
        }
      } catch {
        /* absent or unreadable: start a fresh grant list */
      }
      if (!ids.includes(principalId)) ids.push(principalId)
      mkdirSync(dir, { recursive: true })
      writeFileSync(file, `${JSON.stringify({ principalIds: ids }, null, 2)}\n`, 'utf8')
    } catch {
      /* best-effort: reindex degrades with FORBIDDEN when ungranted */
    }
  }

  private subscribeEvents(homeId: string): void {
    const dispose = this.client.events(homeId, envelope => {
      const payload = envelope.payload as { kind?: string; ref?: Record<string, unknown>; state?: string } | undefined
      const kind = payload?.kind
      const ref = payload?.ref ?? {}
      const runId = typeof ref.runId === 'string' ? ref.runId : undefined
      const nodeId = typeof ref.nodeId === 'string' ? ref.nodeId : undefined
      if (kind === 'node.changed') {
        this.hub.bump(homeId)
        return
      }
      if ((kind === 'run.changed' || kind === 'run.item_changed') && runId !== undefined) {
        this.hub.bump(homeId, {
          id: runId,
          kind: kind === 'run.changed' ? 'run' : 'item',
          ...(nodeId !== undefined ? { nodeId } : {}),
        })
        void this.emitRunEvent(homeId, runId, kind === 'run.item_changed' ? nodeId : undefined)
        return
      }
      // attention.raised / snapshot.resync / node.quarantined: informational.
      this.hub.bump(homeId)
    }, { onError: () => {} })
    this.eventDisposers.set(homeId, dispose)
  }

  // ── stale-home guardrail (TICKET-0132) ────────────────────────────────

  /**
   * Extract the home id from a NOT_FOUND(kind:home) protocol error, if that
   * is what `error` is. Any other error → undefined.
   */
  private staleHomeIdProblem(error: unknown): string | undefined {
    if (!(error instanceof OmtProtocolError)) return undefined
    if (error.problemCode !== 'NOT_FOUND') return undefined
    const details = error.details as { kind?: unknown; id?: unknown } | null
    if (details?.kind !== 'home') return undefined
    return typeof details.id === 'string' && details.id !== '' ? details.id : undefined
  }

  /**
   * Heal a daemon generation change: drop every subscription, force a fresh
   * discover-or-spawn + handshake, rebuild state. Cooldown-guarded so a
   * pathological caller cannot cause a handshake storm; single-flight so
   * concurrent failures share one attempt. Within the cooldown this waits
   * for an in-flight heal (if any) and otherwise returns immediately — the
   * retry then simply runs against current state.
   */
  private async refreshAfterGenerationChange(): Promise<void> {
    const now = Date.now()
    if (now - this.lastHealAttempt < SERVICE_HEAL_COOLDOWN_MS) {
      if (this.healInFlight !== undefined) await this.healInFlight.catch(() => {})
      return
    }
    this.lastHealAttempt = now
    const attempt = (async () => {
      for (const dispose of this.eventDisposers.values()) dispose()
      this.eventDisposers.clear()
      const handshake = await this.client.forceReconnect(10_000)
      await this.learnRuntimeState(handshake)
    })()
    this.healInFlight = attempt
    try {
      await attempt
    } finally {
      if (this.healInFlight === attempt) this.healInFlight = undefined
    }
  }

  /**
   * Single RPC bridge for ALL data operations: on NOT_FOUND(kind:home) with
   * an id THIS service previously learned, heal the generation change once
   * and retry the operation exactly once. The daemon rejects home resolution
   * before executing anything, so the retry is effect-safe for reads AND
   * writes. Everything else — including unknown home ids and every non-stale
   * failure — propagates AS-IS: call sites branch on OmtProtocolError
   * themselves (multi-home ownership probes, NOT_FOUND→undefined maps), so
   * the bridge must not change the error contract.
   */
  private async rpc<T>(
    method: string,
    params: Record<string, unknown>,
    hooks?: { onIssued?: (id: number) => void },
  ): Promise<T> {
    try {
      return await this.client.call<T>(method, params, hooks)
    } catch (error) {
      const staleId = this.staleHomeIdProblem(error)
      if (staleId === undefined || !this.knownHomeIds.has(staleId)) throw error
      try {
        await this.refreshAfterGenerationChange()
      } catch {
        // Healing itself failed (e.g. daemon still down): surface the
        // ORIGINAL problem — it names the actual missing home.
        throw error
      }
      try {
        return await this.client.call<T>(method, params, hooks)
      } catch (retryError) {
        throw retryError
      }
    }
  }

  /** Fetch post-change snapshots and fan out to hook listeners. */
  private async emitRunEvent(homeId: string, runId: string, nodeId: string | undefined): Promise<void> {
    if (this.runListeners.size === 0) return
    try {
      const snapshot = await this.fetchRun(homeId, runId)
      const item = nodeId !== undefined ? snapshot.items.find(candidate => candidate.node_id === nodeId) : undefined
      const event: OmtRunEvent =
        item !== undefined
          ? { kind: 'item', run: snapshot.run, item, items: snapshot.items }
          : { kind: 'run', run: snapshot.run, items: snapshot.items }
      for (const listener of [...this.runListeners]) {
        try {
          listener(event)
        } catch (error: unknown) {
          console.warn('[omt] run-event listener failed', error)
        }
      }
    } catch {
      /* the run may be gone between envelope and fetch; nothing to deliver */
    }
  }

  async close(): Promise<void> {
    this.closed = true
    this.runListeners.clear()
    this.executors.clear()
    this.nudges.clear()
    this.client.close()
  }

  private assertNotClosed(): void {
    if (this.closed) throw new Error('[omt] service already disposed')
  }

  // ── home resolution ───────────────────────────────────────────────────

  /** The global home (handshake kind:"global", else first known). */
  globalHome(): HomeRef {
    this.assertNotClosed()
    if (this.globalHomeRef === undefined) throw noRuntime()
    return this.globalHomeRef
  }

  /**
   * Workspace home for a cwd — present only when the workspace carries an
   * `.omt/` directory AND the daemon opened it (declared at startup or via
   * home/declare, U5/U6).
   */
  workspaceHome(cwd: string | undefined): HomeRef | undefined {
    if (cwd === undefined) return undefined
    const local = join(cwd, '.omt')
    if (!existsSync(local)) return undefined
    return this.homeRegistry.get(local)
  }

  /**
   * U6/R8: declare an on-disk-but-unregistered workspace home into the
   * running daemon, then re-handshake so the new home enters this
   * credential's scoped grant (requiresRehandshake). Returns the fresh
   * HomeRef, or undefined when the daemon predates home/declare
   * (features.homeDeclare absent — F4 version-drift fallback keeps the
   * legacy hard error for that case). Idempotent per path: the daemon
   * dedupes by canonical path, and a second call finds the home already
   * in the registry before ever declaring again.
   */
  private async declareWorkspaceHome(local: string): Promise<HomeRef | undefined> {
    if (this.client.features['homeDeclare'] !== true) return undefined
    const result = await this.client.declareHome(local)
    if (result.requiresRehandshake) {
      await this.client.forceReconnect()
    }
    return this.homeRegistry.get(local)
  }

  /** Resolution rule (pool.ts carried over): workspace wins, global falls back. */
  async homeFor(cwd: string | undefined): Promise<HomeRef> {
    await this.ready()
    const workspace = this.workspaceHome(cwd)
    if (workspace !== undefined) return workspace
    if (cwd !== undefined && existsSync(join(cwd, '.omt'))) {
      const local = join(cwd, '.omt')
      // U6: declare-then-retry exactly once (the declare itself is the
      // retry — the daemon's idempotency makes a repeat resolution a
      // registry hit, never a second declare).
      const declared = await this.declareWorkspaceHome(local)
      if (declared !== undefined) return declared
      throw new OmtError('INVALID_INPUT',
        `工作区 home ${local} 存在，但当前 omt-daemon 未收录该 home（daemon 版本过旧不支持 home/declare，请升级 daemon）`,
        { rule: 'home-not-opened', path: local })
    }
    return this.globalHome()
  }

  /** Explicit create target ('workspace' implies opting in even when absent on disk). */
  async homeForScope(cwd: string | undefined, scope: 'workspace' | 'global'): Promise<HomeRef> {
    await this.ready()
    if (scope === 'workspace') {
      if (cwd === undefined) return this.globalHome()
      const local = join(cwd, '.omt')
      const known = this.homeRegistry.get(local)
      if (known !== undefined) return known
      const declared = await this.declareWorkspaceHome(local)
      if (declared !== undefined) return declared
      throw new OmtError('INVALID_INPUT',
        `工作区 home ${local} 尚未在 omt-daemon 注册，且 daemon 不支持 home/declare 动态收录（请升级 daemon）`,
        { rule: 'home-not-opened', path: local })
    }
    return this.globalHome()
  }

  /** Candidate homes for ownership probes (workspace first, then global). */
  private candidateHomes(cwd: string | undefined): HomeRef[] {
    const homes: HomeRef[] = []
    const workspace = this.workspaceHome(cwd)
    if (workspace !== undefined) homes.push(workspace)
    const global = this.globalHomeRef
    if (global !== undefined && !homes.includes(global)) homes.push(global)
    return homes
  }

  /** Resolve the home CONTAINING a node id (pool.coreForNode heir). */
  async resolveNodeHome(nodeId: string, cwd: string | undefined): Promise<{ home: HomeRef; node?: OmtNode }> {
    this.assertNotClosed()
    await this.ready()
    let fallbackProblem: unknown
    for (const home of this.candidateHomes(cwd)) {
      try {
        const result = await this.rpc<{ node: NodeView }>('node/get', { homeId: home.homeId, nodeId })
        return { home, node: nodeOf(result.node) }
      } catch (error) {
        fallbackProblem ??= error
        if (!(error instanceof OmtProtocolError) || error.problemCode !== 'NOT_FOUND') throw wrap(error)
      }
    }
    const details: ProblemDetails = { kind: 'node', id: nodeId }
    if (fallbackProblem instanceof OmtProtocolError && fallbackProblem.details !== null) {
      details.problem = fallbackProblem.details as ProblemDetails
    }
    throw new OmtError('NOT_FOUND', `unknown node: ${nodeId}`, details)
  }

  /** Resolve the home CONTAINING a run id (run ids count per home). */
  async resolveRunHome(runId: string, cwd: string | undefined): Promise<HomeRef> {
    this.assertNotClosed()
    await this.ready()
    for (const home of this.candidateHomes(cwd)) {
      try {
        await this.rpc('run/get', { homeId: home.homeId, runId })
        return home
      } catch (error) {
        if (!(error instanceof OmtProtocolError) || error.problemCode !== 'NOT_FOUND') throw wrap(error)
      }
    }
    throw new OmtError('NOT_FOUND', `unknown run: ${runId}`, { kind: 'run', id: runId })
  }

  // ── nodes ─────────────────────────────────────────────────────────────

  async createNode(
    home: HomeRef,
    input: { type: string; title: string; parentId?: string; body?: string; priority?: number },
  ): Promise<OmtNode> {
    await this.ready()
    try {
      const result = await this.rpc<{ node: NodeView }>('node/create', {
        homeId: home.homeId,
        type: input.type,
        title: input.title,
        ...(input.parentId !== undefined ? { parentId: input.parentId } : {}),
        ...(input.body !== undefined ? { body: input.body } : {}),
        ...(input.priority !== undefined ? { priority: input.priority } : {}),
      })
      return nodeOf(result.node)
    } catch (error) {
      throw wrap(error)
    }
  }

  async listNodes(home: HomeRef, filter: { type?: string; status?: string; query?: string } = {}): Promise<OmtNode[]> {
    await this.ready()
    try {
      const protoFilter: NodeFilter = {
        ...(filter.type !== undefined ? { type: filter.type as NodeFilter['type'] } : {}),
        ...(filter.status !== undefined ? { status: filter.status as NodeFilter['status'] } : {}),
        ...(filter.query !== undefined ? { query: filter.query } : {}),
      }
      const result = await this.rpc<{ nodes: ProtoNodeSummary[] }>('node/list', {
        homeId: home.homeId,
        filter: protoFilter,
      })
      return (result.nodes ?? []).map(summaryAsNode)
    } catch (error) {
      throw wrap(error)
    }
  }

  async showNode(id: string, cwd: string | undefined): Promise<ShowResult & { home: HomeRef; runs: Array<{ runId: string; title?: string; status: RunStatus; itemState: string; progress: Record<string, number> }> }> {
    const { home } = await this.resolveNodeHome(id, cwd)
    try {
      const result = await this.rpc<{
        node: NodeView
        parent?: ProtoNodeSummary
        children: ProtoNodeSummary[]
        body: string
        runs?: Array<{ runId: string; title?: string; status: ProtoRunStatus; itemState: string; progress: Progress }>
      }>('node/get', { homeId: home.homeId, nodeId: id })
      return {
        home,
        node: nodeOf(result.node),
        ...(result.parent != null ? { parent: summaryAsNode(result.parent) } : {}),
        children: (result.children ?? []).map(summaryAsNode),
        body: result.body,
        runs: (result.runs ?? []).map(link => ({
          runId: link.runId,
          ...(link.title !== undefined ? { title: link.title } : {}),
          status: link.status,
          itemState: link.itemState,
          progress: progressOf(link.progress),
        })),
      }
    } catch (error) {
      throw wrap(error)
    }
  }

  /** Direct ownership probe (cross-home move guard). */
  async getNodeIn(home: HomeRef, id: string): Promise<OmtNode | undefined> {
    await this.ready()
    try {
      const result = await this.rpc<{ node: NodeView }>('node/get', { homeId: home.homeId, nodeId: id })
      return nodeOf(result.node)
    } catch (error) {
      if (error instanceof OmtProtocolError && error.problemCode === 'NOT_FOUND') return undefined
      throw wrap(error)
    }
  }

  async updateNode(
    input: {
      id: string
      title?: string
      status?: string
      archived?: boolean
      priority?: number
      body?: string
      append?: string
    },
    context: { cwd?: string; sessionId?: string } = {},
  ): Promise<{ node: OmtNode; home: HomeRef }> {
    const { home } = await this.resolveNodeHome(input.id, context.cwd)
    try {
      const result = await this.rpc<{ node: NodeView }>('node/update', {
        homeId: home.homeId,
        nodeId: input.id,
        changes: {
          ...(input.title !== undefined ? { title: input.title } : {}),
          ...(input.status !== undefined ? { status: input.status } : {}),
          ...(input.archived !== undefined ? { archived: input.archived } : {}),
          ...(input.priority !== undefined ? { priority: input.priority } : {}),
          ...(input.body !== undefined ? { body: input.body } : {}),
          ...(input.append !== undefined ? { append: input.append } : {}),
        },
      })
      // Executor attribution for hooks: observation dispatch rides the
      // shared actor namespace, so record the session locally. Claim wins
      // (TICKET-0061): an existing entry — a live claim — is never
      // overwritten by a later manual in_progress flip.
      if (context.sessionId !== undefined && input.status === 'in_progress' && !this.executors.has(input.id)) {
        this.noteExecutor(input.id, context.sessionId)
      }
      return { node: nodeOf(result.node), home }
    } catch (error) {
      throw wrap(error)
    }
  }

  async executeNode(id: string, context: { cwd?: string; sessionId?: string } = {}): Promise<{ node: OmtNode; home: HomeRef }> {
    const { home } = await this.resolveNodeHome(id, context.cwd)
    try {
      const result = await this.rpc<{ node: NodeView }>('node/execute', { homeId: home.homeId, nodeId: id })
      if (context.sessionId !== undefined) this.noteExecutor(id, context.sessionId)
      return { node: nodeOf(result.node), home }
    } catch (error) {
      throw wrap(error)
    }
  }

  /** Archive (true, dedicated verb) or restore (false, via update). */
  async setArchived(id: string, archived: boolean, context: { cwd?: string } = {}): Promise<{ node: OmtNode; home: HomeRef }> {
    const resolved = await this.resolveNodeHome(id, context.cwd)
    try {
      if (archived) {
        const result = await this.rpc<{ node: NodeView }>('node/archive', { homeId: resolved.home.homeId, nodeId: id })
        return { node: nodeOf(result.node), home: resolved.home }
      }
      const result = await this.rpc<{ node: NodeView }>('node/update', {
        homeId: resolved.home.homeId,
        nodeId: id,
        changes: { archived: false },
      })
      return { node: nodeOf(result.node), home: resolved.home }
    } catch (error) {
      throw wrap(error)
    }
  }

  async moveNode(id: string, newParentId: string, cwd: string | undefined): Promise<{ node: OmtNode; home: HomeRef }> {
    const { home } = await this.resolveNodeHome(id, cwd)
    const parent = await this.getNodeIn(home, newParentId)
    if (parent === undefined) {
      throw new Error('omt_move 不支持跨 home 移动（节点与目标父节点不在同一个 OMT home）')
    }
    try {
      const result = await this.rpc<{ node: NodeView }>('node/move', {
        homeId: home.homeId,
        nodeId: id,
        newParentId,
      })
      return { node: nodeOf(result.node), home }
    } catch (error) {
      throw wrap(error)
    }
  }

  async reindex(home: HomeRef): Promise<ReindexResult> {
    await this.ready()
    try {
      const result = await this.rpc<{ nodes: number; edges: number; skipped: number }>('home/reindex', {
        homeId: home.homeId,
      })
      return { nodes: result.nodes, edges: result.edges, skipped: result.skipped }
    } catch (error) {
      throw wrap(error)
    }
  }

  async tree(home: HomeRef, rootId?: string): Promise<OmtTreeNode[]> {
    await this.ready()
    try {
      const result = await this.rpc<{ trees: TreeNode[] }>('node/tree', {
        homeId: home.homeId,
        ...(rootId !== undefined ? { rootId } : {}),
      })
      return (result.trees ?? []).map(treeNodeOf)
    } catch (error) {
      throw wrap(error)
    }
  }

  async search(
    home: HomeRef,
    query: string,
    limit: number,
  ): Promise<Array<{ id: string; type: ProtoNodeSummary['type']; title: string; status: ProtoNodeSummary['status']; archived: boolean; priority: number }>> {
    await this.ready()
    try {
      const result = await this.rpc<{ nodes: ProtoNodeSummary[] }>('node/search', {
        homeId: home.homeId,
        query,
        limit,
      })
      return (result.nodes ?? []).map(summary => ({
        id: summary.nodeId,
        type: summary.type,
        title: summary.title,
        status: summary.status,
        archived: summary.archived,
        priority: summary.priority,
      }))
    } catch (error) {
      throw wrap(error)
    }
  }

  // ── runs ──────────────────────────────────────────────────────────────

  async createRun(
    home: HomeRef,
    input: { title?: string; config?: Partial<RunConfig>; nodeIds: readonly string[] },
  ): Promise<{ run: OmtRun; items: OmtRunItem[] }> {
    await this.ready()
    try {
      const result = await this.rpc<{ run: RunView; items: RunItemView[] }>('run/create', {
        homeId: home.homeId,
        ...(input.title !== undefined ? { title: input.title } : {}),
        ...(input.config !== undefined ? { config: input.config } : {}),
        nodeIds: [...input.nodeIds],
      })
      return {
        run: runOf(result.run),
        items: (result.items ?? []).map(item => itemOf(item, this.actorNamespace, this.executors)),
      }
    } catch (error) {
      throw wrap(error)
    }
  }

  async listRuns(home: HomeRef, status?: RunStatus): Promise<OmtRun[]> {
    await this.ready()
    try {
      const result = await this.rpc<{ runs: RunView[] }>('run/list', {
        homeId: home.homeId,
        ...(status !== undefined ? { status: status as ProtoRunStatus } : {}),
      })
      return (result.runs ?? []).map(runOf)
    } catch (error) {
      throw wrap(error)
    }
  }

  /**
   * Run list WITH derived progress (omt_run_list / run-list endpoint). One
   * wire call per home; progress keys convert to the adapter's snake_case.
   */
  async listRunSummaries(home: HomeRef, status?: RunStatus): Promise<Array<{ run: OmtRun; progress: Record<string, number> }>> {
    await this.ready()
    try {
      const result = await this.rpc<{ runs: RunView[] }>('run/list', {
        homeId: home.homeId,
        ...(status !== undefined ? { status: status as ProtoRunStatus } : {}),
      })
      return (result.runs ?? []).map(view => ({
        run: runOf(view),
        progress: progressOf(view.progress),
      }))
    } catch (error) {
      throw wrap(error)
    }
  }

  /** Run + membership snapshot (NOT_FOUND for unknown runs, like requireRun). */
  async fetchRun(homeOrId: HomeRef | string, runId: string): Promise<RunSnapshot> {
    await this.ready()
    const homeId = typeof homeOrId === 'string' ? homeOrId : homeOrId.homeId
    try {
      const result = await this.rpc<{ run: RunView; items: RunItemView[] }>('run/get', { homeId, runId })
      return {
        run: runOf(result.run),
        items: (result.items ?? [])
          .map(item => itemOf(item, this.actorNamespace, this.executors))
          .map(item => applyLedger(item, this.nudges)),
      }
    } catch (error) {
      throw wrap(error)
    }
  }

  async controlRun(
    homeOrId: HomeRef | string,
    runId: string,
    action: 'start' | 'pause' | 'resume' | 'cancel' | 'retry' | 'remove',
    nodeId?: string,
  ): Promise<{ run: OmtRun; item?: OmtRunItem }> {
    await this.ready()
    const homeId = typeof homeOrId === 'string' ? homeOrId : homeOrId.homeId
    try {
      // The daemon answers every control action with the full run view
      // ({run, items}); a retry/remove response's controlled item is the
      // membership row matching nodeId.
      const result = await this.rpc<{ run: RunView; items?: RunItemView[] }>('run/control', {
        homeId,
        runId,
        action,
        ...(nodeId !== undefined ? { nodeId } : {}),
      })
      if ((action === 'retry' || action === 'remove') && nodeId !== undefined) this.nudges.delete(`${runId}:${nodeId}`)
      const items = (result.items ?? []).map(view => applyLedger(itemOf(view, this.actorNamespace, this.executors), this.nudges))
      const controlled = nodeId !== undefined ? items.find(item => item.node_id === nodeId) : undefined
      return {
        run: runOf(result.run),
        ...(controlled !== undefined ? { item: controlled } : {}),
      }
    } catch (error) {
      throw wrap(error)
    }
  }

  /**
   * Claim the next pending item. The daemon binds the ADAPTER's actor
   * namespace as executor (R12); the DSH session is recorded in the local
   * executor map so hooks can still attribute work per session.
   */
  async claimItem(homeOrId: HomeRef | string, runId: string, sessionId: string): Promise<ClaimOutcome> {
    await this.ready()
    const homeId = typeof homeOrId === 'string' ? homeOrId : homeOrId.homeId
    let result: ClaimRunResult
    try {
      result = await this.rpc<ClaimRunResult>('run/claim', { homeId, runId })
    } catch (error) {
      throw wrap(error)
    }
    if (!result.claimed || result.item === undefined) {
      return { homeId, runId, claimed: false }
    }
    // Record the session BEFORE mapping: itemOf translates the daemon's
    // actor-namespace executor into the local session id.
    this.noteExecutor(result.item.nodeId, sessionId)
    const item = applyLedger(itemOf(result.item, this.actorNamespace, this.executors), this.nudges)
    const ticket = result.ticket !== undefined ? nodeOf(result.ticket) : undefined
    return {
      homeId,
      runId,
      claimed: true,
      leaseToken: result.lease?.token,
      item,
      ...(ticket !== undefined ? { ticket } : {}),
      ...(result.context !== undefined ? { context: claimContextOf(result.context) } : {}),
      ...(result.contextError !== undefined ? { context_error: result.contextError } : {}),
    }
  }

  async reportItem(
    homeOrId: HomeRef | string,
    runId: string,
    nodeId: string,
    outcome: RunReportOutcome,
    note?: string,
    leaseToken?: string,
  ): Promise<ReportResult & { homeId: string; run: OmtRun }> {
    await this.ready()
    const homeId = typeof homeOrId === 'string' ? homeOrId : homeOrId.homeId
    try {
      const result = await this.rpc<{ run: RunView; item: RunItemView; node: NodeView }>('run/report', {
        homeId,
        runId,
        nodeId,
        outcome,
        ...(note !== undefined ? { note } : {}),
        ...(leaseToken !== undefined ? { leaseToken } : {}),
      })
      this.nudges.delete(`${runId}:${nodeId}`)
      // Pre-daemon parity (core.reportRunItem): a failed note lands in
      // last_error AND the ticket's progress record. The daemon appends
      // notes only on node-transitioning outcomes (done/blocked/skipped),
      // so the failed case appends here — best-effort, after the item has
      // already transitioned.
      if (outcome === 'failed' && note !== undefined && note.trim() !== '') {
        try {
          await this.rpc('node/update', {
            homeId: (typeof homeOrId === 'string' ? homeId : homeOrId.homeId),
            nodeId,
            changes: { append: note },
          })
        } catch {
          /* cosmetic append must never fail the report */
        }
      }
      return {
        homeId,
        run: runOf(result.run),
        item: itemOf(result.item, this.actorNamespace, this.executors),
        node: nodeOf(result.node),
      }
    } catch (error) {
      throw wrap(error)
    }
  }

  // ── hook-facing queries (session attribution via the executor map) ────

  /**
   * Every item owned by one DSH session across active runs of the resolved
   * home (disposed-hook involvement probe; core.executorItems heir).
   * Ownership survives completion, so callers filter by item state.
   */
  async executorItems(sessionId: string, cwd: string | undefined): Promise<{ home: HomeRef; run: OmtRun; item: OmtRunItem }[]> {
    const home = await this.homeFor(cwd)
    const owned: { home: HomeRef; run: OmtRun; item: OmtRunItem }[] = []
    for (const run of await this.listRuns(home)) {
      if (!isRunActive(run.status)) continue
      const snapshot = await this.fetchRun(home, run.id)
      for (const item of snapshot.items) {
        if (this.sessionOfItem(home.homeId, item) === sessionId) owned.push({ home, run: snapshot.run, item })
      }
    }
    return owned
  }

  /**
   * Continuation candidates (TICKET-0062; core.continuationCandidates heir):
   * for every RUNNING autoContinue run where `sessionId` owns at least one
   * item, the next pending item in position order. Paused runs dispatch no
   * nudges; stalled items stay visible through the nudge ledger.
   */
  async continuationCandidates(sessionId: string, cwd: string | undefined): Promise<{ home: HomeRef; run: OmtRun; item: OmtRunItem }[]> {
    const home = await this.homeFor(cwd)
    const candidates: { home: HomeRef; run: OmtRun; item: OmtRunItem }[] = []
    for (const run of await this.listRuns(home)) {
      if (run.status !== 'running' || !run.config.autoContinue) continue
      const snapshot = await this.fetchRun(home, run.id)
      if (!snapshot.items.some(item => this.sessionOfItem(home.homeId, item) === sessionId)) continue
      const next = snapshot.items
        .slice()
        .sort((a, b) => a.position - b.position)
        .find(item => item.state === 'pending')
      if (next !== undefined) candidates.push({ home, run: snapshot.run, item: applyLedger(next, this.nudges) })
    }
    return candidates
  }

  /** Durable nudge bookkeeping via run/nudge-record (TICKET-0130 item 4):
   * the daemon persists nudged_at/nudge_count on run_items; the local
   * ledger mirrors the server count so applyLedger stays view-consistent. */
  async recordItemNudge(homeOrId: HomeRef | string, runId: string, nodeId: string, at: string): Promise<OmtRunItem> {
    const homeId = typeof homeOrId === 'string' ? homeOrId : homeOrId.homeId
    await this.ready()
    const result = await this.rpc('run/nudge-record', { homeId, runId, nodeId }) as {
      nudged?: { nodeId: string; nudgeCount: number }[]
    }
    const recorded = result.nudged?.find(entry => entry.nodeId === nodeId)
    const count = recorded?.nudgeCount ?? 1
    this.nudges.set(`${runId}:${nodeId}`, { count, at })
    const snapshot = await this.fetchRun(homeId, runId)
    const item = snapshot.items.find(candidate => candidate.node_id === nodeId)
    if (item === undefined) {
      throw new OmtError('NOT_FOUND', `run ${runId} has no item for node: ${nodeId}`, { kind: 'run-item', runId, nodeId })
    }
    return { ...item, nudge_count: count, ...(at !== undefined ? { nudged_at: at } : {}) }
  }

  private sessionOfItem(homeId: string, item: OmtRunItem): string | undefined {
    if (item.executor_session_id === undefined) return undefined
    if (item.executor_session_id === this.actorNamespace) {
      return this.executors.get(`${homeId}:${item.node_id}`) ?? this.executors.get(item.node_id)
    }
    return item.executor_session_id
  }

  private noteExecutor(nodeId: string, sessionId: string): void {
    this.executors.set(nodeId, sessionId)
  }

  // ── ui bags ───────────────────────────────────────────────────────────

  /** Saved tree filters (STORY-0023), persisted per home+key by the daemon. */
  async filtersGet(home: HomeRef, key: string): Promise<SavedFilters> {
    await this.ready()
    try {
      const result = await this.rpc<{ filters: SavedFilters }>('ui/filters-get', { homeId: home.homeId, key })
      return result.filters ?? {}
    } catch (error) {
      throw wrap(error)
    }
  }

  /**
   * DSH-surface filters read with legacy migration (U4/R3-R5): read the
   * prefixed `dsh:ui` bag; on an empty miss fall back to the bare `'ui'`
   * bag written by pre-U4 builds and WRITE THROUGH to the prefixed key so
   * the next read is single-legged. The bare key is never deleted (no
   * delete RPC) — it stays as an orphan by design.
   */
  async filtersGetDsh(home: HomeRef): Promise<SavedFilters> {
    const primary = await this.filtersGet(home, DSH_FILTERS_KEY)
    if (Object.keys(primary).length > 0) return primary
    const legacy = await this.filtersGet(home, LEGACY_FILTERS_KEY)
    if (Object.keys(legacy).length > 0) {
      await this.filtersSet(home, DSH_FILTERS_KEY, legacy)
    }
    return legacy
  }

  async filtersSet(home: HomeRef, key: string, filters: SavedFilters): Promise<SavedFilters> {
    await this.ready()
    try {
      const result = await this.rpc<{ filters: SavedFilters }>('ui/filters-set', { homeId: home.homeId, key, filters })
      return result.filters ?? {}
    } catch (error) {
      throw wrap(error)
    }
  }

  /** Best-effort legacy preference import across every open home (TICKET-0123). */
  private async migrateLegacyUiFilters(homes: Iterable<HomeRef>): Promise<void> {
    for (const home of homes) {
      if (home.path === undefined) continue
      try {
        await importLegacyUiFiltersFile(home.path, home.homeId, async (homeId, key, filters) => {
          await this.rpc('ui/filters-set', { homeId, key, filters })
        })
      } catch {
        // Preference migration must never block plugin load (STORY-0023 rule).
      }
    }
  }

  /**
   * Session recent lists (TICKET-0019), routed through ui/recent-get|set.
   * Bags live on the GLOBAL home (server-side routing without homeId);
   * entries are qualified refs on the wire — the adapter stores bare node
   * ids and re-resolves them by ownership on read, exactly like the
   * pre-daemon meta rows did.
   */
  async recentGet(key: string): Promise<string[] | undefined> {
    await this.ready()
    try {
      const result = await this.rpc<{ refs: Array<{ homeId: string; nodeId: string }> }>('ui/recent-get', { key })
      return (result.refs ?? []).map(ref => ref.nodeId)
    } catch (error) {
      throw wrap(error)
    }
  }

  async recentSet(key: string, ids: readonly string[]): Promise<void> {
    await this.ready()
    try {
      await this.rpc('ui/recent-set', {
        key,
        refs: ids.map(nodeId => ({ homeId: '', nodeId })),
      })
    } catch (error) {
      throw wrap(error)
    }
  }
}

/** Outcome of {@link OmtService.claimItem}. */
export interface ClaimOutcome {
  readonly homeId: string
  readonly runId: string
  readonly claimed: boolean
  readonly leaseToken?: string
  readonly item?: OmtRunItem
  readonly ticket?: OmtNode
  readonly context?: ClaimContextValue
  readonly context_error?: string
}

// ── errors ───────────────────────────────────────────────────────────────

function noRuntime(): OmtError {
  return new OmtError('IO', 'OMT runtime unavailable: service is not connected', { reason: 'runtime-unavailable' })
}

/**
 * Protocol problems carry stable codes; preserve code+details verbatim (R5)
 * so adapter assertions bind to codes/details exactly like pre-daemon ones.
 */
function toOmtError(error: OmtProtocolError): OmtError {
  const details = (error.details ?? {}) as ProblemDetails
  return new OmtError(error.problemCode as ProblemCode, error.message, details)
}

function wrap(error: unknown): unknown {
  if (error instanceof OmtProtocolError) return toOmtError(error)
  return error
}
