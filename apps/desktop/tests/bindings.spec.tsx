/**
 * U9 (R-C) 绑定可见与继承测试：resolveBindingForNode 三态纯函数（直接/
 * 祖先继承/无，archived 祖先绑定仍有效）、bindingViewsForNodes 悬空与降级
 * 冻结、danglingBindingsOf、TreePanel badge（直接高亮 / 继承灰显 / 悬空 /
 * archived 跳过渲染）、AgentsPanel 一键清理、ExecutionDialog 绑定提示与
 * harness 默认优先级（绑定 agent.harness > bag defaultHarness > 首个
 * enabled）。Mock 边界沿用 agents.spec / execution-dialog.spec 的 tauri
 * 工厂（§3.6.8）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { AgentRecord, ExecRecord, HomeInfo, OmtNode } from '../src/types'

afterEach(cleanup)

const invokeMock = vi.fn()
// §3.6.8：mock 工厂必须可 new 且 onmessage 可注入（对齐 @tauri-apps/api
// Channel 的消费方式——store import 链会拉起 Channel）
class ChannelMock {
  onmessage: ((message: unknown) => void) | null = null
  constructor() {
    /* 测试不经 Channel 推流 */
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

const makeExec = (execId: string): ExecRecord => ({
  execId,
  nodeId: null,
  runId: null,
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
  status: 'exited',
  attachment: 'none',
  lastExecId: `exec-${id}`,
  lastSessionId: null,
  execs: [makeExec(`exec-${id}`)],
  ...over,
})

/** 三层树：EPIC-1 → S-1 → T-1（parentId 链供继承解析上溯） */
const tree = (): OmtNode[] => [
  {
    id: 'EPIC-1', type: 'epic', title: '根 Epic', status: 'open', priority: 0,
    children: [
      {
        id: 'S-1', type: 'story', title: '故事一', status: 'open', priority: 0, parentId: 'EPIC-1',
        children: [{ id: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0, parentId: 'S-1' }],
      },
    ],
  },
]

const noop = () => {}

beforeEach(() => {
  invokeMock.mockReset()
  invokeMock.mockResolvedValue({})
})

// ── 纯函数：resolveBindingForNode 三态 ────────────────────────────────────

describe('U9 纯函数 resolveBindingForNode（子树继承解析）', () => {
  it('直接绑定 / 祖先继承（含 viaNodeId）/ 无绑定 三态', async () => {
    const { resolveBindingForNode } = await import('../src/store')
    const nodes = tree()
    // 无绑定
    expect(resolveBindingForNode({}, nodes, 'T-1')).toBeNull()
    // 直接绑定
    expect(resolveBindingForNode({ 'T-1': 'agent-a' }, nodes, 'T-1')).toEqual({
      agentId: 'agent-a',
      source: 'direct',
    })
    // 祖先继承：T-1 无绑定，沿 parentId 上溯取最近（S-1）
    expect(
      resolveBindingForNode({ 'EPIC-1': 'agent-epic', 'S-1': 'agent-story' }, nodes, 'T-1'),
    ).toEqual({ agentId: 'agent-story', source: 'inherited', viaNodeId: 'S-1' })
    // 跳过更近一层缺失：只有 EPIC-1 有绑定 → 经 S-1 继承自 EPIC-1
    expect(resolveBindingForNode({ 'EPIC-1': 'agent-epic' }, nodes, 'T-1')).toEqual({
      agentId: 'agent-epic',
      source: 'inherited',
      viaNodeId: 'EPIC-1',
    })
  })

  it('archived 祖先的绑定仍参与解析（绑定不因 archived 失效）', async () => {
    const { resolveBindingForNode } = await import('../src/store')
    const nodes: OmtNode[] = [
      {
        id: 'EPIC-1', type: 'epic', title: '根 Epic', status: 'open', priority: 0, archived: true,
        children: [{ id: 'T-1', type: 'ticket', title: '登录修复', status: 'open', priority: 0, parentId: 'EPIC-1' }],
      },
    ]
    expect(resolveBindingForNode({ 'EPIC-1': 'agent-a' }, nodes, 'T-1')).toEqual({
      agentId: 'agent-a',
      source: 'inherited',
      viaNodeId: 'EPIC-1',
    })
  })

  it('目标不在树内 → 无绑定', async () => {
    const { resolveBindingForNode } = await import('../src/store')
    expect(resolveBindingForNode({ 'T-9': 'agent-a' }, tree(), 'T-9')).toBeNull()
  })
})

