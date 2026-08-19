/**
 * Change push tests: ChangeHub fan-out and the /omt/events SSE route
 * (headers, frames, unsubscribe on close).
 */
import { describe, expect, it } from 'vitest'
import { ChangeHub } from '../src/host/changes.ts'
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
