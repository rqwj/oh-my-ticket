// @vitest-environment jsdom
/**
 * Controller tests: drawer toggle + tree fetch, doc selection with details
 * shadow attach, mutations with refetch, and close/dispose flows — all
 * against stub RPC/layout doubles.
 */
import { beforeEach, describe, expect, it } from 'vitest'
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