describe('U9 纯函数 bindingViewsForNodes / danglingBindingsOf', () => {
  it('agent 存在 → direct/inherited 视图；不存在 → dangling；degraded → 悬空判定冻结', async () => {
    const { bindingViewsForNodes } = await import('../src/store')
    const nodes = tree()
    const agents = [makeAgent('agent-a')]
    const viewOf = (bindings: Record<string, string>, degraded: boolean) =>
      bindingViewsForNodes(bindings, nodes, agents, degraded).get('T-1')
    expect(viewOf({ 'T-1': 'agent-a' }, false)).toMatchObject({
      state: 'direct',
      agentId: 'agent-a',
    })
    expect(viewOf({ 'S-1': 'agent-a' }, false)).toMatchObject({ state: 'inherited', viaNodeId: 'S-1' })
    expect(viewOf({ 'T-1': 'agent-gone' }, false)).toEqual({
      state: 'dangling',
      agentId: 'agent-gone',
      agent: null,
    })
    // 数据降级（parseFailed）：不做悬空判定 → null（badge 不渲染）
    expect(viewOf({ 'T-1': 'agent-gone' }, true)).toBeNull()
  })

  it('danglingBindingsOf 列出悬空项（Agents 面清理计数）', async () => {
    const { danglingBindingsOf } = await import('../src/store')
    const agents = [makeAgent('agent-a')]
    expect(danglingBindingsOf({ 'T-1': 'agent-a', 'T-2': 'ghost', 'T-3': 'phantom' }, agents)).toEqual([
      { nodeId: 'T-2', agentId: 'ghost' },
      { nodeId: 'T-3', agentId: 'phantom' },
    ])
    expect(danglingBindingsOf({ 'T-1': 'agent-a' }, agents)).toEqual([])
  })
})

// ── TreePanel：badge 渲染 ─────────────────────────────────────────────────

interface TreeProps {
  nodes: OmtNode[]
  filters: Record<string, unknown>
  recentIds: string[]
  selectedId: string | null
  bindings?: Record<string, string>
  agents?: AgentRecord[]
  bindingsDegraded?: boolean
  onSelect: (id: string) => void
  onFilters: (patch: Record<string, unknown>) => void
}

async function renderTree(over: Partial<TreeProps> = {}) {
  const { TreePanel } = await import('../src/TreePanel')
  const props: TreeProps = {
    nodes: tree(),
    filters: {},
    recentIds: [],
    selectedId: null,
    bindings: {},
    agents: [],
    bindingsDegraded: false,
    onSelect: noop,
    onFilters: noop,
    ...over,
  }
  render(<TreePanel {...props} />)
  return props
}

