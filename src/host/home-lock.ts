/**
 * Per-home owner lock (U2b / R2): one writer per opened home. The lock is a
 * single JSON file directly inside the OMT home directory — `<home>/home.lock`
 * (the global home resolves to `~/.omt/home.lock`, a workspace home to
 * `<ws>/.omt/home.lock`). This exact path + schema is the cross-language
 * contract: the future Rust daemon (plan U5) must honor it, and the daemon's
 * kernel-flock layer will attach to the SAME path, so nothing here may move
 * the file or reshape the documented body fields additively-incompatibly.
 *
 * Body (schemaVersion 1):
 *   { schemaVersion: 1, ownerKind: "ts-bridge" | "daemon", pid,
 *     hostname?, acquiredAt, heartbeatAt, token }
 * `token` is an acquisition-unique identity string; `release` unlinks the
 * file ONLY when the on-disk body still carries our token, so a crashed /
 * stolen owner can never delete a successor's lock.
 *
 * Refusal matrix (R2):
 *   - ownerKind "daemon"            → DAEMON_OWNS_HOME {owner}, ALWAYS, even stale
 *   - unknown/future schemaVersion  → HOME_LOCKED (fail closed, never stolen)
 *   - live ts-bridge/other holder   → HOME_LOCKED {pid, acquiredAt}
 *   - stale (no heartbeat > 30 s)   → steal: unlink + fresh create
 *   - corrupt/empty body            → mtime-based liveness fallback
 *
 * Documented residual (accepted by plan U2b): the marker-check-to-write
 * window remains TOCTOU-racy without a kernel flock; U5 closes it daemon-side
 * on this same path.
 */
