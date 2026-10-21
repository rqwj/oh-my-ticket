/**
 * 执行面子项目 1 · Task 11a（P-10 + review F7/F14/R2-7/R2-9）：Agents 面板的
 * 「停止全部」（两步确认 + 进行中 + 结果反馈）与重启后接回实时流。只测新增
 * 行为；失败/orphan/删除文案（F8/F11/F3）属 Task 11b。既有 agents.spec 不改。
 * Mock 边界沿用 agents.spec 的 tauri invoke 工厂（Channel 可 new、onmessage
 * 可注入；通知插件 mock 同款）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { AgentRecord, HomeInfo } from '../src/types'

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
    let resolve!: (n: { signaled: number; failed: string[] }) => void
    const stopAll = vi.fn(
      () => new Promise<{ signaled: number; failed: string[] }>(r => { resolve = r }),
    )
    renderPanel({ onStopAll: stopAll })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    expect(stopAll).not.toHaveBeenCalled()
    expect(screen.getByText(/确认停止 1 个 agent/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    expect(stopAll).toHaveBeenCalledTimes(1)
    expect(screen.getByRole('button', { name: '停止中…' })).toHaveProperty('disabled', true)
    await act(async () => {
      resolve({ signaled: 1, failed: [] })
    })
    expect(screen.getByText(/已向 1 个 agent 发出停止信号/)).toBeTruthy()
  })

  /** F-EXEC-12：部分失败必须如实提示（已停止 N 个、M 个失败）——此前
   * signaled 计数与失败明细一起被吞，UI 按「全部成功」提示。 */
  it('停止全部 partial failure surfaces both counts (F-EXEC-12)', async () => {
    renderPanel({
      onStopAll: vi.fn().mockResolvedValue({ signaled: 2, failed: ['agent b: kill EPERM'] }),
    })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(screen.getByText(/已向 2 个 agent 发出停止信号，1 个停止失败/)).toBeTruthy()
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
    // F-EXEC-5 residue：非 orphan 的失败说明仍如实指路「可续跑」
    expect(screen.getAllByText(/可续跑/).length).toBeGreaterThan(0)
  })

  /** F-EXEC-5 residue（§41）：orphan agent 的最新 exec 带 SHIM_LOST
   * failureDetail 时，行内失败说明不得再承诺「可续跑」（orphan 的 pid 仍
   * 存活，续跑会被服务端拒绝）——改为指路强制终止，且不给任何续跑入口。 */
  it('orphan row with shim-lost failure explains it without promising resume', () => {
    const orphan = agent({ status: 'failed', attachment: 'orphan' })
    orphan.execs[0].failureDetail = 'exec shim exited before finishing'
    orphan.execs[0].nodeId = 'TICKET-9' // 旧判据（非运行中 + 有 ticket）本会放行续跑
    const resume = vi.fn()
    renderPanel({ agents: [orphan], onResumeAgent: resume })
    // 失败说明仍在（SHIM_LOST 的可读解释）
    expect(screen.getAllByText(/执行进程异常退出/).length).toBeGreaterThan(0)
    // 但不得承诺可续跑；指路强制终止
    expect(screen.queryByText(/可续跑/)).toBeNull()
    expect(screen.getAllByText(/请先强制终止|强制终止/).length).toBeGreaterThan(0)
    // 无任何续跑入口
    expect(screen.queryByRole('button', { name: '继续执行' })).toBeNull()
    expect(resume).not.toHaveBeenCalled()
  })

  it('orphan badge no longer promises a live tail', () => {
    renderPanel({ agents: [agent({ status: 'failed', attachment: 'orphan' })] })
    const badge = screen.getByText('不受控·只读')
    expect(badge.getAttribute('title')).toMatch(/不会再有新输出/)
    expect(screen.queryByText('不受控（重启遗留）')).toBeNull()
  })

  /** F-EXEC-5(c)：orphan agent 不可续跑——隐藏「继续执行」，改为指路
   * 「请先强制终止」（append 会把 attachment 覆盖回 Live，丢失 orphan 追踪）。 */
  it('orphan agent has no resume button and points to force-kill instead', () => {
    const orphan = agent({ status: 'failed', attachment: 'orphan' })
    orphan.execs[0].nodeId = 'TICKET-9' // 旧判据（非运行中 + 有 ticket）本会放行续跑
    const resume = vi.fn()
    renderPanel({ agents: [orphan], onResumeAgent: resume })
    expect(screen.queryByRole('button', { name: '继续执行' })).toBeNull()
    expect(screen.getAllByText(/请先强制终止/).length).toBeGreaterThan(0)
    expect(resume).not.toHaveBeenCalled()
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

// ── F-EXEC-12：exec_stop_all 返回形状 { signaled, failed } 的 store 映射 ────

describe('exec plane stop-all mapping (F-EXEC-12)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const renderStoreWithHome = async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'daemon_homes') {
        return {
          homes: [{ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }],
          homeDir: '/tmp',
        }
      }
      return {}
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, waitFor } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    await waitFor(() => expect(result.current.state.activeHome?.homeId).toBe('h1'))
    return { result, unmount }
  }

  const stopAllWith = async (reply: unknown) => {
    const { result, unmount } = await renderStoreWithHome()
    // home 拉取完成后再换路由：只影响 stopAll 的 exec_stop_all 调用点
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === 'exec_stop_all' ? reply : {},
    )
    let outcome: unknown
    await act(async () => {
      outcome = await result.current.stopAll()
    })
    return { outcome, state: result.current.state, unmount }
  }

  it('全失败（signaled=0 且有 failed）→ null + 全局 error（R2-7 语义保留）', async () => {
    const { outcome, state, unmount } = await stopAllWith({
      signaled: 0,
      failed: ['agent a1: 记录损坏', 'exec b2: kill(1, SIGTERM): EPERM'],
    })
    expect(outcome).toBeNull()
    expect(state.error).toContain('记录损坏')
    expect(state.error).toContain('kill(1, SIGTERM)')
    unmount()
  })

  it('部分失败 → 返回 { signaled, failed } 供面板提示，不置全局 error', async () => {
    const { outcome, state, unmount } = await stopAllWith({
      signaled: 1,
      failed: ['exec b2: kill(1, SIGTERM): EPERM'],
    })
    expect(outcome).toEqual({ signaled: 1, failed: ['exec b2: kill(1, SIGTERM): EPERM'] })
    expect(state.error).toBeNull()
    unmount()
  })

  it('全部成功 → 返回计数与空 failed', async () => {
    const { outcome, state, unmount } = await stopAllWith({ signaled: 2, failed: [] })
    expect(outcome).toEqual({ signaled: 2, failed: [] })
    expect(state.error).toBeNull()
    unmount()
  })

  it('invoke 拒绝 → null + 全局 error（既有语义不变）', async () => {
    const { result, unmount } = await renderStoreWithHome()
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'exec_stop_all') throw new Error('daemon 不可达')
      return {}
    })
    let outcome: unknown
    await act(async () => {
      outcome = await result.current.stopAll()
    })
    expect(outcome).toBeNull()
    expect(result.current.state.error).toContain('daemon 不可达')
    unmount()
  })
})

