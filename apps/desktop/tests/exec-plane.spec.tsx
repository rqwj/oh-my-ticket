/**
 * 执行面子项目 1 · Task 11a（P-10 + review F7/F14/R2-7/R2-9）：Agents 面板的
 * 「停止全部」（两步确认 + 进行中 + 结果反馈）与重启后接回实时流。只测新增
 * 行为；失败/orphan/删除文案（F8/F11/F3）属 Task 11b。既有 agents.spec 不改。
 * Mock 边界沿用 agents.spec 的 tauri invoke 工厂（Channel 可 new、onmessage
 * 可注入；通知插件 mock 同款）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { AgentRecord } from '../src/types'

afterEach(cleanup)

const invokeMock = vi.fn()
// 与 agents.spec 一致：mock 工厂必须可 new 且 onmessage 可注入（对齐
// @tauri-apps/api Channel 的消费方式：前端只赋 onmessage，Rust 侧推流）。
// 类内联在工厂里：vi.mock 被提升，具名顶层类会撞 TDZ（agents.spec 靠纯
// 动态 import 避开；本文件有静态 AgentsPanel import，只能内联）。
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: class {
    onmessage: ((message: unknown) => void) | null = null
    toJSON() {
      return '__CHANNEL__'
    }
  },
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
// U16：通知插件 mock（同 agents.spec 边界）
vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn().mockResolvedValue(true),
  requestPermission: vi.fn().mockResolvedValue('granted'),
  sendNotification: vi.fn(),
}))

import { AgentsPanel } from '../src/AgentsPanel'

function agent(over: Partial<AgentRecord> = {}): AgentRecord {
  return {
    id: 'agent-exec-1',
    displayName: 'Agent 1',
    title: null,
    harness: 'pi',
    status: 'starting',
    attachment: 'live',
    lastExecId: 'exec-1',
    lastSessionId: null,
    execs: [
      {
        execId: 'exec-1',
        nodeId: null,
        runId: null,
        startedAt: '1760000000000',
        exitCode: null,
        killReason: null,
        failureDetail: null,
        harnessSessionId: null,
        transcriptFile: null,
        binaryPath: null,
        binaryArgs: [],
        pid: 123,
        processStartId: 'macos:1:1',
      },
    ],
    ...over,
  }
}

function renderPanel(props: Partial<Parameters<typeof AgentsPanel>[0]> = {}) {
  return render(
    <AgentsPanel
      agents={[agent()]}
      parseFailed={false}
      selectedAgentId="agent-exec-1"
      nodes={[]}
      fetchRun={vi.fn().mockResolvedValue({ run: { id: 'RUN-1', status: 'running' }, items: [] })}
      eventsOf={() => []}
      homeId="h_test"
      loadHistoryTail={vi.fn()}
      onSelectAgent={vi.fn()}
      onStopAgent={vi.fn()}
      onResumeAgent={vi.fn()}
      onDeleteAgent={vi.fn()}
      onKillOrphan={vi.fn()}
      onOpenHarnessSettings={vi.fn()}
      {...props}
    />,
  )
}

describe('exec plane P-10', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('re-attaches a live agent that has no events in this session', () => {
    const attach = vi.fn()
    const history = vi.fn()
    renderPanel({ attachTranscript: attach, loadHistoryTail: history })
    expect(attach).toHaveBeenCalledWith('exec-1', 'h_test')
    expect(history).not.toHaveBeenCalled()
  })

  it('does not attach a finished agent (history tail path instead)', () => {
    const attach = vi.fn()
    const history = vi.fn()
    renderPanel({
      agents: [agent({ status: 'exited', attachment: 'none' })],
      attachTranscript: attach,
      loadHistoryTail: history,
    })
    expect(attach).not.toHaveBeenCalled()
    expect(history).toHaveBeenCalledWith('exec-1', 'h_test')
  })

  it('停止全部 needs a second confirm, shows progress, then the result', async () => {
    let resolve!: (n: number) => void
    const stopAll = vi.fn(() => new Promise<number>(r => { resolve = r }))
    renderPanel({ onStopAll: stopAll })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    expect(stopAll).not.toHaveBeenCalled()
    expect(screen.getByText(/确认停止 1 个 agent/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    expect(stopAll).toHaveBeenCalledTimes(1)
    expect(screen.getByRole('button', { name: '停止中…' })).toHaveProperty('disabled', true)
    await act(async () => {
      resolve(1)
    })
    expect(screen.getByText(/已向 1 个 agent 发出停止信号/)).toBeTruthy()
  })

  it('停止全部 failure is reported as failure, not as 0 stopped (review R2-7)', async () => {
    renderPanel({ onStopAll: vi.fn().mockResolvedValue(null) })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(screen.getByText(/停止失败/)).toBeTruthy()
    expect(screen.queryByText(/已向 0 个/)).toBeNull()
  })

  it('hides 停止全部 when nothing is running', () => {
    renderPanel({ agents: [agent({ status: 'exited', attachment: 'none' })], onStopAll: vi.fn() })
    expect(screen.queryByRole('button', { name: '停止全部' })).toBeNull()
  })

  it('explains a failed agent whose shim was lost', () => {
    const failed = agent({ status: 'failed', attachment: 'none' })
    failed.execs[0].failureDetail = 'exec shim exited before finishing'
    renderPanel({ agents: [failed] })
    expect(screen.getAllByText(/执行进程异常退出/).length).toBeGreaterThan(0)
  })

  it('orphan badge no longer promises a live tail', () => {
    renderPanel({ agents: [agent({ status: 'failed', attachment: 'orphan' })] })
    const badge = screen.getByText('不受控·只读')
    expect(badge.getAttribute('title')).toMatch(/不会再有新输出/)
    expect(screen.queryByText('不受控（重启遗留）')).toBeNull()
  })

  it('renders without the new props (existing callers unaffected)', () => {
    renderPanel()
    expect(screen.queryByRole('button', { name: '停止全部' })).toBeNull()
  })
})

describe('exec plane store guards', () => {
  /** review R2-9：派发（含续跑的就绪握手，最长 30s）进行中不接回实时流——否则
   * exec_start 返回后会出现第二个跟随者，事件全部显示两次。 */
  it('does not start a second follower while a dispatch is in flight', async () => {
    let finishStart!: (v: unknown) => void
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'exec_start') return new Promise(resolve => { finishStart = resolve })
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    let dispatch!: Promise<unknown>
    act(() => {
      dispatch = result.current.startDispatch({ home, nodeId: 'TICKET-1', harness: 'pi', unattended: true, agentId: 'agent-x' })
    })
    act(() => { result.current.attachTranscript('exec-new', 'h1') })
    expect(invokeMock).not.toHaveBeenCalledWith('exec_attach', expect.anything())
    await act(async () => {
      finishStart({ execId: 'exec-new', agentId: 'agent-x', runId: 'RUN-1', warnings: [] })
      await dispatch
    })
    act(() => { result.current.attachTranscript('exec-new', 'h1') })
    expect(invokeMock).not.toHaveBeenCalledWith('exec_attach', expect.anything())
    act(() => { result.current.attachTranscript('exec-other', 'h1') })
    expect(invokeMock).toHaveBeenCalledWith('exec_attach', expect.objectContaining({ execId: 'exec-other' }))
  })
})
