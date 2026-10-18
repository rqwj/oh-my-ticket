/**
 * U14：信任门（awaiting_confirmation）的人工裁决面——Runs 面板确认/打回
 * 与 Transcript 流尾提示。RPC 组合契约：确认 = admin run/report done；
 * 打回 = node/update open（daemon 联动 item → interrupted）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, render, screen, waitFor, fireEvent } from '@testing-library/react'

afterEach(cleanup)

const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => {
  class ChannelMock {
    onmessage: ((message: unknown) => void) | null = null
    send(message: unknown) { this.onmessage?.(message) }
    toJSON() { return '__CHANNEL__' }
  }
  return {
    invoke: (...args: unknown[]) => invokeMock(...args),
    Channel: ChannelMock,
  }
})

import { RunsPanel } from '../src/RunsPanel'
import type { HomeInfo, RunDetail, RunSummary } from '../src/types'

const home: HomeInfo = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }

const runDetailOf = (items: Array<{ nodeId: string; state: string }>): RunDetail =>
  ({
    run: { id: 'RUN-1', status: 'running' },
    items: items.map(i => ({ nodeId: i.nodeId, state: i.state, attempts: 1 })),
  }) as never

const runs: RunSummary[] = [{ id: 'RUN-1', status: 'running' }] as never[]

beforeEach(() => {
  invokeMock.mockReset()
})

describe('RunsPanel 信任门裁决 (U14)', () => {
  const setup = (detail: RunDetail) => {
    let current = detail
    invokeMock.mockImplementation(async (_cmd: string, args?: { method?: string }) => {
      if (args?.method === 'run/get') return current
      // 动作后的刷新返回更新后的形态（由用例内改写 current）
      if (args?.method === 'run/report') { current = runDetailOf([{ nodeId: 'T-1', state: 'done' }]); return {} }
      if (args?.method === 'node/update') { current = runDetailOf([{ nodeId: 'T-1', state: 'interrupted' }]); return {} }
      return {}
    })
    return render(
      <RunsPanel home={home} runs={runs} fetchRun={async () => current} onChanged={() => {}} />,
    )
  }

  it('awaiting_confirmation item → 确认按钮触发 admin run/report done', async () => {
    setup(runDetailOf([{ nodeId: 'T-1', state: 'awaiting_confirmation' }]))
    fireEvent.click(screen.getByText('RUN-1'))
    const confirm = await waitFor(() => screen.getByRole('button', { name: '确认' }))
    fireEvent.click(confirm)
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(c => (c[1] as { method: string }).method === 'run/report')
      expect(call).toBeTruthy()
      const params = (call![1] as { params: Record<string, unknown> }).params
      expect(params).toMatchObject({ homeId: 'h1', runId: 'RUN-1', nodeId: 'T-1', outcome: 'done' })
    })
    // 刷新后不再有待确认按钮
    await waitFor(() => expect(screen.queryByRole('button', { name: '确认' })).toBeNull())
    expect(screen.getByText('done')).toBeTruthy()
  })

  it('打回按钮触发 node/update open（daemon 联动 interrupted）', async () => {
    setup(runDetailOf([{ nodeId: 'T-1', state: 'awaiting_confirmation' }]))
    fireEvent.click(screen.getByText('RUN-1'))
    const reject = await waitFor(() => screen.getByRole('button', { name: '打回' }))
    fireEvent.click(reject)
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(c => (c[1] as { method: string }).method === 'node/update')
      expect(call).toBeTruthy()
      const params = (call![1] as { params: Record<string, unknown> }).params
      expect(params).toMatchObject({ homeId: 'h1', nodeId: 'T-1', changes: { status: 'open' } })
    })
    await waitFor(() => expect(screen.getByText('interrupted')).toBeTruthy())
  })

  it('普通状态 item 不渲染裁决按钮', async () => {
    setup(runDetailOf([{ nodeId: 'T-2', state: 'running' }]))
    fireEvent.click(screen.getByText('RUN-1'))
    await waitFor(() => expect(screen.getByText('running')).toBeTruthy())
    expect(screen.queryByRole('button', { name: '确认' })).toBeNull()
    expect(screen.queryByRole('button', { name: '打回' })).toBeNull()
  })
})