describe('U9 TreePanel 绑定 badge', () => {
  it('直接绑定 → 高亮 badge（agent displayName，title 标注派发绑定）', async () => {
    await renderTree({
      bindings: { 'T-1': 'agent-a' },
      agents: [makeAgent('agent-a', { title: '重构登录' })],
    })
    const direct = screen.getByText(/重构登录/)
    expect(direct.className).toContain('direct')
    expect(direct.title).toContain('派发绑定')
    // 直接绑定只标在绑定节点自身
    expect(screen.getAllByText(/重构登录/)).toHaveLength(1)
  })

  it('继承 badge：子树每行灰显、title 标注继承自哪个祖先节点', async () => {
    await renderTree({
      bindings: { 'EPIC-1': 'agent-a' },
      agents: [makeAgent('agent-a')],
    })
    // 继承语义：EPIC-1 直达自身（direct），S-1 / T-1 沿链继承 → 三行都有 badge
    const badges = screen.getAllByText(/Agent agent-a/)
    expect(badges).toHaveLength(3)
    const inherited = badges.find(b => b.className.includes('inherited'))!
    expect(inherited).toBeTruthy()
    expect(inherited.title).toContain('EPIC-1')
    expect(badges.find(b => b.className.includes('direct'))).toBeTruthy()
  })

  it('悬空 → 「绑定已失效」badge；degraded 时不渲染悬空判定', async () => {
    await renderTree({ bindings: { 'T-1': 'ghost' }, agents: [] })
    expect(screen.getByText('绑定已失效')).toBeTruthy()

    cleanup()
    await renderTree({ bindings: { 'T-1': 'ghost' }, agents: [], bindingsDegraded: true })
    expect(screen.queryByText('绑定已失效')).toBeNull()
  })

  it('archived 节点 badge 跳过渲染（restore 后回归——绑定仍在解析链）', async () => {
    const nodes: OmtNode[] = [
      { id: 'T-1', type: 'ticket', title: '已归档', status: 'open', priority: 0, archived: true },
    ]
    await renderTree({ nodes, bindings: { 'T-1': 'agent-a' }, agents: [makeAgent('agent-a')] })
    expect(screen.queryByText(/Agent agent-a/)).toBeNull()
    expect(screen.queryByText('绑定已失效')).toBeNull()
  })
})

// ── AgentsPanel：一键清理 ─────────────────────────────────────────────────

async function renderAgentsPanel(over: Record<string, unknown> = {}) {
  const { AgentsPanel } = await import('../src/AgentsPanel')
  const props = {
    agents: [] as AgentRecord[],
    parseFailed: false,
    bindings: {} as Record<string, string>,
    bindingsDegraded: false,
    selectedAgentId: null,
    nodes: [] as OmtNode[],
    fetchRun: vi.fn().mockResolvedValue({ run: { id: 'R' }, items: [] }),
    eventsOf: () => [],
    pidOf: () => null,
    onSelectAgent: noop,
    onStopAgent: noop,
    onKillOrphan: noop,
    onOpenHarnessSettings: noop,
    onGcBindings: undefined as undefined | (() => void),
    ...over,
  }
  render(<AgentsPanel {...props} />)
  return props
}

describe('U9 AgentsPanel 一键清理', () => {
  it('存在悬空绑定 → 「清理失效绑定 (N)」按钮，点击调 bindings_gc 回调', async () => {
    const onGcBindings = vi.fn()
    await renderAgentsPanel({
      agents: [makeAgent('agent-a')],
      bindings: { 'T-keep': 'agent-a', 'T-dead': 'ghost', 'T-dead2': 'phantom' },
      onGcBindings,
    })
    const button = screen.getByRole('button', { name: '清理失效绑定 (2)' })
    fireEvent.click(button)
    expect(onGcBindings).toHaveBeenCalled()
  })

  it('degraded 时不显示清理按钮（悬空判定冻结）', async () => {
    await renderAgentsPanel({
      agents: [],
      bindings: { 'T-1': 'ghost' },
      bindingsDegraded: true,
      onGcBindings: vi.fn(),
    })
    expect(screen.queryByRole('button', { name: /清理失效绑定/ })).toBeNull()
  })
})

// ── ExecutionDialog：绑定预选与 harness 默认优先级 ─────────────────────────

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

