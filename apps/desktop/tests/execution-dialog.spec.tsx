/**
 * U6 (R-B) 执行弹窗测试：默认选中与失效回退（bag v2 defaultHarness）、零
 * 可用引导态、权限知情确认（勾选前禁用 / ack 后同 workspace 不再要求）、
 * 同节点运行中三选项（查看 / add-members / 取消）、提交态机（派发中禁用、
 * 失败内联不关弹窗、成功回调 + Agents tab 切换）、Epic 成员预览（DFS 跳过
 * done/archived）。Mock 边界沿用 agents.spec 的 tauri 工厂（§3.6.8）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen, act, waitFor } from '@testing-library/react'
import type { AgentRecord, ExecRecord, HomeInfo, OmtNode } from '../src/types'
import type { ExecStartOutcome } from '../src/bridge'

afterEach(cleanup)

const invokeMock = vi.fn()
// §3.6.8：mock 工厂必须可 new 且 onmessage 可注入；instances 供测试模拟
// Rust 推流（startDispatch 每次 new 一个 Channel）。
class ChannelMock {
  onmessage: ((message: unknown) => void) | null = null
  static instances: ChannelMock[] = []
  constructor() {
    ChannelMock.instances.push(this)
  }
  send(message: unknown) {
    this.onmessage?.(message)
  }
  toJSON() {
    return '__CHANNEL__'
  }
}
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: ChannelMock,
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn().mockResolvedValue(null),
}))

// ── fixture 工厂 ──────────────────────────────────────────────────────────

const homeOf = (homeId = 'h1'): HomeInfo => ({ homeId, kind: 'workspace', path: `/tmp/${homeId}/.omt`, name: homeId })

const makeExec = (execId: string, runId: string | null = null): ExecRecord => ({
  execId,
  nodeId: null,
  runId,
  startedAt: '1000',
  exitCode: null,
  killReason: null,
  failureDetail: null,
  harnessSessionId: null,
  transcriptFile: null,
  binaryPath: null,
  binaryArgs: [],
})

const makeAgent = (id: string, over: Partial<AgentRecord> = {}): AgentRecord => ({
  id,
  displayName: `Agent ${id}`,
  title: null,
  harness: 'omp',
  status: 'running',
  attachment: 'live',
  lastExecId: `exec-${id}`,
  lastSessionId: null,
  execs: [makeExec(`exec-${id}`)],
  ...over,
})

const harnessWire = (over: Record<string, unknown>) => ({
  id: 'omp',
  label: 'OMP (oh-my-pi)',
  installed: true,
  detectedPath: '/usr/local/bin/omp',
  detectedVersion: 'v1.0',
  enabled: true,
  binarySource: 'detected',
  manualPath: null,
  state: 'ready',
  unattendedFlag: ['--yolo'],
  ...over,
})

const outcomeOf = (over: Partial<ExecStartOutcome> = {}): ExecStartOutcome => ({
  execId: 'exec-new',
  agentId: 'agent-new',
  runId: 'RUN-NEW',
  warnings: [],
  ...over,
})

type OmtRoute = (method: string, params: Record<string, unknown>) => unknown

const filtersGet = (harnessBag: Record<string, unknown>): OmtRoute => (method, params) => {
  if (method !== 'ui/filters-get') throw new Error(`unexpected method: ${method}`)
  return params.key === 'tauri:harness' ? { filters: harnessBag } : { filters: {} }
}

const baseDialogRoutes = (harnessBag: Record<string, unknown> = {}): Record<string, OmtRoute> => ({
  'ui/filters-get': filtersGet(harnessBag),
  'node/get': () => ({ node: { nodeId: 'T-1', kind: 'ticket', status: 'open' }, children: [], runs: [] }),
})

const invokeRoutesOf = (omtRoutes: Record<string, OmtRoute>, harnesses?: unknown[]) =>
  async (cmd: string, args: { method: string; params: Record<string, unknown> }) => {
    if (cmd === 'omt_call') {
      const handler = omtRoutes[args.method]
      if (!handler) throw new Error(`unexpected method: ${args.method}`)
      return handler(args.method, args.params)
    }
    if (cmd === 'harness_list') return { harnesses: harnesses ?? [harnessWire({})] }
    throw new Error(`unexpected command: ${cmd}`)
  }

interface DialogProps {
  home: HomeInfo
  nodeId: string
  nodeTitle: string
  nodes: OmtNode[]
  agents: AgentRecord[]
  startDispatch: (opts: { home: HomeInfo; nodeId: string; harness: string; unattended: boolean }) => Promise<ExecStartOutcome>
  onClose: () => void
  onDispatched: (outcome: ExecStartOutcome) => void
  onSelectExistingAgent: (agentId: string) => void
  onOpenHarnessSettings: () => void
}

async function renderDialog(
  over: Partial<DialogProps> = {},
  invokeRoutes?: (cmd: string, args: { method: string; params: Record<string, unknown> }) => unknown,
) {
  const { ExecutionDialog } = await import('../src/ExecutionDialog')
  invokeMock.mockImplementation(invokeRoutes ?? invokeRoutesOf(baseDialogRoutes()))
  const props: DialogProps = {
    home: homeOf(),
    nodeId: 'T-1',
    nodeTitle: '登录修复',
    nodes: [],
    agents: [],
    startDispatch: vi.fn().mockResolvedValue(outcomeOf()),
    onClose: vi.fn(),
    onDispatched: vi.fn(),
    onSelectExistingAgent: vi.fn(),
    onOpenHarnessSettings: vi.fn(),
    ...over,
  }
  render(<ExecutionDialog {...props} />)
  return props
}

/** fake timers 下排空异步 effect（对齐 agents.spec 的 act 排空手法） */
const flush = async () => {
  await act(async () => {
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

const clickCheckboxIn = (label: HTMLElement) => {
  const input = label.querySelector('input')
  fireEvent.click(input ?? label)
}

beforeEach(() => {
  invokeMock.mockReset()
  invokeMock.mockResolvedValue({})
})

// ── 纯函数：成员预览 DFS ──────────────────────────────────────────────────

describe('U6 纯函数 collectRunMembers（语义同 daemon collectRunMembers）', () => {
  it('Epic 子树 DFS：跳过 done/skipped/archived，只收 ticket/subticket', async () => {
    const { collectRunMembers } = await import('../src/ExecutionDialog')
    const tree: OmtNode[] = [
      {
        id: 'EPIC-1', type: 'epic', title: 'Epic', status: 'open', priority: 0,
        children: [
          {
            id: 'S-1', type: 'story', title: 'Story', status: 'open', priority: 0,
            children: [
              { id: 'T-1', type: 'ticket', title: 'T1', status: 'open', priority: 0 },
              { id: 'T-2', type: 'ticket', title: 'T2', status: 'done', priority: 0 },
              { id: 'T-3', type: 'ticket', title: 'T3', status: 'open', priority: 0, archived: true },
              { id: 'T-4', type: 'ticket', title: 'T4', status: 'skipped', priority: 0 },
              { id: 'T-5', type: 'subticket', title: 'T5', status: 'blocked', priority: 0 },
            ],
          },
        ],
      },
    ]
    expect(collectRunMembers(tree, 'EPIC-1')).toEqual(['T-1', 'T-5'])
  })

  it('ticket 自身为目标：自身入列；archived 根不收集也不下钻', async () => {
    const { collectRunMembers } = await import('../src/ExecutionDialog')
    const tree: OmtNode[] = [
      { id: 'T-9', type: 'ticket', title: 'self', status: 'open', priority: 0 },
      { id: 'T-8', type: 'ticket', title: 'archived root', status: 'open', priority: 0, archived: true, children: [{ id: 'T-7', type: 'ticket', title: 'under', status: 'open', priority: 0 }] },
    ]
    expect(collectRunMembers(tree, 'T-9')).toEqual(['T-9'])
    expect(collectRunMembers(tree, 'T-8')).toEqual([])
    expect(collectRunMembers(tree, 'missing')).toEqual([])
  })
})

// ── 选择器：默认选中 / 失效回退 / 零可用引导 ──────────────────────────────

describe('U6 弹窗选择器', () => {
  it('bag defaultHarness 未启用 → 回退首个 enabled 并显示提示条', async () => {
    await renderDialog(
      {},
      invokeRoutesOf(
        baseDialogRoutes({ defaultHarness: 'omp' }),
        [
          harnessWire({ id: 'omp', enabled: false, state: 'notDetected', installed: false }),
          harnessWire({ id: 'opencode', label: 'OpenCode' }),
        ],
      ),
    )
    await waitFor(() => {
      const select = screen.getByRole('combobox') as HTMLSelectElement
      expect(select.value).toBe('opencode')
    })
    const notice = screen.getByText(/默认 harness omp/)
    expect(notice.textContent).toContain('已回退')
  })

  it('零可用 harness → 引导态：派发不可用，「配置 Harness」关弹窗切设置', async () => {
    const props = await renderDialog(
      {},
      invokeRoutesOf(baseDialogRoutes(), [harnessWire({ enabled: false, state: 'notDetected', installed: false })]),
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '配置 Harness' })).toBeTruthy())
    expect(screen.queryByRole('button', { name: '派发' })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '配置 Harness' }))
    expect(props.onOpenHarnessSettings).toHaveBeenCalled()
    expect(props.onClose).toHaveBeenCalled()
  })
})

