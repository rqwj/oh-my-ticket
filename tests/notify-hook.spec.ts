/**
 * Run-notification tests (TICKET-0065): the notifier subscribes to core run
 * events (attached per core via the pool's onCoreOpened) and delivers —
 *  1. item 完成进度（done/failed/blocked/skipped）→ inject（不唤醒）;
 *  2. stop-on-failure paused 待决 → followup（唤醒）含失败项/last_error/选项;
 *  3. awaiting_confirmation 待确认提示 → inject;
 *  4. run 终态总结（completed/completed_with_failures/canceled）→ followup;
 *     interrupted 终态不注入。
 * Same-tick events merge into ONE message per session (wake wins);
 * destroyed executor sessions and throwing channels are contained.
 */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { OmtCore } from '../src/host/core.ts'
import { createOmtRunNotifier } from '../src/host/notify-hook.ts'
import { OmtCorePool } from '../src/host/pool.ts'
import type { OmtRun, RunConfig } from '../src/host/types.ts'
import { ticketFixture } from './mocks/fixtures.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

interface FakeAgent {
  id: string
  followups: unknown[]
  injects: unknown[]
}

let home: string
let pool: OmtCorePool
let core: OmtCore
let liveAgents: Map<string, FakeAgent>

function makeAgent(id: string): FakeAgent {
  const agent: FakeAgent = { id, followups: [], injects: [] }
  liveAgents.set(id, agent)
  return agent
}

function textsOf(messages: unknown[]): string[] {
  return messages.map(message => (message as { content: { text: string }[] }).content.map(block => block.text).join('\n'))
}

/** Notifier delivery flushes on a microtask — drain the queue. */
function flush(): Promise<void> {
  return new Promise<void>(resolve => setImmediate(resolve))
}

async function runFixture(count: number, config?: Partial<RunConfig>): Promise<{ run: OmtRun; ticketIds: string[] }> {
  const tickets = await ticketFixture(core, count)
  const run = await core.createRun({ nodeIds: tickets.map(ticket => ticket.id), ...(config !== undefined ? { config } : {}) })
  await core.startRun(run.id)
  return { run, ticketIds: tickets.map(ticket => ticket.id) }
}

beforeEach(async () => {
  home = await mkdtemp(join(tmpdir(), 'omt-notify-hook-'))
  liveAgents = new Map()
  const stubCtx = {
    agents: {
      get: (id: string) => {
        const agent = liveAgents.get(id)
        return agent === undefined
          ? undefined
          : {
              id: agent.id,
              followup: (message: unknown) => agent.followups.push(message),
              inject: (message: unknown) => agent.injects.push(message),
            }
      },
      list: () => [...liveAgents.values()],
    },
  }
  const notifier = createOmtRunNotifier(stubCtx as never)
  pool = new OmtCorePool(home, { onCoreOpened: opened => notifier.attach(opened) })
  core = await pool.coreFor(undefined)
})

afterEach(async () => {
  await pool.closeAll()
  await rm(home, { recursive: true, force: true })
})

describe('item 完成进度', () => {
  it('injects a progress line to the executor session (no wake)', async () => {
    const { run, ticketIds } = await runFixture(2)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[0]!, 'done', '收工')
    await flush()

    expect(agent.followups).toHaveLength(0)
    expect(textsOf(agent.injects)).toHaveLength(1)
    expect(textsOf(agent.injects)[0]).toContain(`${run.id} 进度 1/2`)
    expect(textsOf(agent.injects)[0]).toContain(`${ticketIds[0]} done`)
  })

  it('covers failed/blocked/skipped outcomes too', async () => {
    // Four tickets: the three reports below never terminate the run, so
    // every notification stays a plain progress inject.
    const { run, ticketIds } = await runFixture(4)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[0]!, 'failed', '炸了')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[1]!, 'blocked', '缺依赖')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[2]!, 'skipped', '不需要')
    await flush()

    const texts = textsOf(agent.injects)
    expect(texts).toHaveLength(3)
    expect(texts[0]).toContain(`${ticketIds[0]} failed`)
    expect(texts[1]).toContain(`${ticketIds[1]} blocked`)
    expect(texts[2]).toContain(`${ticketIds[2]} skipped`)
    expect(agent.followups).toHaveLength(0)
  })

  it('skips delivery when the executor session is gone (contained)', async () => {
    const { run, ticketIds } = await runFixture(1)
    await core.claimRunItem(run.id, 'ghost')
    await core.reportRunItem(run.id, ticketIds[0]!, 'done')
    await flush()
    // No live agent, no throw, no message — nothing to assert but survival.
    expect(liveAgents.size).toBe(0)
  })
})

