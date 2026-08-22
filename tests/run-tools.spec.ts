/**
 * Run tool-family tests (EPIC-0003 / STORY-0011): the six omt_run_* tools,
 * the node status extension (blocked/skipped, TICKET-0060), atomic claim
 * (TICKET-0058), explicit report vocabulary (TICKET-0059), and passive
 * observation of node status transitions into run items (TICKET-0061).
 */
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCorePool } from '../src/host/pool.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'
import { RunningRegistry } from '../src/host/running.ts'
import { registerOmtTools } from '../src/host/tools.ts'
import { NUDGE_BUDGET } from '../src/host/types.ts'
import { ticketFixtureViaTools } from './mocks/fixtures.ts'
import { renderToolText, stubToolCtx, toolOf, type RegisteredTool } from './mocks/registered-tool.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

let home: string
let pool: OmtCorePool
let running: RunningRegistry
let tools: Map<string, RegisteredTool>

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-run-tools-'))
  pool = new OmtCorePool(home)
  running = new RunningRegistry()
  tools = new Map()
  registerOmtTools(stubToolCtx(tools) as never, pool, undefined, undefined, running)
})

afterEach(async () => {
  await pool.closeAll()
  await rm(home, { recursive: true, force: true })
})

function tool(name: string): RegisteredTool {
  return toolOf(tools, name)
}

/** Agent-less exec (no session) — like the existing tool tests. */
const NO_EXEC: any = {}

/** Exec carrying a live agent session. */
function agentExec(sessionId: string, cwd?: string): any {
  return { agent: { session: { header: { id: sessionId, cwd } } } }
}

function renderText(toolName: string, args: unknown, value: unknown): string {
  return renderToolText(tools, toolName, args, value)
}

/** epic → story → n tickets, created through the tool surface (global home). */
function ticketFixture(count: number): Promise<string[]> {
  return ticketFixtureViaTools(tools, count)
}

/** A started run over the given tickets. */
async function startedRun(ids: string[], config?: Record<string, unknown>): Promise<any> {
  const created = await tool('omt_run_create').execute({ nodeIds: ids, ...(config !== undefined ? { config } : {}) }, NO_EXEC)
  await tool('omt_run_control').execute({ id: created.run.id, action: 'start' }, NO_EXEC)
  return created.run
}

// ── TICKET-0060: node status extension blocked/skipped ──────────────────

describe('TICKET-0060 node status extension', () => {
  it('omt_update accepts blocked and skipped', async () => {
    const [ticket] = await ticketFixture(1)
    const blocked = await tool('omt_update').execute({ id: ticket, status: 'blocked' }, NO_EXEC)
    expect(blocked.status).toBe('blocked')
    const skipped = await tool('omt_update').execute({ id: ticket, status: 'skipped' }, NO_EXEC)
    expect(skipped.status).toBe('skipped')
  })

  it('still rejects illegal statuses (stopped stays out)', async () => {
    const [ticket] = await ticketFixture(1)
    await expect(tool('omt_update').execute({ id: ticket, status: 'stopped' }, NO_EXEC)).rejects.toThrow(/must be one of|unknown status/)
    await expect(tool('omt_update').execute({ id: ticket, status: 'failed' }, NO_EXEC)).rejects.toThrow(/must be one of|unknown status/)
  })

  it('omt_list filters by the new statuses', async () => {
    const [a, b] = await ticketFixture(2)
    await tool('omt_update').execute({ id: a, status: 'blocked' }, NO_EXEC)
    await tool('omt_update').execute({ id: b, status: 'skipped' }, NO_EXEC)

    const blocked = await tool('omt_list').execute({ status: 'blocked' }, NO_EXEC)
    expect(blocked.map((node: any) => node.id)).toEqual([a])
    const skipped = await tool('omt_list').execute({ status: 'skipped' }, NO_EXEC)
    expect(skipped.map((node: any) => node.id)).toEqual([b])
  })
})

// ── TICKET-0057: omt_run_create / list / show / control ─────────────────