// ── 权限知情确认（§九-10：显式放行 + per-workspace 记住）──────────────────

describe('U6 权限知情确认', () => {
  it('未勾选「我已知悉」→ 派发禁用；勾选后启用并以 unattended=true 提交', async () => {
    const props = await renderDialog({}, invokeRoutesOf(baseDialogRoutes()))
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    const ackLabel = screen.getByText(/我已知悉/)
    expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(true)
    clickCheckboxIn(ackLabel)
    await waitFor(() => expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    await waitFor(() => expect(props.startDispatch).toHaveBeenCalledWith(expect.objectContaining({ harness: 'omp', unattended: true })))
  })

  it('已 ack 的 workspace×provider 不再渲染确认；另一 workspace 仍要求', async () => {
    // workspace h1：bag 已 ack omp → 无确认项，派发直接可用
    const first = await renderDialog({ home: homeOf('h1') }, invokeRoutesOf(baseDialogRoutes({ approvalAck: { omp: true } })))
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    expect(screen.queryByText(/我已知悉/)).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    await waitFor(() => expect(first.startDispatch).toHaveBeenCalled())
    // workspace h2：无 ack → 仍要求确认
    await renderDialog({ home: homeOf('h2') }, invokeRoutesOf(baseDialogRoutes()))
    await waitFor(() => expect(screen.getByText(/我已知悉/)).toBeTruthy())
  })

  it('切换 harness 重置知情确认：派发禁用直到勾选新 provider 的确认', async () => {
    await renderDialog(
      {},
      invokeRoutesOf(baseDialogRoutes(), [
        harnessWire({}),
        harnessWire({ id: 'opencode', label: 'OpenCode' }),
      ]),
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    // 勾选 harness A（omp）的知情确认 → 派发可用
    clickCheckboxIn(screen.getByText(/我已知悉/))
    await waitFor(() => expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(false))
    // 切到 harness B（opencode）→ 确认重置，派发回到禁用
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'opencode' } })
    expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(true)
    // 勾选 B 的确认 → 派发恢复可用
    clickCheckboxIn(screen.getByText(/我已知悉/))
    await waitFor(() => expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(false))
  })
})

