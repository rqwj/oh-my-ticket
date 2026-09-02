/**
 * Home owner-lock tests (U2b / R2): one writer per opened home. The lock is
 * a single JSON file at `<home>/home.lock` (for the global home that is
 * `~/.omt/home.lock`; for workspace homes `<ws>/.omt/home.lock` — this is
 * the cross-language path the Rust daemon honors since U5b/U5c). Semantics:
 *
 *  - atomic O_EXCL create; JSON body {schemaVersion, ownerKind, pid,
 *    hostname?, acquiredAt, heartbeatAt, token};
 *  - heartbeat refreshes `heartbeatAt`/mtime while held;
 *  - daemon marker (ownerKind "daemon") refuses ALWAYS — even stale;
 *  - live ts-bridge holder refuses with HOME_LOCKED {pid, acquiredAt};
 *  - no heartbeat beyond the stale window → steal with a fresh body;
 *  - release unlinks only when the body still matches our token.
 *
 * U7a split: the unit-level describes below still exercise the ts-bridge
 * module directly (it remains for offline maintenance tooling). The former
 * two-pool integration cases are REPLACED by daemon-surface equivalents:
 * omt-daemon now owns home locking, and a foreign marker makes the daemon
 * REFUSE TO BOOT — the adapter surfaces it as a ready()-time failure whose
 * message carries the daemon's problem code (HOME_LOCKED /
 * DAEMON_OWNS_HOME), not a per-call error.
 */
import { spawn } from 'node:child_process'
import { mkdirSync } from 'node:fs'
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
import { OmtService } from '../src/host/service.ts'
import { OmtError } from '../src/host/types.ts'
import { daemonBinary, ensureDaemonBuilt, spawnDaemon } from './mocks/runtime-fixture.ts'

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

