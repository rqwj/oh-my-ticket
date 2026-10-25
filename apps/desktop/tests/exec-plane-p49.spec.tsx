/**
 * §49 剩余 P3 的前端用例：停止全部 3s 复位不抢焦点、停止失败 / 归档提示
 * 按 home 生命周期、租约通知「前往 Runs」先切 home（App 级全链路）、归档
 * 成功提示的常驻 status 区域。mock 边界与 exec-plane-p3.spec 相同。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
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
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
  ask: vi.fn(),
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

function baseProps() {
  return {
    agents: [agent()],
    parseFailed: false,
    selectedAgentId: 'agent-exec-1',
    nodes: [] as never[],
    fetchRun: vi.fn().mockResolvedValue({ run: { id: 'RUN-1', status: 'running' }, items: [] }),
    eventsOf: () => [],
    homeId: 'h_a',
    loadHistoryTail: vi.fn(),
    onSelectAgent: vi.fn(),
    onStopAgent: vi.fn(),
    onResumeAgent: vi.fn().mockResolvedValue(undefined),
    onDeleteAgent: vi.fn().mockResolvedValue(undefined),
    onKillOrphan: vi.fn(),
    onOpenHarnessSettings: vi.fn(),
  }
}

function renderPanel(over: Partial<Parameters<typeof AgentsPanel>[0]> = {}) {
  const props = { ...baseProps(), ...over }
  return render(<AgentsPanel {...props} />)
}

describe('§49 stopall-reset-focus-steal：3s 自动复位不抢焦点', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('焦点已移到别处（其他按钮）时复位不得拉回「停止全部」', async () => {
    vi.useFakeTimers()
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '取消' }))
    // 3s 窗口内用户把焦点移到别处（行内「终止」按钮——列表行与 transcript
    // 各有一个，取任一仍在确认区之外的按钮即可）
    const elsewhere = screen.getAllByRole('button', { name: '终止' })[0]
    elsewhere.focus()
    await act(async () => {
      vi.advanceTimersByTime(3100)
    })
    expect(document.activeElement).toBe(elsewhere, '不得抢回焦点——用户已在别处操作')
  })

  it('焦点仍在确认区（复位后落回 body）时才恢复到「停止全部」', async () => {
    vi.useFakeTimers()
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      vi.advanceTimersByTime(3100)
    })
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '停止全部' }))
  })
})

describe('§49 notices-leak-across-homes：停止失败 / 归档提示按 home 生命周期', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('停止失败警告在切换 home 后清空', async () => {
    vi.useFakeTimers()
    const props = {
      ...baseProps(),
      onStopAll: vi.fn().mockResolvedValue({ signaled: 0, failed: ['Agent 1 (exec-1): kill EPERM'] }),
    }
    const { rerender } = render(<AgentsPanel {...props} />)
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(screen.getByText(/停止失败：Agent 1/)).toBeTruthy()
    // 同一面板实例、换 home（App 只换 homeId prop）——A home 的失败不得残留
    rerender(<AgentsPanel {...props} homeId="h_b" />)
    expect(screen.queryByText(/停止失败/)).toBeNull()
  })

  it('归档成功提示在切换 home 后清空', async () => {
    const corruptLegacy = {
      path: '/rt/agents/h_a/agents.json',
      present: true,
      parseable: false,
      corruptEvidence: [],
    }
    const archive = vi.fn().mockResolvedValue(['agents.json.corrupt-1'])
    const props = {
      ...baseProps(),
      parseFailed: true,
      legacyUnmigrated: corruptLegacy,
      onArchiveLegacy: archive,
    }
    const { rerender } = render(<AgentsPanel {...props} />)
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    expect(screen.getByText(/已归档 1 个文件/)).toBeTruthy()
    rerender(<AgentsPanel {...props} homeId="h_b" />)
    expect(screen.queryByText(/已归档/)).toBeNull()
  })
})

describe('§49 archive-notice-live：归档成功提示进常驻 status 区域', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('区域先于内容挂载、只改文字、不叠加第二个 status 区域（读屏可播报）', async () => {
    const corruptLegacy = {
      path: '/rt/agents/h_a/agents.json',
      present: true,
      parseable: false,
      corruptEvidence: [],
    }
    const archive = vi.fn().mockResolvedValue(['agents.json.corrupt-1'])
    renderPanel({
      agents: [],
      parseFailed: true,
      legacyUnmigrated: corruptLegacy,
      onArchiveLegacy: archive,
    })
    // 前置：区域已挂载且为空（旧实现此时没有任何 status 区域）
    const region = screen.getByRole('status')
    expect(region.getAttribute('aria-live')).toBe('polite')
    expect(region.textContent).toBe('')
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    expect(screen.getByRole('status')).toBe(region, '同一节点，只改文字不重新挂载')
    expect(region.textContent).toContain('已归档 1 个文件')
    expect(screen.getAllByRole('status')).toHaveLength(1, '不得叠加第二个 status 区域')
    // 手动关闭后区域常驻、文字清空
    fireEvent.click(screen.getByRole('button', { name: '知道了' }))
    expect(screen.getByRole('status')).toBe(region)
    expect(region.textContent).toBe('')
  })
})

describe('§49 lease-notice-switch-home：「前往 Runs」先切 home（App 级）', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { homeId?: string; method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({
          homes: [
            { homeId: 'h_a', kind: 'workspace', path: '/tmp/omt-ws1/.omt' },
            { homeId: 'h_b', kind: 'workspace', path: '/tmp/omt-ws2/.omt' },
          ],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({ homes: [] })
      }
      if (cmd === 'agents_list') {
        return args?.homeId === 'h_a'
          ? { agents: [agent({ status: 'failed', attachment: 'none' }, 'RUN-7')], agentSeq: 1, parseFailed: false }
          : { agents: [], agentSeq: 0, parseFailed: false }
      }
      if (cmd === 'agents_delete') {
        return Promise.resolve({
          deleted: 'agent-exec-1',
          leasesReleased: 0,
          leaseReleaseFailures: ['exec exec-1: daemon down'],
        })
      }
      return Promise.resolve({})
    })
  })

  it('通知跨 home 保留；按钮切回所属 home 并打开 Runs（旧实现只开当前 home 的 Runs）', async () => {
    const { App } = await import('../src/App')
    render(<App />)
    // 进入 Agents 标签，删除带租约失败的 agent → 应用级通知（homeId = h_a）
    fireEvent.click(await screen.findByText('Agents'))
    fireEvent.click(await screen.findByRole('button', { name: '删除 Agent 1' }))
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }))
    await waitFor(() => expect(screen.getByText(/租约释放失败/)).toBeTruthy())
    // 切到 home B：通知仍显示（§47 跨 home 保留），ticket 栏顶部显示 ws2
    // （U1：右上角切换弹窗已由图标栏取代）
    fireEvent.click(screen.getByTitle('omt-ws2（/tmp）'))
    await waitFor(() =>
      expect(document.querySelector('.workspace-name')?.textContent).toContain('omt-ws2'),
    )
    expect(screen.getByText(/租约释放失败/)).toBeTruthy()
    // 「前往 Runs」：先切回 h_a 再打开 Runs——标题回到 ws1，Runs 视图渲染
    fireEvent.click(screen.getByRole('button', { name: '前往 Runs' }))
    await waitFor(() =>
      expect(document.querySelector('.workspace-name')?.textContent).toContain('omt-ws1'),
    )
    await waitFor(() => expect(screen.getByText('当前 home 无 run')).toBeTruthy())
  })
})