// ── 同节点运行中派发：三选项 ──────────────────────────────────────────────

describe('U6 同节点运行中三选项', () => {
  // daemon run_add_members 仅接受 Pending run（dispatch.rs run-status-gate）
  // 且拒绝已在 run 中的成员（DUPLICATE_MEMBER）——「追加」只在 pending 且有
  // 新成员时可用，门制与差集必须诚实呈现。
  const conflictRoutes = (runStatus: string, runItems: Array<{ nodeId: string }> = []): Record<string, OmtRoute> => ({
    ...baseDialogRoutes({ approvalAck: { omp: true } }),
    'node/get': () => ({ node: { nodeId: 'T-1', kind: 'ticket', status: 'in_progress' }, children: [], runs: [{ runId: 'RUN-1', status: runStatus }] }),
    'run/get': params => ({ run: { runId: params.runId, status: runStatus }, items: runItems }),
    'run/add-members': (_method, params) => ({ addedCount: (params.nodeIds as string[]).length }),
  })

  it('Running run → 三选项呈现但「追加」禁用并说明 daemon 门制', async () => {
    await renderDialog({ agents: [makeAgent('a1', { execs: [makeExec('exec-a1', 'RUN-1')] })] }, invokeRoutesOf(conflictRoutes('running')))
    await waitFor(() => expect(screen.getByRole('button', { name: '查看运行中 Agent' })).toBeTruthy())
    expect(screen.getByRole('button', { name: '追加到该 Agent 当前 run' })).toBeTruthy()
    expect((screen.getByRole('button', { name: '追加到该 Agent 当前 run' }) as HTMLButtonElement).disabled).toBe(true)
    expect(screen.getByText(/daemon 仅允许向 Pending run 追加成员/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: '派发' })).toBeNull()
  })

  it('pending run 且有新成员 →「追加」可用，nodeIds 只发差集', async () => {
    const nodes: OmtNode[] = [{ id: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0 }]
    const props = await renderDialog(
      { nodes, agents: [makeAgent('a1', { execs: [makeExec('exec-a1', 'RUN-1')] })] },
      invokeRoutesOf(conflictRoutes('pending', [{ nodeId: 'T-9' }])),
    )
    await waitFor(() => {
      expect((screen.getByRole('button', { name: '追加到该 Agent 当前 run' }) as HTMLButtonElement).disabled).toBe(false)
    })
    fireEvent.click(screen.getByRole('button', { name: '追加到该 Agent 当前 run' }))
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'run/add-members')
      expect(call).toBeTruthy()
      // 差集：run 里已有的 T-9 不重发
      expect((call![1] as { params: Record<string, unknown> }).params).toMatchObject({ homeId: 'h1', runId: 'RUN-1', nodeIds: ['T-1'] })
    })
    await waitFor(() => {
      expect(props.onSelectExistingAgent).toHaveBeenCalledWith('a1')
      expect(props.onClose).toHaveBeenCalled()
    })
  })

  it('pending run 但成员已全部在 run 中 →「追加」禁用并说明', async () => {
    const nodes: OmtNode[] = [{ id: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0 }]
    await renderDialog(
      { nodes, agents: [makeAgent('a1', { execs: [makeExec('exec-a1', 'RUN-1')] })] },
      invokeRoutesOf(conflictRoutes('pending', [{ nodeId: 'T-1' }])),
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '追加到该 Agent 当前 run' })).toBeTruthy())
    expect((screen.getByRole('button', { name: '追加到该 Agent 当前 run' }) as HTMLButtonElement).disabled).toBe(true)
    expect(screen.getByText(/成员已全部在 run 中/)).toBeTruthy()
  })

  it('查看运行中 Agent：关弹窗并回调 agentId（切 Agents 由 App 承接）', async () => {
    const props = await renderDialog({ agents: [makeAgent('a1', { execs: [makeExec('exec-a1', 'RUN-1')] })] }, invokeRoutesOf(conflictRoutes('running')))
    await waitFor(() => expect(screen.getByRole('button', { name: '查看运行中 Agent' })).toBeTruthy())
    fireEvent.click(screen.getByRole('button', { name: '查看运行中 Agent' }))
    expect(props.onSelectExistingAgent).toHaveBeenCalledWith('a1')
    expect(props.onClose).toHaveBeenCalled()
  })
})