describe('TICKET-0057 omt_run_create', () => {
  it('creates a run snapshotting members in order', async () => {
    const ids = await ticketFixture(3)
    const created = await tool('omt_run_create').execute({ title: '第一批', nodeIds: ids }, NO_EXEC)
    expect(created.run.id).toBe('RUN-0001')
    expect(created.run.title).toBe('第一批')
    expect(created.run.status).toBe('pending')
    expect(created.items.map((item: any) => item.node_id)).toEqual(ids)
    expect(renderText('omt_run_create', {}, created)).toContain('RUN-0001')
  })

  it('rejects duplicate and unknown members', async () => {
    const [a] = await ticketFixture(1)
    await expect(tool('omt_run_create').execute({ nodeIds: [a, a] }, NO_EXEC)).rejects.toThrow(/duplicate/i)
    await expect(tool('omt_run_create').execute({ nodeIds: ['TICKET-9999'] }, NO_EXEC)).rejects.toThrow()
  })

  it('rejects members spanning multiple homes', async () => {
    // Separate pool: one global home + one workspace home.
    const root = await mkdtemp(join(tmpdir(), 'omt-run-crosshome-'))
    const globalHome = join(root, 'global')
    const workspace = join(root, 'workspace')
    await mkdir(join(workspace, '.omt'), { recursive: true })
    const crossPool = new OmtCorePool(globalHome)
    const crossTools = new Map<string, RegisteredTool>()
    registerOmtTools(stubToolCtx(crossTools) as never, crossPool)
    try {
      const create = crossTools.get('omt_create')!
      const globalEpic = await create.execute({ type: 'epic', title: '全局' }, NO_EXEC)
      const globalTicket = await create.execute(
        { type: 'ticket', title: '全局任务', parentId: (await create.execute({ type: 'story', title: '全局批', parentId: globalEpic.id }, NO_EXEC)).id },
        NO_EXEC,
      )
      const wsExec = agentExec('sess-ws', workspace)
      const wsEpic = await create.execute({ type: 'epic', title: '工作区', scope: 'workspace' }, wsExec)
      const wsStory = await create.execute({ type: 'story', title: '工作区批', parentId: wsEpic.id }, wsExec)
      const wsTicket = await create.execute({ type: 'ticket', title: '工作区任务', parentId: wsStory.id }, wsExec)

      await expect(
        crossTools.get('omt_run_create')!.execute({ nodeIds: [globalTicket.id, wsTicket.id] }, wsExec),
      ).rejects.toThrow(/home/i)
    } finally {
      await crossPool.closeAll()
      await rm(root, { recursive: true, force: true })
    }
  })
})

describe('TICKET-0057 omt_run_list / show', () => {
  it('lists runs with progress stats and filters by status', async () => {
    const ids = await ticketFixture(2)
    const first = await tool('omt_run_create').execute({ nodeIds: ids }, NO_EXEC)
    await tool('omt_run_create').execute({ nodeIds: ids }, NO_EXEC)
    await tool('omt_run_control').execute({ id: first.run.id, action: 'start' }, NO_EXEC)

    const all = await tool('omt_run_list').execute({}, NO_EXEC)
    expect(all).toHaveLength(2)
    const runningOnly = await tool('omt_run_list').execute({ status: 'running' }, NO_EXEC)
    expect(runningOnly).toHaveLength(1)
    expect(runningOnly[0].run.id).toBe(first.run.id)
    expect(runningOnly[0].progress).toMatchObject({ total: 2, pending: 2, done: 0 })
  })

  it('shows run detail with item states, executor, attempts and last_error', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.run.status).toBe('running')
    expect(detail.items).toHaveLength(2)
    expect(detail.items[0]).toMatchObject({ node_id: ids[0], state: 'running', executor_session_id: 'sess-1', attempts: 0 })
    expect(detail.items[0].title).toBe('任务1')
    expect(renderText('omt_run_show', { id: run.id }, detail)).toContain(ids[0]!)
  })

  it('marks a nudge-budget-exhausted pending item as stalled (TICKET-0062)', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    const core = await pool.coreFor(undefined)
    for (let count = 0; count < NUDGE_BUDGET; count += 1) core.recordItemNudge(run.id, ids[1]!)

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[1]).toMatchObject({ node_id: ids[1], state: 'pending', stalled: true })
    expect(detail.items[0].stalled).toBeUndefined()
    expect(renderText('omt_run_show', { id: run.id }, detail)).toContain('停滞')

    // Retry clears the budget → the marker disappears.
    await tool('omt_run_control').execute({ id: run.id, action: 'retry', nodeId: ids[1] }, NO_EXEC)
    const after = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(after.items[1].stalled).toBeUndefined()
  })
})

