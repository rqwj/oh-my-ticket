/**
 * Home owner-lock tests (U2b / R2): one writer per opened home. The lock is
 * a single JSON file at `<home>/home.lock` (for the global home that is
 * `~/.omt/home.lock`; for workspace homes `<ws>/.omt/home.lock` — this is
 * the cross-language path the Rust daemon must honor in U5). Semantics:
 *
 *  - atomic O_EXCL create; JSON body {schemaVersion, ownerKind, pid,
 *    hostname?, acquiredAt, heartbeatAt, token};
 *  - heartbeat refreshes `heartbeatAt`/mtime while held;
 *  - daemon marker (ownerKind "daemon") refuses ALWAYS — even stale;
 *  - live ts-bridge holder refuses with HOME_LOCKED {pid, acquiredAt};
 *  - no heartbeat beyond the stale window → steal with a fresh body;
 *  - release unlinks only when the body still matches our token.
 */
import { mkdir, mkdtemp, rm, utimes, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import {
  DEFAULT_STALE_MS,
  acquireHomeLock,
  homeLockPath,
  readHomeLock,
  releaseHomeLockForTests,
  withHomeLock,
  type HomeLockBody,
} from '../src/host/home-lock.ts'
import { OmtCore } from '../src/host/core.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { OmtError } from '../src/host/types.ts'

let root: string
let home: string

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'omt-home-lock-test-'))
  home = join(root, 'home')
  await mkdir(home, { recursive: true })
})

afterEach(async () => {
  await releaseHomeLockForTests()
  await rm(root, { recursive: true, force: true })
})

/** Fixed base instant for injected-clock scenarios (2026-08-24T00:00:00Z). */
const T0_MS = Date.parse('2026-08-24T00:00:00.000Z')

/** Write an owner-lock body directly (simulating another writer's file). */
async function writeLock(overrides: Partial<HomeLockBody> = {}, atMs = T0_MS): Promise<HomeLockBody> {
  const body: HomeLockBody = {
    schemaVersion: 1,
    ownerKind: 'ts-bridge',
    pid: 4242,
    acquiredAt: new Date(atMs).toISOString(),
    heartbeatAt: new Date(atMs).toISOString(),
    token: 'foreign-token',
    ...overrides,
  }
  await writeFile(homeLockPath(home), JSON.stringify(body), 'utf8')
  return body
}

/** Backdate the lock file's mtime by `ageMs` (fallback staleness evidence). */
async function backdateLock(ageMs: number): Promise<void> {
  const past = new Date(T0_MS - ageMs)
  await utimes(homeLockPath(home), past, past)
}

describe('acquire/release basics', () => {
  it('creates <home>/home.lock atomically and records the ts-bridge owner', async () => {
    const lock = await acquireHomeLock(home)
    try {
      expect(lock.body.schemaVersion).toBe(1)
      expect(lock.body.ownerKind).toBe('ts-bridge')
      expect(lock.body.pid).toBe(process.pid)
      expect(lock.body.token).toBeTruthy()
      const raw = JSON.parse((await readFileText(homeLockPath(home))) ?? '{}')
      expect(raw.token).toBe(lock.body.token)
      expect(await readHomeLock(home)).toMatchObject({ token: lock.body.token })
    } finally {
      await lock.release()
    }
  })

  it('double-acquire fails closed with HOME_LOCKED and holder details', async () => {
    const first = await acquireHomeLock(home)
    try {
      let error: unknown
      try {
        await acquireHomeLock(home)
      } catch (caught) {
        error = caught
      }
      expect(error).toBeInstanceOf(OmtError)
      const omtError = error as OmtError
      expect(omtError.code).toBe('HOME_LOCKED')
      expect(omtError.details).toMatchObject({ pid: process.pid, acquiredAt: first.body.acquiredAt })
      // The original lock survives untouched.
      expect((await readHomeLock(home))?.token).toBe(first.body.token)
    } finally {
      await first.release()
    }
  })

  it('release removes the file; releasing twice is an idempotent no-op', async () => {
    const lock = await acquireHomeLock(home)
    await lock.release()
    expect(await readHomeLock(home)).toBeUndefined()
    // Second release is a no-op (file already gone).
    await expect(lock.release()).resolves.toBeUndefined()
  })

  it('a handle whose lock was replaced does not delete the replacement on release', async () => {
    const lock = await acquireHomeLock(home)
    // Simulate crash + steal by another writer: file replaced under our feet.
    await writeLock({ token: 'new-owner-token', pid: 5555 }, T0_MS)
    await lock.release()
    const after = await readHomeLock(home)
    expect(after?.token).toBe('new-owner-token')
  })

  it('withHomeLock releases even when fn throws', async () => {
    await expect(withHomeLock(home, undefined, async () => {
      throw new Error('boom')
    })).rejects.toThrow('boom')
    expect(await readHomeLock(home)).toBeUndefined()
    await withHomeLock(home, undefined, () => 'ok')
  })
})