// ── 提交态机 + 详情预览 ───────────────────────────────────────────────────

describe('U6 提交态机与详情', () => {
  it('派发中按钮禁用显示「派发中…」，>5s 展开 ①–⑧ 步骤清单', async () => {
    vi.useFakeTimers()
    try {
      await renderDialog(
        { startDispatch: () => new Promise<ExecStartOutcome>(() => { /* 挂起不返回 */ }) },
        invokeRoutesOf(baseDialogRoutes({ approvalAck: { omp: true } })),
      )
      await flush()
      const dispatch = screen.getByRole('button', { name: '派发' }) as HTMLButtonElement
      expect(dispatch.disabled).toBe(false)
      fireEvent.click(dispatch)
      const submitting = screen.getByRole('button', { name: '派发中…' }) as HTMLButtonElement
      expect(submitting.disabled).toBe(true)
      act(() => { vi.advanceTimersByTime(5100) })
      expect(screen.getByText(/① 预检/)).toBeTruthy()
      expect(screen.getByText(/⑧ spawn/)).toBeTruthy()
    } finally {
      vi.useRealTimers()
    }
  })

  it('失败：保留弹窗就地内联报错，不关闭', async () => {
    const props = await renderDialog(
      { startDispatch: vi.fn().mockRejectedValue(new Error('spawn omp 失败: not found')) },
      invokeRoutesOf(baseDialogRoutes({ approvalAck: { omp: true } })),
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    await waitFor(() => expect(screen.getByText(/spawn omp 失败/)).toBeTruthy())
    expect(props.onClose).not.toHaveBeenCalled()
    expect(props.onDispatched).not.toHaveBeenCalled()
    expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(false)
  })

  it('成功：onDispatched 收到 outcome（warnings 随行）+ 记住 defaultHarness（不写 ack），关弹窗', async () => {
    const props = await renderDialog(
      { startDispatch: vi.fn().mockResolvedValue(outcomeOf({ warnings: ['绑定写入失败：boom'] })) },
      invokeRoutesOf(baseDialogRoutes()),
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    clickCheckboxIn(screen.getByText(/我已知悉/))
    await waitFor(() => expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    await waitFor(() => {
      expect(props.onDispatched).toHaveBeenCalledWith(outcomeOf({ warnings: ['绑定写入失败：boom'] }))
      expect(props.onClose).toHaveBeenCalled()
    })
    // 未勾「记住」：bag 只写 defaultHarness，不得动 approvalAck
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'ui/filters-set')
      expect(call).toBeTruthy()
      expect((call![1] as { params: { filters: Record<string, unknown> } }).params.filters).toEqual({ defaultHarness: 'omp' })
    })
  })

  it('「本 workspace 记住」勾选 → 成功后 bag v2 合并写 approvalAck，旧键保留', async () => {
    const props = await renderDialog(
      { startDispatch: vi.fn().mockResolvedValue(outcomeOf()) },
      invokeRoutesOf({
        ...baseDialogRoutes(),
        'ui/filters-get': (method, params) => {
          if (method !== 'ui/filters-get') throw new Error(`unexpected: ${method}`)
          return params.key === 'tauri:harness'
            ? { filters: { harness: { legacy: true }, approvalAck: { opencode: true } } }
            : { filters: {} }
        },
      }),
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    clickCheckboxIn(screen.getByText(/我已知悉/))
    clickCheckboxIn(screen.getByText(/本 workspace 记住/))
    await waitFor(() => expect((screen.getByRole('button', { name: '派发' }) as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    await waitFor(() => expect(props.onDispatched).toHaveBeenCalled())
    await waitFor(() => {
      const call = invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'ui/filters-set')
      expect(call).toBeTruthy()
      const filters = (call![1] as { params: { filters: Record<string, unknown> } }).params.filters
      expect(filters.harness).toEqual({ legacy: true })
      expect(filters.approvalAck).toEqual({ opencode: true, omp: true })
      expect(filters.defaultHarness).toBe('omp')
    })
  })

  it('详情展开：launch 预览（binary/args/env 键名/unattendedFlag）+ 成员清单', async () => {
    const nodes: OmtNode[] = [
      {
        id: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0,
        children: [{ id: 'T-2', type: 'subticket', title: '子项', status: 'open', priority: 0 }],
      },
    ]
    await renderDialog(
      { nodes },
      (cmd, args) => {
        if (cmd === 'harness_list') return { harnesses: [harnessWire({})] }
        if (cmd === 'harness_launch_preview') {
          return { program: '/usr/local/bin/omp', args: ['--model', 'k3'], envKeys: ['OMP_KEY'], unattendedFlag: ['--yolo'] }
        }
        if (cmd === 'omt_call') {
          const handler = baseDialogRoutes({ approvalAck: { omp: true } })[args.method]
          if (handler) return handler(args.method, args.params)
        }
        throw new Error(`unexpected: ${cmd}`)
      },
    )
    await waitFor(() => expect(screen.getByRole('button', { name: '详情' })).toBeTruthy())
    fireEvent.click(screen.getByRole('button', { name: '详情' }))
    await waitFor(() => expect(screen.getByText(/\/usr\/local\/bin\/omp/)).toBeTruthy())
    expect(screen.getByText(/--model k3/)).toBeTruthy()
    expect(screen.getByText(/OMP_KEY/)).toBeTruthy()
    expect(screen.getByText(/--yolo/)).toBeTruthy()
    // 成员预览含自身 + 子树 subticket；标题与弹窗头同名 → 用成员 chip 全文定位
    expect(screen.getByText(/将进入 run 的成员（2）/)).toBeTruthy()
    expect(screen.getByText('T-1 登录修复')).toBeTruthy()
    expect(screen.getByText('T-2 子项')).toBeTruthy()
  })
})

// ── store：startDispatch / bag v2 helpers / harness 封装 ──────────────────

describe('U6 store 切片', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('startDispatch：Channel onmessage 接 ingest 管线；成功后防抖拉 agents_list', async () => {
    vi.useFakeTimers()
    try {
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === 'exec_start') return outcomeOf()
        if (cmd === 'agents_list') return { agents: [makeAgent('agent-new')], agentSeq: 1, parseFailed: false }
        return {}
      })
      const { useTreeStore } = await import('../src/store')
      const { renderHook } = await import('@testing-library/react')
      const { result } = renderHook(() => useTreeStore())
      // 派发总是发生在 activeHome 上下文（refreshAgents 按 activeHome 拉）
      await act(async () => {
        await result.current.selectHome(homeOf())
      })
      let outcome: ExecStartOutcome | null = null
      await act(async () => {
        outcome = await result.current.startDispatch({ home: homeOf(), nodeId: 'T-1', harness: 'omp', unattended: true })
      })
      expect(outcome).toEqual(outcomeOf())
      const call = invokeMock.mock.calls.find(c => c[0] === 'exec_start')
      expect(call).toBeTruthy()
      const args = call![1] as Record<string, unknown>
      expect(args).toMatchObject({ homeId: 'h1', homePath: '/tmp/h1/.omt', nodeId: 'T-1', harness: 'omp', unattended: true })
      // Channel 推流进归并管线（raw 50ms flush）
      const channel = ChannelMock.instances[ChannelMock.instances.length - 1]
      expect(typeof channel.onmessage).toBe('function')
      act(() => { channel.send({ execId: 'exec-new', seq: 0, ts: '1', kind: 'raw', stream: 'stdout', text: 'hello' }) })
      act(() => { vi.advanceTimersByTime(60) })
      expect(result.current.transcriptWindow('exec-new').map(e => (e.kind === 'raw' ? e.text : ''))).toEqual(['hello'])
      // 成功后防抖拉 agents_list（400ms 防抖）
      const callsBefore = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
      act(() => { vi.advanceTimersByTime(500) })
      const callsAfter = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
      expect(callsAfter).toBeGreaterThan(callsBefore)
    } finally {
      vi.useRealTimers()
    }
  })

  it('readHarnessBag：旧 shape 不改写（legacy 探测如实报告）；v2 字段如实读出；缺失/读失败 → 空', async () => {
    invokeMock.mockImplementation(async (cmd: string, args: { method: string }) => {
      if (cmd === 'omt_call' && args.method === 'ui/filters-get') {
        return { filters: { harness: { type: 'dsh', mode: 'dev' }, defaultHarness: 'omp', approvalAck: { omp: true, bad: 'no' } } }
      }
      return {}
    })
    const { readHarnessBag } = await import('../src/store')
    await act(async () => {
      // U1b：返回值扩展 legacy 探测字段（旧 harness.type 命中即报告，读不删改）
      expect(await readHarnessBag('h1')).toEqual({ defaultHarness: 'omp', approvalAck: { omp: true }, legacy: 'dsh' })
    })
    invokeMock.mockRejectedValue(new Error('x'))
    await act(async () => {
      expect(await readHarnessBag('h1')).toEqual({ legacy: null })
    })
  })

  it('writeHarnessBag：read-merge-write 只动 v2 字段，旧 harness 键保留', async () => {
    invokeMock.mockImplementation(async (cmd: string, args: { method: string }) => {
      if (cmd === 'omt_call' && args.method === 'ui/filters-get') {
        return { filters: { harness: { legacy: 1 }, approvalAck: { opencode: true } } }
      }
      return {}
    })
    const { writeHarnessBag } = await import('../src/store')
    await act(async () => {
      await writeHarnessBag('h1', { approvalAck: { omp: true } })
    })
    const call = invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'ui/filters-set')
    expect(call).toBeTruthy()
    const filters = (call![1] as { params: { filters: Record<string, unknown> } }).params.filters
    expect(filters).toEqual({ harness: { legacy: 1 }, approvalAck: { opencode: true, omp: true } })
  })

  it('harnessList/harnessConfigSet：wire 投影透传', async () => {
    const entry = harnessWire({})
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'harness_list') return { harnesses: [entry] }
      if (cmd === 'harness_config_set') return entry
      throw new Error(`unexpected: ${cmd}`)
    })
    const { harnessList, harnessConfigSet } = await import('../src/store')
    await act(async () => {
      expect(await harnessList()).toEqual([entry])
      expect(await harnessConfigSet('omp', { enabled: true, binary: { kind: 'detected' }, extraArgs: [], env: {} })).toEqual(entry)
      expect(invokeMock.mock.calls.find(c => c[0] === 'harness_config_set')![1]).toMatchObject({ harness: 'omp' })
    })
  })
})

