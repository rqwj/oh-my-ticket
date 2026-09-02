/**
 * Run-notification tests (TICKET-0065): the notifier subscribes to the
 * runtime service's run-event stream (service.onRunEvent over the real
 * omt-daemon subscription) and delivers —
 *  1. item 完成进度（done/failed/blocked/skipped）→ inject（不唤醒）;
 *  2. stop-on-failure paused 待决 → followup（唤醒）含失败项/last_error/选项;
 *  3. awaiting_confirmation 待确认提示 → inject;
 *  4. run 终态总结（completed/completed_with_failures/canceled）→ followup.
 * Same-window events merge into ONE message per session (wake wins);
 * destroyed executor sessions and throwing channels are contained.
 *
 * U7a note: the old suite's interrupted-run case relied on
 * core.janitorSweep forcing status `interrupted` — that state is
 * UNREACHABLE on this daemon build for in-test timeframes (leases expire
 * only after LEASE_TTL_MS; derive_terminal never yields Interrupted), so
 * its "never notify interrupted" rule has no drivable scenario here and the
 * case was dropped with the rest of the semantics preserved.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createOmtRunNotifier } from '../src/host/notify-hook.ts'
import type { HomeRef, OmtService } from '../src/host/service.ts'
import type { OmtRun, RunConfig } from '../src/host/types.ts'
import { ticketFixture } from './mocks/fixtures.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

interface FakeAgent {
  id: string
  followups: unknown[]
  injects: unknown[]
}

let fixture: RuntimeFixture
let service: OmtService
let globalHome: HomeRef
let liveAgents: Map<string, FakeAgent>

function makeAgent(id: string): FakeAgent {
  const agent: FakeAgent = { id, followups: [], injects: [] }
  liveAgents.set(id, agent)
  return agent
}

function textsOf(messages: unknown[]): string[] {
  return messages.map(message => (message as { content: { text: string }[] }).content.map(block => block.text).join('\n'))
}

/** Poll until a channel holds at least `min` texts (events + flush window). */
async function waitForTexts(get: () => string[], min: number): Promise<string[]> {
  const deadline = Date.now() + 4000
  for (;;) {
    const current = get()
    if (current.length >= min) return current
    if (Date.now() > deadline) return current
    await new Promise(resolve => setTimeout(resolve, 10))
  }
}

/**
 * Poll until the UNION of delivered texts covers every marker. The notifier
 * coalesces per session per flush window BY DESIGN, so rapid successive
 * outcomes can land in one message — per-index assertions race the flush
 * timer (CI timing differs from dev machines and flakes).
 */
async function waitForMarkers(get: () => string[], markers: string[]): Promise<string[]> {
  const deadline = Date.now() + 4000
  for (;;) {
    const current = get()
    const all = current.join('\n')
    if (markers.every(marker => all.includes(marker))) return current
    if (Date.now() > deadline) return current
    await new Promise(resolve => setTimeout(resolve, 10))
  }
}

async function runFixture(count: number, config?: Partial<RunConfig>): Promise<{ run: OmtRun; ticketIds: string[] }> {
  const tickets = await ticketFixture(service, globalHome, count)
  const created = await service.createRun(globalHome, {
    nodeIds: tickets.map(ticket => ticket.id),
    ...(config !== undefined ? { config } : {}),
  })
  await service.controlRun(globalHome, created.run.id, 'start')
  return { run: created.run, ticketIds: tickets.map(ticket => ticket.id) }
}

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'notify-hook' })
  service = fixture.service
  globalHome = fixture.globalHome
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
  notifier.attach(service)
})

afterEach(async () => {
  await fixture.stop()
})

describe('item 完成进度', () => {
  it('injects a progress line to the executor session (no wake)', async () => {
    const { run, ticketIds } = await runFixture(2)
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'done', '收工')

    const texts = await waitForTexts(() => textsOf(agent.injects), 1)
    expect(agent.followups).toHaveLength(0)
    expect(texts).toHaveLength(1)
    expect(texts[0]).toContain(`${run.id} 进度 1/2`)
    expect(texts[0]).toContain(`${ticketIds[0]} done`)
  })

  it('covers failed/blocked/skipped outcomes too', async () => {
    // Four tickets: the three reports below never terminate the run, so
    // every notification stays a plain progress inject.
    const { run, ticketIds } = await runFixture(4)
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'failed', '炸了')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[1]!, 'blocked', '缺依赖')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[2]!, 'skipped', '不需要')

    const texts = await waitForMarkers(() => textsOf(agent.injects), [
      `${ticketIds[0]} failed`,
      `${ticketIds[1]} blocked`,
      `${ticketIds[2]} skipped`,
    ])
    // Coalescing is by design: outcomes may share one message, so assert on
    // the union rather than one text per outcome.
    const all = texts.join('\n')
    expect(all).toContain(`${ticketIds[0]} failed`)
    expect(all).toContain(`${ticketIds[1]} blocked`)
    expect(all).toContain(`${ticketIds[2]} skipped`)
    expect(agent.followups).toHaveLength(0)
  })

  it('skips delivery when the executor session is gone (contained)', async () => {
    const { run, ticketIds } = await runFixture(1)
    await service.claimItem(globalHome, run.id, 'ghost')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'done')
    await new Promise(resolve => setTimeout(resolve, 120))
    // No live agent, no throw, no message — nothing to assert but survival.
    expect(liveAgents.size).toBe(0)
  })
})

