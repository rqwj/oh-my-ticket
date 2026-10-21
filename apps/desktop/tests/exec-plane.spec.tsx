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

  /** F-EXEC-12 + §43 P3：部分失败必须如实提示（已停止 N 个、M 个失败）并
   * 带失败明细（agent id 级，Rust 失败串形如 `agent <id>: <err>`）——此前
   * signaled 计数与失败明细一起被吞，UI 按「全部成功」提示。 */
  it('停止全部 partial failure surfaces both counts and details (F-EXEC-12, §43 P3)', async () => {
    renderPanel({
      onStopAll: vi.fn().mockResolvedValue({ signaled: 2, failed: ['agent b: kill EPERM'] }),
    })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(
      screen.getByText(/已向 2 个 agent 发出停止信号，1 个停止失败：agent b: kill EPERM/),
    ).toBeTruthy()
  })

  /** §43 P3：面板不得只依赖 store 的全失败 null 折算——outcome 本身携带
   * `signaled=0 && failed>0` 时也要按失败处理并带明细（而非「没有运行中」）。 */
  it('停止全部 full failure outcome reports the failure details (§43 P3)', async () => {
    renderPanel({
      onStopAll: vi
        .fn()
        .mockResolvedValue({ signaled: 0, failed: ['agent-a: kill(1, SIGTERM): EPERM', 'agent-b: 记录损坏'] }),
    })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(
      screen.getByText(/停止失败：agent-a: kill\(1, SIGTERM\): EPERM；agent-b: 记录损坏/),
    ).toBeTruthy()
    // 不得误报「没有运行中的 agent」
    expect(screen.queryByText(/当前没有运行中的 agent/)).toBeNull()
  })

  /** §43 P3：失败明细过长截断——只列前 2 条 + 「等 N 个」，第三条的 agent id 不出现。 */
  it('停止全部 failure details truncate beyond the first two entries (§43 P3)', async () => {
    renderPanel({
      onStopAll: vi.fn().mockResolvedValue({
        signaled: 1,
        failed: ['agent-a: kill EPERM', 'agent-b: 记录损坏', 'agent-c: kill ESRCH'],
      }),
    })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(
      screen.getByText(/已向 1 个 agent 发出停止信号，3 个停止失败：agent-a: kill EPERM；agent-b: 记录损坏；等 3 个/),
    ).toBeTruthy()
    expect(screen.queryByText(/agent-c/)).toBeNull()
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

// ── F-EXEC-14(4)：删除的待对账租约守卫——两段式「放弃租约并删除」──────────

describe('exec plane delete lease guard (F-EXEC-14(4))', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  /** 可删除的 agent：终态（exited/none）——运行中/orphan 不给删除入口。 */
  const deletable = (over: Partial<AgentRecord> = {}) =>
    agent({ status: 'exited', attachment: 'none', ...over })

  /** 非租约拒绝：原错误就地呈现，不得误给「放弃租约」入口（force 只绕过
   * 待对账租约守卫，其余错误照旧）。 */
  it('非租约拒绝 → 原错误就地呈现，无放弃租约入口', async () => {
    renderPanel({
      agents: [deletable()],
      onDeleteAgent: vi.fn().mockRejectedValue(new Error('agents.json 正在被写入，请稍后重试')),
    })
    fireEvent.click(screen.getByRole('button', { name: '删除 Agent 1' }))
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }))
    await waitFor(() => expect(screen.getByText(/正在被写入/)).toBeTruthy())
    expect(screen.queryByText(/放弃租约/)).toBeNull()
    // 未删除成功：弹窗保留
    expect(screen.getByRole('alertdialog', { name: '确认删除 agent' })).toBeTruthy()
  })

  /** 租约守卫拒绝 → 第二步确认「放弃租约并删除？」；确认以 force=true 重发，
   * 成功后关闭弹窗。 */
  it('租约守卫拒绝 → 第二步确认；确认以 force=true 重发并关闭弹窗', async () => {
    const del = vi
      .fn()
      .mockRejectedValueOnce(
        new Error('agent agent-exec-1 存在待对账的租约——请等待对账完成后再删除（或放弃租约强制删除）'),
      )
      .mockResolvedValueOnce(undefined)
    renderPanel({ agents: [deletable()], onDeleteAgent: del })
    fireEvent.click(screen.getByRole('button', { name: '删除 Agent 1' }))
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }))
    await waitFor(() => expect(del).toHaveBeenCalledTimes(1))
    // 第一步保持既有单参契约（force 缺省——store/bridge 侧按 false 走常规守卫）
    expect(del).toHaveBeenNthCalledWith(1, expect.objectContaining({ id: 'agent-exec-1' }))
    // 第二步：放弃租约的确认文案（含 确认/取消），原始长错误不重复刷屏
    expect(screen.getByText(/该 agent 有未释放的租约。放弃租约并删除？/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(del).toHaveBeenCalledTimes(2))
    expect(del).toHaveBeenNthCalledWith(2, expect.objectContaining({ id: 'agent-exec-1' }), true)
    await waitFor(() =>
      expect(screen.queryByRole('alertdialog', { name: '确认删除 agent' })).toBeNull(),
    )
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

  /** §43 P3 判别说明：旧 store（§41 前，`const count = await execStopAll(...)`
   * 后原样 return）对部分失败输入同样返回原对象——本测试以「精确 outcome +
   * 不置 error」+ 相邻的全失败折算（第二次调用 → null + error）组合判别：
   * 旧透传在全失败时会返回原对象而非 null，第二段在旧代码上必红。 */
  it('部分失败 → 返回 { signaled, failed } 供面板提示，不置全局 error；相邻全失败仍折算 null', async () => {
    const { outcome, state, unmount } = await stopAllWith({
      signaled: 1,
      failed: ['agent b2: kill(1, SIGTERM): EPERM'],
    })
    expect(outcome).toEqual({ signaled: 1, failed: ['agent b2: kill(1, SIGTERM): EPERM'] })
    expect(state.error).toBeNull()
    unmount()
    // 相邻判别（§43 P3）：同款全失败 wire 在 store 折算为 null + 全局 error
    const second = await stopAllWith({
      signaled: 0,
      failed: ['agent c3: kill(1, SIGTERM): EPERM'],
    })
    expect(second.outcome).toBeNull()
    expect(second.state.error).toContain('kill(1, SIGTERM)')
    second.unmount()
  })

  /** §43 P3 判别说明：逐属性断言返回形状——旧 store（§41 前）+ 旧 Rust 的
   * wire 是裸计数（number），`.failed` 为 undefined、`.signaled` 缺失，
   * 本测试在旧端到端契约上必红（reviewer：old code returned a plain
   * number）。旧 store 吃到对象 mock 时确会透传——成功路径与部分失败路径
   * 的差异在上一测试的第二段钉死。 */
  it('全部成功 → 返回 { signaled, failed: [] }（对象形状；裸计数透传时 failed 为 undefined）', async () => {
    const { outcome, state, unmount } = await stopAllWith({ signaled: 2, failed: [] })
    expect(outcome).not.toBeNull()
    const o = outcome as { signaled?: unknown; failed?: unknown }
    expect(o.signaled).toBe(2)
    expect(Array.isArray(o.failed)).toBe(true)
    expect(o.failed).toEqual([])
    expect(state.error).toBeNull()
    unmount()
  })

  /** §43 P3：全失败也重拉 agents_list（防抖 400ms）——此前 repull 只在非
   * 全失败路径，全失败后行内残留的运行态不会被刷新。 */
  it('全失败（signaled=0 且有 failed）→ 也触发 agents_list 防抖重拉（§43 P3）', async () => {
    vi.useFakeTimers()
    try {
      invokeMock.mockReset()
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === 'daemon_homes') return { homes: [], homeDir: '/tmp' }
        if (cmd === 'exec_stop_all') {
          return { signaled: 0, failed: ['agent a1: kill(1, SIGTERM): EPERM'] }
        }
        return {}
      })
      const { useTreeStore } = await import('../src/store')
      const { renderHook } = await import('@testing-library/react')
      const { result, unmount } = renderHook(() => useTreeStore())
      const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
      await act(async () => {
        await result.current.selectHome(home)
      })
      const before = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
      let outcome: unknown
      await act(async () => {
        outcome = await result.current.stopAll()
      })
      expect(outcome).toBeNull()
      await act(async () => {
        vi.advanceTimersByTime(500)
      })
      const after = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
      expect(after).toBeGreaterThan(before)
      unmount()
    } finally {
      vi.useRealTimers()
    }
  })

  /** 行为护栏（非回归钉）：invoke 拒绝的 null + 全局 error 语义在旧 store
   * （§41 前）即存在，本测试无法判别新旧——它钉住的是「不得回归」的现状。 */
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

// ── F-EXEC-14(4)：deleteAgent 的 force 透传（agents_delete wire 形状）──────

describe('exec plane delete store (F-EXEC-14(4))', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const setupStore = async () => {
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    await act(async () => {
      await result.current.selectHome(home)
    })
    return { result, unmount }
  }

  it('deleteAgent force=true → agents_delete 带 force；成功后清选中态并重拉', async () => {
    const { result, unmount } = await setupStore()
    act(() => {
      result.current.selectAgent('agent-exec-1')
    })
    await act(async () => {
      await result.current.deleteAgent('agent-exec-1', true)
    })
    expect(invokeMock).toHaveBeenCalledWith('agents_delete', {
      homeId: 'h1',
      agentId: 'agent-exec-1',
      force: true,
    })
    expect(result.current.state.selectedAgentId).toBeNull()
    unmount()
  })

  it('deleteAgent 缺省 → force=false（Rust 侧守卫按常规执行）', async () => {
    const { result, unmount } = await setupStore()
    await act(async () => {
      await result.current.deleteAgent('agent-exec-2')
    })
    expect(invokeMock).toHaveBeenCalledWith('agents_delete', {
      homeId: 'h1',
      agentId: 'agent-exec-2',
      force: false,
    })
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
