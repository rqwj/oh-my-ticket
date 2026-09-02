/**
 * Change push tests: ChangeHub fan-out and the /omt/events SSE route
 * (headers, frames, unsubscribe on close), plus the run-dimension payload
 * (TICKET-0071). U7a: the core→hub bridge is replaced by the runtime
 * service's daemon-event subscription — verified end-to-end with two
 * clients on one daemon (mutations from B stream into A's hub; closing A
 * stops delivery).
 */
import { describe, expect, it } from 'vitest'
import type { OmtChangeEvent } from '../src/host/service.ts'
import { ChangeHub, OmtService } from '../src/host/service.ts'
import { registerOmtEvents } from '../src/host/events.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

/** Poll until `predicate` holds (daemon events arrive asynchronously). */
async function waitFor<T>(predicate: () => T | undefined, timeoutMs = 5000): Promise<T> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const value = predicate()
    if (value !== undefined) return value
    if (Date.now() > deadline) throw new Error('waitFor: condition not met in time')
    await new Promise(resolve => setTimeout(resolve, 25))
  }
}

it('ChangeHub fans out versioned events and survives listener errors', () => {
  const hub = new ChangeHub()
  const seen: number[] = []
  hub.subscribe(() => {
    throw new Error('broken listener')
  })
  hub.subscribe(event => seen.push(event.version))
  hub.bump('/home/a')
  hub.bump('/home/b')
  expect(seen).toEqual([1, 2])
})

it('ChangeHub unsubscribe stops delivery', () => {
  const hub = new ChangeHub()
  const seen: number[] = []
  const off = hub.subscribe(event => seen.push(event.version))
  hub.bump('/home')
  off()
  hub.bump('/home')
  expect(seen).toEqual([1])
})

it('SSE route streams change frames until the request closes', () => {
  const hub = new ChangeHub()
  let registered: any
  registerOmtEvents({ webServer: { register: (route: any) => { registered = route } } } as never, hub)

  expect(registered.kind).toBe('exact')
  expect(registered.path).toBe('/omt/events')

  const chunks: string[] = []
  let closeFn: (() => void) | undefined
  const res = {
    headers: undefined as Record<string, string> | undefined,
    writeHead(_status: number, headers: Record<string, string>) { this.headers = headers },
    write(chunk: string) { chunks.push(chunk) },
    end() {},
  }
  const req = { on(_event: 'close', fn: () => void) { closeFn = fn } }

  registered.handler(req, res)
  expect(res.headers?.['content-type']).toContain('text/event-stream')

  hub.bump('/home/x')
  expect(chunks.some(chunk => chunk.includes('"version":1') && chunk.includes('/home/x'))).toBe(true)

  // After close, later bumps produce no more frames.
  closeFn?.()
  const before = chunks.length
  hub.bump('/home/x')
  expect(chunks.length).toBe(before)
})

it('bump carries an optional run hint (additive; old clients ignore it)', () => {
  const hub = new ChangeHub()
  const seen: OmtChangeEvent[] = []
  hub.subscribe(event => seen.push(event))
  hub.bump('/home/a')
  hub.bump('/home/a', { id: 'RUN-0001', kind: 'item', nodeId: 'TICKET-0001' })
  expect(seen[0]).toEqual({ version: 1, home: '/home/a' })
  expect(seen[1]).toEqual({ version: 2, home: '/home/a', run: { id: 'RUN-0001', kind: 'item', nodeId: 'TICKET-0001' } })
})

// REWRITTEN for U7a: was bridgeRunEvents(core→hub, in-process); now the
// service's daemon-event subscription feeds the hub. Client A observes;
// client B mutates through the same daemon — proving the wire path, the
// homeId stamping, and that closing A stops the forwarding.
it('daemon events stream into the change hub until the client detaches', async () => {
  const fixture: RuntimeFixture = await createRuntimeFixture({ label: 'events' })
  try {
    const observer = new OmtService({ runtimeDir: fixture.runtimeDir, name: 'observer', noSpawn: true })
    await observer.ready()
    const hub = observer.hub
    const seen: OmtChangeEvent[] = []
    hub.subscribe(event => seen.push(event))
    // Give the daemon-side subscription a beat to establish before mutating.
    await new Promise(resolve => setTimeout(resolve, 50))

    const home = fixture.globalHome
    // Subscribe FIRST, then mutate: the collector must see every bump from
    // node creation through the run transitions.
    const epic = await fixture.service.createNode(home, { type: 'epic', title: '桥接背景' })
    const story = await fixture.service.createNode(home, { type: 'story', title: '桥接范围', parentId: epic.id })
    const ticket = await fixture.service.createNode(home, { type: 'ticket', title: '桥接任务', parentId: story.id })
    const run = await fixture.service.createRun(home, { nodeIds: [ticket.id] })
    await fixture.service.controlRun(home, run.run.id, 'start')
    await fixture.service.claimItem(home, run.run.id, 'sess-1')

    // Node creations arrive as plain bumps; the run lifecycle carries hints
    // (run.changed → kind 'run'; claim dispatch → kind 'item' with nodeId).
    const itemHint = await waitFor(() =>
      seen.find(event => event.run?.kind === 'item')?.run)
    expect(itemHint).toMatchObject({ id: run.run.id, kind: 'item', nodeId: ticket.id })
    await waitFor(() => (seen.some(event => event.run?.kind === 'run') ? true : undefined))
    expect(seen.every(event => event.home === home.homeId)).toBe(true)

    // Detach stops the forwarding (plugin teardown path): closing the
    // observing client tears down its subscriptions — the pause mutation
    // afterwards must deliver nothing more.
    await observer.close()
    const atClose = seen.length
    await fixture.service.controlRun(home, run.run.id, 'pause')
    await new Promise(resolve => setTimeout(resolve, 200))
    expect(seen.length).toBe(atClose)
  } finally {
    await fixture.stop()
  }
})

// TICKET-0123 acceptance: SSE survives a daemon restart BY CURSOR — the
// observing service reconnects (client auto-resume replays from its last
// delivered cursor), and a mutation committed by the NEW daemon still lands
// in the hub with a fresh, monotonic version.
it('hub keeps receiving frames after a daemon restart (cursor resume)', { timeout: 40_000 }, async () => {
  const fixture: RuntimeFixture = await createRuntimeFixture({ label: 'events-restart' })
  try {
    const observer = new OmtService({ runtimeDir: fixture.runtimeDir, name: 'observer-restart', noSpawn: true })
    await observer.ready()
    const seen: OmtChangeEvent[] = []
    observer.hub.subscribe(event => seen.push(event))
    await new Promise(resolve => setTimeout(resolve, 50))

    const home = fixture.globalHome
    await fixture.service.createNode(home, { type: 'epic', title: '重启前' })
    await waitFor(() => (seen.length > 0 ? true : undefined))
    // Baseline captured BEFORE the restart: every later frame proves the
    // reconnect + cursor-replay path delivered post-restart state.
    const baseline = seen.length

    // Restart the daemon over the SAME runtime dir; the handed-out service
    // reconnects with backoff and re-subscribes from its stored cursor.
    await fixture.restart()

    // The observer may still be mid-backoff when this mutation commits —
    // exactly the gap the cursor replay exists to cover.
    await fixture.service.createNode(home, { type: 'epic', title: '重启后' })
    const deadline = Date.now() + 20_000
    while (seen.length <= baseline && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    expect(seen.length).toBeGreaterThan(baseline)
    expect(seen.at(-1)?.home).toBe(home.homeId)
    await observer.close()
  } finally {
    await fixture.stop()
  }
})
