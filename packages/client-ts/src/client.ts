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
 * then `~/.omt/run`, matching crates/omt-runtime/src/paths.rs.
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
}

export class OmtClient {
  private transport: Transport | null = null
  private handshakeResult: HandshakeOutcome | null = null
  private readonly options: ClientOptions

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
  async connect(kind: ClientKind, scopes: RequestedScopes = {}, name?: string): Promise<HandshakeOutcome> {
    if (this.transport?.isConnected && this.handshakeResult) return this.handshakeResult

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
      },
    })
    this.handshakeResult = (await this.transport.call('handshake/request', {
      protocolVersion: '1.0',
      client: { kind, name: name ?? `client-ts-${process.pid}` },
      requestedScopes: scopes,
    })) as HandshakeOutcome
    return this.handshakeResult
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
  async call<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    if (!this.connected || !this.credential || !this.transport) {
      throw new Error('client not connected; call connect() first')
    }
    const authedParams = { ...params, credential: { token: this.credential.token } }
    return (await this.transport.call(method, authedParams)) as T
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
    void resume()

    return () => {
      disposed = true
      if (this.notificationBridge === notificationHandler) {
        this.notificationBridge = previousNotificationHandler
      }
      void transport
    }
  }

  /** Single-notification bridge wired at connect(); see events(). */
  private notificationBridge: ((method: string, params: unknown) => void) | null = null

  /** Tear down the connection (credentials die server-side with expiry). */
  close(): void {
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
