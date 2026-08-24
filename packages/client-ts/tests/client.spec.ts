/**
 * U5a integration smoke: the TypeScript client library drives a REAL
 * omt-daemon end-to-end — discovery/spawn, handshake enrollment, typed
 * calls over newline-delimited JSON-RPC, live event subscription, and
 * idempotency-key replay semantics.
 *
 * Skips when the daemon binary has not been built yet
 * (`cargo build -p omt-runtime` produces target/debug/omt-daemon).
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest'
import { execSync } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
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
})