describe('TICKET-0057 omt_run_control', () => {
  it('walks start/pause/resume/cancel and rejects illegal moves', async () => {
    const ids = await ticketFixture(1)
    const created = await tool('omt_run_create').execute({ nodeIds: ids }, NO_EXEC)
    const control = tool('omt_run_control')

    await expect(control.execute({ id: created.run.id, action: 'pause' }, NO_EXEC)).rejects.toThrow()
    expect((await control.execute({ id: created.run.id, action: 'start' }, NO_EXEC)).run.status).toBe('running')
    expect((await control.execute({ id: created.run.id, action: 'pause' }, NO_EXEC)).run.status).toBe('paused')
    expect((await control.execute({ id: created.run.id, action: 'resume' }, NO_EXEC)).run.status).toBe('running')
    expect((await control.execute({ id: created.run.id, action: 'cancel' }, NO_EXEC)).run.status).toBe('canceled')
    await expect(control.execute({ id: created.run.id, action: 'resume' }, NO_EXEC)).rejects.toThrow()
    await expect(control.execute({ id: created.run.id, action: 'bogus' }, NO_EXEC)).rejects.toThrow(/action/i)
  })

  it('retry resets a failed item (requires nodeId)', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    await tool('omt_run_report').execute({ id: run.id, nodeId: claimed.item.node_id, outcome: 'failed', note: '编译失败' }, agentExec('sess-1'))

    const control = tool('omt_run_control')
    await expect(control.execute({ id: run.id, action: 'retry' }, NO_EXEC)).rejects.toThrow(/nodeId/)
    const retried = await control.execute({ id: run.id, action: 'retry', nodeId: claimed.item.node_id }, NO_EXEC)
    expect(retried.item).toMatchObject({ state: 'pending', attempts: 1, last_error: '编译失败' })
  })

  it('remove drops an item without touching the ticket node', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    const control = tool('omt_run_control')

    await expect(control.execute({ id: run.id, action: 'remove' }, NO_EXEC)).rejects.toThrow(/nodeId/)
    const removed = await control.execute({ id: run.id, action: 'remove', nodeId: ids[1] }, NO_EXEC)
    expect(removed.run.status).toBe('running')

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items.map((item: any) => item.node_id)).toEqual([ids[0]])
    // The ticket node itself is untouched by the item-level removal.
    const node = await tool('omt_show').execute({ id: ids[1] }, NO_EXEC)
    expect(node.node.status).toBe('open')
  })

  it('refuses to remove an in-flight item', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    await expect(
      tool('omt_run_control').execute({ id: run.id, action: 'remove', nodeId: ids[0] }, NO_EXEC),
    ).rejects.toThrow(/running|in-flight|进行/i)
  })
})

// ── TICKET-0058: omt_run_claim ───────────────────────────────────────────

