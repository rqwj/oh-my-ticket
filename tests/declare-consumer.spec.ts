/**
 * U6 (R8/KD2-KTD3): DSH consumer wiring for home/declare — a workspace home
 * that exists on disk but is unknown to the daemon is auto-declared on
 * first resolution (declare → forceReconnect → registry hit), with the F4
 * feature-gate fallback for pre-U5 daemons and the defensive
 * requiresRehandshake re-handshake layer for stale home grants.
 */
import { mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { OmtProtocolError } from '../packages/client-ts/src/transport.ts'
import type { OmtClient } from '../packages/client-ts/src/client.ts'
import type { OmtService } from '../src/host/service.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

let fixture: RuntimeFixture

afterEach(async () => {
  await fixture?.stop()
})

/** Reach the service's private OmtClient (white-box stubbing seams). */
function clientOf(service: OmtService): OmtClient {
  return (service as any).client as OmtClient
}

describe('U6 declare consumer wiring', () => {
  beforeEach(async () => {
    // Daemon knows ONLY the global home — the workspace home is created
    // AFTER boot, so it is genuinely unregistered.
    fixture = await createRuntimeFixture({ label: 'u6' })
  })

  it('auto-declares an unknown workspace home on first resolution, then CRUD works', async () => {
    const wsRoot = mkdtempSync(join(tmpdir(), 'omt-u6-ws-'))
    mkdirSync(join(wsRoot, '.omt'), { recursive: true })

    const declareSpy = vi.spyOn(clientOf(fixture.service), 'declareHome')
    const home = await fixture.service.homeFor(wsRoot)
    expect(declareSpy).toHaveBeenCalledTimes(1)
    expect(home.kind).toBe('workspace')
    expect(home.path).toBe(join(wsRoot, '.omt'))

    // The declared home is fully usable (registry insertion preceded the
    // response; the re-handshake folded it into the scoped grant).
    const epic = await fixture.service.createNode(home, { type: 'epic', title: '自动声明的工作区' })
    expect(epic.id).toMatch(/^EPIC-/)
    const listed = await fixture.service.listNodes(home, {})
    expect(listed.some(n => n.id === epic.id)).toBe(true)
  })

  it('second resolution of the same cwd is a registry hit — no repeated declare', async () => {
    const wsRoot = mkdtempSync(join(tmpdir(), 'omt-u6-ws2-'))
    mkdirSync(join(wsRoot, '.omt'), { recursive: true })

    const declareSpy = vi.spyOn(clientOf(fixture.service), 'declareHome')
    const first = await fixture.service.homeFor(wsRoot)
    const second = await fixture.service.homeFor(wsRoot)
    expect(second.homeId).toBe(first.homeId)
    expect(declareSpy).toHaveBeenCalledTimes(1) // idempotent reentry
  })

  it('falls back to the legacy hard error when the daemon lacks features.homeDeclare (F4)', async () => {
    const client = clientOf(fixture.service)
    const featuresSpy = vi.spyOn(client, 'features', 'get').mockReturnValue({})
    const declareSpy = vi.spyOn(client, 'declareHome')

    const wsRoot = mkdtempSync(join(tmpdir(), 'omt-u6-ws3-'))
    mkdirSync(join(wsRoot, '.omt'), { recursive: true })

    await expect(fixture.service.homeFor(wsRoot)).rejects.toMatchObject({
      code: 'INVALID_INPUT',
      details: { rule: 'home-not-opened' },
    })
    // Updated copy: blames the daemon version, never mentions --home.
    await expect(fixture.service.homeFor(wsRoot)).rejects.toThrow(/home\/declare/)
    await expect(fixture.service.homeFor(wsRoot)).rejects.not.toThrow(/--home/)
    expect(declareSpy).not.toHaveBeenCalled()
    featuresSpy.mockRestore()
  })

  it('a declare that still fails the original resolution throws — no retry loop', async () => {
    const client = clientOf(fixture.service)
    // Stub declareHome to "succeed" but leave the registry untouched:
    // resolution must NOT loop declaring forever.
    const declareSpy = vi.spyOn(client, 'declareHome').mockResolvedValue({
      homeId: 'HOME-FAKE',
      requiresRehandshake: false,
    } as any)

    const wsRoot = mkdtempSync(join(tmpdir(), 'omt-u6-ws4-'))
    mkdirSync(join(wsRoot, '.omt'), { recursive: true })

    await expect(fixture.service.homeFor(wsRoot)).rejects.toMatchObject({
      details: { rule: 'home-not-opened' },
    })
    expect(declareSpy).toHaveBeenCalledTimes(1) // exactly one attempt
    declareSpy.mockRestore()
  })

  it('a home-scope denial carrying requiresRehandshake triggers one re-handshake + retry', async () => {
    const client = clientOf(fixture.service)
    // The defensive layer is opt-in (default off: OmtService owns the
    // curated TICKET-0132 heal); enable it as a thin client would.
    ;(client as any).options.rehandshakeOnHomeScopeHint = true
    const transport = (client as any).transport
    const originalCall = transport.call.bind(transport)
    let poisoned = true
    transport.call = async (method: string, params: unknown, hooks?: unknown) => {
      if (poisoned && method === 'node/list') {
        poisoned = false
        throw new OmtProtocolError({
          code: 'FORBIDDEN',
          message: 'home not in credential scope',
          details: { rule: 'home-not-scoped', requiresRehandshake: true },
        } as any)
      }
      return originalCall(method, params, hooks)
    }

    const reconnectSpy = vi.spyOn(client, 'forceReconnect')
    const home = fixture.globalHome
    const nodes = await fixture.service.listNodes(home, {})
    expect(Array.isArray(nodes)).toBe(true) // retry succeeded
    expect(reconnectSpy).toHaveBeenCalledTimes(1)
    reconnectSpy.mockRestore()
    transport.call = originalCall
  })

  it('an op-family FORBIDDEN (no hint) propagates untouched — KTD3 split', async () => {
    const client = clientOf(fixture.service)
    ;(client as any).options.rehandshakeOnHomeScopeHint = true
    const transport = (client as any).transport
    const originalCall = transport.call.bind(transport)
    transport.call = async (method: string, params: unknown, hooks?: unknown) => {
      if (method === 'node/list') {
        throw new OmtProtocolError({
          code: 'FORBIDDEN',
          message: 'operation family not granted',
          details: { rule: 'operation-family' },
        } as any)
      }
      return originalCall(method, params, hooks)
    }

    const reconnectSpy = vi.spyOn(client, 'forceReconnect')
    await expect(fixture.service.listNodes(fixture.globalHome, {})).rejects.toThrow(/operation family/)
    expect(reconnectSpy).not.toHaveBeenCalled()
    reconnectSpy.mockRestore()
    transport.call = originalCall
  })
})