// REWRITTEN for U7a (daemon owns home locking): the old two-pool cases
// became daemon-boot-surface cases. A foreign marker makes omt-daemon EXIT
// before publishing its descriptor; @omt/client-ts spawns with stdio
// ignored and surfaces only a generic "no live descriptor" failure, so the
// specific problem code is pinned here at process level (captured stderr),
// plus ONE adapter-level assertion that ready() fails closed.
describe('integration: home ownership through the daemon surface', () => {
  /** A fresh ts-bridge-style marker body written directly into the home. */
  async function plantMarker(overrides: Partial<HomeLockBody> = {}): Promise<void> {
    const body: HomeLockBody = {
      schemaVersion: 1,
      ownerKind: 'ts-bridge',
      pid: process.pid,
      acquiredAt: new Date().toISOString(),
      heartbeatAt: new Date().toISOString(),
      token: 'foreign-token',
      ...overrides,
    }
    await writeFile(homeLockPath(home), JSON.stringify(body), 'utf8')
  }

  /**
   * Boot one daemon over `home` directly, capturing stderr until exit.
   * Ownership refusals print their problem JSON there before exiting.
   */
  async function tryBootDaemon(): Promise<string> {
    await ensureDaemonBuilt()
    const runtimeDir = join(root, 'runtime')
    mkdirSync(runtimeDir, { recursive: true })
    return new Promise((resolve, reject) => {
      const child = spawn(daemonBinary, ['--runtime-dir', runtimeDir, '--home', home], {
        cwd: runtimeDir,
        env: { ...process.env, OMT_RUNTIME_DIR: runtimeDir, OMT_HOME: home },
        stdio: ['ignore', 'ignore', 'pipe'],
      })
      let stderr = ''
      child.stderr?.on('data', chunk => {
        stderr += String(chunk)
      })
      const timer = setTimeout(() => {
        child.kill('SIGKILL')
        reject(new Error(`daemon did not exit within 15s; stderr so far:\n${stderr}`))
      }, 15_000)
      child.on('exit', () => {
        clearTimeout(timer)
        resolve(stderr)
      })
    })
  }

  /** Connect a service whose runtime dir spawns a fresh daemon over `home`. */
  function connectService(): OmtService {
    // daemonArgs pins the spawned daemon to the TEST home: without it the
    // daemon boots the default global home (~/.omt) and the assertion only
    // passes on machines where a real daemon already holds that lock
    // (CI runners have an empty ~/.omt → the daemon boots fine → ready()
    // resolves and the test fails).
    return new OmtService({ runtimeDir: join(root, 'runtime'), name: 'oh-my-ticket-home-lock-test', daemonArgs: ['--home', home] })
  }

  it('a live ts-bridge marker refuses the daemon at boot with HOME_LOCKED', async () => {
    await plantMarker()
    // Process level: the refusal problem JSON with takeover guidance.
    const stderr = await tryBootDaemon()
    expect(stderr).toContain('HOME_LOCKED')
    expect(stderr).toMatch(/ts-bridge|takeover/i)
    // The refusal never deletes the marker.
    expect((await readHomeLock(home))?.ownerKind).toBe('ts-bridge')
    // Adapter level: ready() fails closed (the client cannot see WHY — the
    // spawned daemon died before publishing its descriptor).
    const service = connectService()
    try {
      await expect(service.ready()).rejects.toBeInstanceOf(OmtError)
    } finally {
      await service.close()
    }
  }, 30_000) // the adapter half waits out the client's 10s spawn window

  it('a LIVE daemon lease (flock held) refuses a second writer with DAEMON_OWNS_HOME', async () => {
    // TICKET-0124: liveness authority is the kernel flock. Boot a REAL
    // daemon over the home (it holds the lock), then attempt a second
    // boot: refused while the first lease lives.
    const runtimeDir = join(root, 'runtime')
    mkdirSync(runtimeDir, { recursive: true })
    const first = await spawnDaemon(runtimeDir, [{ path: home, global: true }])
    try {
      const stderr = await tryBootDaemon()
      expect(stderr).toContain('DAEMON_OWNS_HOME')
      expect((await readHomeLock(home))?.ownerKind).toBe('daemon')
    } finally {
      try {
        process.kill(first.pid, 'SIGTERM')
      } catch {
        /* already gone */
      }
    }
  }, 30_000)

  it('an un-flocked daemon marker is a tombstone the boot recovers (TICKET-0124)', async () => {
    // Alive pid but NO flock behind the marker: the takeover fence shape.
    // The new-world boot probes the kernel lease and takes over cleanly.
    await plantMarker({ ownerKind: 'daemon', pid: process.pid })
    const runtimeDir = join(root, 'runtime')
    mkdirSync(runtimeDir, { recursive: true })
    const daemon = await spawnDaemon(runtimeDir, [{ path: home, global: true }])
    const service = new OmtService({ runtimeDir: daemon.runtimeDir, name: 'oh-my-ticket-home-lock-test' })
    try {
      await expect(service.ready()).resolves.toBeUndefined()
      const marker = await readHomeLock(home)
      expect(marker?.ownerKind).toBe('daemon')
      expect(marker?.token).not.toBe('foreign-token')
      expect(marker?.pid).not.toBe(process.pid)
      expect(service.homes().length).toBeGreaterThan(0)
    } finally {
      await service.close()
      try {
        process.kill(daemon.pid, 'SIGTERM')
      } catch {
        /* already gone */
      }
    }
  }, 30_000)

  it('a DEAD predecessor\'s daemon marker is auto-recovered, not refused', async () => {
    // Binding ruling 1 (U5b): only our own kind may auto-recover, after a
    // kernel-flock probe. The boot must SUCCEED and clear the stale marker.
    await plantMarker({ ownerKind: 'daemon', pid: 2_147_000_000 })
    const runtimeDir = join(root, 'runtime')
    const daemon = await spawnDaemon(runtimeDir, [{ path: home, global: true }])
    const service = new OmtService({ runtimeDir: daemon.runtimeDir, name: 'oh-my-ticket-home-lock-test' })
    try {
      await expect(service.ready()).resolves.toBeUndefined()
      // The stale marker was replaced by the new daemon's OWN live marker.
      const marker = await readHomeLock(home)
      expect(marker?.ownerKind).toBe('daemon')
      expect(marker?.token).not.toBe('foreign-token')
      expect(marker?.pid).not.toBe(2_147_000_000)
      expect(service.homes().length).toBeGreaterThan(0)
    } finally {
      await service.close()
      try {
        process.kill(daemon.pid, 'SIGTERM')
      } catch {
        /* already gone */
      }
    }
  }, 30_000)
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
