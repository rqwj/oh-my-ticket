// @vitest-environment jsdom
/**
 * Run UI controller tests (STORY-0013): run list/detail stores, the
 * join-run flow (直建/唯一直加/多选弹窗/跨 home 报错), run-control and
 * run-confirm wiring, and the SSE run-hint refresh (TICKET-0071).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { OmtController } from '../src/client/controller.ts'
import { canRemoveItem, canRetryItem, groupRuns, runControlActions } from '../src/client/run-view.ts'
import type { RunSummary } from '../src/client/store.ts'
import type { RpcResultLike } from '../src/client/trigger/source.ts'
import { runFixture, type RunFixtureOptions } from './mocks/run-fixtures.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

function run(id: string, status: string, overrides: RunFixtureOptions = {}): RunSummary {
  return runFixture(id, status as RunSummary['status'], overrides)
}

const RUN_DETAIL = {
  run: { ...run('RUN-1', 'running'), config: { stopOnFailure: false, autoContinue: true, autoVerify: false, concurrency: 1 } },
  items: [
    { node_id: 'TICKET-0001', position: 0, state: 'done', attempts: 1 },
    { node_id: 'TICKET-0002', position: 1, state: 'running', attempts: 0, executor: { sessionId: 'sess-1', label: '工作区 的会话' } },
  ],
}

let calls: { endpoint: string; payload: any }[]
let listValue: any
let controller: OmtController

function makeController(extra?: Record<string, RpcResultLike>): OmtController {
  const rpc = {
    async call(_channel: string, endpoint: string, payload: any): Promise<RpcResultLike> {
      calls.push({ endpoint, payload })
      if (extra !== undefined && endpoint in extra) return extra[endpoint]
      if (endpoint === 'run-list') return { ok: true, value: { runs: listValue } }
      if (endpoint === 'run-show') return { ok: true, value: RUN_DETAIL }
      if (endpoint === 'get') {
        return {
          ok: true,
          value: {
            node: { id: payload.id, type: 'ticket', title: '登录接口', status: 'open', priority: 0, path: 'p', created_at: '', updated_at: '', children: [] },
            children: [],
            body: '## 描述',
            runs: [],
          },
        }
      }
      return {
        ok: true,
        value: {
          run: run(payload.id ?? 'RUN-NEW', 'pending'),
          added: [payload.nodeIds?.[0] ?? 'TICKET-0001'],
          addedRunning: [],
          duplicates: [],
          skippedDone: 0,
          skippedArchived: 0,
        },
      }
    },
  }
  return new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })
}

beforeEach(() => {
  calls = []
  listValue = []
  controller = makeController()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('run list / detail stores', () => {
  it('refreshRuns populates the runs store from run-list', async () => {
    listValue = [run('RUN-1', 'running')]
    await controller.refreshRuns('s1')
    expect(calls[0]).toEqual({ endpoint: 'run-list', payload: { sessionId: 's1' } })
    expect(controller.runs.getSnapshot()).toMatchObject({ status: 'ready' })
    const snapshot = controller.runs.getSnapshot()
    if (snapshot.status !== 'ready') throw new Error('expected ready')
    expect(snapshot.runs.map(r => r.id)).toEqual(['RUN-1'])
  })

  it('refreshRuns surfaces RPC failure as an error state', async () => {
    const failing = makeController({ 'run-list': { ok: false, error: { message: 'boom' } } })
    await failing.refreshRuns('s1')
    expect(failing.runs.getSnapshot()).toMatchObject({ status: 'error', message: 'boom' })
  })

  it('openRun loads the detail; closeRunDetail resets to idle', async () => {
    await controller.openRun('RUN-1', 's1')
    expect(calls.some(call => call.endpoint === 'run-show' && call.payload.id === 'RUN-1')).toBe(true)
    const detail = controller.runDetail.getSnapshot()
    expect(detail).toMatchObject({ status: 'ready', id: 'RUN-1' })
    controller.closeRunDetail()
    expect(controller.runDetail.getSnapshot()).toEqual({ status: 'idle' })
  })

  it('showRuns flips the panel section and fetches; showTickets flips back', async () => {
    controller.showRuns('s1')
    expect(controller.panelSection.getSnapshot()).toBe('runs')
    await Promise.resolve()
    await Promise.resolve()
    expect(calls.some(call => call.endpoint === 'run-list')).toBe(true)
    controller.showTickets()
    expect(controller.panelSection.getSnapshot()).toBe('tickets')
  })

  it('showRunInPanel opens the drawer, switches to runs, and loads the detail', async () => {
    await controller.showRunInPanel('RUN-1', 's1')
    expect(controller.drawerOpen.getSnapshot()).toBe(true)
    expect(controller.panelSection.getSnapshot()).toBe('runs')
    expect(controller.runDetail.getSnapshot()).toMatchObject({ status: 'ready', id: 'RUN-1' })
  })
})

describe('runControl / runConfirm', () => {
  it('runControl sends the action and applies the response to list and open detail', async () => {
    const controlled = makeController({ 'run-control': { ok: true, value: { run: run('RUN-1', 'paused') } } })
    listValue = [run('RUN-1', 'running')]
    await controlled.refreshRuns('s1')
    await controlled.openRun('RUN-1', 's1')
    calls = []
    await controlled.runControl('RUN-1', 'pause', undefined, 's1')
    expect(calls).toEqual([{ endpoint: 'run-control', payload: { id: 'RUN-1', action: 'pause', sessionId: 's1' } }])
    // Response-driven stores: no manual list/detail refetch (SSE covers it).
    const list = controlled.runs.getSnapshot()
    if (list.status !== 'ready') throw new Error('expected ready')
    expect(list.runs[0]).toMatchObject({ id: 'RUN-1', status: 'paused' })
    const detail = controlled.runDetail.getSnapshot()
    if (detail.status !== 'ready') throw new Error('expected ready')
    expect(detail.data.run.status).toBe('paused')
    // The summary merge preserves the detail-only config.
    expect(detail.data.run.config).toEqual({ stopOnFailure: false, autoContinue: true, autoVerify: false, concurrency: 1 })
  })

  it('runControl retry/remove pass nodeId', async () => {
    await controller.runControl('RUN-1', 'retry', 'TICKET-0009', 's1')
    expect(calls[0]).toEqual({ endpoint: 'run-control', payload: { id: 'RUN-1', action: 'retry', nodeId: 'TICKET-0009', sessionId: 's1' } })
  })

  it('runConfirm sends the decision, applies the response, and reloads the open doc', async () => {
    await controller.select('TICKET-0002', 's1')
    await controller.openRun('RUN-1', 's1')
    calls = []
    await controller.runConfirm('RUN-1', 'TICKET-0002', 'confirm', 's1')
    expect(calls[0]).toEqual({
      endpoint: 'run-confirm',
      payload: { id: 'RUN-1', nodeId: 'TICKET-0002', decision: 'confirm', sessionId: 's1' },
    })
    // Response-driven stores: no manual list/detail refetch (SSE covers it).
    expect(calls.some(call => call.endpoint === 'run-show')).toBe(false)
    expect(calls.some(call => call.endpoint === 'run-list')).toBe(false)
    // The confirmed ticket's doc reloads (item done → ticket done).
    expect(calls.some(call => call.endpoint === 'get' && call.payload.id === 'TICKET-0002')).toBe(true)
  })

  it('runConfirm surfaces conflicts as an error notice', async () => {
    const failing = makeController({
      'run-confirm': { ok: false, error: { message: 'CONFLICT: item is done' } },
    })
    await failing.runConfirm('RUN-1', 'TICKET-0002', 'reject', 's1')
    expect(failing.notice.getSnapshot()).toMatchObject({ kind: 'error' })
  })
})

describe('joinRun flow (TICKET-0067)', () => {
  it('creates a run with default config when no active run exists', async () => {
    listValue = [run('RUN-OLD', 'completed')]
    await controller.joinRun('TICKET-0001', 's1')
    const create = calls.find(call => call.endpoint === 'run-create')
    expect(create).toBeDefined()
    expect(create?.payload).toMatchObject({ nodeIds: ['TICKET-0001'], sessionId: 's1' })
    expect(calls.some(call => call.endpoint === 'run-add')).toBe(false)
    expect(controller.runPicker.getSnapshot()).toBeUndefined()
    expect(controller.notice.getSnapshot()).toMatchObject({ kind: 'ok' })
  })

  it('joins the single active run directly (no picker)', async () => {
    listValue = [run('RUN-1', 'running'), run('RUN-2', 'completed')]
    await controller.joinRun('TICKET-0001', 's1')
    const add = calls.find(call => call.endpoint === 'run-add')
    expect(add?.payload).toEqual({ id: 'RUN-1', nodeIds: ['TICKET-0001'], sessionId: 's1' })
    expect(controller.runPicker.getSnapshot()).toBeUndefined()
  })

  it('offers only non-terminal runs in the picker when several are active', async () => {
    listValue = [run('RUN-1', 'running'), run('RUN-2', 'paused'), run('RUN-3', 'interrupted'), run('RUN-4', 'completed')]
    await controller.joinRun('TICKET-0001', 's1')
    const picker = controller.runPicker.getSnapshot()
    expect(picker?.nodeId).toBe('TICKET-0001')
    // interrupted is neither active nor history — it accepts no new members.
    expect(picker?.options.map(option => option.id)).toEqual(['RUN-1', 'RUN-2'])
    expect(calls.some(call => call.endpoint === 'run-add')).toBe(false)
  })

  it('pickRun adds to the chosen run, closes the picker, and reports counts', async () => {
    listValue = [run('RUN-1', 'running'), run('RUN-2', 'paused')]
    const rpc = {
      async call(_c: string, endpoint: string, payload: any): Promise<RpcResultLike> {
        calls.push({ endpoint, payload })
        if (endpoint === 'run-list') return { ok: true, value: { runs: listValue } }
        if (endpoint === 'run-add') {
          return {
            ok: true,
            value: {
              run: run(payload.id, 'running'),
              added: ['TICKET-0001', 'TICKET-0002'],
              addedRunning: ['TICKET-0002'],
              duplicates: ['TICKET-0003'],
              skippedDone: 2,
              skippedArchived: 1,
            },
          }
        }
        return { ok: true, value: {} }
      },
    }
    controller = new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })
    await controller.joinRun('TICKET-0001', 's1')
    expect(controller.runPicker.getSnapshot()).toBeDefined()
    await controller.pickRun('RUN-2', 's1')
    expect(calls.some(call => call.endpoint === 'run-add' && call.payload.id === 'RUN-2')).toBe(true)
    expect(controller.runPicker.getSnapshot()).toBeUndefined()
    const notice = controller.notice.getSnapshot()
    expect(notice).toMatchObject({ kind: 'ok', key: 'run.noticeAdded' })
    expect(notice?.params).toMatchObject({ added: 2, running: 1, duplicates: 1, skippedDone: 2, skippedArchived: 1 })
  })

  it('cancelRunPicker dismisses the picker without an RPC', async () => {
    listValue = [run('RUN-1', 'running'), run('RUN-2', 'paused')]
    await controller.joinRun('TICKET-0001', 's1')
    calls = []
    controller.cancelRunPicker()
    expect(controller.runPicker.getSnapshot()).toBeUndefined()
    expect(calls).toHaveLength(0)
  })

  it('reports 跨 home rejections as an error notice', async () => {
    listValue = [run('RUN-1', 'running')]
    const failing = makeController({
      'run-add': { ok: false, error: { message: 'INVALID_INPUT: 跨 home 加入被拒绝：TICKET-0099 属于 /other' } },
    })
    await failing.joinRun('TICKET-0099', 's1')
    const notice = failing.notice.getSnapshot()
    expect(notice).toMatchObject({ kind: 'error' })
    expect(notice?.text).toContain('跨 home')
  })
})

describe('SSE run hint (TICKET-0071)', () => {
  function stubEventSource(): { emit(data: unknown): void } {
    let handler: ((event: { data: string }) => void) | undefined
    class FakeEventSource {
      constructor(_url: string) {
        // eslint-disable-next-line @typescript-eslint/no-this-alias
        instances.push(this)
      }
      set onmessage(fn: ((event: { data: string }) => void) | undefined) {
        handler = fn
      }
    }
    const instances: FakeEventSource[] = []
    vi.stubGlobal('EventSource', FakeEventSource)
    return {
      emit(data: unknown) {
        handler?.({ data: JSON.stringify(data) })
      },
    }
  }

  it('a run hint refreshes the runs list and the matching open detail', async () => {
    vi.useFakeTimers()
    const source = stubEventSource()
    controller.connectEvents()
    await controller.openRun('RUN-1', 's1')
    calls = []
    source.emit({ version: 2, home: '/home', run: { id: 'RUN-1', kind: 'item', nodeId: 'TICKET-0002' } })
    vi.advanceTimersByTime(400)
    await vi.waitFor(() => {
      expect(calls.some(call => call.endpoint === 'run-list')).toBe(true)
      expect(calls.some(call => call.endpoint === 'run-show' && call.payload.id === 'RUN-1')).toBe(true)
    })
  })

  it('a hint for another run does not reload the open detail', async () => {
    vi.useFakeTimers()
    const source = stubEventSource()
    controller.connectEvents()
    await controller.openRun('RUN-1', 's1')
    calls = []
    source.emit({ version: 2, home: '/home', run: { id: 'RUN-OTHER', kind: 'run' } })
    vi.advanceTimersByTime(400)
    await Promise.resolve()
    await Promise.resolve()
    expect(calls.some(call => call.endpoint === 'run-list')).toBe(true)
    expect(calls.some(call => call.endpoint === 'run-show')).toBe(false)
  })
})

describe('run-view helpers (TICKET-0068)', () => {
  it('groupRuns keeps interrupted in the main list and folds terminal runs into history', () => {
    const runs = [
      run('RUN-1', 'running'),
      run('RUN-2', 'interrupted'),
      run('RUN-3', 'completed'),
      run('RUN-4', 'completed_with_failures'),
      run('RUN-5', 'canceled'),
      run('RUN-6', 'pending'),
    ]
    const grouped = groupRuns(runs)
    expect(grouped.main.map(r => r.id)).toEqual(['RUN-1', 'RUN-2', 'RUN-6'])
    expect(grouped.history.map(r => r.id)).toEqual(['RUN-3', 'RUN-4', 'RUN-5'])
  })

  it('runControlActions follow the run status', () => {
    expect(runControlActions(run('R', 'pending'))).toEqual(['start', 'cancel'])
    expect(runControlActions(run('R', 'running'))).toEqual(['pause', 'cancel'])
    expect(runControlActions(run('R', 'paused'))).toEqual(['resume', 'cancel'])
    expect(runControlActions(run('R', 'interrupted'))).toEqual(['resume', 'cancel'])
    expect(runControlActions(run('R', 'completed'))).toEqual([])
    expect(runControlActions(run('R', 'canceled'))).toEqual([])
  })

  it('canRetryItem allows failed/interrupted/stalled-pending only', () => {
    expect(canRetryItem({ state: 'failed' })).toBe(true)
    expect(canRetryItem({ state: 'interrupted' })).toBe(true)
    expect(canRetryItem({ state: 'pending', stalled: true })).toBe(true)
    expect(canRetryItem({ state: 'pending' })).toBe(false)
    expect(canRetryItem({ state: 'running' })).toBe(false)
    expect(canRetryItem({ state: 'done' })).toBe(false)
    expect(canRetryItem({ state: 'awaiting_confirmation' })).toBe(false)
  })

  it('canRemoveItem excludes in-flight items', () => {
    expect(canRemoveItem({ state: 'pending' })).toBe(true)
    expect(canRemoveItem({ state: 'failed' })).toBe(true)
    expect(canRemoveItem({ state: 'done' })).toBe(true)
    expect(canRemoveItem({ state: 'running' })).toBe(false)
    expect(canRemoveItem({ state: 'awaiting_confirmation' })).toBe(false)
  })
})
