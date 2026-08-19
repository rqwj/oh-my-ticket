/**
 * ReferencedBar tests (TICKET-0005): occurrence-table derivation (dedup,
 * ticket-only) and controller summary caching.
 */
import { describe, expect, it } from 'vitest'
import { ticketRefs } from '../src/client/components/ReferencedBar.tsx'
import { OmtController } from '../src/client/controller.ts'
import type { RpcResultLike } from '../src/client/trigger/source.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

describe('ticketRefs', () => {
  it('extracts ticket refs, deduped, order preserved', () => {
    const input = {
      occurrences: [
        { source: 'ticket', ref: 'TICKET-0001', label: 'TICKET-0001 a' },
        { source: 'subagent', ref: 'agent-1', label: 'agent-1' },
        { source: 'ticket', ref: 'STORY-0003', label: 'STORY-0003 b' },
        { source: 'ticket', ref: 'TICKET-0001', label: 'TICKET-0001 a' },
      ],
    }
    expect(ticketRefs(input)).toEqual(['TICKET-0001', 'STORY-0003'])
  })

  it('handles absent input and empty occurrences', () => {
    expect(ticketRefs(undefined)).toEqual([])
    expect(ticketRefs({})).toEqual([])
    expect(ticketRefs({ occurrences: [] })).toEqual([])
  })
})

describe('controller.ensureSummaries', () => {
  it('fetches only missing summaries and caches them', async () => {
    const fetched: string[] = []
    const rpc = {
      async call(_c: string, endpoint: string, payload: any): Promise<RpcResultLike> {
        if (endpoint === 'get') {
          fetched.push(payload.id)
          return {
            ok: true,
            value: { node: { id: payload.id, type: 'ticket', title: `标题-${payload.id}`, status: 'open' } },
          }
        }
        return { ok: false, error: { message: 'unexpected' } }
      },
    }
    const controller = new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })

    await controller.ensureSummaries('s1', ['TICKET-0001', 'TICKET-0002'])
    expect(fetched).toEqual(['TICKET-0001', 'TICKET-0002'])
    expect(controller.summaries.getSnapshot()['TICKET-0001']?.title).toBe('标题-TICKET-0001')

    // Second call: everything cached, no new fetches.
    await controller.ensureSummaries('s1', ['TICKET-0001', 'TICKET-0002'])
    expect(fetched).toHaveLength(2)
  })
})
