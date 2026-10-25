/**
 * Part B（P3 清理）的前端用例：降级横幅与「确认并归档」（B6）、强制删除的
 * 释放结果、停止全部提示与焦点、续跑失败后补接（B7）。mock 边界与
 * exec-plane.spec 相同；既有 spec 不改。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { AgentRecord } from '../src/types'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

const invokeMock = vi.fn()
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
vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn().mockResolvedValue(true),
  requestPermission: vi.fn().mockResolvedValue('granted'),
  sendNotification: vi.fn(),
}))

import { AgentsPanel } from '../src/AgentsPanel'
import { LeaseNoticeBanner } from '../src/agentUi'

function agent(over: Partial<AgentRecord> = {}, runId: string | null = null): AgentRecord {
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
        runId,
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

describe('B6 legacy-degraded banner (§41)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const corruptLegacy = {
    path: '/rt/agents/h_test/agents.json',
    present: true,
    parseable: false,
    corruptEvidence: [],
  }

  it('旧文件损坏：如实说明路径与冻结，不再声称「已降级为空列表」', () => {
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: vi.fn() })
    expect(screen.getByText(/\/rt\/agents\/h_test\/agents\.json/)).toBeTruthy()
    expect(screen.getByText(/绑定清理已冻结/)).toBeTruthy()
    expect(screen.queryByText(/已降级为空列表/)).toBeNull()
  })

  it('「确认并归档」是两步控件：先确认、可取消，确认后才调用；成功显示可关闭的状态行', async () => {
    const archive = vi.fn().mockResolvedValue(['agents.json.1760000000000', 'agents.json.corrupt-1'])
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: archive })
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    expect(archive).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: '取消' }))
    expect(screen.getByRole('button', { name: '确认并归档' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    expect(archive).toHaveBeenCalledTimes(1)
    const done = screen.getByText(/已归档 2 个文件到 \.recovered\//)
    expect(done.textContent).toContain('agents.json.corrupt-1')
    fireEvent.click(screen.getByRole('button', { name: '知道了' }))
    expect(screen.queryByText(/已归档 2 个文件/)).toBeNull()
  })

  it('归档进行中按钮禁用', async () => {
    const archive = vi.fn(() => new Promise<string[]>(() => {}))
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: archive })
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    expect(screen.getByRole('button', { name: '归档中…' })).toHaveProperty('disabled', true)
  })

  it('归档失败：错误就地显示在横幅里、按钮旁，按钮复位可重试', async () => {
    const archive = vi.fn().mockRejectedValue('rename: Permission denied')
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: archive })
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    const inline = screen.getByText(/归档失败：rename: Permission denied/)
    expect(inline.closest('.degraded-banner')).not.toBeNull()
    expect(screen.getByRole('button', { name: '确认并归档' })).toBeTruthy()
  })

  it('空列表分支同样显示横幅', () => {
    renderPanel({
      agents: [],
      parseFailed: true,
      legacyUnmigrated: corruptLegacy,
      onArchiveLegacy: vi.fn(),
    })
    expect(screen.getByRole('button', { name: '确认并归档' })).toBeTruthy()
  })

  it('可解析的旧文件：提示重启自动迁移，无归档入口', () => {
    renderPanel({
      parseFailed: true,
      legacyUnmigrated: { ...corruptLegacy, parseable: true },
      onArchiveLegacy: vi.fn(),
    })
    expect(screen.getByText(/重启桌面端会自动迁移/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: '确认并归档' })).toBeNull()
  })

  it('单个 agent 记录损坏：写明目录与留证文件名和解冻办法；横幅不依赖 parseFailed', () => {
    const { unmount } = renderPanel({
      parseFailed: false,
      corruptAgents: [
        {
          agentId: 'agent-exec-9',
          dir: '/rt/agents/h_test/agent-exec-9',
          evidence: ['agent.json.corrupt-1760000000000'],
        },
      ],
    })
    const line = screen.getByText(/\/rt\/agents\/h_test\/agent-exec-9/)
    expect(line.textContent).toContain('agent.json.corrupt-1760000000000')
    expect(line.textContent).toContain('下次刷新')
    unmount()
    renderPanel({ parseFailed: true })
    expect(screen.getByText(/部分 agent 记录读取失败；绑定清理已冻结/)).toBeTruthy()
  })
})

describe('B6 archive store action (§41)', () => {
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

  it('archiveLegacyAgents → agents_archive_legacy，随后重拉 agents 与 bindings，返回文件名', async () => {
    const { result, unmount } = await setupStore()
    invokeMock.mockClear()
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === 'agents_archive_legacy' ? { archived: ['agents.json.1'] } : {},
    )
    let archived: unknown
    await act(async () => {
      archived = await result.current.archiveLegacyAgents()
    })
    expect(archived).toEqual(['agents.json.1'])
    const cmds = invokeMock.mock.calls.map(c => c[0])
    expect(invokeMock).toHaveBeenCalledWith('agents_archive_legacy', { homeId: 'h1' })
    expect(cmds.indexOf('agents_archive_legacy')).toBeLessThan(cmds.lastIndexOf('agents_list'))
    expect(cmds).toContain('bindings_get')
    unmount()
  })

  it('归档失败：archiveLegacyAgents 抛出，不写全局 error（错误由横幅就地呈现）', async () => {
    const { result, unmount } = await setupStore()
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'agents_archive_legacy') throw 'rename: Permission denied'
      return {}
    })
    await act(async () => {
      await expect(result.current.archiveLegacyAgents()).rejects.toBe('rename: Permission denied')
    })
    expect(result.current.state.error).toBeNull()
    unmount()
  })
})

describe('B7 force-delete release results (§47)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const setupStoreWithAgents = async (agents: AgentRecord[], deleteReply: (agentId: string) => unknown) => {
    invokeMock.mockImplementation(async (cmd: string, args?: { agentId?: string }) => {
      if (cmd === 'agents_list') return { agents, agentSeq: agents.length, parseFailed: false }
      if (cmd === 'agents_delete') return deleteReply(args?.agentId ?? '')
      return {}
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    await act(async () => {
      await result.current.selectHome(home)
      await result.current.refreshAgents()
    })
    return { result, unmount }
  }

  it('释放失败的租约进全局通知列表（按被删 agent 追加、不覆盖），列出 exec 与 run，可逐条关闭', async () => {
    const a = agent({ id: 'agent-a', displayName: 'Agent 1', status: 'failed', attachment: 'none', lastExecId: 'exec-a' }, 'RUN-7')
    a.execs[0].execId = 'exec-a'
    const b = agent({ id: 'agent-b', displayName: 'Agent 2', status: 'failed', attachment: 'none', lastExecId: 'exec-b' }, 'RUN-8')
    b.execs[0].execId = 'exec-b'
    const { result, unmount } = await setupStoreWithAgents([a, b], agentId => ({
      deleted: agentId,
      leasesReleased: 0,
      leaseReleaseFailures: [`exec ${agentId === 'agent-a' ? 'exec-a' : 'exec-b'}: daemon down`],
    }))
    await act(async () => {
      await result.current.deleteAgent('agent-a', true)
    })
    await act(async () => {
      await result.current.deleteAgent('agent-b', true)
    })
    const notices = result.current.state.leaseNotices
    expect(notices.map(n => n.agentId)).toEqual(['agent-a', 'agent-b'])
    expect(notices[0].text).toContain('exec-a（RUN-7）')
    expect(notices[0].text).toContain('到 Runs 页')
    expect(notices[0].runIds).toEqual(['RUN-7'])
    act(() => {
      result.current.dismissLeaseNotice('agent-a')
    })
    expect(result.current.state.leaseNotices.map(n => n.agentId)).toEqual(['agent-b'])
    unmount()
  })

  it('store.deleteAgent 返回 agents_delete 的释放结果；全部释放成功时不产生通知', async () => {
    const reply = { deleted: 'agent-exec-1', leasesReleased: 1, leaseReleaseFailures: [] }
    const { result, unmount } = await setupStoreWithAgents([agent()], () => reply)
    let returned: unknown
    await act(async () => {
      returned = await result.current.deleteAgent('agent-exec-1', true)
    })
    expect(returned).toEqual(reply)
    expect(result.current.state.leaseNotices).toEqual([])
    unmount()
  })

  it('LeaseNoticeBanner：每条通知带「前往 Runs」与「知道了」', () => {
    const openRuns = vi.fn()
    const dismiss = vi.fn()
    render(
      <LeaseNoticeBanner
        notices={[
          { agentId: 'agent-a', homeId: 'h1', runIds: ['RUN-7'], text: '已删除 Agent 1，但 1 个租约释放失败、已被放弃：exec-a（RUN-7）。' },
          { agentId: 'agent-b', homeId: 'h2', runIds: ['RUN-8'], text: '已删除 Agent 2，但 1 个租约释放失败、已被放弃：exec-b（RUN-8）。' },
        ]}
        onDismiss={dismiss}
        onOpenRuns={openRuns}
      />,
    )
    expect(screen.getByText(/exec-a（RUN-7）/)).toBeTruthy()
    expect(screen.getByText(/exec-b（RUN-8）/)).toBeTruthy()
    fireEvent.click(screen.getAllByRole('button', { name: '前往 Runs' })[0])
    // §49 lease-notice-switch-home：「前往 Runs」携带该条通知的 homeId——
    // App 侧据此先切 home 再打开 Runs（通知跨 home 保留时开当前 home 的
    // Runs 找不到被放弃的 run）
    expect(openRuns).toHaveBeenCalledWith('h1')
    fireEvent.click(screen.getAllByRole('button', { name: '前往 Runs' })[1])
    expect(openRuns).toHaveBeenCalledWith('h2')
    fireEvent.click(screen.getAllByRole('button', { name: '知道了' })[1])
    expect(dismiss).toHaveBeenCalledWith('agent-b')
  })
})

describe('B7 stop-all notice and focus (§39)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const confirmStopAll = async () => {
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
  }

  it('结果区域常驻挂载（role=status、aria-live=polite），只换文字不重新挂载', async () => {
    const stopAll = vi.fn().mockResolvedValue({ signaled: 1, failed: [] })
    renderPanel({ onStopAll: stopAll })
    const region = screen.getByRole('status')
    expect(region.getAttribute('aria-live')).toBe('polite')
    expect(region.textContent).toBe('')
    await confirmStopAll()
    expect(screen.getByRole('status')).toBe(region)
    expect(region.textContent).toContain('已向 1 个 agent 发出停止信号')
  })

  it('相同提示连续出现时计时器按 seq 重置，不提前清空（fe-stopall-toast）', async () => {
    vi.useFakeTimers()
    const stopAll = vi.fn().mockResolvedValue({ signaled: 1, failed: [] })
    renderPanel({ onStopAll: stopAll })
    await confirmStopAll()
    await act(async () => {
      vi.advanceTimersByTime(4000)
    })
    await confirmStopAll()
    await act(async () => {
      vi.advanceTimersByTime(2000) // 距第一次 6s、距第二次 2s
    })
    expect(screen.getByRole('status').textContent).toContain('已向 1 个 agent 发出停止信号')
    await act(async () => {
      vi.advanceTimersByTime(3100)
    })
    expect(screen.getByRole('status').textContent).toBe('')
  })

  it('含失败的结果进需手动关闭的 alert，不随 5 秒消失', async () => {
    vi.useFakeTimers()
    const stopAll = vi.fn().mockResolvedValue({ signaled: 1, failed: ['Agent 2 (exec-2): kill EPERM'] })
    renderPanel({ onStopAll: stopAll })
    await confirmStopAll()
    await act(async () => {
      vi.advanceTimersByTime(6000)
    })
    const alert = screen.getByText(/1 个停止失败：Agent 2 \(exec-2\): kill EPERM/)
    expect(alert.closest('[role="alert"]')).not.toBeNull()
    expect(screen.getByRole('status').textContent).toBe('')
    fireEvent.click(screen.getByRole('button', { name: '知道了' }))
    expect(screen.queryByText(/1 个停止失败/)).toBeNull()
  })

  it('第二步出现时焦点落在「取消」；Esc 取消并把焦点还给「停止全部」（fe-a11y-stopall）', () => {
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '取消' }))
    fireEvent.keyDown(document.activeElement as Element, { key: 'Escape' })
    const trigger = screen.getByRole('button', { name: '停止全部' })
    expect(document.activeElement).toBe(trigger)
  })

  it('确认并完成后焦点回到「停止全部」', async () => {
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    await confirmStopAll()
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '停止全部' }))
  })

  it('3s 自动复位后焦点回到「停止全部」', async () => {
    vi.useFakeTimers()
    renderPanel({ onStopAll: vi.fn() })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      vi.advanceTimersByTime(3100)
    })
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '停止全部' }))
  })
})

describe('§49 5s poll side effects: refreshAgents skips unchanged results', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('silent 轮询结果结构未变时跳过 setState——agents 引用保持稳定，面板 effect 不重跑；变化仍更新', async () => {
    const failed = agent({ id: 'agent-a', status: 'failed', attachment: 'none' })
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === 'agents_list'
        ? { agents: [failed], agentSeq: 1, parseFailed: false }
        : {},
    )
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    await act(async () => {
      await result.current.selectHome(home)
    })
    const first = result.current.state.agents
    expect(first.map(a => a.id)).toEqual(['agent-a'])
    // 5s 轮询：同样的数据再来两轮——引用必须稳定（AgentsPanel 键在
    // [agents] 上的 effect——run/list 兜底扫描、exec_transcript_tail 回载、
    // exec_attach 重试——不会每 5s 重跑）
    await act(async () => {
      await result.current.refreshAgents({ silent: true })
    })
    expect(result.current.state.agents).toBe(first)
    await act(async () => {
      await result.current.refreshAgents({ silent: true })
    })
    expect(result.current.state.agents).toBe(first)
    // 数据变化（agent 状态翻转）仍要落到 state
    const exited = { ...failed, status: 'exited' as const }
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === 'agents_list'
        ? { agents: [exited], agentSeq: 1, parseFailed: false }
        : {},
    )
    await act(async () => {
      await result.current.refreshAgents({ silent: true })
    })
    expect(result.current.state.agents).not.toBe(first)
    expect(result.current.state.agents[0].status).toBe('exited')
    unmount()
  })
})

describe('B7 resume failure re-attach (§39)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('续跑派发失败后，派发期间被推迟的 live agent 立即补接（fe-resume-fail-reattach）', async () => {
    let failStart!: (error: unknown) => void
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'exec_start') return new Promise((_, reject) => { failStart = reject })
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    let dispatch!: Promise<unknown>
    act(() => {
      dispatch = result.current
        .startDispatch({ home, nodeId: 'TICKET-1', harness: 'pi', unattended: true, agentId: 'agent-x' })
        .catch(() => undefined)
    })
    act(() => {
      result.current.attachTranscript('exec-live', 'h1')
    })
    expect(invokeMock).not.toHaveBeenCalledWith('exec_attach', expect.anything())
    await act(async () => {
      failStart(new Error('目标 agent 仍在运行，拒绝并发派发'))
      await dispatch
    })
    expect(invokeMock).toHaveBeenCalledWith(
      'exec_attach',
      expect.objectContaining({ execId: 'exec-live', homeId: 'h1' }),
    )
    unmount()
  })
})
