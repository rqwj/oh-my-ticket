/**
 * Tool-layer tests: registerOmtTools against a stub ctx, driving each tool's
 * execute directly. Covers schema-typed args reaching OmtCore, render output
 * for the model, and error surfacing (hierarchy violations, missing change).
 */
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import { registerOmtTools } from '../src/host/tools.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

interface RegisteredTool {
  name: string
  execute: (args: any, exec?: any) => Promise<any>
  output: { render: (args: any, value: any) => { type: string; text?: string }[] }
}

let home: string
let core: OmtCore
let tools: Map<string, RegisteredTool>

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-tools-test-'))
  core = await OmtCore.open(home)
  tools = new Map()
  const stubCtx = {
    tools: {
      register(def: RegisteredTool) {
        tools.set(def.name, def)
      },
    },
  }
  registerOmtTools(stubCtx as never, new OmtCorePool(home))
})

afterEach(async () => {
  core.close()
  await rm(home, { recursive: true, force: true })
})

function renderText(toolName: string, args: unknown, value: unknown): string {
  const tool = tools.get(toolName)
  expect(tool).toBeDefined()
  return tool!.output.render(args, value).map(block => block.text ?? '').join('\n')
}

it('registers the six omt_* tools', () => {
  expect([...tools.keys()].sort()).toEqual([
    'omt_create', 'omt_list', 'omt_move', 'omt_reindex', 'omt_show', 'omt_update',
  ])
})

it('omt_create creates nodes and renders the node line', async () => {
  const create = tools.get('omt_create')!
  const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
  expect(epic.id).toBe('EPIC-0001')
  expect(renderText('omt_create', {}, epic)).toContain('EPIC-0001 [epic · open] 用户体系')

  const story = await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})
  expect(story.id).toBe('STORY-0001')

  // The file landed on disk.
  const file = await readFile(join(home, story.path), 'utf8')
  expect(file).toContain('parent: EPIC-0001')
})

it('omt_create surfaces hierarchy violations as thrown errors', async () => {
  const create = tools.get('omt_create')!
  await expect(create.execute({ type: 'story', title: '孤儿' }, {})).rejects.toThrow(/requires a parent/)
})

it('omt_list filters and searches', async () => {
  const create = tools.get('omt_create')!
  const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
  await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})

  const list = tools.get('omt_list')!
  expect(await list.execute({ type: 'story' }, {})).toHaveLength(1)
  expect(await list.execute({ query: '登录' }, {})).toHaveLength(1)
  expect(renderText('omt_list', {}, await list.execute({}, {}))).toContain('共 2 个节点')
})

it('omt_show returns structured detail and renders markdown', async () => {
  const create = tools.get('omt_create')!
  const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
  const story = await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})

  const show = tools.get('omt_show')!
  const detail = await show.execute({ id: story.id }, {})
  expect(detail.parent.id).toBe('EPIC-0001')
  expect(detail.children).toHaveLength(0)

  const text = renderText('omt_show', {}, detail)
  expect(text).toContain('# STORY-0001 登录')
  expect(text).toContain('父节点: EPIC-0001 用户体系')
})

it('omt_update requires at least one change and appends progress', async () => {
  const create = tools.get('omt_create')!
  const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})

  const update = tools.get('omt_update')!
  await expect(update.execute({ id: epic.id }, {})).rejects.toThrow(/至少需要一项变更/)

  const updated = await update.execute({ id: epic.id, status: 'in_progress', append: '- 已立项' }, {})
  expect(updated.status).toBe('in_progress')
  const detail = await tools.get('omt_show')!.execute({ id: epic.id }, {})
  expect(detail.body).toContain('已立项')
})

it('omt_move relocates a subtree', async () => {
  const create = tools.get('omt_create')!
  const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
  const s1 = await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})
  const s2 = await create.execute({ type: 'story', title: '注册', parentId: epic.id }, {})
  const ticket = await create.execute({ type: 'ticket', title: '接口', parentId: s1.id }, {})

  const moved = await tools.get('omt_move')!.execute({ id: ticket.id, newParentId: s2.id }, {})
  expect(moved.path).toContain('STORY-0002-注册')
})

it('omt_reindex reports counts', async () => {
  const create = tools.get('omt_create')!
  const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
  await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})

  const result = await tools.get('omt_reindex')!.execute({}, {})
  expect(result).toEqual({ nodes: 2, edges: 1, skipped: 0 })
  expect(renderText('omt_reindex', {}, result)).toContain('2 个节点、1 条关系')
})
