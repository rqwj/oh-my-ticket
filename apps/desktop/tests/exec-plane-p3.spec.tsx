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