// ── DetailPanel：运行按钮改开弹窗（Epic/Story 详情页同样有入口，R-B）──────

describe('U6 DetailPanel 派发入口', () => {
  it('Execute → onDispatch(title)（不再直接 node/execute）', async () => {
    const { DetailPanel } = await import('../src/DetailPanel')
    invokeMock.mockImplementation(async (cmd: string, args: { method: string }) => {
      if (cmd === 'omt_call' && args.method === 'node/get') {
        return { node: { nodeId: 'EPIC-1', type: 'epic', title: '大 Epic', status: 'open', priority: 0, revision: 1 }, children: [], runs: [] }
      }
      return {}
    })
    const onDispatch = vi.fn()
    render(
      <DetailPanel
        home={homeOf()}
        nodeId="EPIC-1"
        onUpdated={vi.fn().mockResolvedValue(null)}
        onSelect={() => {}}
        onChanged={() => {}}
        onDispatch={onDispatch}
      />,
    )
    await waitFor(() => expect(screen.getByRole('button', { name: 'Execute' })).toBeTruthy())
    fireEvent.click(screen.getByRole('button', { name: 'Execute' }))
    expect(onDispatch).toHaveBeenCalledWith('大 Epic')
    expect(invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'node/execute')).toBeUndefined()
  })
})