describe('refusal paths', () => {
  it('daemon marker refuses ALWAYS, even when the heartbeat is ancient', async () => {
    await writeLock({ ownerKind: 'daemon', pid: 900 }, T0_MS - DEFAULT_STALE_MS * 10)
    await backdateLock(DEFAULT_STALE_MS * 10)
    await expect(acquireHomeLock(home, { now: () => T0_MS })).rejects.toMatchObject({
      name: 'OmtError',
      code: 'DAEMON_OWNS_HOME',
      details: { owner: { ownerKind: 'daemon', pid: 900 } },
    })
    // Refusal never deletes the daemon's marker.
    expect((await readHomeLock(home))?.ownerKind).toBe('daemon')
  })

  it('unknown future schemaVersion fails closed regardless of staleness', async () => {
    await writeLock({ schemaVersion: 99 }, T0_MS - DEFAULT_STALE_MS * 10)
    await backdateLock(DEFAULT_STALE_MS * 10)
    await expect(acquireHomeLock(home, { now: () => T0_MS })).rejects.toMatchObject({
      code: 'HOME_LOCKED',
    })
  })

  it('corrupt body falls back to mtime liveness: fresh refuses, old steals', async () => {
    await writeFile(homeLockPath(home), '{not json', 'utf8')
    await expect(acquireHomeLock(home, { now: () => T0_MS })).rejects.toMatchObject({
      code: 'HOME_LOCKED',
      details: { pid: null, acquiredAt: null },
    })

    await writeFile(homeLockPath(home), '{not json', 'utf8')
    await backdateLock(DEFAULT_STALE_MS + 1)
    const stolen = await acquireHomeLock(home, { now: () => T0_MS })
    try {
      expect(stolen.body.pid).toBe(process.pid)
    } finally {
      await stolen.release()
    }
  })
})

describe('stale detection with injected clock', () => {
  it('steals a ts-bridge lock whose heartbeat passed the stale window', async () => {
    await writeLock({ pid: 111 }, T0_MS)
    // Just inside the window: still refused.
    await expect(acquireHomeLock(home, { now: () => T0_MS + DEFAULT_STALE_MS })).rejects.toMatchObject({
      code: 'HOME_LOCKED',
      details: { pid: 111 },
    })
    // One tick beyond: steal allowed, new owner recorded.
    const lock = await acquireHomeLock(home, { now: () => T0_MS + DEFAULT_STALE_MS + 1 })
    try {
      expect(lock.body.pid).toBe(process.pid)
      expect(lock.body.acquiredAt).toBe(new Date(T0_MS + DEFAULT_STALE_MS + 1).toISOString())
      expect((await readHomeLock(home))?.token).not.toBe('foreign-token')
    } finally {
      await lock.release()
    }
  })

  it('crash simulation: abandoned handle (no release, heartbeats off) is stealable', async () => {
    const crashed = await acquireHomeLock(home, { now: () => T0_MS, heartbeatMs: 0 })
    expect(crashed.body.pid).toBe(process.pid)
    // Process "dies" without release; time passes beyond the window.
    const successor = await acquireHomeLock(home, { now: () => T0_MS + DEFAULT_STALE_MS + 1 })
    try {
      expect(successor.body.token).not.toBe(crashed.body.token)
      // The dead handle's late release must not unlink the successor's lock.
      await crashed.release()
      expect((await readHomeLock(home))?.token).toBe(successor.body.token)
    } finally {
      await successor.release()
    }
  })
})

describe('heartbeat', () => {
  it('refreshes heartbeatAt while held (real short interval)', async () => {
    const lock = await acquireHomeLock(home, { heartbeatMs: 15 })
    try {
      const before = Date.parse((await readHomeLock(home))?.heartbeatAt ?? '')
      await new Promise(resolve => setTimeout(resolve, 60))
      const after = Date.parse((await readHomeLock(home))?.heartbeatAt ?? '')
      expect(after).toBeGreaterThanOrEqual(before)
      // A contender inside the window sees a LIVE holder.
      await expect(acquireHomeLock(home, { heartbeatMs: 0 })).rejects.toMatchObject({ code: 'HOME_LOCKED' })
    } finally {
      await lock.release()
    }
  })
})

describe('integration: pools and cores on one home', () => {
  it('two OmtCorePool instances on one home: second open throws HOME_LOCKED until the first disposes', async () => {
    const pool1 = new OmtCorePool(home)
    const core1 = await pool1.coreForHome(home)
    const epic = await core1.create({ type: 'epic', title: '持有者' })

    const pool2 = new OmtCorePool(home)
    await expect(pool2.coreForHome(home)).rejects.toMatchObject({
      name: 'OmtError',
      code: 'HOME_LOCKED',
      details: { pid: process.pid },
    })

    await pool1.closeAll()
    // After dispose the second pool succeeds (failed open evicted, lock free).
    const core2 = await pool2.coreForHome(home)
    try {
      expect(core2.tree().map(node => node.id)).toEqual([epic.id])
    } finally {
      await pool2.closeAll()
    }
  })

  it('OmtCore close releases the lock; reopen sees persisted data', async () => {
    const core1 = await OmtCore.open(home)
    const epic = await core1.create({ type: 'epic', title: '重启' })
    await core1.close()

    const core2 = await OmtCore.open(home)
    try {
      expect(core2.getNode(epic.id)?.title).toBe('重启')
    } finally {
      await core2.close()
    }
    // Home left unlocked after both closes.
    expect(await readHomeLock(home)).toBeUndefined()
  })

  it('a failed open does not poison the pool cache for later retries', async () => {
    const blocker = await acquireHomeLock(home)
    const pool = new OmtCorePool(home)
    await expect(pool.coreForHome(home)).rejects.toMatchObject({ code: 'HOME_LOCKED' })
    await blocker.release()
    const core = await pool.coreForHome(home)
    expect(core.home).toBe(home)
    await pool.closeAll()
  })
})

/** Tiny helper: text of a file (undefined when missing). */
async function readFileText(path: string): Promise<string | undefined> {
  try {
    const { readFile } = await import('node:fs/promises')
    return await readFile(path, 'utf8')
  } catch {
    return undefined
  }
}
