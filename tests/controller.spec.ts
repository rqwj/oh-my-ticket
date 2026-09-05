// @vitest-environment jsdom
/**
 * Controller tests: drawer toggle + tree fetch, doc selection with details
 * shadow attach, mutations with refetch, and close/dispose flows — all
 * against stub RPC/layout doubles.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { OmtController } from '../src/client/controller.ts'
import type { RpcResultLike } from '../src/client/trigger/source.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

const TREE_OK: RpcResultLike = {
  ok: true,
  value: [{ id: 'EPIC-0001', type: 'epic', title: '用户体系', status: 'open', priority: 0, path: 'p', created_at: '', updated_at: '', children: [] }],
}

const GET_OK: RpcResultLike = {
  ok: true,
  value: {
    node: { id: 'TICKET-0001', type: 'ticket', title: '登录接口', status: 'open', priority: 0, path: 'p', created_at: '', updated_at: '', children: [] },
    children: [],
    body: '## 描述',
  },
}

let calls: { endpoint: string; payload: any }[]
let layoutCalls: string[]
let controller: OmtController
let shadowDispose: { called: boolean }
let shadowAttachCount: number

beforeEach(() => {
  calls = []
  layoutCalls = []
  shadowDispose = { called: false }
  shadowAttachCount = 0
  const rpc = {
    async call(_channel: string, endpoint: string, payload: any): Promise<RpcResultLike> {
      calls.push({ endpoint, payload })
      if (endpoint === 'tree') return TREE_OK
      if (endpoint === 'get') return GET_OK
      return { ok: true, value: {} }
    },
  }
  const layout = {
    openDetails: () => layoutCalls.push('openDetails'),
    closeDetails: () => layoutCalls.push('closeDetails'),
  }
  controller = new OmtController(rpc, layout)
  controller.attachDetailsShadow(() => {
    shadowAttachCount += 1
    return () => {
      shadowDispose.called = true
    }
  })
})

it('toggleDrawer opens the drawer and fetches the tree', async () => {
  expect(controller.drawerOpen.getSnapshot()).toBe(false)
  controller.toggleDrawer()
  expect(controller.drawerOpen.getSnapshot()).toBe(true)
  await Promise.resolve()
  await Promise.resolve()
  expect(calls.some(call => call.endpoint === 'tree')).toBe(true)
  expect(controller.tree.getSnapshot()).toMatchObject({ status: 'ready' })
})

it('select loads the doc, pins active, attaches the shadow, opens details', async () => {
  await controller.select('TICKET-0001')
  expect(controller.doc.getSnapshot()).toMatchObject({ status: 'ready' })
  expect(controller.active.getSnapshot()).toMatchObject({ id: 'TICKET-0001', title: '登录接口' })
  expect(shadowAttachCount).toBe(1)
  expect(layoutCalls).toEqual(['openDetails'])

  // A second select reuses the live shadow.
  await controller.select('TICKET-0001')
  expect(shadowAttachCount).toBe(1)
})

it('select forwards an explicit home scope for colliding ids', async () => {
  await controller.select('TICKET-0001', 's1', 'global')
  expect(calls.find(call => call.endpoint === 'get')?.payload).toEqual({ sessionId: 's1', id: 'TICKET-0001', scope: 'global' })
})

it('SSE refresh preserves the selected home and subsequent action scope for colliding ids', async () => {
  vi.useFakeTimers()
  let events!: { onmessage?: (message: { data: string }) => void }
  vi.stubGlobal('EventSource', class {
    constructor() { events = this }
    onmessage?: (message: { data: string }) => void
  })
  try {
    const scoped = new OmtController({
      async call(_channel: string, endpoint: string, payload: any): Promise<RpcResultLike> {
        calls.push({ endpoint, payload })
        if (endpoint === 'get') {
          const scope = payload.scope ?? 'workspace'
          return { ok: true, value: { ...(GET_OK.value as any), scope, body: `${scope} body` } }
        }
        return endpoint === 'tree' ? TREE_OK : { ok: true, value: [] }
      },
    }, { openDetails: () => {}, closeDetails: () => {} })
    scoped.noteSession('s1')
    await scoped.select('TICKET-0001', 's1', 'global')
    scoped.connectEvents()
    calls = []
    events.onmessage!({ data: '{}' })
    await vi.advanceTimersByTimeAsync(300)
    expect(calls.find(call => call.endpoint === 'get')?.payload).toEqual({ id: 'TICKET-0001', sessionId: 's1', scope: 'global' })
    expect(scoped.doc.getSnapshot()).toMatchObject({ status: 'ready', data: { scope: 'global', body: 'global body' } })
    const active = scoped.active.getSnapshot()!
    await scoped.setStatus(active.id, 'done', 's1', active.scope)
    expect(calls.find(call => call.endpoint === 'update')?.payload.scope).toBe('global')
    expect(scoped.relatedOf('s1')).toEqual([])
  } finally {
    vi.unstubAllGlobals()
    vi.useRealTimers()
  }
})

it('keeps the latest selection when an older request resolves last', async () => {
  const pending = new Map<string, (result: RpcResultLike) => void>()
  const latest = new OmtController({
    async call(_channel: string, _endpoint: string, payload: { id: string }): Promise<RpcResultLike> {
      return await new Promise(resolve => { pending.set(payload.id, resolve) })
    },
  }, { openDetails: () => {}, closeDetails: () => {} })
  const first = latest.select('TICKET-0001')
  const second = latest.select('TICKET-0002')
  pending.get('TICKET-0002')!({
    ok: true,
    value: { ...(GET_OK.value as object), node: { ...(GET_OK.value as any).node, id: 'TICKET-0002', title: '新选择' } },
  })
  await second
  pending.get('TICKET-0001')!(GET_OK)
  await first
  expect(latest.doc.getSnapshot()).toMatchObject({ status: 'ready', data: { node: { id: 'TICKET-0002' } } })
})

it('closeDoc disposes the shadow, resets the doc, and closes the details column', async () => {
  await controller.select('TICKET-0001')
  controller.closeDoc()
  expect(shadowDispose.called).toBe(true)
  expect(controller.doc.getSnapshot()).toEqual({ status: 'idle' })
  expect(layoutCalls).toContain('closeDetails')
})

it('select surfaces RPC failures as doc errors without shadowing', async () => {
  const rpc = {
    async call(): Promise<RpcResultLike> {
      return { ok: false, error: { message: 'NOT_FOUND' } }
    },
  }
  const failing = new OmtController(rpc, { openDetails: () => {}, closeDetails: () => {} })
  failing.attachDetailsShadow(() => {
    shadowAttachCount += 1
    return () => {}
  })
  await failing.select('NOPE-0001')
  expect(failing.doc.getSnapshot()).toMatchObject({ status: 'error', id: 'NOPE-0001' })
  expect(shadowAttachCount).toBe(0)
})

it('setStatus updates then refreshes tree and doc', async () => {
  await controller.select('TICKET-0001')
  calls = []
  await controller.setStatus('TICKET-0001', 'in_progress')
  expect(calls[0]).toEqual({ endpoint: 'update', payload: { id: 'TICKET-0001', status: 'in_progress' } })
  expect(calls.some(call => call.endpoint === 'tree')).toBe(true)
  expect(calls.some(call => call.endpoint === 'get')).toBe(true)
})

it('saveBody qualifies the home and sends the optimistic revision', async () => {
  await controller.saveBody('TICKET-0001', '正文', 7, 's1', 'global')
  expect(calls.find(call => call.endpoint === 'update')?.payload).toEqual({
    sessionId: 's1', id: 'TICKET-0001', body: '正文', expectedRevision: 7, scope: 'global',
  })
})

it.each(['priority', 'status'] as const)('defers %s doc refresh while editing but refreshes after successful body save', async field => {
  await controller.select('TICKET-0001', 's1')
  controller.setBodyEditing(true)
  calls = []
  if (field === 'priority') await controller.setPriority('TICKET-0001', 2, 's1')
  else await controller.setStatus('TICKET-0001', 'done', 's1')
  expect(calls.map(call => call.endpoint)).toEqual(expect.arrayContaining(['update', 'tree']))
  expect(calls.some(call => call.endpoint === 'get')).toBe(false)
  calls = []
  await controller.saveBody('TICKET-0001', 'saved draft', 1, 's1')
  expect(calls.map(call => call.endpoint)).toContain('get')
  expect(controller.doc.getSnapshot()).toMatchObject({ status: 'ready' })
})

it.each(['envelope', 'transport'] as const)('does not refresh the editing document after a %s body save failure', async kind => {
  const failing = new OmtController({
    async call(_channel: string, endpoint: string): Promise<RpcResultLike> {
      calls.push({ endpoint, payload: {} })
      if (endpoint === 'get') return GET_OK
      if (kind === 'transport') throw new Error('connection lost')
      return { ok: false, error: { message: 'revision conflict' } }
    },
  }, { openDetails: () => {}, closeDetails: () => {} })
  await failing.select('TICKET-0001')
  failing.setBodyEditing(true)
  calls = []
  await expect(failing.saveBody('TICKET-0001', 'draft', 1)).rejects.toThrow(kind === 'transport' ? 'connection lost' : 'revision conflict')
  expect(calls.map(call => call.endpoint)).toEqual(['update'])
  expect(failing.doc.getSnapshot()).toMatchObject({ status: 'ready', data: { body: '## 描述' } })
})

describe('createNode failures', () => {
  it.each(['envelope', 'transport'] as const)('rejects %s failures so the form can display them', async kind => {
    const failing = new OmtController({
      async call(): Promise<RpcResultLike> {
        if (kind === 'transport') throw new Error('connection lost')
        return { ok: false, error: { message: 'creation rejected' } }
      },
    }, { openDetails: () => {}, closeDetails: () => {} })
    await expect(failing.createNode({ type: 'epic', title: 'draft', scope: 'global' }))
      .rejects.toThrow(kind === 'transport' ? 'connection lost' : 'creation rejected')
  })

  it('returns the created id on success', async () => {
    const creating = new OmtController({
      async call(): Promise<RpcResultLike> { return { ok: true, value: { id: 'EPIC-0099' } } },
    }, { openDetails: () => {}, closeDetails: () => {} })
    await expect(creating.createNode({ type: 'epic', title: 'draft', scope: 'global' })).resolves.toBe('EPIC-0099')
  })
})

it('appendNote ignores blank input', async () => {
  await controller.appendNote('TICKET-0001', '   ')
  expect(calls.filter(call => call.endpoint === 'update')).toHaveLength(0)
})

it('setDrawerWidth clamps to 240–480', () => {
  controller.setDrawerWidth(100)
  expect(controller.drawerWidth.getSnapshot()).toBe(240)
  controller.setDrawerWidth(999)
  expect(controller.drawerWidth.getSnapshot()).toBe(480)
  controller.setDrawerWidth(333.6)
  expect(controller.drawerWidth.getSnapshot()).toBe(334)
})

it('toggleCollapsed flips and keeps entries per node', () => {
  controller.toggleCollapsed('EPIC-0001')
  expect(controller.collapsed.getSnapshot()).toEqual({ 'EPIC-0001': true })
  controller.toggleCollapsed('STORY-0001')
  controller.toggleCollapsed('EPIC-0001')
  expect(controller.collapsed.getSnapshot()).toEqual({ 'EPIC-0001': false, 'STORY-0001': true })
})

it('setPanelMode swaps the overlay presentation without touching the open fact', () => {
  expect(controller.panelMode.getSnapshot()).toBe('drawer')
  controller.toggleDrawer()
  controller.setPanelMode('float')
  expect(controller.panelMode.getSnapshot()).toBe('float')
  expect(controller.drawerOpen.getSnapshot()).toBe(true)
  controller.setPanelMode('drawer')
  expect(controller.panelMode.getSnapshot()).toBe('drawer')
})

it('openPanel opens a closed panel and is a no-op on an open one', async () => {
  expect(controller.drawerOpen.getSnapshot()).toBe(false)
  controller.openPanel()
  expect(controller.drawerOpen.getSnapshot()).toBe(true)
  await Promise.resolve()
  await Promise.resolve()
  const treeCalls = calls.filter(call => call.endpoint === 'tree').length
  expect(treeCalls).toBe(1)

  // Already open: no re-toggle, no extra fetch.
  controller.openPanel()
  expect(controller.drawerOpen.getSnapshot()).toBe(true)
  expect(calls.filter(call => call.endpoint === 'tree')).toHaveLength(treeCalls)
})

it('setFloatPos / setFloatSize round and store raw (shell clamps per viewport)', () => {
  controller.setFloatPos({ x: 96.4, y: 48.6 })
  expect(controller.floatPos.getSnapshot()).toEqual({ x: 96, y: 49 })
  controller.setFloatSize({ w: 380.5, h: 520.4 })
  expect(controller.floatSize.getSnapshot()).toEqual({ w: 381, h: 520 })
})

it('view tab hides while the float window is active and returns after (TICKET-0040)', () => {
  let registered = 0
  let disposed = 0
  controller.attachViewTab(() => {
    registered += 1
    return () => { disposed += 1 }
  })
  // Registered at attach (panel closed).
  expect(registered).toBe(1)
  expect(disposed).toBe(0)

  // Drawer open: tab stays.
  controller.toggleDrawer()
  expect(registered).toBe(1)
  expect(disposed).toBe(0)

  // Float active: tab disposed — the shell falls back to Chat.
  controller.setPanelMode('float')
  expect(registered).toBe(1)
  expect(disposed).toBe(1)

  // Idempotent: staying in float mode does not dispose again.
  controller.setPanelMode('float')
  expect(disposed).toBe(1)

  // Back to drawer: tab re-registered.
  controller.setPanelMode('drawer')
  expect(registered).toBe(2)
  expect(disposed).toBe(1)

  // Float again, then close the panel: tab returns.
  controller.setPanelMode('float')
  expect(disposed).toBe(2)
  controller.toggleDrawer() // closes the panel
  expect(registered).toBe(3)
})

it('forget unpins active, drops related entries, and closes the doc', async () => {
  await controller.select('TICKET-0001')
  controller.noteRelated('s1', [{ id: 'TICKET-0001', type: 'ticket', title: 't', status: 'open', archived: false, priority: 0 }])
  expect(controller.relatedOf('s1')).toHaveLength(1)

  controller.forget('TICKET-0001', 's1')
  expect(controller.active.getSnapshot()).toBeUndefined()
  expect(controller.relatedOf('s1')).toHaveLength(0)
  expect(controller.doc.getSnapshot()).toEqual({ status: 'idle' })
  expect(shadowDispose.called).toBe(true)
})

it('tool-row clicks yield the doc WITHOUT closing the details column', async () => {
  await controller.select('TICKET-0001')
  expect(shadowAttachCount).toBe(1)

  // Simulate a capture-phase click on a tool-call row.
  const row = document.createElement('div')
  row.setAttribute('data-chat-call-id', 'call-1')
  document.body.appendChild(row)
  row.dispatchEvent(new MouseEvent('click', { bubbles: true }))

  expect(shadowDispose.called).toBe(true)
  expect(controller.doc.getSnapshot()).toEqual({ status: 'idle' })
  expect(layoutCalls).not.toContain('closeDetails') // column stays open
  row.remove()
})

it('clearActive resets active and disposes the doc', async () => {
  await controller.select('TICKET-0001')
  controller.clearActive()
  expect(controller.active.getSnapshot()).toBeUndefined()
  expect(shadowDispose.called).toBe(true)
})
