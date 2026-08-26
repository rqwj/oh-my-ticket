/**
 * OMT client library (plan U5a): discovery or detached spawn of omt-daemon,
 * handshake/enrollment, typed RPC calls, and auto-resuming event streams.
 *
 * Discovery order (F1):
 *   1. read `<runtime-dir>/descriptor.json` and probe liveness
 *      (pid alive AND the endpoint accepts a connection),
 *   2. otherwise spawn a fresh daemon detached from
 *      `OMT_DAEMON` (explicit binary path) or `omt-daemon` on PATH,
 *   3. poll for the new generation's descriptor for up to 10 s.
 *
 * The runtime directory resolves as `OMT_RUNTIME_DIR` (tests/sandboxes)
 * then `~/.omt/run`, matching crates/omt-runtime/src/paths.rs. The full
 * resolution contract (precedence, multi-surface agreement, invariants)
 * lives in docs/runtime/config.md (U2/R2 backlink).
 */
import { spawn } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'
import {
  Transport,
  OmtProtocolError,
  type ProblemShape,
} from './transport.js'
import type { EventEnvelope } from './generated/events.js'

export { OmtProtocolError }
export type { ProblemShape }

/** Descriptor published atomically by the daemon (schemaVersion 1). */
export interface DaemonDescriptor {
  schemaVersion: number
  endpoint: string
  generation: number
  pid: number
  bootToken: string
  startedAt: string
}

/** Credential block issued by handshake/request (additive on capabilities). */
export interface CredentialInfo {
  token: string
  principalId: string
  actorNamespace: string
  homes: string[]
  operations: string[]
  expiresAt: string
}

export interface HandshakeOutcome {
  protocolVersion: string
  daemon: { name: string; version: string }
  homes: Array<{ homeId: string; name?: string; kind?: string; path?: string }>
  credential: CredentialInfo
  [k: string]: unknown
}

export type ClientKind = 'dsh' | 'cli' | 'desktop' | 'mcp' | 'external'

export interface RequestedScopes {
  actorNamespace?: string
  homes?: string[]
  operations?: string[]
}

export interface ClientOptions {
  /** Explicit runtime dir; default `OMT_RUNTIME_DIR` then `~/.omt/run`. */
  runtimeDir?: string
  /** Daemon binary for spawning. Default `OMT_DAEMON` env then `omt-daemon` on PATH. */
  daemonPath?: string
  /** Extra args passed to a spawned daemon (e.g. ['--home', path]). */
  daemonArgs?: string[]
  /** Per-request timeout ms (default 30_000). */
  requestTimeoutMs?: number
  /** Skip spawn when no live descriptor exists (connect-only mode). */
  noSpawn?: boolean
  /**
   * Reconnect policy (U5c). Enabled by default: on an unexpected close the
   * client re-runs discover-or-spawn + handshake with CAPPED BACKOFF
   * (`initialDelayMs` doubling up to `maxDelayMs`), then replays every
   * active events() subscription from its last delivered cursor.
   */
  reconnect?: { initialDelayMs?: number; maxDelayMs?: number; enabled?: boolean }
  /**
   * Called after an AUTOMATIC reconnect completes its fresh handshake and
   * BEFORE subscription replay. A daemon generation change can mint new
   * home ids / credentials, so session-state owners (e.g. the DSH adapter's
   * home registry) rebuild their derived state here. Listener faults are
   * swallowed: they must not break reconnection (TICKET-0131).
   */
  onReconnected?: (handshake: HandshakeOutcome) => void | Promise<void>
}

export class OmtClient {
  private transport: Transport | null = null
  private handshakeResult: HandshakeOutcome | null = null
  private readonly options: ClientOptions
  /** connect() arguments retained for automatic reconnection (U5c). */
  private connectArgs: {
    kind: ClientKind
    scopes: RequestedScopes
    name?: string
    sessionId?: string
  } | null = null
  private closedByCaller = false
  private reconnecting = false
  private reconnectAttempt = 0
  /** Active events() subscriptions replayed from their cursor after a
   *  reconnect completes. */
  private readonly resubscribers = new Set<() => void>()

  constructor(options: ClientOptions = {}) {
    this.options = options
  }

  // ── discovery ────────────────────────────────────────────────────────

  /** Resolve the per-user runtime directory (mirrors paths.rs precedence). */
  static resolveRuntimeDir(explicit?: string): string {
    if (explicit && explicit.trim() !== '') return explicit
    const env = process.env.OMT_RUNTIME_DIR
    if (env && env.trim() !== '') return env
    return join(homedir(), '.omt', 'run')
  }

