/**
 * Daemon-generation-change resilience (TICKET-0131/0132).
 *
 * The field incident: a daemon restart over a WIPED home minted a new home
 * id; the adapter kept its old registry and every idle tick logged
 * NOT_FOUND(kind:home) until the whole DSH instance was restarted. These
 * specs pin the fix at three layers:
 *
 *   1. same-id daemon restart → ops recover, events do not double-deliver
 *      (onReconnected must not double-subscribe surviving ids);
 *   2. wiped home (fresh id) → the registry heals automatically through the
 *      reconnect callback, no instance restart;
 *   3. the NOT_FOUND guardrail heals ONLY ids this service previously
 *      trusted, exactly once per cooldown window, and unknown ids fail fast.
 */
import { mkdirSync, rmSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import type { HomeRef } from '../src/host/service.ts'
import { createRuntimeFixture } from './mocks/runtime-fixture.ts'

/** Poll until `probe` returns a value (reconnects complete asynchronously). */
async function waitFor<T>(probe: () => T | undefined, timeoutMs = 8_000): Promise<T> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const value = probe()
    if (value !== undefined) return value
    if (Date.now() > deadline) throw new Error('waitFor: condition not met in time')
    await new Promise(resolve => setTimeout(resolve, 25))
  }
}

function ghostHome(id: string): HomeRef {
  return { homeId: id, kind: 'global', name: 'ghost' }
}

describe('daemon generation changes (TICKET-0131/0132)', () => {
  it('same-id restart recovers ops and does NOT double-deliver events', async () => {
    const fx = await createRuntimeFixture({ label: 'rc-id1' })
    try {
      const epic = await fx.service.createNode(fx.globalHome, { type: 'epic', title: 'restart epic' })
      let bumps = 0
      const off = fx.service.onChange(() => {
        bumps += 1
      })

      await fx.restart()

      // Reconnect completes asynchronously; poll until data ops answer again.
      await waitFor(() => {
        return fx.service.listNodes(fx.globalHome).then(
          () => true,
          () => undefined,
        )
      })

      // Exactly ONE user-visible change → exactly one hub bump. A duplicate
      // subscription on the surviving home id would deliver the same
      // node.changed envelope twice and bump twice.
      await fx.service.updateNode({ id: epic.id, status: 'in_progress' }, {})
      await new Promise(resolve => setTimeout(resolve, 400))
      expect(bumps).toBe(1)
      off()
    } finally {
      await fx.stop()
    }
  })

  it('wiped home (fresh id): registry heals via onReconnected without an instance restart', async () => {
    const fx = await createRuntimeFixture({ label: 'rc-new2' })
    try {
      const oldId = fx.globalHome.homeId
      await fx.halt()
      // The field incident exactly: home directory deleted while the daemon
      // is down; the respawned daemon mints a brand-new id for it.
      rmSync(fx.globalHome.path, { recursive: true, force: true })
      mkdirSync(fx.globalHome.path, { recursive: true })
      await fx.restart()

      // No service restart here — the reconnect callback must swap the
      // registry to the new generation on its own.
      const healed = await waitFor(() => {
        const current = fx.service.homes().find(home => home.path === fx.globalHome.path)
        if (current !== undefined && current.homeId !== oldId) return current
        return undefined
      })
      expect(healed.homeId).not.toBe(oldId)

      // End-to-end: normal resolution now operates on the fresh home.
      const created = await fx.service.createNode(healed, { type: 'epic', title: 'after heal' })
      expect(created.id).toBeTruthy()
    } finally {
      await fx.stop()
    }
  })

  it('guardrail: stale-known id heals once within cooldown; unknown ids fail fast', async () => {
    const fx = await createRuntimeFixture({ label: 'rc-grd3' })
    try {
      // The bridge + heal internals are private; this spec drives them
      // behaviorally through typed-any access on purpose.
      const svc = fx.service as unknown as {
        knownHomeIds: Set<string>
        client: { forceReconnect: (timeoutMs?: number) => Promise<unknown> }
        rpc: (method: string, params: Record<string, unknown>) => Promise<unknown>
      }
      // Forge "an id from a previous generation": whitelisted, but the live
      // daemon has never heard of it.
      svc.knownHomeIds.add('h_prev_generation00')

      let heals = 0
      const realForceReconnect = svc.client.forceReconnect.bind(svc.client)
      svc.client.forceReconnect = (timeoutMs?: number) => {
        heals += 1
        return realForceReconnect(timeoutMs)
      }

      // Whitelisted stale id → one heal + one retry; retry still fails (the
      // id never existed on THIS daemon either) and the ORIGINAL problem is
      // surfaced instead of hanging or masking.
      await expect(svc.rpc('run/list', { homeId: 'h_prev_generation00' })).rejects.toMatchObject({
        problemCode: 'NOT_FOUND',
      })
      expect(heals).toBe(1)

      // Immediate repeat lands inside the cooldown window → no second heal.
      await expect(svc.rpc('run/list', { homeId: 'h_prev_generation00' })).rejects.toMatchObject({
        problemCode: 'NOT_FOUND',
      })
      expect(heals).toBe(1)

      // Unknown id (never in ANY registry) → fast NOT_FOUND, zero heals:
      // real bugs must stay loud, not trigger reconnection churn.
      // (listRuns wraps protocol problems into OmtError itself.)
      const t0 = Date.now()
      await expect(fx.service.listRuns(ghostHome('h_ghost000000000'))).rejects.toMatchObject({
        code: 'NOT_FOUND',
      })
      expect(Date.now() - t0).toBeLessThan(5_000)
      expect(heals).toBe(1)
    } finally {
      await fx.stop()
    }
  })
})