// ── F-EXEC-5 residue（§41）：orphan 不得出现在任何续跑入口 ────────────────

describe('exec plane orphan resume residue (F-EXEC-5)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const dialogHome: HomeInfo = {
    homeId: 'h1',
    kind: 'workspace',
    path: '/tmp/ws/.omt',
    name: 'ws',
  }

  /** 派发弹窗的空闲 Agent optgroup 必须剔除 orphan——「空闲（可续跑）」与
   * 服务端 append_exec 的 orphan 守卫直接矛盾（§41：orphan 行仍显示可续跑）。 */
  it('执行弹窗「空闲 Agent（续跑）」不含 orphan agent', async () => {
    const { ExecutionDialog } = await import('../src/ExecutionDialog')
    invokeMock.mockImplementation(
      async (cmd: string, args?: { method?: string }) => {
        if (cmd === 'harness_list') {
          return {
            harnesses: [
              {
                id: 'omp',
                label: 'OMP',
                installed: true,
                detectedPath: '/usr/local/bin/omp',
                detectedVersion: 'v1.0',
                enabled: true,
                binarySource: 'detected',
                manualPath: null,
                state: 'ready',
                unattendedFlag: ['--yolo'],
              },
            ],
          }
        }
        if (cmd === 'harness_list_models') return { models: [] }
        if (cmd === 'omt_call' && args?.method === 'ui/filters-get') {
          return { filters: { approvalAck: { omp: true } } }
        }
        if (cmd === 'omt_call' && args?.method === 'node/get') {
          return { node: {}, children: [], runs: [] }
        }
        return {}
      },
    )
    const orphan = agent({ id: 'agent-orphan', status: 'failed', attachment: 'orphan' })
    const idle = agent({ id: 'agent-idle', status: 'exited', attachment: 'none' })
    render(
      <ExecutionDialog
        home={dialogHome}
        nodeId="T-1"
        nodeTitle="登录修复"
        nodes={[]}
        agents={[orphan, idle]}
        startDispatch={vi.fn().mockResolvedValue({ execId: 'e', agentId: 'a', runId: 'r', warnings: [] })}
        onClose={vi.fn()}
        onDispatched={vi.fn()}
        onSelectExistingAgent={vi.fn()}
        onOpenHarnessSettings={vi.fn()}
      />,
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    const select = screen.getAllByRole('combobox')[0] as HTMLSelectElement
    const values = Array.from(select.options).map(o => o.value)
    expect(values).toContain('agent:agent-idle')
    expect(values).not.toContain('agent:agent-orphan')
  })
})
