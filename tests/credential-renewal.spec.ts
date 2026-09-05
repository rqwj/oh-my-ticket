import { createServer, type Socket } from 'node:net'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { OmtClient, type ClientOptions } from '../packages/client-ts/src/client.ts'

type Request = { id: number; method: string; params: Record<string, any> }
type Reply = (result: unknown) => void
type Reject = (code?: string, reason?: string, extra?: object) => void
const cleanups: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})

// Real Transport and Unix socket; only daemon discovery is replaced. The
// server deterministically expires tokens rather than sleeping for 12 hours.
async function fixture(options: ClientOptions = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'omt-renew-'))
  const endpoint = join(dir, 's')
  const sockets = new Set<Socket>()
  const handshakes: Request[] = []
  const calls: Request[] = []
  let connections = 0
  let accept: (request: Request, reply: Reply, reject: Reject, socket: Socket) => void = (_r, reply) => reply('ok')
  let enroll: ((request: Request, reply: Reply, reject: Reject) => void) | undefined
  const outcome = (token: string) => ({ protocolVersion: '1.0', homes: [], daemon: { name: 'test', version: '1' }, credential: { token, principalId: 'test', actorNamespace: 'actor/session', homes: ['h'], operations: ['node/*'], expiresAt: 'later' } })
  const server = createServer(socket => {
    connections++
    sockets.add(socket)
    socket.on('close', () => sockets.delete(socket))
    // Lifecycle tests intentionally send a late answer after client close.
    socket.on('error', error => {
      if ((error as NodeJS.ErrnoException).code !== 'EPIPE') throw error
    })
    let buffer = ''
    socket.setEncoding('utf8')
    socket.on('data', chunk => {
      buffer += chunk
      while (buffer.includes('\n')) {
        const end = buffer.indexOf('\n')
        const request: Request = JSON.parse(buffer.slice(0, end))
        buffer = buffer.slice(end + 1)
        const reply: Reply = result => socket.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n')
        const reject: Reject = (code = 'UNAUTHORIZED', reason = 'unknown-credential', extra = {}) => socket.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32000, message: code, data: { code, details: { reason, ...extra } } } }) + '\n')
        if (request.method === 'handshake/request') {
          handshakes.push(request)
          if (enroll) enroll(request, reply, reject)
          else reply(outcome(`token-${handshakes.length}`))
        } else {
          calls.push(request)
          accept(request, reply, reject, socket)
        }
      }
    })
  })
  await new Promise<void>(resolve => server.listen(endpoint, resolve))
  vi.spyOn(OmtClient, 'discoverOrSpawn').mockResolvedValue({ schemaVersion: 1, endpoint, generation: 1, pid: process.pid, bootToken: 'test', startedAt: '' })
  const client = new OmtClient({ noSpawn: true, requestTimeoutMs: 1000, reconnect: { enabled: false }, ...options })
  cleanups.push(async () => {
    client.close()
    for (const socket of sockets) socket.destroy()
    await new Promise<void>(resolve => server.close(() => resolve()))
    rmSync(dir, { recursive: true, force: true })
  })
  await client.connect('dsh', { actorNamespace: 'actor', homes: ['h'], operations: ['node/*'] }, 'test-client', 'session')
  return { client, calls, handshakes, outcome, get connections() { return connections }, set accept(value: typeof accept) { accept = value }, set enroll(value: typeof enroll) { enroll = value } }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(r => { resolve = r })
  return { promise, resolve }
}