describe('TICKET-0058 omt_run_claim', () => {
  it('claims the next pending item atomically with the executor session', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)

    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    expect(claimed.claimed).toBe(true)
    expect(claimed.item).toMatchObject({ node_id: ids[0], state: 'running', executor_session_id: 'sess-1' })
    expect(claimed.ticket).toMatchObject({ id: ids[0], title: '任务1' })
    expect(renderText('omt_run_claim', { id: run.id }, claimed)).toContain(ids[0]!)
  })

  it('returns the latest ancestor bodies as read-only context and the full current ticket', async () => {
    const epic = await tool('omt_create').execute({ type: 'epic', title: '运行平台', body: 'Epic 全局目标' }, NO_EXEC)
    const story = await tool('omt_create').execute({ type: 'story', title: '批量执行', parentId: epic.id, body: 'Story 初始约束' }, NO_EXEC)
    const substory = await tool('omt_create').execute({ type: 'substory', title: '失败恢复', parentId: story.id, body: 'SubStory 局部规则' }, NO_EXEC)
    const parentTicket = await tool('omt_create').execute({ type: 'ticket', title: '实现重试', parentId: substory.id, body: '父 Ticket 约束' }, NO_EXEC)
    const subticket = await tool('omt_create').execute({ type: 'subticket', title: '重试校验', parentId: parentTicket.id, body: 'SubTicket 完整任务\n\n## 验收标准\n- 重试成功' }, NO_EXEC)
    const run = await startedRun([subticket.id])
    await tool('omt_update').execute({ id: story.id, body: 'Story 最新约束' }, NO_EXEC)

    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-context'))

    expect(claimed.context.ancestors.map((entry: any) => entry.node.id)).toEqual([epic.id, story.id, substory.id, parentTicket.id])
    expect(claimed.context.ancestors.map((entry: any) => entry.body)).toEqual([
      'Epic 全局目标',
      'Story 最新约束',
      'SubStory 局部规则',
      '父 Ticket 约束',
    ])
    expect(claimed.context.read_errors).toEqual([])
    expect(claimed.context.current.node.id).toBe(subticket.id)
    expect(claimed.context.current.body).toContain('SubTicket 完整任务')
    expect(claimed.context.ancestors.every((entry: any) => !entry.body.includes('omt:children'))).toBe(true)
    const rendered = renderText('omt_run_claim', { id: run.id }, claimed)
    expect(rendered).toMatch(/背景（只读，不可执行）/)
    expect(rendered).toMatch(/当前执行项（唯一可执行、可报告）/)
    expect(rendered).toContain('Story 最新约束')
    expect(rendered).toContain('SubTicket 完整任务')
  })

  it('truncates oversized ancestor bodies visibly while prioritizing the nearest parent', async () => {
    const epicBody = `EPIC:${'界'.repeat(12_000)}`
    const storyBody = `STORY:${'S'.repeat(12_000)}`
    const epic = await tool('omt_create').execute({ type: 'epic', title: '大背景', body: epicBody }, NO_EXEC)
    const story = await tool('omt_create').execute({ type: 'story', title: '近端背景', parentId: epic.id, body: storyBody }, NO_EXEC)
    const ticket = await tool('omt_create').execute({ type: 'ticket', title: '当前任务', parentId: story.id, body: '完整 Ticket 正文' }, NO_EXEC)
    const run = await startedRun([ticket.id])

    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-budget'))
    const [epicContext, storyContext] = claimed.context.ancestors

    expect(claimed.context.truncated).toBe(true)
    expect(claimed.context.ancestor_used_bytes).toBeLessThanOrEqual(claimed.context.ancestor_budget_bytes)
    expect(storyContext).toMatchObject({ truncated: false, original_bytes: Buffer.byteLength(storyBody) })
    expect(storyContext.body).toBe(storyBody)
    expect(epicContext.truncated).toBe(true)
    expect(Buffer.byteLength(epicContext.body)).toBeLessThan(Buffer.byteLength(epicBody))
    expect(epicContext.body).not.toContain('�')
    expect(claimed.context.current.body).toBe('完整 Ticket 正文')
    expect(renderText('omt_run_claim', { id: run.id }, claimed)).toContain('上下文已截断')
  })

  it('preserves the current body and readable ancestors when one ancestor file is missing', async () => {
    const epic = await tool('omt_create').execute({ type: 'epic', title: '缺失背景', body: 'Epic 正文' }, NO_EXEC)
    const story = await tool('omt_create').execute({ type: 'story', title: '可用背景', parentId: epic.id, body: 'Story 正文' }, NO_EXEC)
    const ticket = await tool('omt_create').execute({ type: 'ticket', title: '仍可执行', parentId: story.id, body: '当前 Ticket 正文' }, NO_EXEC)
    const run = await startedRun([ticket.id])
    await rm(join(home, epic.path), { force: true })

    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-partial-context'))

    expect(claimed.context.current).toMatchObject({ node: { id: ticket.id }, body: '当前 Ticket 正文' })
    expect(claimed.context.ancestors.map((entry: any) => entry.node.id)).toEqual([story.id])
    expect(claimed.context.read_errors).toEqual([
      expect.objectContaining({ node: expect.objectContaining({ id: epic.id }), error: expect.stringMatching(/node file missing/i) }),
    ])
    expect(claimed.context_error).toBeUndefined()
    const rendered = renderText('omt_run_claim', { id: run.id }, claimed)
    expect(rendered).toContain('部分祖先上下文读取失败')
    expect(rendered).toContain('当前 Ticket 正文')
  })

  it('keeps the claimed result visible when the current context file cannot be read', async () => {
    const [ticketId] = await ticketFixture(1)
    const core = await pool.coreFor(undefined)
    const ticket = core.getNode(ticketId!)!
    const run = await startedRun([ticket.id])
    await rm(join(home, ticket.path), { force: true })

    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-missing-context'))

    expect(claimed).toMatchObject({
      claimed: true,
      item: { node_id: ticket.id, state: 'running', executor_session_id: 'sess-missing-context' },
    })
    expect(claimed.context).toBeUndefined()
    expect(claimed.context_error).toMatch(/node file missing|ENOENT|no such file/i)
    expect(renderText('omt_run_claim', { id: run.id }, claimed)).toContain('执行上下文读取失败')
  })

  it('claim alone activates open ancestors without executor bookkeeping', async () => {
    const epic = await tool('omt_create').execute({ type: 'epic', title: '联动平台', body: 'E' }, NO_EXEC)
    const story = await tool('omt_create').execute({ type: 'story', title: '联动批次', parentId: epic.id, body: 'S' }, NO_EXEC)
    const ticket = await tool('omt_create').execute({ type: 'ticket', title: '联动任务', parentId: story.id, body: 'T' }, NO_EXEC)
    const run = await startedRun([ticket.id])
    const core = await pool.coreFor(undefined)

    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-activation'))
    expect(claimed.claimed).toBe(true)

    expect(core.getNode(epic.id)?.status).toBe('in_progress')
    expect(core.getNode(story.id)?.status).toBe('in_progress')
    expect(core.getNode(ticket.id)?.status).toBe('open')
  })

  it('two concurrent claims never receive the same item; empty queue is an explicit signal', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)

    const [first, second] = await Promise.all([
      tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1')),
      tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-2')),
    ])
    expect(first.claimed).toBe(true)
    expect(second.claimed).toBe(true)
    expect(first.item.node_id).not.toBe(second.item.node_id)
    expect(first.item.executor_session_id).toBe('sess-1')
    expect(second.item.executor_session_id).toBe('sess-2')

    const third = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-3'))
    expect(third.claimed).toBe(false)
    expect(renderText('omt_run_claim', { id: run.id }, third)).toMatch(/没有可认领|无 pending/i)
  })

  it('rejects claim on a paused, pending, or terminal run', async () => {
    const ids = await ticketFixture(1)
    const created = await tool('omt_run_create').execute({ nodeIds: ids }, NO_EXEC)

    // Not started yet.
    await expect(tool('omt_run_claim').execute({ id: created.run.id }, agentExec('sess-1'))).rejects.toThrow()

    await tool('omt_run_control').execute({ id: created.run.id, action: 'start' }, NO_EXEC)
    await tool('omt_run_control').execute({ id: created.run.id, action: 'pause' }, NO_EXEC)
    await expect(tool('omt_run_claim').execute({ id: created.run.id }, agentExec('sess-1'))).rejects.toThrow(/paused/)

    // canceled is absolute terminal: no dispatch ever again.
    await tool('omt_run_control').execute({ id: created.run.id, action: 'resume' }, NO_EXEC)
    await tool('omt_run_control').execute({ id: created.run.id, action: 'cancel' }, NO_EXEC)
    await expect(tool('omt_run_claim').execute({ id: created.run.id }, agentExec('sess-1'))).rejects.toThrow(/canceled/)

    // A run that derived completed (last item claimed + reported done)
    // rejects claims too.
    const doneRun = await startedRun(await ticketFixture(1))
    const claimed = await tool('omt_run_claim').execute({ id: doneRun.id }, agentExec('sess-1'))
    await tool('omt_run_report').execute(
      { id: doneRun.id, nodeId: claimed.item.node_id, outcome: 'done' },
      agentExec('sess-1'),
    )
    expect((await tool('omt_run_show').execute({ id: doneRun.id }, NO_EXEC)).run.status).toBe('completed')
    await expect(tool('omt_run_claim').execute({ id: doneRun.id }, agentExec('sess-1'))).rejects.toThrow(/completed/)
  })

  it('rejects agent-less claims with a clear error', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    await expect(tool('omt_run_claim').execute({ id: run.id }, NO_EXEC)).rejects.toThrow(/会话|agent|执行者/i)
  })
})

