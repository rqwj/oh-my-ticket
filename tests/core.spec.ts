/**
 * OmtCore unit tests: dual-write consistency (SQLite ↔ Markdown), hierarchy
 * validation, update/move semantics, FTS search, and reindex recovery.
 * Each test runs against a fresh temporary OMT home.
 */
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { OmtError } from '../src/host/types.ts'

let home: string
let core: OmtCore

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-test-'))
  core = await OmtCore.open(home)
})

afterEach(async () => {
  core.close()
  await rm(home, { recursive: true, force: true })
})

/** Build the standard fixture: epic → story → ticket. */
async function standardFixture() {
  const epic = await core.create({ type: 'epic', title: '用户体系' })
  const story = await core.create({ type: 'story', title: '登录', parentId: epic.id })
  const ticket = await core.create({ type: 'ticket', title: '登录接口', parentId: story.id })
  return { epic, story, ticket }
}

describe('create', () => {
  it('creates an epic→story→ticket chain with dual-write consistency', async () => {
    const { epic, story, ticket } = await standardFixture()

    expect(epic.id).toBe('EPIC-0001')
    expect(story.id).toBe('STORY-0001')
    expect(ticket.id).toBe('TICKET-0001')

    // Markdown files exist at the laid-out paths with correct frontmatter.
    const ticketFile = await readFile(join(home, ticket.path), 'utf8')
    expect(ticketFile).toContain('id: TICKET-0001')
    expect(ticketFile).toContain('type: ticket')
    expect(ticketFile).toContain('parent: STORY-0001')
    expect(ticketFile).toContain('status: open')

    // The parent's managed children block lists the child with a relative link.
    const storyFile = await readFile(join(home, story.path), 'utf8')
    expect(storyFile).toContain('## 子节点')
    expect(storyFile).toContain('- [TICKET-0001 登录接口](TICKET-0001-登录接口/ticket.md) — open')

    // The tree assembles from the SQLite index.
    const forest = core.tree()
    expect(forest).toHaveLength(1)
    expect(forest[0]?.id).toBe('EPIC-0001')
    expect(forest[0]?.children[0]?.children[0]?.id).toBe('TICKET-0001')
  })

  it('supports the optional substory and subticket levels', async () => {
    const { story, ticket } = await standardFixture()
    const substory = await core.create({ type: 'substory', title: '第三方登录', parentId: story.id })
    const subTicket = await core.create({ type: 'subticket', title: '参数校验', parentId: ticket.id })
    const nestedTicket = await core.create({ type: 'ticket', title: '微信登录', parentId: substory.id })

    expect(substory.id).toBe('SUBSTORY-0001')
    expect(subTicket.id).toBe('SUBTICKET-0001')
    expect(nestedTicket.path).toContain('SUBSTORY-0001-第三方登录')
  })

  it('uses role-specific templates that encode hierarchy content boundaries', async () => {
    const epic = await core.create({ type: 'epic', title: '平台能力' })
    const story = await core.create({ type: 'story', title: '批量执行', parentId: epic.id })
    const substory = await core.create({ type: 'substory', title: '失败恢复', parentId: story.id })
    const ticket = await core.create({ type: 'ticket', title: '重试入口', parentId: story.id })
    const subticket = await core.create({ type: 'subticket', title: '错误提示', parentId: ticket.id })

    expect((await core.show(epic.id)).body).toMatch(/## 总体目标[\s\S]*## 范围[\s\S]*## 非范围[\s\S]*## 全局约束[\s\S]*## 成功标准/)
    for (const node of [story, substory]) {
      expect((await core.show(node.id)).body).toMatch(/## 能力结果[\s\S]*## 使用者或调用方[\s\S]*## 范围[\s\S]*## 非范围[\s\S]*## 共享规则与约束[\s\S]*## 验收标准/)
    }
    for (const node of [ticket, subticket]) {
      expect((await core.show(node.id)).body).toMatch(/## 交付结果[\s\S]*## 工作范围[\s\S]*## 依赖[\s\S]*## 验收标准[\s\S]*## 进度记录/)
    }
  })

  it('rejects hierarchy violations', async () => {
    const { epic, story, ticket } = await standardFixture()

    // Root creation is epic-only.
    await expect(core.create({ type: 'story', title: '孤儿' })).rejects.toThrow(OmtError)
    // epic only contains story; story contains substory|ticket; nothing contains epic.
    await expect(core.create({ type: 'ticket', title: '跨层', parentId: epic.id })).rejects.toThrow(/cannot contain/)
    await expect(core.create({ type: 'substory', title: '错层', parentId: ticket.id })).rejects.toThrow(/cannot contain/)
    await expect(core.create({ type: 'epic', title: '嵌套', parentId: story.id })).rejects.toThrow(/cannot contain/)
    await expect(core.create({ type: 'ticket', title: '无父', parentId: 'STORY-9999' })).rejects.toThrow(/unknown node/)
  })

  it('rejects an empty title', async () => {
    await expect(core.create({ type: 'epic', title: '  ' })).rejects.toThrow(/title/)
  })
})

describe('update', () => {
  it('updates status/title/priority across db, frontmatter, and parent listing', async () => {
    const { story, ticket } = await standardFixture()

    const updated = await core.update({ id: ticket.id, title: '登录接口 v2', status: 'in_progress', priority: 1 })
    expect(updated.status).toBe('in_progress')

    const file = await readFile(join(home, ticket.path), 'utf8')
    expect(file).toContain('title: 登录接口 v2')
    expect(file).toContain('status: in_progress')
    expect(file).toContain('priority: 1')

    const storyFile = await readFile(join(home, story.path), 'utf8')
    expect(storyFile).toContain('- [TICKET-0001 登录接口 v2]')
  })

  it('appends progress notes to the body and refreshes the FTS index', async () => {
    const { ticket } = await standardFixture()
    await core.update({ id: ticket.id, append: '- 2026-08-17 完成参数校验' })

    const shown = await core.show(ticket.id)
    expect(shown.body).toContain('完成参数校验')

    const hits = core.list({ query: '参数校验' })
    expect(hits.map(node => node.id)).toContain(ticket.id)
  })

  it('replaces the whole body when body is given', async () => {
    const { ticket } = await standardFixture()
    await core.update({ id: ticket.id, body: '## 全新正文\n' })
    const shown = await core.show(ticket.id)
    expect(shown.body).toContain('全新正文')
    expect(shown.body).not.toContain('进度记录')
  })
})

describe('ancestor activation', () => {
  it('activates the full ancestor chain when a ticket starts', async () => {
    const { epic, story, ticket } = await standardFixture()

    await core.update({ id: ticket.id, status: 'in_progress' })

    expect(core.getNode(epic.id)?.status).toBe('in_progress')
    expect(core.getNode(story.id)?.status).toBe('in_progress')
    const storyFile = await readFile(join(home, story.path), 'utf8')
    expect(storyFile).toContain('status: in_progress')
  })

  it('activates through the parent ticket for subtickets', async () => {
    const { story, ticket } = await standardFixture()
    const epic = core.getNode((await core.show(ticket.id)).parent!.id)!
    const subticket = await core.create({ type: 'subticket', title: '参数校验', parentId: ticket.id })

    await core.update({ id: subticket.id, status: 'in_progress' })

    for (const node of [epic, story, ticket]) {
      expect(core.getNode(node.id)?.status).toBe('in_progress')
    }
  })

  it('never reopens non-open ancestors and skips archived ones without failing', async () => {
    const { epic, story, ticket } = await standardFixture()
    await core.update({ id: story.id, status: 'blocked' })
    await core.update({ id: epic.id, archived: true })

    const updated = await core.update({ id: ticket.id, status: 'in_progress' })
    expect(updated.status).toBe('in_progress')

    expect(core.getNode(story.id)?.status).toBe('blocked')
    const archivedEpic = core.getNode(epic.id)!
    expect(archivedEpic.archived).toBe(true)
    expect(archivedEpic.status).toBe('open')
  })
})

describe('move', () => {
  it('moves a ticket to another story and syncs both parents', async () => {
    const { epic, ticket } = await standardFixture()
    const story2 = await core.create({ type: 'story', title: '注册', parentId: epic.id })
    const oldPath = ticket.path

    const moved = await core.move(ticket.id, story2.id)
    expect(moved.path).not.toBe(oldPath)
    expect(moved.path).toContain('STORY-0002-注册')

    // The file actually moved.
    const file = await readFile(join(home, moved.path), 'utf8')
    expect(file).toContain('parent: STORY-0002')

    // Both parents' children blocks reflect the move.
    const forest = core.tree()
    const stories = forest[0]?.children ?? []
    expect(stories.find(s => s.id === 'STORY-0001')?.children).toHaveLength(0)
    expect(stories.find(s => s.id === 'STORY-0002')?.children[0]?.id).toBe(ticket.id)

    const story1File = await readFile(join(home, 'tickets/EPIC-0001-用户体系/STORY-0001-登录/story.md'), 'utf8')
    expect(story1File).not.toContain('TICKET-0001 登录接口')
  })

  it('rejects moving a node under its own descendant', async () => {
    const { story, ticket } = await standardFixture()
    await expect(core.move(story.id, ticket.id)).rejects.toThrow(/descendant|cannot contain/)
  })
})

describe('reindex', () => {
  it('rebuilds the index from scratch after the database is lost', async () => {
    await standardFixture()
    core.close()
    await rm(join(home, 'omt.db'), { force: true })
    await rm(join(home, 'omt.db-wal'), { force: true })
    await rm(join(home, 'omt.db-shm'), { force: true })

    core = await OmtCore.open(home)
    const forest = core.tree()
    expect(forest).toHaveLength(1)
    expect(forest[0]?.children[0]?.children[0]?.id).toBe('TICKET-0001')

    // Counters survived the rebuild: the next epic continues the sequence.
    const epic2 = await core.create({ type: 'epic', title: '第二个' })
    expect(epic2.id).toBe('EPIC-0002')
  })

  it('picks up hand-edited frontmatter and drops illegal relations', async () => {
    const { story, ticket } = await standardFixture()

    // Hand-edit: mark the ticket done, and illegally re-parent the story
    // under its own ticket (must be dropped by reindex).
    const ticketPath = join(home, ticket.path)
    const ticketFile = await readFile(ticketPath, 'utf8')
    await writeFile(ticketPath, ticketFile.replace('status: open', 'status: done'))
    const storyPath = join(home, story.path)
    const storyFile = await readFile(storyPath, 'utf8')
    await writeFile(storyPath, storyFile.replace('parent: EPIC-0001', 'parent: TICKET-0001'))

    const result = await core.reindex()
    expect(result.skipped).toBeGreaterThanOrEqual(1)

    const shown = await core.show(ticket.id)
    expect(shown.node.status).toBe('done')

    // The illegal edge is gone: the story is back at root level.
    const forest = core.tree()
    expect(forest.map(node => node.id)).toContain('STORY-0001')
    expect(forest.map(node => node.id)).toContain('EPIC-0001')
  })

  it('is idempotent', async () => {
    await standardFixture()
    const first = await core.reindex()
    const second = await core.reindex()
    expect(second).toEqual(first)
    expect(core.tree()).toHaveLength(1)
  })
})

describe('search', () => {
  it('finds nodes by title and body keywords', async () => {
    const { ticket } = await standardFixture()
    await core.update({ id: ticket.id, append: '支持 OAuth 授权码模式' })

    expect(core.list({ query: '登录' }).map(n => n.id)).toContain('STORY-0001')
    expect(core.list({ query: 'OAuth' }).map(n => n.id)).toContain(ticket.id)
    expect(core.list({ query: '不存在的词' })).toHaveLength(0)
  })

  it('filters by type and status', async () => {
    const { ticket } = await standardFixture()
    await core.update({ id: ticket.id, status: 'done' })

    expect(core.list({ type: 'ticket' })).toHaveLength(1)
    expect(core.list({ status: 'done' }).map(n => n.id)).toEqual([ticket.id])
    expect(core.list({ status: 'open' }).map(n => n.id)).not.toContain(ticket.id)
  })
})