describe('credential renewal', () => {
  it.each(['unknown-credential', 'expired-credential'])('renews %s on the same socket with original enrollment and a fresh token', async reason => {
    const fx = await fixture()
    fx.accept = (request, reply, reject) => request.params.credential.token === 'token-1' ? reject('UNAUTHORIZED', reason) : reply(request.params.value)
    const issued: number[] = []
    await expect(fx.client.call('node/create', { value: 42 }, { onIssued: id => issued.push(id) })).resolves.toBe(42)
    expect(fx.handshakes).toHaveLength(2)
    expect(fx.handshakes[1].params).toEqual(fx.handshakes[0].params)
    expect(fx.calls.map(r => r.params.credential.token)).toEqual(['token-1', 'token-2'])
    expect(issued).toHaveLength(2)
    expect(fx.connections).toBe(1)
  })

  it('coalesces concurrent and late failures without interrupting another pending write', async () => {
    const fx = await fixture()
    const held = deferred<Reply>()
    const late = deferred<Reject>()
    fx.accept = (r, reply, reject) => {
      if (r.method === 'write/pending') held.resolve(reply)
      else if (r.method === 'late' && r.params.credential.token === 'token-1') late.resolve(reject)
      else if (r.params.credential.token === 'token-1') reject()
      else reply('ok')
    }
    const pending = fx.client.call('write/pending')
    const finish = await held.promise
    const delayed = fx.client.call('late')
    const rejectLate = await late.promise
    await expect(Promise.all([fx.client.call('a'), fx.client.call('b')])).resolves.toEqual(['ok', 'ok'])
    rejectLate()
    await expect(delayed).resolves.toBe('ok')
    finish('written-once')
    await expect(pending).resolves.toBe('written-once')
    expect(fx.handshakes).toHaveLength(2)
    expect(fx.connections).toBe(1)
    expect(fx.calls.filter(r => r.method === 'write/pending')).toHaveLength(1)
  })

  it.each([
    ['FORBIDDEN', 'unknown-credential'], ['UNAUTHORIZED', 'missing-credential'],
    ['UNAUTHORIZED', 'other'], ['UNKNOWN', 'unknown-credential'],
  ])('does not renew %s / %s', async (code, reason) => {
    const fx = await fixture()
    fx.accept = (_r, _reply, reject) => reject(code, reason)
    await expect(fx.client.call('node/create')).rejects.toMatchObject({ problemCode: code })
    expect(fx.handshakes).toHaveLength(1)
    expect(fx.calls).toHaveLength(1)
  })

  it('stops after one retry', async () => {
    const fx = await fixture()
    fx.accept = (_r, _reply, reject) => reject()
    await expect(fx.client.call('node/create')).rejects.toMatchObject({ problemCode: 'UNAUTHORIZED' })
    expect(fx.handshakes).toHaveLength(2)
    expect(fx.calls).toHaveLength(2)
  })

  it('does not cache a failed renewal promise', async () => {
    const fx = await fixture()
    fx.accept = (r, reply, reject) => r.params.credential.token === 'token-1' ? reject() : reply('ok')
    fx.enroll = (_r, _reply, reject) => reject('FORBIDDEN', 'enrollment-denied')
    await expect(fx.client.call('node/create')).rejects.toMatchObject({ problemCode: 'FORBIDDEN' })
    fx.enroll = undefined
    await expect(fx.client.call('node/create')).resolves.toBe('ok')
    expect(fx.handshakes).toHaveLength(3)
  })

  it('does not replay an uncertain network failure', async () => {
    const fx = await fixture()
    fx.accept = (_r, _reply, _reject, socket) => socket.destroy()
    await expect(fx.client.call('node/create')).rejects.toThrow('connection closed')
    expect(fx.calls).toHaveLength(1)
    expect(fx.handshakes).toHaveLength(1)
  })

  it('close while renewal is pending cannot revive the client', async () => {
    const fx = await fixture()
    const renewal = deferred<Reply>()
    fx.accept = (_r, _reply, reject) => reject()
    fx.enroll = (_r, reply) => renewal.resolve(reply)
    const result = fx.client.call('node/create').catch(error => error)
    const reply = await renewal.promise
    fx.client.close()
    reply(fx.outcome('stale'))
    expect(await result).toBeInstanceOf(Error)
    expect(fx.client.credential).toBeNull()
    expect(fx.client.connected).toBe(false)
    expect(fx.calls).toHaveLength(1)
  })

  it('home-scope recovery stays opt-in and retries with the reconnected token', async () => {
    const fx = await fixture({ rehandshakeOnHomeScopeHint: true, reconnect: { initialDelayMs: 1, maxDelayMs: 1 } })
    fx.accept = (r, reply, reject) => r.params.credential.token === 'token-1' ? reject('FORBIDDEN', 'home-not-scoped', { requiresRehandshake: true }) : reply('ok')
    await expect(fx.client.call('node/create')).resolves.toBe('ok')
    expect(fx.calls.map(r => r.params.credential.token)).toEqual(['token-1', 'token-2'])
    expect(fx.connections).toBe(2)
  })

  it('keeps subscription notifications flowing across renewal', async () => {
    const fx = await fixture()
    const subscribed = deferred<Socket>()
    fx.accept = (r, reply, reject, socket) => {
      if (r.method === 'events/resume') {
        reply({ cursor: r.params.cursor, events: [] })
        subscribed.resolve(socket)
      } else if (r.params.credential.token === 'token-1') reject()
      else reply('ok')
    }
    const received: number[] = []
    const delivered = deferred<void>()
    const errors: Error[] = []
    const off = fx.client.events('h', event => {
      received.push(event.cursor)
      if (event.cursor === 2) delivered.resolve()
    }, { onError: error => errors.push(error) })
    const socket = await subscribed.promise
    const notify = (cursor: number) => socket.write(JSON.stringify({ jsonrpc: '2.0', method: 'omt/event', params: { homeId: 'h', cursor } }) + '\n')
    notify(1)
    await fx.client.call('node/create')
    notify(2)
    await delivered.promise
    off()
    expect(received).toEqual([1, 2])
    expect(errors).toEqual([])
    expect(fx.connections).toBe(1)
  })

  it('a real reconnect during renewal keeps the replacement credential', async () => {
    const fx = await fixture({ reconnect: { initialDelayMs: 1, maxDelayMs: 1 } })
    const renewal = deferred<Reply>()
    fx.accept = (_r, _reply, reject) => reject()
    fx.enroll = (_r, reply) => renewal.resolve(reply)
    const result = fx.client.call('node/create').catch(error => error)
    const lateReply = await renewal.promise
    fx.enroll = undefined
    await fx.client.forceReconnect()
    lateReply(fx.outcome('stale'))
    expect(await result).toBeInstanceOf(Error)
    expect(fx.client.credential?.token).toBe('token-3')
    expect(fx.calls).toHaveLength(1)
    expect(fx.connections).toBe(2)
  })

  it('does not enable home-scope recovery by default', async () => {
    const fx = await fixture()
    fx.accept = (_r, _reply, reject) => reject('FORBIDDEN', 'home-not-scoped', { requiresRehandshake: true })
    await expect(fx.client.call('node/create')).rejects.toMatchObject({ problemCode: 'FORBIDDEN' })
    expect(fx.handshakes).toHaveLength(1)
  })
})
