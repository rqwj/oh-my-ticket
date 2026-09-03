/**
 * Scope routing tests: omt_create target resolution — explicit scope param,
 * UI-backed user question, fallback when no answerer, parent-home ownership
 * for children, and cross-home move rejection. U7a: one real daemon opens
 * BOTH homes (workspace fixture).
 *
 * U7a note on bare ids: the old pool synchronized id counters ACROSS homes,
 * so ids were globally unique; the daemon allocates per home. Fixtures
 * offset one home's counter (filler nodes) wherever a test needs to hold
 * bare ids from two homes at once.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { HomeRef, OmtService } from '../src/host/service.ts'
import { registerOmtTools } from '../src/host/tools.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

let fixture: RuntimeFixture
let service: OmtService
let globalHome: HomeRef
let workspaceHome: HomeRef
const workspaceCwd = () => fixture.root + '/workspace'
let tools: Map<string, { execute: (args: any, exec?: any) => Promise<any> }>
let askBehavior: (() => Promise<any>) | undefined

const execAt = (cwd?: string) => ({
  agent: cwd === undefined ? undefined : { session: { header: { cwd } } },
})

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'scope', workspace: true })
  service = fixture.service
  globalHome = fixture.globalHome
  workspaceHome = fixture.workspaceHome!
  tools = new Map()
  askBehavior = undefined
  const stubCtx = {
    tools: { register(def: any) { tools.set(def.name, def) } },
    userQuestions: {
      ask: () => (askBehavior ?? (() => Promise.reject(new Error('no answerer'))))(),
    },
  }
  registerOmtTools(stubCtx as never, service)
})

afterEach(async () => {
  await fixture.stop()
})

const create = (args: any, exec?: any) => tools.get('omt_create')!.execute(args, exec)

/** Node ids of one home's forest (root level), via the service. */
async function rootIds(home: HomeRef): Promise<string[]> {
  return (await service.tree(home)).map(node => node.id)
}

it('explicit scope=workspace creates the local home and lands there', async () => {
  const epic = await create({ type: 'epic', title: '本地项目', scope: 'workspace' }, execAt(workspaceCwd()))
  expect(epic.path).toContain('tickets')
  expect(await rootIds(workspaceHome)).toEqual(['EPIC-0001'])
  expect(await rootIds(globalHome)).toEqual([])
})

it('explicit scope=global bypasses an existing local home', async () => {
  await create({ type: 'epic', title: '全局项目', scope: 'global' }, execAt(workspaceCwd()))
  expect(await rootIds(globalHome)).toEqual(['EPIC-0001'])
})

it('asks the user when scope is omitted (workspace answer)', async () => {
  askBehavior = () => Promise.resolve({ answers: [{ id: 'scope', selected: ['当前工作区（随项目存储）'] }] })
  await create({ type: 'epic', title: '问一下' }, execAt(workspaceCwd()))
  expect(await rootIds(workspaceHome)).toHaveLength(1)
})

it('asks the user when scope is omitted (global answer)', async () => {
  // Local home exists (fixture opened it); user still picks global.
  askBehavior = () => Promise.resolve({ answers: [{ id: 'scope', selected: ['全局（所有项目共享）'] }] })
  await create({ type: 'epic', title: '问一下' }, execAt(workspaceCwd()))
  expect(await rootIds(globalHome)).toHaveLength(1)
  expect(await rootIds(workspaceHome)).toHaveLength(0)
})

it('refuses to guess a root Epic scope when the answerer is unavailable', async () => {
  await expect(create({ type: 'epic', title: '不自动路由' }, execAt(workspaceCwd())))
    .rejects.toThrow('scope selection')
  expect(await rootIds(workspaceHome)).toEqual([])
  expect(await rootIds(globalHome)).toEqual([])
})

it('children always land in the parent\'s home, regardless of cwd', async () => {
  const epic = await create({ type: 'epic', title: '全局项目', scope: 'global' }, execAt(workspaceCwd()))
  const story = await create({ type: 'story', title: '子节点', parentId: epic.id }, execAt(workspaceCwd()))
  expect(story.id).toBe('STORY-0001')
  // Child went to the global home (parent's), not the local one.
  const globalTree = await service.tree(globalHome)
  expect(globalTree[0]?.children).toHaveLength(1)
  expect(await rootIds(workspaceHome)).toEqual([])
})

// REWRITTEN for U7a: filler epic keeps the two homes' bare ids distinct
// (the daemon allocates ids per home; the old pool counter-synced them).
it('show/update resolve nodes across homes by ownership', async () => {
  const filler = await create({ type: 'epic', title: '占位', scope: 'global' }, execAt(workspaceCwd()))
  void filler // consumes EPIC-0001 in the global home
  const localEpic = await create({ type: 'epic', title: '本地', scope: 'workspace' }, execAt(workspaceCwd()))
  const globalEpic = await create({ type: 'epic', title: '全局', scope: 'global' }, execAt(workspaceCwd()))
  expect(localEpic.id).toBe('EPIC-0001')
  expect(globalEpic.id).toBe('EPIC-0002')

  const show = tools.get('omt_show')!
  expect((await show.execute({ id: localEpic.id }, execAt(workspaceCwd()))).node.title).toBe('本地')
  expect((await show.execute({ id: globalEpic.id }, execAt(workspaceCwd()))).node.title).toBe('全局')

  const update = tools.get('omt_update')!
  await update.execute({ id: globalEpic.id, status: 'done' }, execAt(workspaceCwd()))
  expect((await show.execute({ id: globalEpic.id }, execAt(workspaceCwd()))).node.status).toBe('done')
})

// REWRITTEN for U7a: the daemon allocates ids PER HOME (the old pool
// counter-synced them globally), so a same-type cross-home pair is
// unreachable via bare ids — the node's home always contains a same-named
// candidate. The fixture therefore splits the pair across TYPES: the moved
// SUBTICKET only exists in the global home, while the target TICKET id
// exists only in the workspace home.
it('rejects cross-home moves', async () => {
  // Global side first (workspace still empty, so every parent probe with
  // cwd=workspace falls through to the global home).
  const gEpic = await service.createNode(globalHome, { type: 'epic', title: '全局' })
  const gStory = await service.createNode(globalHome, { type: 'story', title: 'GS', parentId: gEpic.id })
  const gTicket = await service.createNode(globalHome, { type: 'ticket', title: 'GT', parentId: gStory.id })
  const subticket = await service.createNode(globalHome, { type: 'subticket', title: '移动我', parentId: gTicket.id })
  expect(subticket.id).toBe('SUBTICKET-0001')

  // Workspace side: its own tree plus the target ticket (TICKET-0002 —
  // absent from the global home, which holds only TICKET-0001).
  const wsExec = execAt(workspaceCwd())
  await create({ type: 'epic', title: '本地', scope: 'workspace' }, wsExec)
  const wsStory = await create({ type: 'story', title: 'LS', parentId: 'EPIC-0001' }, wsExec)
  await create({ type: 'ticket', title: '占位', parentId: wsStory.id }, wsExec)
  const targetTicket = await create({ type: 'ticket', title: '目标父', parentId: wsStory.id }, wsExec)
  expect(targetTicket.id).toBe('TICKET-0002')

  await expect(tools.get('omt_move')!.execute({ id: subticket.id, newParentId: targetTicket.id }, wsExec))
    .rejects.toThrow(/跨 home/)
})
