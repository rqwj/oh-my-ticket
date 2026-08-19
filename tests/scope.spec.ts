/**
 * Scope routing tests: omt_create target resolution — explicit scope param,
 * UI-backed user question, fallback when no answerer, parent-home ownership
 * for children, and cross-home move rejection.
 */
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCorePool } from '../src/host/pool.ts'
import { registerOmtTools } from '../src/host/tools.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

let root: string
let globalHome: string
let workspace: string
let pool: OmtCorePool
let tools: Map<string, { execute: (args: any, exec?: any) => Promise<any> }>
let askBehavior: (() => Promise<any>) | undefined

const execAt = (cwd?: string) => ({
  agent: cwd === undefined ? undefined : { session: { header: { cwd } } },
})

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'omt-scope-test-'))
  globalHome = join(root, 'global')
  workspace = join(root, 'workspace')
  await mkdir(workspace, { recursive: true })
  pool = new OmtCorePool(globalHome)
  tools = new Map()
  askBehavior = undefined
  const stubCtx = {
    tools: { register(def: any) { tools.set(def.name, def) } },
    userQuestions: {
      ask: () => (askBehavior ?? (() => Promise.reject(new Error('no answerer'))))(),
    },
  }
  registerOmtTools(stubCtx as never, pool)
})

afterEach(async () => {
  await pool.closeAll()
  await rm(root, { recursive: true, force: true })
})

const create = (args: any, exec?: any) => tools.get('omt_create')!.execute(args, exec)

it('explicit scope=workspace creates the local home and lands there', async () => {
  const epic = await create({ type: 'epic', title: '本地项目', scope: 'workspace' }, execAt(workspace))
  expect(pool.homeFor(workspace)).toBe(join(workspace, '.omt'))
  expect(epic.path).toContain('tickets')
  const local = await pool.coreFor(workspace)
  expect(local.tree().map(n => n.id)).toEqual(['EPIC-0001'])
  const global = await pool.coreFor(undefined)
  expect(global.tree()).toHaveLength(0)
})

it('explicit scope=global bypasses an existing local home', async () => {
  await mkdir(join(workspace, '.omt'))
  await create({ type: 'epic', title: '全局项目', scope: 'global' }, execAt(workspace))
  const global = await pool.coreFor(undefined)
  expect(global.tree().map(n => n.id)).toEqual(['EPIC-0001'])
})

it('asks the user when scope is omitted (workspace answer)', async () => {
  askBehavior = () => Promise.resolve({ answers: [{ id: 'scope', selected: ['当前工作区（随项目存储）'] }] })
  await create({ type: 'epic', title: '问一下' }, execAt(workspace))
  expect((await pool.coreFor(workspace)).tree()).toHaveLength(1)
})

it('asks the user when scope is omitted (global answer)', async () => {
  await mkdir(join(workspace, '.omt'))  // local home exists; user still picks global
  askBehavior = () => Promise.resolve({ answers: [{ id: 'scope', selected: ['全局（所有项目共享）'] }] })
  await create({ type: 'epic', title: '问一下' }, execAt(workspace))
  expect((await pool.coreFor(undefined)).tree()).toHaveLength(1)
  expect((await pool.coreFor(workspace)).tree()).toHaveLength(0)
})

it('falls back to the automatic rule when no answerer is available', async () => {
  await mkdir(join(workspace, '.omt'))
  // askBehavior unset → ask rejects → automatic rule (local home wins)
  await create({ type: 'epic', title: '自动路由' }, execAt(workspace))
  expect((await pool.coreFor(workspace)).tree()).toHaveLength(1)
})

it('children always land in the parent\'s home, regardless of cwd', async () => {
  const epic = await create({ type: 'epic', title: '全局项目', scope: 'global' }, execAt(workspace))
  await mkdir(join(workspace, '.omt'))  // workspace later gains a local home
  const story = await create({ type: 'story', title: '子节点', parentId: epic.id }, execAt(workspace))
  expect(story.id).toBe('STORY-0001')
  // Child went to the global home (parent's), not the local one.
  expect((await pool.coreFor(undefined)).tree()[0]?.children).toHaveLength(1)
  expect((await pool.coreFor(workspace)).tree()).toHaveLength(0)
})

it('show/update resolve nodes across homes by ownership', async () => {
  await mkdir(join(workspace, '.omt'))
  const localEpic = await create({ type: 'epic', title: '本地', scope: 'workspace' }, execAt(workspace))
  const globalEpic = await create({ type: 'epic', title: '全局', scope: 'global' }, execAt(workspace))

  const show = tools.get('omt_show')!
  expect((await show.execute({ id: localEpic.id }, execAt(workspace))).node.title).toBe('本地')
  expect((await show.execute({ id: globalEpic.id }, execAt(workspace))).node.title).toBe('全局')

  const update = tools.get('omt_update')!
  await update.execute({ id: globalEpic.id, status: 'done' }, execAt(workspace))
  expect((await show.execute({ id: globalEpic.id }, execAt(workspace))).node.status).toBe('done')
})

it('rejects cross-home moves', async () => {
  const globalEpic = await create({ type: 'epic', title: '全局', scope: 'global' }, execAt(workspace))
  const localEpic = await create({ type: 'epic', title: '本地', scope: 'workspace' }, execAt(workspace))
  const story = await create({ type: 'story', title: 'S', parentId: globalEpic.id }, execAt(workspace))
  await expect(tools.get('omt_move')!.execute({ id: story.id, newParentId: localEpic.id }, execAt(workspace)))
    .rejects.toThrow(/跨 home/)
})