describe('paused 待决通知', () => {
  it('stop-on-failure pause wakes the executor with failure detail and options', async () => {
    const { run, ticketIds } = await runFixture(2, { stopOnFailure: true })
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[0]!, 'failed', '编译失败')
    await flush()

    // Progress line + paused notice merge into ONE waking followup.
    expect(agent.injects).toHaveLength(0)
    expect(textsOf(agent.followups)).toHaveLength(1)
    const text = textsOf(agent.followups)[0]!
    expect(text).toContain(`${ticketIds[0]} failed`)
    expect(text).toContain('编译失败')
    expect(text).toContain('resume')
    expect(text).toContain('cancel')
    expect(core.getRun(run.id)?.status).toBe('paused')
  })

  it('a manual pause (no failed item) sends nothing', async () => {
    const { run } = await runFixture(2)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.pauseRun(run.id)
    await flush()
    expect(agent.followups).toHaveLength(0)
    expect(agent.injects).toHaveLength(0)
  })
})

describe('awaiting_confirmation 待确认提示', () => {
  it('injects a confirmation hint when the trust gate routes a bare done', async () => {
    const { run, ticketIds } = await runFixture(1)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    // Bare done by the executor session (autoVerify=false default) → gate.
    await core.update({ id: ticketIds[0]!, status: 'done', executorSessionId: 's1' })
    await flush()

    expect(textsOf(agent.injects)).toHaveLength(1)
    const text = textsOf(agent.injects)[0]!
    expect(text).toContain(ticketIds[0])
    expect(text).toContain('awaiting_confirmation')
    expect(text).toContain('omt_run_report')
    expect(agent.followups).toHaveLength(0)
  })
})

describe('run 终态总结', () => {
  it('merges the last item progress and the completion summary into one followup', async () => {
    const { run, ticketIds } = await runFixture(1)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[0]!, 'done')
    await flush()

    expect(agent.injects).toHaveLength(0)
    expect(textsOf(agent.followups)).toHaveLength(1)
    const text = textsOf(agent.followups)[0]!
    expect(text).toContain(`${run.id} 进度 1/1`)
    expect(text).toContain('completed')
  })

  it('completed_with_failures summary lists the failed item and its last_error', async () => {
    const { run, ticketIds } = await runFixture(2)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[0]!, 'failed', '验收不过')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[1]!, 'done')
    await flush()

    // Two batches: failed inject, then merged done+summary followup.
    const summary = textsOf(agent.followups).pop()!
    expect(summary).toContain('completed_with_failures')
    expect(summary).toContain(ticketIds[0]!)
    expect(summary).toContain('验收不过')
  })

  it('canceled runs get a terminal summary too', async () => {
    const { run } = await runFixture(1)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    await core.cancelRun(run.id)
    await flush()

    expect(textsOf(agent.followups)).toHaveLength(1)
    expect(textsOf(agent.followups)[0]).toContain('canceled')
  })

  it('interrupted runs are NEVER notified (executor is usually gone)', async () => {
    const { run } = await runFixture(2)
    const agent = makeAgent('s1')
    await core.claimRunItem(run.id, 's1')
    core.janitorSweep(() => false)
    await flush()

    expect(core.getRun(run.id)?.status).toBe('interrupted')
    expect(agent.followups).toHaveLength(0)
    expect(agent.injects).toHaveLength(0)
  })

  it('delivers the terminal summary to EVERY distinct executor session', async () => {
    const { run, ticketIds } = await runFixture(2)
    const first = makeAgent('s1')
    const second = makeAgent('s2')
    await core.claimRunItem(run.id, 's1')
    await core.reportRunItem(run.id, ticketIds[0]!, 'done')
    await core.claimRunItem(run.id, 's2')
    await core.reportRunItem(run.id, ticketIds[1]!, 'done')
    await flush()

    expect(core.getRun(run.id)?.status).toBe('completed')
    // 两位执行者都收到终态总结（s2 的总结与其进度行合并为一条 followup）。
    const firstTexts = textsOf(first.followups)
    const secondTexts = textsOf(second.followups)
    expect(firstTexts.some(text => text.includes(run.id) && text.includes('completed'))).toBe(true)
    expect(secondTexts.some(text => text.includes(run.id) && text.includes('completed'))).toBe(true)
  })

  it('delivers nothing when no item has an executor session (contained, no throw)', async () => {
    const { run } = await runFixture(1)
    const bystander = makeAgent('s9')
    // 无人认领直接取消：executor_session_id 全部缺失 → 不投递、不抛错。
    await core.cancelRun(run.id)
    await flush()

    expect(core.getRun(run.id)?.status).toBe('canceled')
    expect(bystander.followups).toHaveLength(0)
    expect(bystander.injects).toHaveLength(0)
  })
})

describe('错误包容', () => {
  it('a throwing inject/followup is contained (warn, no rejection)', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const { run, ticketIds } = await runFixture(2)
      const broken = makeAgent('s1')
      // Poison the delivery channel: the notifier's agent.inject lands here.
      broken.injects.push = (() => { throw new Error('channel gone') }) as never

      await core.claimRunItem(run.id, 's1')
      await core.reportRunItem(run.id, ticketIds[0]!, 'done')
      await flush()
      expect(warn).toHaveBeenCalled()
    } finally {
      warn.mockRestore()
    }
  })
})