// ── TICKET-0059: omt_run_report ──────────────────────────────────────────

describe('TICKET-0059 omt_run_report', () => {
  it('done: ticket → done, item → done, note appended', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))

    const reported = await tool('omt_run_report').execute(
      { id: run.id, nodeId: claimed.item.node_id, outcome: 'done', note: '完成：已联调' },
      agentExec('sess-1'),
    )
    expect(reported.item.state).toBe('done')
    expect(reported.node.status).toBe('done')
    const detail = await tool('omt_show').execute({ id: claimed.item.node_id }, NO_EXEC)
    expect(detail.body).toContain('完成：已联调')
    // The explicit report stops the running mark (execution concluded).
    expect(running.get(claimed.item.node_id)).toBeUndefined()
  })

  it('failed: node status unchanged, item failed, note → last_error + body', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: claimed.item.node_id, status: 'in_progress' }, agentExec('sess-1'))

    const reported = await tool('omt_run_report').execute(
      { id: run.id, nodeId: claimed.item.node_id, outcome: 'failed', note: '编译失败：缺少依赖' },
      agentExec('sess-1'),
    )
    expect(reported.item.state).toBe('failed')
    expect(reported.item.last_error).toBe('编译失败：缺少依赖')
    // The node enum has no failed: the ticket stays in_progress.
    expect(reported.node.status).toBe('in_progress')
    const detail = await tool('omt_show').execute({ id: claimed.item.node_id }, NO_EXEC)
    expect(detail.body).toContain('编译失败：缺少依赖')
  })

  it('blocked / skipped: double-write node and item', async () => {
    const ids = await ticketFixture(3)
    const run = await startedRun(ids)
    const first = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    const blockedReport = await tool('omt_run_report').execute(
      { id: run.id, nodeId: first.item.node_id, outcome: 'blocked', note: '等待上游接口' },
      agentExec('sess-1'),
    )
    expect(blockedReport.item.state).toBe('blocked')
    expect(blockedReport.node.status).toBe('blocked')

    const second = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    const skippedReport = await tool('omt_run_report').execute(
      { id: run.id, nodeId: second.item.node_id, outcome: 'skipped', note: '产品确认不做' },
      agentExec('sess-1'),
    )
    expect(skippedReport.item.state).toBe('skipped')
    expect(skippedReport.node.status).toBe('skipped')
  })

  it('rejects illegal outcomes and reports on non-in-flight items', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    await expect(
      tool('omt_run_report').execute({ id: run.id, nodeId: ids[0], outcome: 'bogus' }, agentExec('sess-1')),
    ).rejects.toThrow(/outcome/i)
    // Item still pending (never claimed): nothing to report.
    await expect(
      tool('omt_run_report').execute({ id: run.id, nodeId: ids[0], outcome: 'done' }, agentExec('sess-1')),
    ).rejects.toThrow()
  })

  it('stop-on-failure: a failed report pauses the run, blocked does not', async () => {
    const ids = await ticketFixture(3)
    const run = await startedRun(ids, { stopOnFailure: true })

    const first = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    const afterBlocked = await tool('omt_run_report').execute(
      { id: run.id, nodeId: first.item.node_id, outcome: 'blocked' }, agentExec('sess-1'),
    )
    expect(afterBlocked.run.status).toBe('running')

    const second = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    const afterFailed = await tool('omt_run_report').execute(
      { id: run.id, nodeId: second.item.node_id, outcome: 'failed', note: '炸了' }, agentExec('sess-1'),
    )
    expect(afterFailed.run.status).toBe('paused')
  })

  it('rejects a report whose ticket was archived after the claim, leaving the body untouched', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    const before = (await tool('omt_show').execute({ id: ids[0]! }, NO_EXEC)).body

    // Archiving after the claim seals the node (observation skips the item);
    // a late report is refused by the archived guard instead of rewriting
    // the ticket — the guard fires before the item-state check.
    await tool('omt_update').execute({ id: ids[0]!, archived: true }, agentExec('sess-1'))
    await expect(
      tool('omt_run_report').execute(
        { id: run.id, nodeId: claimed.item.node_id, outcome: 'done', note: '不应落盘' },
        agentExec('sess-1'),
      ),
    ).rejects.toThrow(/归档|archived/)

    const after = await tool('omt_show').execute({ id: ids[0]! }, NO_EXEC)
    expect(after.body).toBe(before)
    expect(after.node.status).not.toBe('done')
  })
})

