/**
 * §51 前端用例：
 * - in-flight-stop-all-archive：停止全部 / 归档的**在途结果**在切 home 后
 *   整个丢弃（§49 只清了已落地的提示，进行中的结果回来仍会写进新 home 的
 *   面板）。mock 边界与 exec-plane-p49.spec 相同。
 * - attach-retry（store 级）：exec_attach 瞬时失败后按轮询周期有界重试
 *   （§49 让 agents 引用稳定后，失败不再被 5s 重渲染自动救回，运行中 agent
 *   的实时 transcript 会冻结）。
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

describe('§51 in-flight-stop-all-archive：在途结果不落到切换后的 home', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('停止全部的结果在切 home 后回来 → 丢弃（不写新 home 的提示）', async () => {
    let resolveStop: ((value: { signaled: number; failed: string[] } | null) => void) | undefined
    const onStopAll = vi.fn().mockReturnValue(
      new Promise(resolve => {
        resolveStop = resolve
      }),
    )
    const props = { ...baseProps(), onStopAll }
    const first = render(<AgentsPanel {...props} />)
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    // 发起后、结果回来前切换到 B home（同一面板实例，只换 homeId prop）
    first.rerender(<AgentsPanel {...props} homeId="h_b" />)
    await act(async () => {
      resolveStop?.({ signaled: 2, failed: [] })
    })
    // A home 的「已向 2 个 agent 发出停止信号」不得出现在 B home 的面板上
    expect(screen.queryByText(/已向 2 个 agent 发出停止信号/)).toBeNull()
    expect(screen.queryByText(/停止失败/)).toBeNull()
    first.unmount()
    // 失败形态（outcome=null）同样丢弃
    let resolveFail: ((value: { signaled: number; failed: string[] } | null) => void) | undefined
    const onStopAllFail = vi.fn().mockReturnValue(
      new Promise(resolve => {
        resolveFail = resolve
      }),
    )
    const props2 = { ...baseProps(), onStopAll: onStopAllFail }
    const second = render(<AgentsPanel {...props2} />)
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    second.rerender(<AgentsPanel {...props2} homeId="h_b" />)
    await act(async () => {
      resolveFail?.(null)
    })
    expect(screen.queryByText(/停止失败/)).toBeNull()
  })

  it('未切 home 时停止结果照常提示（护栏：不误吞正常路径）', async () => {
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
    expect(await screen.findByText(/已向 1 个 agent 发出停止信号/)).toBeTruthy()
  })

  it('归档结果在切 home 后回来 → 丢弃', async () => {
    const corruptLegacy = {
      path: '/rt/agents/h_a/agents.json',
      present: true,
      parseable: false,
      corruptEvidence: [],
    }
    let resolveArchive: ((value: string[]) => void) | undefined
    const archive = vi.fn().mockReturnValue(
      new Promise(resolve => {
        resolveArchive = resolve
      }),
    )
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
    rerender(<AgentsPanel {...props} homeId="h_b" />)
    await act(async () => {
      resolveArchive?.(['agents.json.corrupt-1'])
    })
    expect(screen.queryByText(/已归档 1 个文件/)).toBeNull()
  })
})

/** 假时钟下等 store 的 boot 链（homes → 自动选中 → per-home 装载）收敛：
 * 定步推进假时钟 + 微任务冲刷，直到断言通过或步数耗尽。 */
async function advanceUntil(
  act: typeof import('@testing-library/react').act,
  vi: typeof import('vitest').vi,
  predicate: () => boolean,
  steps = 50,
): Promise<void> {
  for (let i = 0; i < steps && !predicate(); i += 1) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
  }
  if (!predicate()) throw new Error('advanceUntil: condition never met')
}

function renderPanel(over: Partial<Parameters<typeof AgentsPanel>[0]> = {}) {
  const props = { ...baseProps(), ...over }
  return render(<AgentsPanel {...props} />)
}

describe('§51 attach-retry：exec_attach 瞬时失败后有界重试（store 级）', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { homeId?: string; method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({
          homes: [{ homeId: 'h_a', kind: 'workspace', path: '/tmp/a/.omt' }],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({ homes: [] })
      }
      if (cmd === 'agents_list') {
        return Promise.resolve({ agents: [agent({ status: 'running' })], parseFailed: false })
      }
      if (cmd === 'exec_attach') {
        return Promise.reject(new Error('transient attach failure'))
      }
      return Promise.resolve({})
    })
  })

  it('失败后在一个轮询周期内重试；仍失败则止于有界次数', async () => {
    vi.useFakeTimers()
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())
    // 等 boot：homes 拉取 → 自动选中 h_a → per-home 数据装载（agents 就位）
    await advanceUntil(act, vi, () => result.current.state.agents.length === 1)
    const attachCalls = () => invokeMock.mock.calls.filter(c => c[0] === 'exec_attach')
    await act(async () => {
      result.current.attachTranscript('exec-1', 'h_a')
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(attachCalls()).toHaveLength(1, '首次尝试')
    // 一个轮询周期后自动重试（§49 引用稳定化后此前无人再触发）
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000)
    })
    expect(attachCalls()).toHaveLength(2, '瞬时失败应在一个轮询周期内重试')
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000)
    })
    expect(attachCalls()).toHaveLength(3)
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000)
    })
    expect(attachCalls()).toHaveLength(4, '第三次重试')
    // 有界：继续走时间不再产生新的尝试（1 次原始 + 3 次重试）
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000)
    })
    expect(attachCalls()).toHaveLength(4, '重试必须有界（ATTACH_RETRY_MAX = 3）')
  })

  it('重试成功后不再继续尝试', async () => {
    vi.useFakeTimers()
    let attachAttempts = 0
    invokeMock.mockImplementation((cmd: string, args?: { homeId?: string; method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({
          homes: [{ homeId: 'h_a', kind: 'workspace', path: '/tmp/a/.omt' }],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({ homes: [] })
      }
      if (cmd === 'agents_list') {
        return Promise.resolve({ agents: [agent({ status: 'running' })], parseFailed: false })
      }
      if (cmd === 'exec_attach') {
        attachAttempts += 1
        if (attachAttempts === 1) return Promise.reject(new Error('transient attach failure'))
        return Promise.resolve(undefined)
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())
    await advanceUntil(act, vi, () => result.current.state.agents.length === 1)
    await act(async () => {
      result.current.attachTranscript('exec-1', 'h_a')
      await vi.advanceTimersByTimeAsync(0)
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000)
    })
    expect(attachAttempts).toBe(2, '重试成功')
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000)
    })
    expect(attachAttempts).toBe(2, '成功后不得继续尝试（attached 防重入）')
  })
})