// ── App 集成：成功后切 Agents 内层 tab 并选中新 Agent ─────────────────────

describe('U6 App 全局门控', () => {
  it('树选中 → Execute → 弹窗派发成功 → 弹窗关闭 + Agents tab 激活 + 选中 Agent + warnings 警告条', async () => {
    const { App } = await import('../src/App')
    invokeMock.mockImplementation(async (cmd: string, args?: { method: string; params: Record<string, unknown> }) => {
      if (cmd === 'daemon_homes') return { homes: [homeOf()], homeDir: '/tmp' }
      if (cmd === 'exec_start') return outcomeOf({ warnings: ['绑定写入失败：boom'] })
      if (cmd === 'agents_list') return { agents: [makeAgent('agent-new', { displayName: 'Agent 1' })], agentSeq: 1, parseFailed: false }
      if (cmd === 'harness_list') return { harnesses: [harnessWire({})] }
      if (cmd === 'harness_launch_preview') return { program: '/usr/local/bin/omp', args: [], envKeys: [], unattendedFlag: ['--yolo'] }
      if (cmd === 'omt_call') {
        const method = args!.method
        if (method === 'home/list-known' || method === 'ui/recent-get') return {}
        if (method === 'ui/filters-get') return { filters: {} }
        if (method === 'ui/filters-set') return {}
        if (method === 'node/tree') {
          return { trees: [{ nodeId: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0 }] }
        }
        if (method === 'run/list') return { runs: [] }
        if (method === 'node/get') return { node: { nodeId: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0, revision: 1 }, children: [], runs: [] }
        throw new Error(`unexpected method: ${method}`)
      }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<App />)
    // 树选中 ticket → 详情加载 → Execute 开弹窗
    await waitFor(() => expect(screen.getByText('登录修复')).toBeTruthy())
    fireEvent.click(screen.getByText('登录修复'))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Execute' })).toBeTruthy())
    fireEvent.click(screen.getByRole('button', { name: 'Execute' }))
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    // 权限知情确认 → 派发
    clickCheckboxIn(screen.getByText(/我已知悉/))
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    // 成功：弹窗关闭、Agents 内层 tab 激活、新 Agent 选中、warnings 持久警告条
    await waitFor(() => expect(screen.queryByText(/我已知悉/)).toBeNull())
    await waitFor(() => expect(screen.getAllByText('Agent 1').length).toBeGreaterThan(0))
    expect(screen.getByText(/绑定写入失败：boom/)).toBeTruthy()
  })
})

describe('U6 App 派发警告条随 workspace 切换清除', () => {
  it('派发产生 warnings 后切换 workspace → 警告条消失', async () => {
    window.localStorage.setItem('omt-desktop-last-home', 'h1')
    const { App } = await import('../src/App')
    invokeMock.mockImplementation(async (cmd: string, args?: { method: string; params: Record<string, unknown> }) => {
      if (cmd === 'daemon_homes') return { homes: [homeOf('h1'), homeOf('h2')], homeDir: '/tmp' }
      if (cmd === 'exec_start') return outcomeOf({ warnings: ['绑定写入失败：boom'] })
      if (cmd === 'agents_list') return { agents: [makeAgent('agent-new', { displayName: 'Agent 1' })], agentSeq: 1, parseFailed: false }
      if (cmd === 'harness_list') return { harnesses: [harnessWire({})] }
      if (cmd === 'harness_launch_preview') return { program: '/usr/local/bin/omp', args: [], envKeys: [], unattendedFlag: ['--yolo'] }
      if (cmd === 'omt_call') {
        const method = args!.method
        if (method === 'home/list-known' || method === 'ui/recent-get') return {}
        if (method === 'ui/filters-get') return { filters: {} }
        if (method === 'ui/filters-set') return {}
        if (method === 'node/tree') {
          return { trees: [{ nodeId: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0 }] }
        }
        if (method === 'run/list') return { runs: [] }
        if (method === 'node/get') return { node: { nodeId: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0, revision: 1 }, children: [], runs: [] }
        throw new Error(`unexpected method: ${method}`)
      }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<App />)
    // 树选中 ticket → Execute → 知情确认 → 派发成功 → 警告条出现
    await waitFor(() => expect(screen.getByText('登录修复')).toBeTruthy())
    fireEvent.click(screen.getByText('登录修复'))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Execute' })).toBeTruthy())
    fireEvent.click(screen.getByRole('button', { name: 'Execute' }))
    await waitFor(() => expect(screen.getByRole('button', { name: '派发' })).toBeTruthy())
    clickCheckboxIn(screen.getByText(/我已知悉/))
    fireEvent.click(screen.getByRole('button', { name: '派发' }))
    await waitFor(() => expect(screen.getByText(/绑定写入失败：boom/)).toBeTruthy())
    // 设置页选第二个 workspace（h2）→ activeHome 变化 → 警告条清除
    fireEvent.click(screen.getByText('设置'))
    await waitFor(() => expect(document.querySelectorAll('.home-row').length).toBe(2))
    await act(async () => {
      fireEvent.click(document.querySelectorAll('.home-row')[1])
      await Promise.resolve()
    })
    await waitFor(() => expect(screen.queryByText(/绑定写入失败：boom/)).toBeNull())
  })
})