import { open, rename, rm, mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { readFileSync, rmSync } from 'node:fs'
import { hostname as osHostname } from 'node:os'
import { randomUUID } from 'node:crypto'
import { join } from 'node:path'
import { OmtError } from './types.ts'

/** Lock file name inside the home directory (cross-language fixed path). */
export const LOCK_FILE_NAME = 'home.lock'

/** Owner-lock schema version written by this implementation. */
export const LOCK_SCHEMA_VERSION = 1

/** A holder whose heartbeat is older than this is considered dead (ms). */
export const DEFAULT_STALE_MS = 30_000

/** Default heartbeat cadence while a lock is held (ms); 0 disables the timer. */
export const HEARTBEAT_INTERVAL_MS = 10_000

export type HomeLockOwnerKind = 'ts-bridge' | 'daemon'

/**
 * One owner-lock document. Foreign writers may carry other `ownerKind`/
 * `schemaVersion` values — readers stay tolerant, policy decides refusals.
 */
export interface HomeLockBody {
  readonly schemaVersion: number
  readonly ownerKind: HomeLockOwnerKind | (string & {})
  readonly pid: number | null
  readonly hostname?: string
  /** ISO timestamps; `heartbeatAt` is the liveness witness. */
  readonly acquiredAt: string
  readonly heartbeatAt: string
  /** Acquisition-unique identity; only the matching token may unlink. */
  readonly token: string
}

export interface HomeLockOptions {
  /** Who is acquiring (default 'ts-bridge'; the daemon writes its own kind). */
  readonly ownerKind?: HomeLockOwnerKind
  /** Injectable clock (ms epoch) for deterministic stale detection in tests. */
  readonly now?: () => number
  /** Liveness window override (default DEFAULT_STALE_MS). */
  readonly staleMs?: number
  /** Heartbeat cadence override (default HEARTBEAT_INTERVAL_MS; 0 disables). */
  readonly heartbeatMs?: number
  /** Holder hostname override (defaults to os.hostname()). */
  readonly hostname?: string
}

export interface HomeLockHandle {
  readonly home: string
  readonly token: string
  /** Snapshot of the body this owner last wrote (heartbeat refreshes it). */
  readonly body: Readonly<HomeLockBody>
  /** Force one heartbeat write (the interval calls this automatically). */
  heartbeat(): Promise<void>
  /**
   * Stop heartbeating and unlink the lock — but only when the on-disk body
   * still matches our token. Idempotent; never throws for a vanished or
   * replaced lock.
   */
  release(): Promise<void>
}

// ── internals ────────────────────────────────────────────────────────────

function lockPathFor(home: string): string {
  return join(home, LOCK_FILE_NAME)
}

function iso(ms: number): string {
  return new Date(ms).toISOString()
}

function delay(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}

function parseBody(raw: string): HomeLockBody | undefined {
  try {
    const parsed: unknown = JSON.parse(raw)
    if (parsed !== null && typeof parsed === 'object') return parsed as HomeLockBody
    return undefined
  } catch {
    return undefined
  }
}

async function readRaw(path: string): Promise<{ raw: string; mtimeMs: number } | undefined> {
  try {
    const [raw, stats] = await Promise.all([readFile(path, 'utf8'), stat(path)])
    return { raw, mtimeMs: stats.mtimeMs }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
    throw error
  }
}

/** Atomic body rewrite (tmp + rename): readers never see a partial body. */
async function writeBodyAtomic(path: string, body: HomeLockBody): Promise<void> {
  const tmp = `${path}.${randomUUID()}.tmp`
  await writeFile(tmp, JSON.stringify(body), 'utf8')
  await rename(tmp, path)
}

type Inspection =
  | { kind: 'gone' }
  | { kind: 'empty'; mtimeMs?: number }
  | { kind: 'corrupt'; mtimeMs?: number }
  | { kind: 'body'; body: HomeLockBody; mtimeMs?: number }

async function inspect(path: string): Promise<Inspection> {
  const rawFile = await readRaw(path)
  if (rawFile === undefined) return { kind: 'gone' }
  if (rawFile.raw.trim() === '') return { kind: 'empty', mtimeMs: rawFile.mtimeMs }
  const body = parseBody(rawFile.raw)
  if (body === undefined) return { kind: 'corrupt', mtimeMs: rawFile.mtimeMs }
  return { kind: 'body', body, mtimeMs: rawFile.mtimeMs }
}

/** Live handles held by this process (diagnostics + test teardown hook). */
const liveHandles = new Set<HomeLockHandle>()

/** Liveness evidence of an observed lock relative to the injected clock. */
function staleAgeMs(inspection: Inspection, now: () => number): number {
  const stamp = inspection.kind === 'body' ? Date.parse(inspection.body.heartbeatAt) : Number.NaN
  if (Number.isFinite(stamp)) return now() - stamp
  if (inspection.kind !== 'gone' && inspection.mtimeMs !== undefined) return now() - inspection.mtimeMs
  return Number.POSITIVE_INFINITY
}

function lockedError(code: 'HOME_LOCKED' | 'DAEMON_OWNS_HOME', message: string, details: Record<string, unknown>): OmtError {
  return new OmtError(code, message, details)
}

// ── public API ───────────────────────────────────────────────────────────

/**
 * Acquire this process's writer lock on `home`. Throws `DAEMON_OWNS_HOME`
 * when a daemon marker is present, `HOME_LOCKED` for any other conflicting
 * or uninterruptible state. Stale locks (no heartbeat within `staleMs`) are
 * stolen transparently.
 */
export async function acquireHomeLock(home: string, options: HomeLockOptions = {}): Promise<HomeLockHandle> {
  const now = options.now ?? Date.now
  const staleMs = options.staleMs ?? DEFAULT_STALE_MS
  const heartbeatMs = options.heartbeatMs ?? HEARTBEAT_INTERVAL_MS
  const ownerKind = options.ownerKind ?? 'ts-bridge'
  const hostname = options.hostname ?? osHostname()
  const path = lockPathFor(home)

  // The lock file lives inside the home; make sure the directory exists
  // before any exclusive create. (Creating the dir itself claims nothing.)
  await mkdir(home, { recursive: true })

  let steals = 0
  let attempts = 0
  let lastRefusal: { code: 'HOME_LOCKED'; details: Record<string, unknown> } | undefined

  while (attempts++ < 200) {
    // 1. Try the atomic exclusive create.
    const created = await tryCreate(path, { ownerKind, hostname, now })
    if (created !== undefined) return register(created)

    // 2. File exists (or vanished mid-loop): inspect the contender.
    let verdict = await inspect(path)
    if (verdict.kind === 'gone') continue

    // An empty file means a creator died between O_EXCL create and body
    // publish — give a live creator a short grace window to finish before
    // judging the body by its mtime.
    if (verdict.kind === 'empty') {
      for (let round = 0; round < 50 && verdict.kind === 'empty'; round += 1) {
        await delay(5)
        verdict = await inspect(path)
      }
      if (verdict.kind === 'gone') continue
      if (verdict.kind === 'empty') verdict = { kind: 'corrupt', mtimeMs: verdict.mtimeMs }
    }

    // 3. Daemon markers refuse unconditionally — even ancient ones (R2).
    if (verdict.kind === 'body' && verdict.body.ownerKind === 'daemon') {
      throw lockedError(
        'DAEMON_OWNS_HOME',
        `home ${home} is owned by an omt-daemon (pid ${String(verdict.body.pid)}); ` +
          'close the daemon (or its owner marker) before opening this home from the TypeScript bridge',
        { owner: verdict.body },
      )
    }

    // 4. Unknown / future schema versions fail closed and are never stolen:
    // a newer writer owns this format even when its heartbeat went silent.
    if (verdict.kind === 'body' && verdict.body.schemaVersion !== LOCK_SCHEMA_VERSION) {
      throw lockedError(
        'HOME_LOCKED',
        `home ${home} is locked by an incompatible writer (schemaVersion ${String(verdict.body.schemaVersion)})`,
        {
          pid: typeof verdict.body.pid === 'number' ? verdict.body.pid : null,
          acquiredAt: typeof verdict.body.acquiredAt === 'string' ? verdict.body.acquiredAt : null,
          schemaVersion: verdict.body.schemaVersion,
        },
      )
    }

    // 5. Liveness decision: heartbeat age (mtime fallback for corrupt bodies).
    const age = staleAgeMs(verdict, now)
    if (age <= staleMs) {
      lastRefusal = {
        code: 'HOME_LOCKED',
        details: {
          pid: verdict.kind === 'body' && typeof verdict.body.pid === 'number' ? verdict.body.pid : null,
          acquiredAt: verdict.kind === 'body' && typeof verdict.body.acquiredAt === 'string'
            ? verdict.body.acquiredAt
            : null,
        },
      }
      const who = lastRefusal.details.pid !== null ? `pid ${String(lastRefusal.details.pid)}` : 'an unreadable lock'
      throw lockedError(
        'HOME_LOCKED',
        `home ${home} is already owned by another writer (${who}); ` +
          'dispose the owning core/pool first, or wait for the lock to go stale',
        lastRefusal.details,
      )
    }

    // 6. Stale → steal. Bounded so two pathological stealers cannot spin.
    if (++steals > 8) {
      throw lockedError(
        'HOME_LOCKED',
        `home ${home} lock kept being replaced while stealing; refusing after ${String(steals - 1)} steals`,
        lastRefusal?.details ?? { pid: null, acquiredAt: null },
      )
    }
    await rm(path, { force: true })
  }
  throw lockedError(
    'HOME_LOCKED',
    `home ${home} could not be locked after ${String(attempts)} attempts`,
    lastRefusal?.details ?? { pid: null, acquiredAt: null },
  )

  function register(handle: HomeLockHandle): HomeLockHandle {
    liveHandles.add(handle)
    return handle
  }

  async function tryCreate(
    path: string,
    identity: { ownerKind: HomeLockOwnerKind; hostname: string; now: () => number },
  ): Promise<HomeLockHandle | undefined> {
    const token = randomUUID()
    let handle: Awaited<ReturnType<typeof open>> | undefined
    try {
      handle = await open(path, 'wx')
      await handle.writeFile(JSON.stringify({
        schemaVersion: LOCK_SCHEMA_VERSION,
        ownerKind: identity.ownerKind,
        pid: process.pid,
        hostname: identity.hostname,
        acquiredAt: iso(identity.now()),
        heartbeatAt: iso(identity.now()),
        token,
      }), 'utf8')
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') return undefined
      throw error
    } finally {
      if (handle !== undefined) await handle.close().catch(() => {})
    }
    // Confirm we still own the visible file: a concurrent stealer may have
    // replaced it between our create and our body publish.
    const confirm = await inspect(path)
    if (confirm.kind !== 'body' || confirm.body.token !== token) return undefined
    return buildHandle(home, token, confirm.body, { heartbeatMs, now })
  }
}

/** Read + parse the current owner-lock body of `home` (undefined if absent/unreadable). */
export async function readHomeLock(home: string): Promise<HomeLockBody | undefined> {
  const rawFile = await readRaw(lockPathFor(home)).catch(() => undefined)
  if (rawFile === undefined) return undefined
  return parseBody(rawFile.raw)
}

/** Exact lock-file path for a home (cross-language contract surface). */
export function homeLockPath(home: string): string {
  return lockPathFor(home)
}

/** Run `fn` under the home lock, releasing afterwards (even on throw). */
export async function withHomeLock<T>(
  home: string,
  options: HomeLockOptions | undefined,
  fn: (lock: HomeLockHandle) => Promise<T> | T,
): Promise<T> {
  const lock = await acquireHomeLock(home, options)
  try {
    return await fn(lock)
  } finally {
    await lock.release()
  }
}

/** Test/diagnostic teardown: release every lock this process still holds. */
export async function releaseHomeLockForTests(): Promise<void> {
  for (const handle of [...liveHandles]) {
    await handle.release().catch(() => {})
  }
}

// ── handle machinery ─────────────────────────────────────────────────────

function buildHandle(
  home: string,
  token: string,
  initialBody: Readonly<HomeLockBody>,
  timing: { heartbeatMs: number; now: () => number },
): HomeLockHandle {
  const path = lockPathFor(home)
  let body: HomeLockBody = initialBody
  let released = false
  let lost = false

  const handle: HomeLockHandle = {
    home,
    token,
    get body(): Readonly<HomeLockBody> {
      return body
    },
    async heartbeat(): Promise<void> {
      if (released || lost) return
      const current = await inspect(path)
      // Lost ownership (stolen/replaced/removed): stop refreshing silently.
      if (current.kind !== 'body' || current.body.token !== token) {
        lost = true
        stopTimer()
        return
      }
      body = { ...body, heartbeatAt: iso(timing.now()) }
      await writeBodyAtomic(path, body)
    },
    async release(): Promise<void> {
      if (released) return
      released = true
      stopTimer()
      liveHandles.delete(handle)
      // Synchronous handoff: release reads + unlinks OUR OWN lock with sync
      // fs so that an un-awaited `core.close()` followed immediately by a
      // fresh `OmtCore.open(home)` can never observe the stale file (the
      // crash-residue reopen path depends on this determinism). Unlink ONLY
      // when the on-disk body still carries our token — a successor's lock
      // is never touched.
      try {
        const parsed = parseBody(readFileSync(path, 'utf8'))
        if (parsed?.token === token) rmSync(path, { force: true })
      } catch {
        /* already gone or unreadable: nothing to release */
      }
    },
  }

  let timer: ReturnType<typeof setInterval> | undefined
  function startTimer(): void {
    if (timing.heartbeatMs <= 0) return
    timer = setInterval(() => {
      void handle.heartbeat().catch(() => { /* best-effort liveness */ })
    }, timing.heartbeatMs)
    // Never keep the event loop alive just for heartbeats.
    ;(timer as { unref?: () => void }).unref?.()
  }
  function stopTimer(): void {
    if (timer !== undefined) {
      clearInterval(timer)
      timer = undefined
    }
  }

  startTimer()
  return handle
}