describe('paused 待决通知', () => {
  it('stop-on-failure pause wakes the executor with failure detail and options', async () => {
    const { run, ticketIds } = await runFixture(2, { stopOnFailure: true })
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'failed', '编译失败')

    // Progress line + paused notice merge into ONE waking followup.
    const followupTexts = await waitForTexts(() => textsOf(agent.followups), 1)
    expect(agent.injects).toHaveLength(0)
    expect(followupTexts).toHaveLength(1)
    const text = followupTexts[0]!
    expect(text).toContain(`${ticketIds[0]} failed`)
    expect(text).toContain('编译失败')
    expect(text).toContain('resume')
    expect(text).toContain('cancel')
    expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('paused')
  })

  it('a manual pause (no failed item) sends nothing', async () => {
    const { run } = await runFixture(2)
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    await service.controlRun(globalHome, run.id, 'pause')
    await new Promise(resolve => setTimeout(resolve, 120))
    expect(agent.followups).toHaveLength(0)
    expect(agent.injects).toHaveLength(0)
  })
})

describe('awaiting_confirmation 待确认提示', () => {
  it('injects a confirmation hint when the trust gate routes a bare done', async () => {
    const { run, ticketIds } = await runFixture(1)
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    // Bare done by the executor session (autoVerify=false default) → gate.
    await service.updateNode({ id: ticketIds[0]!, status: 'done' }, { sessionId: 's1' })

    const texts = await waitForTexts(() => textsOf(agent.injects), 1)
    expect(texts).toHaveLength(1)
    const text = texts[0]!
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
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'done')

    const texts = await waitForTexts(() => textsOf(agent.followups), 1)
    expect(agent.injects).toHaveLength(0)
    expect(texts).toHaveLength(1)
    const text = texts[0]!
    expect(text).toContain(`${run.id} 进度 1/1`)
    expect(text).toContain('completed')
  })

  it('completed_with_failures summary lists the failed item and its last_error', async () => {
    const { run, ticketIds } = await runFixture(2)
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'failed', '验收不过')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[1]!, 'done')

    // Two batches: failed inject, then merged done+summary followup.
    const followupTexts = await waitForTexts(() => textsOf(agent.followups), 1)
    const summary = followupTexts[followupTexts.length - 1]!
    expect(summary).toContain('completed_with_failures')
    expect(summary).toContain(ticketIds[0]!)
    expect(summary).toContain('验收不过')
  })

  it('canceled runs get a terminal summary too', async () => {
    const { run } = await runFixture(1)
    const agent = makeAgent('s1')
    await service.claimItem(globalHome, run.id, 's1')
    await service.controlRun(globalHome, run.id, 'cancel')

    const texts = await waitForTexts(() => textsOf(agent.followups), 1)
    expect(texts).toHaveLength(1)
    expect(texts[0]).toContain('canceled')
  })

  it('delivers the terminal summary to EVERY distinct executor session', async () => {
    const { run, ticketIds } = await runFixture(2)
    const first = makeAgent('s1')
    const second = makeAgent('s2')
    await service.claimItem(globalHome, run.id, 's1')
    await service.reportItem(globalHome, run.id, ticketIds[0]!, 'done')
    await service.claimItem(globalHome, run.id, 's2')
    await service.reportItem(globalHome, run.id, ticketIds[1]!, 'done')

    await waitForTexts(() => textsOf(first.followups), 1)
    await waitForTexts(() => textsOf(second.followups), 1)
    expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('completed')
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
    await service.controlRun(globalHome, run.id, 'cancel')
    await new Promise(resolve => setTimeout(resolve, 120))

    expect((await service.fetchRun(globalHome, run.id)).run.status).toBe('canceled')
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

      await service.claimItem(globalHome, run.id, 's1')
      await service.reportItem(globalHome, run.id, ticketIds[0]!, 'done')
      await new Promise(resolve => setTimeout(resolve, 150))
      expect(warn).toHaveBeenCalled()
    } finally {
      warn.mockRestore()
    }
  })
})
