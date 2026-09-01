/**
 * Tool-layer tests: registerOmtTools against a stub ctx, driving each tool's
 * execute directly. Covers schema-typed args reaching the runtime service,
 * render output for the model, and error surfacing (hierarchy violations,
 * missing change). U7a: runs against a REAL omt-daemon via the fixture.
 */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { OmtService } from '../src/host/service.ts'
import { registerOmtTools } from '../src/host/tools.ts'
import { renderToolText, stubToolCtx, type RegisteredTool } from './mocks/registered-tool.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

let fixture: RuntimeFixture
let service: OmtService
let tools = new Map<string, RegisteredTool>()

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'tools' })
  service = fixture.service
  tools = new Map()
  registerOmtTools(stubToolCtx(tools) as never, service)
})

afterEach(async () => {
  await fixture.stop()
})

function renderText(toolName: string, args: unknown, value: unknown): string {
  return renderToolText(tools, toolName, args, value)
}

describe('tool registration and node tools', () => {
  it('registers the omt_* node tools and the omt_run_* family', () => {
    expect([...tools.keys()].sort()).toEqual([
      'omt_create', 'omt_list', 'omt_move', 'omt_reindex',
      'omt_run_claim', 'omt_run_control', 'omt_run_create', 'omt_run_list', 'omt_run_report', 'omt_run_show',
      'omt_show', 'omt_update',
    ])
  })

  it('omt_create creates nodes and renders the node line', async () => {
    const create = tools.get('omt_create')!
    const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
    expect(epic.id).toBe('EPIC-0001')
    expect(renderText('omt_create', {}, epic)).toContain('EPIC-0001 [epic · open] 用户体系')

    const story = await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})
    expect(story.id).toBe('STORY-0001')

    // The file landed on disk (daemon-owned layout <home>/tickets/<path>).
    const file = await readFile(join(fixture.globalHome.path, story.path as string), 'utf8')
    expect(file).toContain('parent: EPIC-0001')
  })

  it('omt_create surfaces hierarchy violations as thrown errors', async () => {
    const create = tools.get('omt_create')!
    // Daemon problem text (same rule as pre-U7a): root requires an epic.
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

  it('omt_reindex reports counts (admin grant self-issued by the service)', async () => {
    const create = tools.get('omt_create')!
    const epic = await create.execute({ type: 'epic', title: '用户体系' }, {})
    await create.execute({ type: 'story', title: '登录', parentId: epic.id }, {})

    const result = await tools.get('omt_reindex')!.execute({}, {})
    expect(result).toEqual({ nodes: 2, edges: 1, skipped: 0 })
    expect(renderText('omt_reindex', {}, result)).toContain('2 个节点、1 条关系')
  })
})