const baseRoutes = (harnessBag: Record<string, unknown>, bindings: Record<string, string> | null) =>
  async (cmd: string, args: { method: string; params: Record<string, unknown> }) => {
    if (cmd === 'bindings_get') {
      if (bindings === null) throw new Error('bindings unavailable')
      return { bindings, parseFailed: false }
    }
    if (cmd === 'harness_list') {
      return {
        harnesses: [
          harnessWire({}),
          harnessWire({ id: 'opencode', label: 'OpenCode' }),
        ],
      }
    }
    if (cmd === 'omt_call') {
      if (args.method === 'ui/filters-get') {
        return args.params.key === 'tauri:harness' ? { filters: harnessBag } : { filters: {} }
      }
      if (args.method === 'node/get') {
        return { node: { nodeId: 'T-1', kind: 'ticket', status: 'open' }, children: [], runs: [] }
      }
      throw new Error(`unexpected method: ${args.method}`)
    }
    throw new Error(`unexpected command: ${cmd}`)
  }

async function renderDialog(over: Record<string, unknown> = {}, routes?: Awaited<ReturnType<typeof baseRoutes>>) {
  const { ExecutionDialog } = await import('../src/ExecutionDialog')
  invokeMock.mockImplementation(routes ?? baseRoutes({}, {}))
  const props = {
    home: homeOf(),
    nodeId: 'T-1',
    nodeTitle: '登录修复',
    nodes: tree(),
    agents: [] as AgentRecord[],
    startDispatch: vi.fn().mockResolvedValue({ execId: 'e', agentId: 'a', runId: 'r', warnings: [] }),
    onClose: noop,
    onDispatched: noop,
    onSelectExistingAgent: noop,
    onOpenHarnessSettings: noop,
    ...over,
  }
  render(<ExecutionDialog {...props} />)
  return props
}

describe('U9 ExecutionDialog 绑定预选', () => {
  it('直接绑定：顶部提示 + harness 默认值取绑定 agent.harness（覆盖 bag defaultHarness）', async () => {
    await renderDialog(
      {
        agents: [makeAgent('agent-a', { harness: 'opencode', title: '重构登录' })],
      },
      baseRoutes({ defaultHarness: 'omp' }, { 'T-1': 'agent-a' }),
    )
    await waitFor(() => {
      const select = screen.getByRole('combobox') as HTMLSelectElement
      expect(select.value).toBe('opencode')
    })
    const hint = screen.getByText(/将派发至/)
    expect(hint.textContent).toContain('重构登录')
    expect(hint.textContent).toContain('直接')
    expect(hint.textContent).toContain('U11')
  })

  it('继承绑定：提示标注继承自哪个祖先节点', async () => {
    await renderDialog(
      { agents: [makeAgent('agent-a', { harness: 'opencode' })] },
      baseRoutes({}, { 'EPIC-1': 'agent-a' }),
    )
    await waitFor(() => expect(screen.getByText(/将派发至/)).toBeTruthy())
    expect(screen.getByText(/将派发至/).textContent).toContain('根 Epic')
  })

  it('绑定 agent 的 harness 未启用 → 不提示、回退 bag defaultHarness', async () => {
    await renderDialog(
      { agents: [makeAgent('agent-a', { harness: 'ghost-harness' })] },
      baseRoutes({ defaultHarness: 'omp' }, { 'T-1': 'agent-a' }),
    )
    await waitFor(() => {
      const select = screen.getByRole('combobox') as HTMLSelectElement
      expect(select.value).toBe('omp')
    })
    expect(screen.queryByText(/将派发至/)).toBeNull()
  })

  it('悬空绑定（agent 不在列表）→ 不提示、走默认链路；parseFailed → 同', async () => {
    await renderDialog({}, baseRoutes({}, { 'T-1': 'agent-gone' }))
    await waitFor(() => {
      const select = screen.getByRole('combobox') as HTMLSelectElement
      expect(select.value).toBe('omp')
    })
    expect(screen.queryByText(/将派发至/)).toBeNull()

    cleanup()
    // parseFailed：bindings_get 抛错（降级/不可用）→ 静默无提示
    await renderDialog({}, baseRoutes({ defaultHarness: 'omp' }, null))
    await waitFor(() => {
      const select = screen.getByRole('combobox') as HTMLSelectElement
      expect(select.value).toBe('omp')
    })
    expect(screen.queryByText(/将派发至/)).toBeNull()
  })
})