// ── TICKET-0061: passive observation ─────────────────────────────────────

describe('TICKET-0061 passive observation', () => {
  it('ticket → in_progress advances the pending item to running with the observing session', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)

    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0]).toMatchObject({ state: 'running', executor_session_id: 'sess-1' })
    expect(detail.items[1].state).toBe('pending')
  })

  it('ticket → done routes the running item to awaiting_confirmation (TICKET-0064 default)', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))

    // The ticket itself lands done; the ITEM waits for confirmation.
    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('awaiting_confirmation')
    expect(detail.run.status).toBe('running')
  })

  it('direct blocked/skipped sets map onto items (pending included — no wedged runs)', async () => {
    const ids = await ticketFixture(3)
    const run = await startedRun(ids)

    // In-flight item → blocked.
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'blocked' }, agentExec('sess-1'))
    // Pending item directly set skipped/blocked must not wedge the run.
    await tool('omt_update').execute({ id: ids[1], status: 'skipped' }, NO_EXEC)
    await tool('omt_update').execute({ id: ids[2], status: 'blocked' }, NO_EXEC)

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items.map((item: any) => item.state)).toEqual(['blocked', 'skipped', 'blocked'])
    // blocked counts as not-successful for terminal derivation.
    expect(detail.run.status).toBe('completed_with_failures')
  })

  it('archiving a ticket maps its item to skipped', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    const archived = await tool('omt_update').execute({ id: ids[0], archived: true }, agentExec('sess-1'))

    // The observed change must match what actually landed: the node side is
    // archived too (not just the item advanced).
    expect(archived.archived).toBe(true)
    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('skipped')
  })

  it('claim wins: later manual transitions never overwrite the claimed executor', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    const claimed = await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))

    // Another session flips the ticket in_progress: state advances are fine,
    // but the executor attribution stays with the claimer.
    await tool('omt_update').execute({ id: claimed.item.node_id, status: 'in_progress' }, agentExec('sess-2'))
    let detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0]).toMatchObject({ state: 'running', executor_session_id: 'sess-1' })

    await tool('omt_update').execute({ id: claimed.item.node_id, status: 'done' }, agentExec('sess-2'))
    detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0]).toMatchObject({ state: 'done', executor_session_id: 'sess-1' })
  })

  it('broadcasts progress to every active run holding the ticket', async () => {
    const ids = await ticketFixture(1)
    // autoVerify: the broadcast mechanics under test are independent of the
    // TICKET-0064 confirmation gate (covered in its own describe below).
    const runA = await startedRun(ids, { autoVerify: true })
    const runB = await startedRun(ids, { autoVerify: true })

    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    let detailA = await tool('omt_run_show').execute({ id: runA.id }, NO_EXEC)
    let detailB = await tool('omt_run_show').execute({ id: runB.id }, NO_EXEC)
    expect(detailA.items[0].state).toBe('running')
    expect(detailB.items[0].state).toBe('running')

    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    detailA = await tool('omt_run_show').execute({ id: runA.id }, NO_EXEC)
    detailB = await tool('omt_run_show').execute({ id: runB.id }, NO_EXEC)
    expect(detailA.items[0].state).toBe('done')
    expect(detailB.items[0].state).toBe('done')
    expect(detailA.run.status).toBe('completed')
    expect(detailB.run.status).toBe('completed')
  })

  it('paused runs keep observing in-flight items but dispatch nothing new', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids, { autoVerify: true })
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_run_control').execute({ id: run.id, action: 'pause' }, NO_EXEC)

    // In-flight item still advances while paused.
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    // …but the pending sibling is not dispatched by a status flip.
    await tool('omt_update').execute({ id: ids[1], status: 'in_progress' }, agentExec('sess-1'))

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.run.status).toBe('paused')
    expect(detail.items[0].state).toBe('done')
    expect(detail.items[1].state).toBe('pending')
  })

  it('reopening a finished ticket replays its item back to pending', async () => {
    const ids = await ticketFixture(2)
    // autoVerify: replay mechanics need the item to actually reach done
    // (default runs route the bare done to awaiting_confirmation, TICKET-0064).
    const run = await startedRun(ids, { autoVerify: true })
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    let detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')

    // done → open while the run is in progress: the item falls back (decision 11).
    await tool('omt_update').execute({ id: ids[0], status: 'open' }, NO_EXEC)
    detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('pending')
    expect(detail.items[0].position).toBe(0)
  })

  it('observes status changes coming through the RPC update endpoint too', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)

    let handler: ((endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>) | undefined
    const stubCtx = {
      connection: {
        rpc: {
          handle(_channel: string, h: any) {
            handler = h
          },
        },
      },
    }
    registerOmtRpc(stubCtx as never, pool, undefined, undefined, running)

    const signal = new AbortController().signal
    const updated = await handler!('update', { id: ids[0], status: 'blocked', sessionId: 'sess-rpc' }, signal)
    expect(updated.ok).toBe(true)
    expect(updated.value.status).toBe('blocked')

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('blocked')

    // The execute endpoint (执行 button) dispatches the pending item.
    const executed = await handler!('execute', { id: ids[1], sessionId: 'sess-rpc' }, signal)
    expect(executed.ok).toBe(true)
    const after = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(after.items[1]).toMatchObject({ state: 'running', executor_session_id: 'sess-rpc' })
  })

  it('an explicit report completion broadcasts to other active runs holding the ticket', async () => {
    const ids = await ticketFixture(1)
    const runA = await startedRun(ids)
    const runB = await startedRun(ids)

    const claimed = await tool('omt_run_claim').execute({ id: runA.id }, agentExec('sess-1'))
    await tool('omt_run_report').execute(
      { id: runA.id, nodeId: claimed.item.node_id, outcome: 'done', note: '完成' },
      agentExec('sess-1'),
    )

    // The report's ticket double-write goes through core.update, so the
    // second run's item advances with no separate observation wiring.
    const detailA = await tool('omt_run_show').execute({ id: runA.id }, NO_EXEC)
    const detailB = await tool('omt_run_show').execute({ id: runB.id }, NO_EXEC)
    expect(detailA.items[0].state).toBe('done')
    expect(detailA.run.status).toBe('completed')
    expect(detailB.items[0].state).toBe('done')
    expect(detailB.run.status).toBe('completed')
  })

  it('manual status changes still never START a running mark (TICKET-0028)', async () => {
    const ids = await ticketFixture(1)
    await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    expect(running.get(ids[0]!)).toBeDefined() // model tool flow starts the mark (existing rule)
    await tool('omt_update').execute({ id: ids[0], status: 'blocked' }, agentExec('sess-1'))
    // blocked ends active execution: the mark clears like done/archive do.
    expect(running.get(ids[0]!)).toBeUndefined()
  })
})

