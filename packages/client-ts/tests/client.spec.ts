/**
 * U5a/U5c integration smoke: the TypeScript client library drives a REAL
 * omt-daemon end-to-end — discovery/spawn, handshake enrollment, typed
 * calls over newline-delimited JSON-RPC, live event subscription,
 * idempotency-key replay semantics, and (U5c) automatic reconnection with
 * capped backoff plus cursor-exact event resubscription after the daemon
 * is killed with SIGKILL.
 *
 * Skips when the daemon binary has not been built yet
 * (`cargo build -p omt-runtime` produces target/debug/omt-daemon).
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { execSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { OmtClient } from '../src/client.js'

const REPO_ROOT = join(import.meta.dirname, '..', '..', '..')
const DAEMON = process.env.OMT_DAEMON_TEST ?? join(REPO_ROOT, 'target', 'debug', 'omt-daemon')
const haveDaemon = existsSync(DAEMON)

describe.skipIf(!haveDaemon)('OmtClient against a live omt-daemon', () => {
  let workDir: string
  let runtimeDir: string
  let homeDir: string

  beforeAll(async () => {
    workDir = mkdtempSync(join(tmpdir(), 'omt-client-'))
    runtimeDir = join(workDir, 'rt')
    homeDir = join(workDir, 'home')
    const fs = await import('node:fs')
    fs.mkdirSync(runtimeDir, { recursive: true })
    fs.mkdirSync(homeDir, { recursive: true })
  })

  afterAll(() => {
    // SIGTERM every daemon bound to THIS temp work dir; graceful drain
    // releases home locks and removes descriptors before exit.
    try {
      const stdout = execSync(`pgrep -f "omt-daemon --home ${workDir}" || true`, {
        encoding: 'utf8',
      })
      for (const pid of stdout.trim().split('\n').filter(Boolean)) {
        try {
          process.kill(Number(pid), 'SIGTERM')
        } catch {
          /* already gone */
        }
      }
    } catch {
      /* pgrep missing or no match */
    }
    rmSync(workDir, { recursive: true, force: true })
  })

  it('spawns the daemon on demand, enrolls, and performs typed calls', async () => {
    const client = new OmtClient({
      runtimeDir,
      daemonPath: DAEMON,
      daemonArgs: ['--home', homeDir],
      requestTimeoutMs: 15_000,
    })

    const handshake = await client.connect('cli', {}, 'client-ts-smoke')
    expect(handshake.protocolVersion).toBe('1.0')
    expect(handshake.credential.token).toHaveLength(64)
    expect(handshake.homes.length).toBeGreaterThan(0)

    const homeId = handshake.homes[0]!.homeId

    // Hierarchy fixture through the typed call layer.
    const epic = await client.call<{ node: { nodeId: string } }>('node/create', {
      homeId,
      type: 'epic',
      title: 'client-ts epic',
    })
    const story = await client.call<{ node: { nodeId: string } }>('node/create', {
      homeId,
      type: 'story',
      title: 'client-ts story',
      parentId: epic.node.nodeId,
    })
    const ticket = await client.call<{ node: { nodeId: string; revision: number } }>('node/create', {
      homeId,
      commandId: 'TS-SMOKE-CMD-1',
      type: 'ticket',
      title: 'client-ts ticket',
      parentId: story.node.nodeId,
    })
    expect(ticket.node.revision).toBeGreaterThan(0)

    // Optimistic revision gate surfaces as CONFLICT.
    await expect(
      client.call('node/update', {
        homeId,
        nodeId: ticket.node.nodeId,
        expectedRevision: ticket.node.revision - 1,
        changes: { priority: 5 },
      }),
    ).rejects.toMatchObject({ problemCode: 'CONFLICT' })

    // Idempotency: identical replay returns the same node, not a new one.
    const replay = await client.call<{ node: { nodeId: string } }>('node/create', {
      homeId,
      commandId: 'TS-SMOKE-CMD-1',
      type: 'ticket',
      title: 'client-ts ticket',
      parentId: story.node.nodeId,
    })
    expect(replay.node.nodeId).toBe(ticket.node.nodeId)

    await client.close()
  }, 30_000)

  it('streams live omt/event envelopes to an events() subscriber', async () => {
    const client = new OmtClient({
      runtimeDir,
      daemonPath: DAEMON,
      requestTimeoutMs: 15_000,
    })
    await client.connect('external')

    const received: Array<{ cursor: number; type: string }> = []
    const homeId = client.homes[0]!.homeId
    const dispose = client.events(homeId, (envelope) => {
      received.push({ cursor: envelope.cursor, type: envelope.type })
    })

    // Commit one mutation from ANOTHER principal; the subscriber sees it.
    const writer = new OmtClient({ runtimeDir, daemonPath: DAEMON })
    await writer.connect('cli')
    const epic = await writer.call<{ node: { nodeId: string } }>('node/create', {
      type: 'epic',
      title: 'event probe epic',
    })
    await writer.call('node/update', {
      nodeId: epic.node.nodeId,
      changes: { priority: 1 },
    })

    const deadline = Date.now() + 10_000
    while (!received.some((e) => e.type === 'node.changed') && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
    expect(received.some((e) => e.type === 'node.changed')).toBe(true)
    const cursors = received.map((e) => e.cursor)
    expect([...cursors].sort((a, b) => a - b)).toEqual(cursors)

    dispose()
    await writer.close()
    await client.close()
  }, 30_000)

  /**
   * U5c resilience contract: kill -9 the daemon mid-subscription; the
   * client must respawn it through discover-or-spawn, re-handshake with
   * capped backoff, and replay the subscription from its LAST DELIVERED
   * CURSOR — every event committed after the crash arrives exactly once
   * and contiguously (no gap, no duplicate range).
   */
  it('resumes a subscription without cursor gap after the daemon is killed -9', async () => {
    // Dedicated home + runtime dir: the kill/respawn cycle must not see
    // the other tests' event history, so cursors are fully deterministic.
    const rt2 = join(workDir, 'rt-reconnect')
    const home2 = join(workDir, 'home-reconnect')
    mkdirSync(rt2, { recursive: true })
    mkdirSync(home2, { recursive: true })
    const subscriber = new OmtClient({
      runtimeDir: rt2,
      daemonPath: DAEMON,
      daemonArgs: ['--home', home2],
      requestTimeoutMs: 15_000,
      reconnect: { initialDelayMs: 50, maxDelayMs: 1_000 },
    })
    const handshake = await subscriber.connect('cli')
    const homeId = handshake.homes[0]!.homeId

    const received: Array<{ cursor: number; type: string }> = []
    const dispose = subscriber.events(homeId, (envelope) => {
      received.push({ cursor: envelope.cursor, type: envelope.type })
    })

    // Phase 1 — exactly ONE committed event while connected.
    const writer = new OmtClient({
      runtimeDir: rt2,
      daemonPath: DAEMON,
      daemonArgs: ['--home', home2],
      requestTimeoutMs: 15_000,
      reconnect: { enabled: false },
    })
    await writer.connect('cli')
    await writer.call('node/create', { homeId, type: 'epic', title: 'pre-kill' })
    await waitFor(() => received.length >= 1, 'first event delivered pre-kill')
    const maxPreKill = Math.max(...received.map((e) => e.cursor))
    expect(received.map((e) => e.cursor)).toEqual([maxPreKill])
    await writer.close()

    // Crash: SIGKILL leaves the descriptor AND the daemon owner marker
    // behind (dead pid) — exactly the state auto-recovery exists for.
    const descriptor = JSON.parse(
      readFileSync(join(rt2, 'descriptor.json'), 'utf8'),
    ) as { pid: number; generation: number }
    const tokenBeforeKill = subscriber.credential!.token
    process.kill(descriptor.pid, 'SIGKILL')

    // The subscriber's automatic reconnect must land on a genuinely NEW
    // session: `connected` alone can read stale-true for a few event-loop
    // turns (a dead UDS peer is invisible until the close event lands), so
    // require connected AND a fresh handshake token — proof the reconnect
    // loop completed discover-or-spawn + handshake against the new daemon.
    await waitFor(
      () => subscriber.connected && subscriber.credential!.token !== tokenBeforeKill,
      'subscriber reconnected with a fresh handshake after SIGKILL',
      30_000,
    )

    // Respawn proof: the serving generation is strictly newer than the
    // killed one (a different process owns the home now). Exact generation
    // numbers are intentionally not pinned — a lost respawn race may burn
    // one.
    let respawnedGeneration = 0
    await waitFor(
      () => {
        const current = OmtClient.readDescriptor(rt2)
        if (current && current.generation > descriptor.generation) {
          respawnedGeneration = current.generation
        }
        return respawnedGeneration > 0
      },
      'respawned daemon of a newer generation',
    )

    // Phase 2 — TWO events committed only after reconnection. If resume
    // started anywhere but lastCursor these are lost; from 0 they'd dupe.
    const writer2 = new OmtClient({
      runtimeDir: rt2,
      daemonPath: DAEMON,
      daemonArgs: ['--home', home2],
      requestTimeoutMs: 15_000,
      reconnect: { enabled: false },
    })
    await writer2.connect('cli')
    await writer2.call('node/create', { homeId, type: 'epic', title: 'post-crash-a' })
    await writer2.call('node/create', { homeId, type: 'epic', title: 'post-crash-b' })

    // Both post-crash events arrive, contiguous after the pre-kill cursor.
    await waitFor(() => received.length >= 3, 'post-crash events delivered after resume')

    const cursors = received.map((e) => e.cursor)
    expect([...cursors].sort((a, b) => a - b)).toEqual(cursors) // strictly ordered
    expect(new Set(cursors).size).toBe(cursors.length) // no duplicates
    const post = cursors.filter((c) => c > maxPreKill)
    expect(post).toEqual([maxPreKill + 1, maxPreKill + 2]) // no gap, exactly-once

    // The respawned generation is a different process than the killed one.
    expect(respawnedGeneration).toBeGreaterThan(descriptor.generation)

    dispose()
    await writer2.close()
    await subscriber.close()
  }, 60_000)
})

/** Poll until `condition` holds; polls every 50ms up to `timeoutMs`. */
async function waitFor(condition: () => boolean, what: string, timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
}
