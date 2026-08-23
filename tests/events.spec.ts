/**
 * Change push tests: ChangeHub fan-out and the /omt/events SSE route
 * (headers, frames, unsubscribe on close), plus the run-dimension payload
 * (TICKET-0071): an optional `run` hint that old clients simply ignore.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { bridgeRunEvents, ChangeHub, type OmtChangeEvent } from '../src/host/changes.ts'
import { OmtCore } from '../src/host/core.ts'
import { registerOmtEvents } from '../src/host/events.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

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

it('bridgeRunEvents forwards core run/item transitions into hub bumps', async () => {
  const home = await mkdtemp(join(tmpdir(), 'omt-events-bridge-'))
  const core = await OmtCore.open(home)
  try {
    const hub = new ChangeHub()
    const seen: OmtChangeEvent[] = []
    hub.subscribe(event => seen.push(event))
    const detach = bridgeRunEvents(core, hub)

    const epic = await core.create({ type: 'epic', title: '桥接背景' })
    const story = await core.create({ type: 'story', title: '桥接范围', parentId: epic.id })
    const ticket = await core.create({ type: 'ticket', title: '桥接任务', parentId: story.id })
    const run = await core.createRun({ nodeIds: [ticket.id] })
    await core.startRun(run.id)
    await core.transitionItem(run.id, ticket.id, 'running', { executorSessionId: 'sess-1' })

    expect(seen.map(event => event.run)).toEqual([
      { id: run.id, kind: 'run' },
      { id: run.id, kind: 'item', nodeId: ticket.id },
    ])
    expect(seen.every(event => event.home === home)).toBe(true)

    // Detach stops the forwarding (plugin teardown path).
    detach()
    await core.pauseRun(run.id)
    expect(seen).toHaveLength(2)
  } finally {
    core.close()
    await rm(home, { recursive: true, force: true })
  }
})