// ── TICKET-0064: awaiting_confirmation 信任策略 ─────────────────────────

describe('TICKET-0064 trust policy (awaiting_confirmation)', () => {
  it('autoVerify=true: a bare done from the executor lands done directly', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids, { autoVerify: true })
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')
    expect(detail.run.status).toBe('completed')
  })

  it('an explicit omt_run_report done lands done directly (never gated)', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    await tool('omt_run_claim').execute({ id: run.id }, agentExec('sess-1'))
    await tool('omt_run_report').execute({ id: run.id, nodeId: ids[0], outcome: 'done' }, agentExec('sess-1'))

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')
    expect(detail.run.status).toBe('completed')
  })

  it('a bare done from a NON-executor session is not gated', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    // A different session flips the ticket done (e.g. a human via another session).
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-2'))

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')
  })

  it('a bare done on a PENDING item (never dispatched) is not gated', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, NO_EXEC)

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')
  })

  it('confirmation: a report on an awaiting_confirmation item completes it', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    expect((await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)).items[0].state).toBe('awaiting_confirmation')

    // 确认通道：显式 report（awaiting_confirmation 是 in-flight，接受 report）。
    await tool('omt_run_report').execute({ id: run.id, nodeId: ids[0], outcome: 'done', note: '确认无误' }, agentExec('sess-1'))
    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')
    expect(detail.run.status).toBe('completed')
  })

  it('rejection: reopening the ticket interrupts the awaiting_confirmation item (打回)', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    expect((await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)).items[0].state).toBe('awaiting_confirmation')

    // 打回通道：ticket 打回 open → item interrupted（等 retry，不是 replay）。
    await tool('omt_update').execute({ id: ids[0], status: 'open' }, NO_EXEC)
    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('interrupted')
    expect(detail.run.status).toBe('running')
  })

  it('awaiting_confirmation never auto-completes: an unrelated update leaves it in place', async () => {
    const ids = await ticketFixture(2)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    // 无响应/含糊：对 ticket 做非状态修改不会改变待确认状态。
    await tool('omt_update').execute({ id: ids[0], append: '补充说明' }, agentExec('sess-1'))

    const detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('awaiting_confirmation')
    expect(detail.run.status).toBe('running')
  })

  it('a repeated bare done does NOT complete an awaiting_confirmation item (gate holds)', async () => {
    const ids = await ticketFixture(1)
    const run = await startedRun(ids)
    await tool('omt_update').execute({ id: ids[0], status: 'in_progress' }, agentExec('sess-1'))
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    expect((await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)).items[0].state).toBe('awaiting_confirmation')

    // 第二次裸 done（不带 report）必须被信任门拦下：只有 omt_run_report /
    // UI confirm 能完成 gated 项。
    await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    let detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('awaiting_confirmation')
    expect(detail.run.status).toBe('running')

    // report 路径照常完成（reported=true 绕过此拦截）。
    await tool('omt_run_report').execute({ id: run.id, nodeId: ids[0], outcome: 'done' }, agentExec('sess-1'))
    detail = await tool('omt_run_show').execute({ id: run.id }, NO_EXEC)
    expect(detail.items[0].state).toBe('done')
    expect(detail.run.status).toBe('completed')
  })

  it('non-run tickets are untouched by the trust policy', async () => {
    const ids = await ticketFixture(1)
    // No run holds this ticket: a bare done is an ordinary status change.
    const updated = await tool('omt_update').execute({ id: ids[0], status: 'done' }, agentExec('sess-1'))
    expect(updated.status).toBe('done')
  })
})