  /** Read + parse the published descriptor, if present and well-formed. */
  static readDescriptor(runtimeDir?: string): DaemonDescriptor | null {
    const dir = OmtClient.resolveRuntimeDir(runtimeDir)
    try {
      const raw = readFileSync(join(dir, 'descriptor.json'), 'utf8')
      const value = JSON.parse(raw) as DaemonDescriptor
      if (
        value &&
        typeof value.endpoint === 'string' &&
        typeof value.pid === 'number' &&
        typeof value.generation === 'number' &&
        value.schemaVersion === 1
      ) {
        return value
      }
    } catch {
      /* absent or unreadable: treated as no descriptor */
    }
    return null
  }

  /**
   * Find a live daemon: descriptor whose pid is alive AND whose endpoint
   * answers. Never spawns. Returns the live descriptor or null.
   */
  static async discover(runtimeDir?: string, timeoutMs = 2_000): Promise<DaemonDescriptor | null> {
    const candidate = OmtClient.readDescriptor(runtimeDir)
    if (!candidate) return null
    if (!(await probeAlive(candidate))) return null
    void timeoutMs
    return candidate
  }

  /**
   * discoverOrSpawn (F1): return a live daemon's descriptor, or launch one
   * detached and poll up to 10 s for its readiness. Spawn resolution:
   * `options.daemonPath` > `OMT_DAEMON` env > `omt-daemon` on PATH.
   */
  static async discoverOrSpawn(options: ClientOptions = {}): Promise<DaemonDescriptor> {
    const runtimeDir = OmtClient.resolveRuntimeDir(options.runtimeDir)

    const existing = await OmtClient.discover(runtimeDir)
    if (existing) return existing
    if (options.noSpawn) throw new Error('no live omt-daemon found (noSpawn mode)')

    const before = OmtClient.readDescriptor(runtimeDir)
    const binary =
      options.daemonPath ?? process.env.OMT_DAEMON ?? 'omt-daemon'
    const child = spawn(binary, [...(options.daemonArgs ?? []), '--runtime-dir', runtimeDir], {
      detached: true,
      stdio: 'ignore',
      env: { ...process.env, OMT_RUNTIME_DIR: runtimeDir },
    })
    child.unref()
    child.on('error', () => {
      /* spawn failure surfaces as the discovery timeout below */
    })

    const deadline = Date.now() + 10_000
    while (Date.now() < deadline) {
      const candidate = OmtClient.readDescriptor(runtimeDir)
      if (candidate) {
        const replaced =
          !before ||
          candidate.generation > before.generation ||
          candidate.bootToken !== before.bootToken
        if (replaced && (await probeAlive(candidate))) return candidate
      }
      await sleep(100)
    }
    throw new Error(
      `spawned omt-daemon (${binary}) produced no live descriptor within 10s (runtime dir: ${runtimeDir})`,
    )
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /**
   * Discover-or-spawn, connect, and enroll via handshake/request. Idempotent:
   * an already-connected client re-handshakes only when kinds/scopes differ
   * from the active credential's origin is impossible to know — so callers
   * should treat this as connect-once per client instance.
   */
  async connect(
    kind: ClientKind,
    scopes: RequestedScopes = {},
    name?: string,
    sessionId?: string,
  ): Promise<HandshakeOutcome> {
    if (this.transport?.isConnected && this.handshakeResult) return this.handshakeResult
    this.connectArgs = { kind, scopes, name, sessionId }
    this.closedByCaller = false

    const descriptor = await OmtClient.discoverOrSpawn(this.options)
    this.transport = await Transport.connect(descriptor.endpoint, {
      requestTimeoutMs: this.options.requestTimeoutMs,
      onNotification: (method, params) => {
        // Stable routing lambda: events() swaps `notificationBridge` under
        // it, so subscriptions attach without reconnecting.
        this.notificationBridge?.(method, params)
      },
      onClose: () => {
        this.handshakeResult = null
        this.transport = null
        this.scheduleReconnect()
      },
    })
    this.handshakeResult = (await this.transport.call('handshake/request', {
      protocolVersion: '1.0',
      // sessionId (TICKET-0130 item 3): the daemon derives a per-session
      // actor namespace '<base>/<sessionId>' from this identity, keeping
      // concurrent model sessions of one process separately attributable.
      client: { kind, name: name ?? `client-ts-${process.pid}`, ...(sessionId ? { sessionId } : {}) },
      requestedScopes: scopes,
    })) as HandshakeOutcome
    return this.handshakeResult
  }

  /**
   * U5c resilience: unexpected socket loss re-establishes the session with
   * capped backoff (default 100ms → ×2 → 5s ceiling), then replays every
   * active subscription from its LAST DELIVERED CURSOR — no gap, no dupes
   * beyond the documented page/live boundary dedupe.
   */
  private scheduleReconnect(): void {
    if (this.closedByCaller || this.reconnecting) return
    if (this.options.reconnect?.enabled === false) return
    if (!this.connectArgs) return
    this.reconnecting = true
    void (async () => {
      const initial = this.options.reconnect?.initialDelayMs ?? 100
      const max = this.options.reconnect?.maxDelayMs ?? 5_000
      try {
        while (!this.closedByCaller && !this.connected && this.connectArgs) {
          const delay = Math.min(max, initial * 2 ** this.reconnectAttempt)
          this.reconnectAttempt += 1
          await sleep(delay)
          try {
            const handshake = await this.connect(this.connectArgs.kind, this.connectArgs.scopes, this.connectArgs.name, this.connectArgs.sessionId)
            this.reconnectAttempt = 0
            // TICKET-0131: hand the FRESH handshake to session-state owners
            // BEFORE replaying subscriptions, so they can dispose state bound
            // to dead home ids and register replacements without double
            // delivery; surviving ids keep their cursor-based replay.
            if (this.options.onReconnected !== undefined) {
              try {
                await this.options.onReconnected(handshake)
              } catch {
                /* a broken listener must not break reconnection */
              }
            }
            for (const resubscribe of [...this.resubscribers]) {
              try {
                resubscribe()
              } catch {
                /* a broken listener must not block the others */
              }
            }
            break
          } catch {
            /* keep backing off */
          }
        }
      } finally {
        this.reconnecting = false
      }
    })()
  }

  /**
   * Cancel one in-flight call (U5c): sends `$/cancelRequest` for the given
   * wire id. The original promise settles with either a CANCELED problem or
   * the completed result — cancellation lands only at linearization-safe
   * points server-side.
   */
  cancel(callId: number): void {
    this.transport?.sendCancel(callId)
  }

  /** Active credential after a successful {@link connect}. */
  get credential(): CredentialInfo | null {
    return this.handshakeResult?.credential ?? null
  }

  /** Homes visible to the enrolled principal (handshake projection). */
  get homes(): Array<{ homeId: string; name?: string; kind?: string; path?: string }> {
    return this.handshakeResult?.homes ?? []
  }

  get connected(): boolean {
    return this.transport?.isConnected === true && this.handshakeResult !== null
  }

  // ── calls ────────────────────────────────────────────────────────────

  /**
   * Typed JSON-RPC call: attaches `params.credential.token` automatically,
   * casts the result to T. Rejects with {@link OmtProtocolError} carrying
   * problem code/details on failure (R5).
   */
  async call<T = unknown>(
    method: string,
    params: Record<string, unknown> = {},
    hooks?: { onIssued?: (id: number) => void },
  ): Promise<T> {
    if (!this.connected || !this.credential || !this.transport) {
      throw new Error('client not connected; call connect() first')
    }
    const authedParams = { ...params, credential: { token: this.credential.token } }
    return (await this.transport.call(method, authedParams, hooks)) as T
  }

  // ── events ───────────────────────────────────────────────────────────

  /**
   * Subscribe to one home's event stream with auto-resume (F4/R11).
   * Replays the backlog since `since` (default cursor 0), invokes
   * `onEnvelope` for each replayed AND live envelope, then keeps the live
   * subscription attached. Envelopes dedupe by cursor across the
   * page/live boundary. Returns a disposer.
   */
  events(
    homeId: string,
    onEnvelope: (envelope: EventEnvelope) => void,
    options: { since?: number; onError?: (error: Error) => void } = {},
  ): () => void {
    if (!this.transport) {
      throw new Error('client not connected; call connect() first')
    }
    const transport = this.transport
    let lastCursor = options.since ?? 0
    let disposed = false
    let resuming = false
    let pendingTail = false

    const deliver = (envelope: EventEnvelope): void => {
      if (disposed) return
      if ((envelope.cursor ?? 0) <= (options.since ?? 0)) return
      // Page/live boundary dedupe: a live notification may arrive while
      // the backlog page containing the same cursor is still in flight
      // (and vice versa). Cursors are strictly monotonic per home, so any
      // envelope at or below the high-water mark was already delivered.
      if ((envelope.cursor ?? 0) <= lastCursor) return
      lastCursor = Math.max(lastCursor, envelope.cursor)
      try {
        onEnvelope(envelope)
      } catch {
        /* listener faults must not kill the stream */
      }
    }

    const resume = async (): Promise<void> => {
      if (disposed || resuming) {
        pendingTail = true
        return
      }
      resuming = true
      try {
        for (;;) {
          const page = await this.call<{ cursor: number; events: EventEnvelope[] }>(
            'events/resume',
            { homeId, cursor: lastCursor, limit: 500 },
          )
          for (const envelope of page.events ?? []) deliver(envelope)
          const nextCursor = typeof page.cursor === 'number' ? Math.max(page.cursor, lastCursor) : lastCursor
          if ((page.events?.length ?? 0) === 0 && nextCursor === lastCursor) break
          lastCursor = nextCursor
          if ((page.events?.length ?? 0) < 500) break
        }
      } catch (error) {
        options.onError?.(error instanceof Error ? error : new Error(String(error)))
      } finally {
        resuming = false
        if (pendingTail && !disposed) {
          pendingTail = false
          void resume()
        }
      }
    }

    const notificationHandler = (method: string, params: unknown): void => {
      if (method !== 'omt/event') return
      const envelope = params as EventEnvelope
      if (!envelope || envelope.homeId !== homeId) return
      deliver(envelope)
      void resume()
    }

    const previousNotificationHandler = this.notificationBridge
    this.notificationBridge = notificationHandler
    // U5c: after an automatic reconnect, replaying resume() re-pages from
    // lastCursor (no gap) AND re-attaches the live server subscription the
    // dead connection used to hold.
    const resubscribe = (): void => {
      if (!disposed && this.transport !== null) void resume()
    }
    this.resubscribers.add(resubscribe)
    void resume()

    return () => {
      disposed = true
      this.resubscribers.delete(resubscribe)
      if (this.notificationBridge === notificationHandler) {
        this.notificationBridge = previousNotificationHandler
      }
      void transport
    }
  }

  /** Single-notification bridge wired at connect(); see events(). */
  private notificationBridge: ((method: string, params: unknown) => void) | null = null

  /**
   * Drop the live connection so the reconnect loop performs a FRESH
   * discover-or-spawn + handshake, and resolve with the new handshake
   * outcome. This is the state-healing path (TICKET-0132): unlike close()
   * the client stays open for business — onReconnected fires and event
   * subscriptions replay as after any unexpected close.
   */
  async forceReconnect(timeoutMs = 10_000): Promise<HandshakeOutcome> {
    if (this.closedByCaller) throw new Error('client closed; call connect() first')
    const transport = this.transport
    if (transport !== null) {
      // Clear fields BEFORE end(): onClose also resets them and schedules
      // the reconnect; pre-clearing keeps the sequence unambiguous.
      this.transport = null
      this.handshakeResult = null
      transport.end()
    }
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
      if (this.handshakeResult !== null && this.transport?.isConnected) return this.handshakeResult
      await sleep(25)
    }
    throw new Error(`omt client rehandshake timed out after ${timeoutMs}ms`)
  }

  /** Tear down the connection (credentials die server-side with expiry).
   *  Stops the automatic reconnect loop. */
  close(): void {
    this.closedByCaller = true
    this.resubscribers.clear()
    this.handshakeResult = null
    this.transport?.end()
    this.transport = null
  }
}

// ── helpers ────────────────────────────────────────────────────────────

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/**
 * Liveness = pid alive AND the endpoint accepts a local connection. On
 * platforms without process probing (`process.kill(pid, 0)` restrictions),
 * the connect probe alone decides.
 */
async function probeAlive(descriptor: DaemonDescriptor): Promise<boolean> {
  if (descriptor.pid > 0) {
    try {
      process.kill(descriptor.pid, 0)
    } catch (error) {
      const code = (error as NodeJS.ErrnoException)?.code
      if (code === 'ESRCH') return false
      // EPERM means "alive but not ours": keep the connect probe as judge.
    }
  }
  try {
    const transport = await Transport.connect(descriptor.endpoint, { requestTimeoutMs: 1_000 })
    transport.end()
    return true
  } catch {
    return false
  }
}
