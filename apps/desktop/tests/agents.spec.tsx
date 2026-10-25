/**
 * U5 agents/transcript 面板测试：事件按 execId 归并、尾窗有界、raw 合并
 * 刷新、跟随判定（纯函数 + jsdom 滚动度量覆写）、两段式终止、Failed
 * badge、列表呈现（displayName / Orphan 角标 / 零 claim 标注 / 空态）。
 * Mock 边界沿用 store.spec 的 tauri invoke 工厂——按 §3.6.8 补导出
 * Channel（凡 import Channel 的模块在旧 mock 下会崩）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen, act, waitFor, within } from '@testing-library/react'
import type { AgentRecord, ExecRecord, RunDetail } from '../src/types'
import type { TranscriptEvent } from '../src/bridge'

afterEach(cleanup)

const invokeMock = vi.fn()
// §3.6.8：mock 工厂必须可 new 且 onmessage 可注入（对齐 @tauri-apps/api
// Channel 的消费方式：前端只赋 onmessage，Rust 侧推流）
class ChannelMock {
  onmessage: ((message: unknown) => void) | null = null
  static instances: ChannelMock[] = []
  constructor() {
    ChannelMock.instances.push(this)
  }
  /** 测试注入入口：模拟 Rust 推流 */
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
// U16：通知插件 mock
const sendNotificationMock = vi.fn()
vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn().mockResolvedValue(true),
  requestPermission: vi.fn().mockResolvedValue('granted'),
  sendNotification: (...args: unknown[]) => sendNotificationMock(...args),
}))

// ── fixture 工厂 ──────────────────────────────────────────────────────────

const raw = (execId: string, seq: number, text: string, stream: 'stdout' | 'stderr' = 'stdout'): TranscriptEvent =>
  ({ execId, seq, ts: String(1000 + seq), kind: 'raw', stream, text })
const sessionStart = (execId: string, pid: number): TranscriptEvent =>
  ({ execId, seq: 0, ts: '900', kind: 'session_start', harness: 'omp', program: '/bin/omp', args: ['--x'], cwd: '/ws', pid })
const exitEvent = (execId: string, status: 'exited' | 'failed' | 'killed'): TranscriptEvent =>
  ({ execId, seq: 999, ts: '2000', kind: 'exit', status, code: status === 'failed' ? 1 : 0, killReason: null, durationMs: 5 })

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

function makeAgent(id: string, execId: string, over: Partial<AgentRecord> = {}): AgentRecord {
  return {
    id,
    displayName: `Agent ${id}`,
    title: null,
    harness: 'omp',
    status: 'running',
    attachment: 'live',
    lastExecId: execId,
    lastSessionId: null,
    execs: [makeExec(execId)],
    ...over,
  }
}

const runDetail = (items: Array<{ state: string; executorActor?: string }>): RunDetail =>
  ({ run: { id: 'RUN-1', status: 'running' }, items: items.map(i => ({ nodeId: 'T-1', state: i.state, attempts: 1, executorActor: i.executorActor })) })

const noop = () => {}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void
  const promise = new Promise<T>(done => { resolve = done })
  return { promise, resolve }
}

async function setupStore() {
  const { useTreeStore } = await import('../src/store')
  const { renderHook } = await import('@testing-library/react')
  return renderHook(() => useTreeStore())
}

interface PanelProps {
  agents: AgentRecord[]
  parseFailed: boolean
  selectedAgentId: string | null
  nodes: Array<{ id: string; type: string; title: string; status: string; priority: number; children?: Array<{ id: string; type: string; title: string; status: string; priority: number }> }>
  fetchRun: (runId: string) => Promise<RunDetail>
  eventsOf: (execId: string) => TranscriptEvent[]
  onSelectAgent: (id: string) => void
  onStopAgent: (execId: string) => void
  onResumeAgent?: (agent: AgentRecord) => Promise<void>
  onDeleteAgent?: (agent: AgentRecord) => Promise<void>
  onKillOrphan: (agentId: string, execId: string) => void
  onOpenHarnessSettings: () => void
}

async function renderPanel(over: Partial<PanelProps> = {}) {
  const { AgentsPanel } = await import('../src/AgentsPanel')
  const props: PanelProps = {
    agents: [],
    parseFailed: false,
    selectedAgentId: null,
    nodes: [],
    fetchRun: vi.fn().mockResolvedValue(runDetail([])),
    eventsOf: () => [],
    onSelectAgent: noop,
    onStopAgent: noop,
    onKillOrphan: noop,
    onOpenHarnessSettings: noop,
    ...over,
  }
  render(<AgentsPanel {...props} />)
  return props
}

// ── store：事件归并 / 尾窗 / 合并刷新 / badge ─────────────────────────────

describe('U5 store: transcript 归并', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('多 agent 事件按 execId 归并；切换灌入互不串窗', async () => {
    vi.useFakeTimers()
    try {
      const { result } = await setupStore()
      act(() => {
        result.current.ingestTranscriptEvent(raw('exec-a', 0, 'A0'))
        result.current.ingestTranscriptEvent(raw('exec-b', 0, 'B0'))
        result.current.ingestTranscriptEvent(raw('exec-a', 1, 'A1'))
      })
      act(() => { vi.advanceTimersByTime(60) })
      const textsOf = (execId: string) =>
        result.current.transcriptWindow(execId).map(e => (e.kind === 'raw' ? e.text : ''))
      expect(textsOf('exec-a')).toEqual(['A0', 'A1'])
      expect(textsOf('exec-b')).toEqual(['B0'])
      // 继续给 A 灌事件，B 的尾窗保留（每 exec 独立尾窗）
      act(() => { result.current.ingestTranscriptEvent(raw('exec-a', 2, 'A2')) })
      act(() => { vi.advanceTimersByTime(60) })
      expect(result.current.transcriptWindow('exec-b')).toHaveLength(1)
      expect(result.current.transcriptWindow('exec-a')).toHaveLength(3)
    } finally {
      vi.useRealTimers()
    }
  })

  it('单 exec 超 2000 条 → 尾窗有界（丢最旧）', async () => {
    vi.useFakeTimers()
    try {
      const { result } = await setupStore()
      act(() => {
        for (let i = 0; i < 2500; i++) result.current.ingestTranscriptEvent(raw('exec-cap', i, `t${i}`))
      })
      act(() => { vi.advanceTimersByTime(60) })
      const tail = result.current.transcriptWindow('exec-cap')
      expect(tail).toHaveLength(2000)
      expect((tail[0] as { text: string }).text).toBe('t500')
      expect((tail[1999] as { text: string }).text).toBe('t2499')
    } finally {
      vi.useRealTimers()
    }
  })

  it('raw 事件 50ms 合并刷新：500 条灌入只递增一次窗口版本', async () => {
    vi.useFakeTimers()
    try {
      const { result } = await setupStore()
      const before = result.current.state.transcriptVersion
      act(() => {
        for (let i = 0; i < 500; i++) result.current.ingestTranscriptEvent(raw('exec-flush', i, `x${i}`))
      })
      // flush 窗口未到期：不触发渲染信号
      expect(result.current.state.transcriptVersion).toBe(before)
      act(() => { vi.advanceTimersByTime(50) })
      expect(result.current.state.transcriptVersion).toBe(before + 1)
      expect(result.current.transcriptWindow('exec-flush')).toHaveLength(500)
    } finally {
      vi.useRealTimers()
    }
  })

  it('exit failed 事件：本地 status/attachment 联动 + badge +1；选中清零', async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'agents_list') {
        return Promise.resolve({ agents: [makeAgent('agent-a', 'exec-a')], agentSeq: 1, parseFailed: false })
      }
      return Promise.resolve({})
    })
    const { result } = await setupStore()
    await act(async () => {
      await result.current.selectHome({ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' })
    })
    expect(result.current.state.agents).toHaveLength(1)
    act(() => { result.current.ingestTranscriptEvent(exitEvent('exec-a', 'failed')) })
    expect(result.current.state.agents[0].status).toBe('failed')
    expect(result.current.state.agents[0].attachment).toBe('none')
    expect(result.current.state.unviewedFailedIds).toEqual(['agent-a'])
    // 选中该 agent → badge 清除且不复活
    act(() => { result.current.selectAgent('agent-a') })
    expect(result.current.state.unviewedFailedIds).toEqual([])
    await act(async () => { await result.current.refreshAgents() })
    expect(result.current.state.unviewedFailedIds).toEqual([])
  })

  it('U16：exit failed → 系统通知（agent 名 + 终止详情）；exited 不通知', async () => {
    sendNotificationMock.mockClear()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'agents_list')
        return Promise.resolve({ agents: [makeAgent('agent-n', 'exec-n', { title: '冒烟任务' })], agentSeq: 1, parseFailed: false })
      return Promise.resolve({})
    })
    const { result } = await setupStore()
    await act(async () => {
      await result.current.selectHome({ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' })
    })
    // exited：不通知
    act(() => { result.current.ingestTranscriptEvent(exitEvent('exec-n', 'exited')) })
    await act(async () => { await Promise.resolve() })
    expect(sendNotificationMock).not.toHaveBeenCalled()
    // failed：通知带 agent 标题与退出码
    act(() => { result.current.ingestTranscriptEvent(exitEvent('exec-n', 'failed')) })
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(sendNotificationMock).toHaveBeenCalledTimes(1)
    const note = sendNotificationMock.mock.calls[0][0] as { title: string; body: string }
    expect(note.title).toContain('失败')
    expect(note.title).toContain('冒烟任务')
    expect(note.body).toMatch(/exit 1/)
  })

  it('切换 workspace 后丢弃上一个 home 延迟返回的 agents/bindings 结果', async () => {
    const agentsH1 = deferred<unknown>()
    const agentsH2 = deferred<unknown>()
    const bindingsH1 = deferred<unknown>()
    const bindingsH2 = deferred<unknown>()
    invokeMock.mockImplementation((cmd: string, args?: { homeId?: string }) => {
      if (cmd === 'agents_list') return args?.homeId === 'h1' ? agentsH1.promise : agentsH2.promise
      if (cmd === 'bindings_get') return args?.homeId === 'h1' ? bindingsH1.promise : bindingsH2.promise
      return Promise.resolve({})
    })
    const { result } = await setupStore()
    const home1 = { homeId: 'h1', kind: 'workspace' as const, path: '/tmp/h1/.omt' }
    const home2 = { homeId: 'h2', kind: 'workspace' as const, path: '/tmp/h2/.omt' }

    let selectH1!: Promise<void>
    act(() => { selectH1 = result.current.selectHome(home1) })
    let selectH2!: Promise<void>
    act(() => { selectH2 = result.current.selectHome(home2) })

    await act(async () => {
      agentsH2.resolve({ agents: [makeAgent('agent-h2', 'exec-h2')], parseFailed: false })
      bindingsH2.resolve({ bindings: { 'T-2': 'agent-h2' }, parseFailed: true })
      await selectH2
    })
    expect(result.current.state.activeHome?.homeId).toBe('h2')
    expect(result.current.state.agents.map(agent => agent.id)).toEqual(['agent-h2'])
    expect(result.current.state.bindingsByHome.h2).toEqual({ 'T-2': 'agent-h2' })
    expect(result.current.state.bindingsParseFailed).toBe(true)

    await act(async () => {
      agentsH1.resolve({ agents: [makeAgent('agent-h1', 'exec-h1')], parseFailed: true })
      bindingsH1.resolve({ bindings: { 'T-1': 'agent-h1' }, parseFailed: false })
      await selectH1
    })
    expect(result.current.state.activeHome?.homeId).toBe('h2')
    expect(result.current.state.agents.map(agent => agent.id)).toEqual(['agent-h2'])
    expect(result.current.state.parseFailed).toBe(false)
    expect(result.current.state.bindingsByHome.h2).toEqual({ 'T-2': 'agent-h2' })
    expect(result.current.state.bindingsParseFailed).toBe(true)
  })
})

describe('U5 store: claimed 判定', () => {
  it('claimed 判定用 endsWith（嵌套 actor 命名空间，防前缀撞名）', async () => {
    const { claimedCountForExec } = await import('../src/store')
    const items = [
      { executorActor: 'mcp:4242/exec:exec-a' }, // 嵌套命名空间：命中
      { executorActor: 'mcp:9/exec:exec-ab' },   // 前缀撞名：不得计入
      { executorActor: 'exec:exec-a' },          // 裸 actor（无命名空间）：不命中
      { executorActor: 'cli:1' },
      {},
    ]
    expect(claimedCountForExec(items, 'exec-a')).toBe(1)
  })
})

// ── store：绑定清理 / 终止动作（错误呈现与防抖重拉契约）───────────────────

describe('U9/U5 store 动作：gcBindings / stopAgent / killOrphan', () => {
  const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }

  beforeEach(() => {
    invokeMock.mockReset()
  })

  it('gcBindings：bindings_gc 成功后重拉绑定表更新 state', async () => {
    let bindings: Record<string, string> = { 'T-1': 'ghost', 'T-2': 'phantom' }
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'bindings_get') return { bindings, parseFailed: false }
      if (cmd === 'bindings_gc') {
        bindings = {}
        return {}
      }
      return {}
    })
    const { result } = await setupStore()
    await act(async () => { await result.current.selectHome(home) })
    expect(result.current.state.bindingsByHome['h1']).toEqual({ 'T-1': 'ghost', 'T-2': 'phantom' })
    await act(async () => { await result.current.gcBindings() })
    expect(result.current.state.bindingsByHome['h1']).toEqual({})
    expect(result.current.state.error).toBeNull()
  })

  it('gcBindings：bindings_gc reject（降级冻结）→ error 呈现且不重拉绑定表', async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'bindings_get') return { bindings: { 'T-1': 'ghost' }, parseFailed: true }
      if (cmd === 'bindings_gc') return Promise.reject(new Error('agents.json 解析失败，GC 冻结'))
      return {}
    })
    const { result } = await setupStore()
    await act(async () => { await result.current.selectHome(home) })
    const callsBefore = invokeMock.mock.calls.filter(c => c[0] === 'bindings_get').length
    await act(async () => { await result.current.gcBindings() })
    // presentError 原样透传（非已知契约错误）
    expect(result.current.state.error).toContain('冻结')
    // 失败路径不重拉（重拉只会原样返回降级表）
    const callsAfter = invokeMock.mock.calls.filter(c => c[0] === 'bindings_get').length
    expect(callsAfter).toBe(callsBefore)
  })

  it('U10 resumeAgent：未做过知情确认 → 拒绝并引导；已确认 → 定向续跑参数齐备', async () => {
    let bag: Record<string, unknown> = { models: { omp: 'k3' } } // 无 approvalAck
    const execStartCalls: Array<Record<string, unknown>> = []
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'omt_call' && args?.method === 'ui/filters-get')
        return { filters: (args.params as { key: string }).key === 'tauri:harness' ? bag : {} }
      if (cmd === 'omt_call') return { node: {}, children: [], runs: [] }
      if (cmd === 'exec_start') {
        execStartCalls.push(args ?? {})
        return { execId: 'exec-new', agentId: 'agent-1', runId: 'run-1', warnings: [] }
      }
      return {}
    })
    const agent = makeAgent('agent-1', 'exec-old', {
      status: 'exited',
      attachment: 'none',
      lastSessionId: 'ses_prev',
    })
    // execs[0] 带 nodeId——makeAgent 的 makeExec 无 nodeId，手工补
    agent.execs[0].nodeId = 'T-9'
    const { result } = await setupStore()
    await act(async () => { await result.current.selectHome(home) })

    // 门 1：未确认 → 拒绝且不发 exec_start
    await expect(result.current.resumeAgent(agent)).rejects.toThrow(/知情确认/)
    expect(execStartCalls.length).toBe(0)

    // 门 2：已确认 → 定向续跑（agentId/resumeSession/记忆模型齐备）
    bag = { approvalAck: { omp: true }, models: { omp: 'k3' } }
    await act(async () => { await result.current.resumeAgent(agent) })
    expect(execStartCalls.length).toBe(1)
    expect(execStartCalls[0]).toMatchObject({
      agentId: 'agent-1',
      resumeSession: 'ses_prev',
      model: 'k3',
      harness: 'omp',
      nodeId: 'T-9',
    })
  })

  it('stopAgent：exec_stop reject → 错误经 presentError 进全局 error', async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'exec_stop') return Promise.reject(new Error('CONFLICT: revision mismatch expected 3 got 5'))
      return {}
    })
    const { result } = await setupStore()
    await act(async () => { await result.current.stopAgent('exec-a') })
    expect(result.current.state.error).toContain('revision 冲突')
  })

  it('killOrphan：exec_kill_orphan 成功 → 防抖后 agents_list 重拉', async () => {
    vi.useFakeTimers()
    try {
      invokeMock.mockResolvedValue({})
      const { result } = await setupStore()
      await act(async () => { await result.current.selectHome(home) })
      const before = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
      await act(async () => { await result.current.killOrphan('agent-o', 'exec-o') })
      expect(result.current.state.error).toBeNull()
      expect(invokeMock).toHaveBeenCalledWith('exec_kill_orphan', {
        homeId: 'h1',
        agentId: 'agent-o',
        execId: 'exec-o',
      })
      // 400ms 防抖（等 Rust 侧落盘收敛）到期才重拉
      act(() => { vi.advanceTimersByTime(500) })
      const after = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
      expect(after).toBeGreaterThan(before)
    } finally {
      vi.useRealTimers()
    }
  })
})

// ── 纯函数：跟随判定 / 窗口切片 / 空闲判定 ────────────────────────────────

describe('U5 纯函数（TranscriptView 视口语义）', () => {
  it('shouldFollow：贴底/阈值内跟随，上滚解除', async () => {
    const { shouldFollow } = await import('../src/TranscriptView')
    // scrollHeight=1000 / clientHeight=500 → 最大 scrollTop=500
    expect(shouldFollow(500, 1000, 500)).toBe(true) // 贴底
    expect(shouldFollow(460, 1000, 500)).toBe(true) // 距底 40px（阈值内）
    expect(shouldFollow(459, 1000, 500)).toBe(false) // 距底 41px（出阈值）
    expect(shouldFollow(200, 1000, 500)).toBe(false)
  })

  it('visibleRange：视口 + overscan 切片且不越界', async () => {
    const { visibleRange } = await import('../src/TranscriptView')
    expect(visibleRange(0, 440, 100)).toEqual({ start: 0, end: 40 })
    expect(visibleRange(2200, 440, 100)).toEqual({ start: 90, end: 100 })
    expect(visibleRange(0, 440, 0)).toEqual({ start: 0, end: 0 })
  })

  // ── F-ACC-5 回归：多行文本曾撑破 22px 行高导致绝对定位行互相叠印 ──

  it('stripAnsi：剥 CSI/OSC 转义与裸控制符', async () => {
    const { stripAnsi } = await import('../src/TranscriptView')
    expect(stripAnsi('\u001b[91mError\u001b[0m')).toBe('Error')
    expect(stripAnsi('\u001b]0;title\u0007after')).toBe('after')
    expect(stripAnsi('a\u0007b\u0000c\u007f')).toBe('abc')
  })

  it('flattenEvents：多行 raw / \\r 重写展开为独立行，stderr 语义保留、key 唯一', async () => {
    const { flattenEvents } = await import('../src/TranscriptView')
    const lines = flattenEvents([
      raw('e', 1, 'a\nb\r\nc\rd'),
      raw('e', 2, '\u001b[91mboom\u001b[0m', 'stderr'),
    ])
    expect(lines.map((l) => [l.kind, l.text])).toEqual([
      ['out', 'a'], ['out', 'b'], ['out', 'c'], ['out', 'd'], ['err', 'boom'],
    ])
    expect(new Set(lines.map((l) => l.key)).size).toBe(lines.length)
  })

  it('flattenEvents：session_start 携带多行 prompt 时按行展开（不再撑破行高）', async () => {
    const { flattenEvents } = await import('../src/TranscriptView')
    const lines = flattenEvents([{
      execId: 'e', seq: 0, ts: '1', kind: 'session_start', harness: 'opencode',
      program: '/bin/opencode', args: ['--auto', 'You are executing…\nrule 1\nrule 2'],
      cwd: '/ws', pid: 7,
    }])
    expect(lines.length).toBe(3)
    expect(lines[0].text).toContain('▶ /bin/opencode')
    expect(lines.every((l) => l.kind === 'start')).toBe(true)
    expect(lines.every((l) => !l.text.includes('\n') && !l.text.includes('\r'))).toBe(true)
  })

  it('flattenEvents：U12 结构化事件（text/thinking/tool/usage/error）映射与截断', async () => {
    const { flattenEvents } = await import('../src/TranscriptView')
    const base = { execId: 'e', ts: '1' }
    const lines = flattenEvents([
      { ...base, seq: 1, kind: 'assistant_text', text: 'DONE' },
      { ...base, seq: 2, kind: 'assistant_thinking', text: 'step1\nstep2' },
      { ...base, seq: 3, kind: 'tool_call', name: 'bash', input: { command: 'echo hi' } },
      { ...base, seq: 4, kind: 'tool_result', summary: 'hi', isError: true },
      { ...base, seq: 5, kind: 'usage', tokens: 17137, cost: 0.0209 },
      { ...base, seq: 6, kind: 'error', message: 'boom' },
    ])
    expect(lines.map(l => [l.kind, l.text])).toEqual([
      ['out', 'DONE'],
      ['think', 'step1'],
      ['think', 'step2'],
      ['tool', '⚙ bash {"command":"echo hi"}'],
      ['toolres', '↳ hi'],
      ['usage', '⚡ · 17,137 tok · $0.0209'],
      ['err', '⚠ boom'],
    ])
    expect(lines[4].toolError).toBe(true)
    // 超长 tool input 截断为单行
    const long = flattenEvents([
      { ...base, seq: 7, kind: 'tool_call', name: 'write', input: { body: 'x'.repeat(400) } },
    ])
    expect(long[0].text.length).toBeLessThanOrEqual(212)
  })

  it('flattenEvents：exit 行携带 failed 标记与结构化文本', async () => {
    const { flattenEvents } = await import('../src/TranscriptView')
    const lines = flattenEvents([exitEvent('e', 'failed')])
    expect(lines).toEqual([
      { key: 'exit-999-0', kind: 'exit', failed: true, text: '■ failed · code 1 · 0.0s' },
    ])
  })

  it('isIdle：live 且距最后事件超阈值判空闲', async () => {
    const { isIdle } = await import('../src/TranscriptView')
    expect(isIdle(undefined, 999_999)).toBe(false)
    expect(isIdle(1000, 61_001)).toBe(true) // 60001ms > 阈值
    expect(isIdle(1000, 61_000)).toBe(false) // 恰在阈值上不计
  })
})

// ── TranscriptView：跟随 / pill / 行渲染 ──────────────────────────────────

describe('U5 TranscriptView 自动跟随', () => {
  it('上滚后新事件不抢滚动位、pill 计数递增；点击回贴底并恢复跟随', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const events0 = Array.from({ length: 50 }, (_, i) => raw('exec-a', i, `line-${i}`))
    const agent = makeAgent('agent-a', 'exec-a')
    const { rerender } = render(<TranscriptView agent={agent} events={events0} />)
    fireEvent.click(screen.getByRole('button', { name: 'Raw' }))
    const scrollEl = document.querySelector('.t-scroll') as HTMLElement
    expect(scrollEl).toBeTruthy()
    // jsdom 无布局：覆写该实例的滚动度量（getter/setter 捕获赋值）
    let scrollTop = 0
    Object.defineProperty(scrollEl, 'scrollTop', { get: () => scrollTop, set: (v: number) => { scrollTop = v }, configurable: true })
    Object.defineProperty(scrollEl, 'clientHeight', { value: 500, configurable: true })
    Object.defineProperty(scrollEl, 'scrollHeight', { value: 1000, configurable: true })
    // 初始挂载时 jsdom 度量尚未注入（scrollHeight=0）——覆写后以新数组
    // rerender 触发贴底效果
    rerender(<TranscriptView agent={agent} events={[...events0]} />)
    expect(scrollTop).toBe(1000)
    // 模拟上滚 → 解除跟随
    act(() => {
      scrollTop = 300
      fireEvent.scroll(scrollEl)
    })
    // 新事件到达：不抢滚动位，pill 出现且计数递增
    const events1 = [...events0, raw('exec-a', 50, 'line-50'), raw('exec-a', 51, 'line-51')]
    rerender(<TranscriptView agent={agent} events={events1} />)
    expect(scrollTop).toBe(300)
    expect(screen.getByText('回到最新 (2)')).toBeTruthy()
    // 点击 pill → 回贴底 + 恢复跟随 + pill 消失
    fireEvent.click(screen.getByText(/回到最新/))
    expect(scrollTop).toBe(1000)
    expect(screen.queryByText(/回到最新/)).toBeNull()
  })

  it('U10 一键续跑：空闲 agent 渲染「继续执行」，点击回调；live 不渲染', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const idle = makeAgent('agent-r', 'exec-r', { status: 'exited', attachment: 'none', lastSessionId: 'ses_x' })
    const { rerender } = render(
      <TranscriptView agent={idle} events={[]} onResumeAgent={() => Promise.resolve()} />,
    )
    const btn = screen.getByRole('button', { name: '继续执行' })
    expect(btn.getAttribute('title')).toContain('续跑上次会话')
    fireEvent.click(btn) // 回调型 prop——不抛错即通过（行为断言在面板层）
    const live = makeAgent('agent-r2', 'exec-r2', { status: 'running' })
    rerender(<TranscriptView agent={live} events={[]} onResumeAgent={() => Promise.resolve()} />)
    expect(screen.queryByRole('button', { name: '继续执行' })).toBeNull()
  })

  it('U14：底部信任门弹窗——待确认项渲染 + 确认/打回动作与刷新', async () => {
    const { AgentsPanel } = await import('../src/AgentsPanel')
    let detail: Record<string, unknown> = {
      run: { id: 'RUN-G', status: 'running' },
      items: [
        { nodeId: 'T-G', state: 'awaiting_confirmation', attempts: 1, title: 'Gate' },
      ],
    }
    const calls: Array<{ method: string; params: Record<string, unknown> }> = []
    invokeMock.mockImplementation(async (_cmd: string, args?: { method: string; params: Record<string, unknown> }) => {
      if (args?.method === 'run/get') return detail
      if (args) calls.push(args)
      if (args?.method === 'run/report') {
        detail = { run: { id: 'RUN-G', status: 'running' }, items: [{ nodeId: 'T-G', state: 'done', attempts: 1 }] }
        return {}
      }
      if (args?.method === 'node/update') {
        detail = { run: { id: 'RUN-G', status: 'running' }, items: [{ nodeId: 'T-G', state: 'interrupted', attempts: 1 }] }
        return {}
      }
      return {}
    })
    const agent = makeAgent('agent-g', 'exec-g', { status: 'exited' })
    agent.execs[0].runId = 'RUN-G'
    // fetchRun 是面板的数据源（读实时可变的 detail——动作后刷新拿新状态）
    renderPanel({ agents: [agent], fetchRun: async () => detail as never })
    // 弹窗出现且含待确认项
    const dialog = await waitFor(() => screen.getByRole('alertdialog'))
    expect(dialog.textContent).toContain('1 项完成待人工确认')
    expect(dialog.textContent).toContain('T-G')
    // 确认 → run/report done + 弹窗消失（刷新后无待确认项）
    fireEvent.click(within(dialog).getByRole('button', { name: '确认' }))
    await waitFor(() => {
      const call = calls.find(c => c.method === 'run/report')
      expect(call?.params).toMatchObject({ runId: 'RUN-G', nodeId: 'T-G', outcome: 'done' })
    })
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
  })

  it('U14 兜底扫描：外部执行者触发的待确认 run（无 agent 记录）弹窗仍出现', async () => {
    const { AgentsPanel } = await import('../src/AgentsPanel')
    invokeMock.mockImplementation(async (_cmd: string, args?: { method: string; params?: { runId?: string } }) => {
      if (args?.method === 'run/list')
        return { runs: [{ runId: 'RUN-EXT', status: 'running' }, { runId: 'RUN-OLD', status: 'completed' }] }
      if (args?.method === 'run/get' && args.params?.runId === 'RUN-EXT')
        return {
          run: { id: 'RUN-EXT', status: 'running' },
          items: [{ nodeId: 'T-EXT', state: 'awaiting_confirmation', attempts: 1, title: '外部' }],
        }
      return { run: { id: 'X', status: 'completed' }, items: [] }
    })
    // 空列表表面板：仅兜底扫描发现（fetchRun 按 runId 路由到同一 mock 数据）
    const route = async (_cmd: string, args?: { method: string; params?: { runId?: string } }) => {
      if (args?.method === 'run/get' && args.params?.runId === 'RUN-EXT')
        return {
          run: { id: 'RUN-EXT', status: 'running' },
          items: [{ nodeId: 'T-EXT', state: 'awaiting_confirmation', attempts: 1, title: '外部' }],
        }
      return { run: { id: 'X', status: 'completed' }, items: [] }
    }
    renderPanel({ agents: [], fetchRun: (runId: string) => route('omt_call', { method: 'run/get', params: { runId } }) as never })
    const dialog = await waitFor(() => screen.getByRole('alertdialog'))
    expect(dialog.textContent).toContain('T-EXT')
  })

  it('U-VIEW groupTurns：轮次边界/摘要计数/用量挂轮尾/raw JSON 收纳', async () => {
    const { groupTurns } = await import('../src/TranscriptView')
    const base = { execId: 'e', ts: '1000' }
    const turns = groupTurns([
      { ...base, seq: 1, ts: '1000', kind: 'assistant_thinking' as const, text: 'plan' },
      { ...base, seq: 2, ts: '1500', kind: 'tool_call' as const, name: 'bash', input: { command: 'echo hi' } },
      { ...base, seq: 3, ts: '2000', kind: 'tool_result' as const, summary: 'hi', isError: false },
      { ...base, seq: 4, ts: '2500', kind: 'assistant_text' as const, text: 'answer-1' },
      { ...base, seq: 5, ts: '3000', kind: 'usage' as const, tokens: 100, cost: 0.01 },
      { ...base, seq: 6, ts: '3500', kind: 'assistant_text' as const, text: 'answer-2' },
    ])
    // 轮边界：answer-1 结束第一轮（usage 归第一轮），answer-2 独立一轮
    expect(turns.length).toBe(2)
    expect(turns[0].thinkingCount).toBe(1)
    expect(turns[0].toolCount).toBe(1)
    expect(turns[0].durationMs).toBe(2000) // 1000 → 3000（usage 在回答后到达，归本轮）
    expect(turns[0].usage).toEqual({ tokens: 100, cost: 0.01 })
    expect(turns[0].answered).toBe(true)
    // 工具配对
    const tool = turns[0].entries.find(e => e.kind === 'tool') as { kind: 'string' } as never as { summary: string }
    expect(tool.summary).toBe('hi')
    // 第二轮：无过程无用量
    expect(turns[1].toolCount).toBe(0)
    expect(turns[1].usage).toBeNull()
    expect(turns[1].durationMs).toBe(0)
  })

  it('U-VIEW 回归：store 尾窗同引用原地追加事件 → 普通视图实时更新（运行中不动 bug）', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const agent = makeAgent('agent-live', 'exec-live', { status: 'running' })
    // 镜像 store 尾窗语义：稳定数组引用 + 原地 push
    const window: TranscriptEvent[] = [raw('exec-live', 1, 'boot-line')]
    const { rerender } = render(<TranscriptView agent={agent} events={window} />)
    expect(screen.getByText('boot-line')).toBeTruthy()
    act(() => {
      window.push({ execId: 'exec-live', seq: 2, ts: '2', kind: 'assistant_text', text: 'live-answer' })
    })
    rerender(<TranscriptView agent={agent} events={window} />)
    expect(screen.getByText('live-answer')).toBeTruthy()
  })

  it('U-VIEW 普通视图（轮次）：完成轮折叠摘要可展开、markdown 正文、用量轮尾', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const agent = makeAgent('agent-t', 'exec-t', { status: 'exited' })
    const events = [
      raw('exec-t', 1, '{"type":"legacy_json"}'),
      { execId: 'exec-t', seq: 2, ts: '2', kind: 'assistant_thinking' as const, text: 'first-line-only\nsecret-thought-detail' },
      { execId: 'exec-t', seq: 3, ts: '3', kind: 'tool_call' as const, name: 'bash', input: { command: 'echo TOOLPROBE' } },
      { execId: 'exec-t', seq: 4, ts: '4', kind: 'tool_result' as const, summary: 'TOOLPROBE', isError: false },
      { execId: 'exec-t', seq: 5, ts: '5', kind: 'assistant_text' as const, text: '**bold answer**' },
      { execId: 'exec-t', seq: 6, ts: '6', kind: 'usage' as const, tokens: 42, cost: 0.001 },
      exitEvent('exec-t', 'exited'),
    ]
    render(<TranscriptView agent={agent} events={events} />)
    // 完成轮折叠：思考/工具正文不可见，轮头摘要可见
    expect(screen.queryByText(/secret-thought-detail/)).toBeNull()
    expect(screen.getByText(/1 次工具调用/)).toBeTruthy()
    // markdown 正文渲染（粗体成 strong）
    expect(screen.getByText('bold answer')).toBeTruthy()
    // 用量挂轮尾
    expect(screen.getByText(/本轮用量 · 42 tok/)).toBeTruthy()
    // 旧 JSON raw 行不直接糊屏（收纳，不在主列表逐行展示）
    expect(screen.queryByText('{"type":"legacy_json"}')).toBeNull()
    // 展开轮 → 思考/工具明细出现
    fireEvent.click(screen.getByRole('button', { name: /1 次工具调用/ }))
    expect(screen.getByText('first-line-only')).toBeTruthy() // 思考条目折叠摘要可见
    // 终态分隔行
    expect(screen.getByText(/■ exited/)).toBeTruthy()
  })

  it('U14：awaitingCount > 0 渲染信任门提示条；0/null 不渲染', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const agent = makeAgent('agent-a9', 'exec-a9', { status: 'exited' })
    const { rerender } = render(<TranscriptView agent={agent} events={[]} awaitingCount={2} />)
    expect(screen.getByText(/2 项完成待人工确认/)).toBeTruthy()
    rerender(<TranscriptView agent={agent} events={[]} awaitingCount={0} />)
    expect(screen.queryByText(/待人工确认/)).toBeNull()
  })

  it('行渲染：stdout/stderr 区分、session_start/exit 结构化呈现', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const events = [
      sessionStart('exec-a', 4242),
      raw('exec-a', 1, 'out-line'),
      raw('exec-a', 2, 'err-line', 'stderr'),
      exitEvent('exec-a', 'exited'),
    ]
    const { container } = render(<TranscriptView agent={makeAgent('agent-a', 'exec-a', { status: 'exited' })} events={events} />)
    // 行式断言面在 Raw 视图（默认普通视图不再渲染 .t-row 行）
    fireEvent.click(screen.getByRole('button', { name: 'Raw' }))
    expect(container.querySelector('.t-start')?.textContent).toContain('pid 4242')
    expect(container.querySelector('.t-stderr')?.textContent).toBe('err-line')
    const stderrCount = container.querySelectorAll('.t-stderr').length
    expect(stderrCount).toBe(1)
    expect(container.querySelector('.t-exit')?.textContent).toContain('code 0')
  })

  it('live agent 超过空闲阈值提示「长时间无输出」', async () => {
    const { TranscriptView } = await import('../src/TranscriptView')
    const stale = { ...raw('exec-a', 0, 'old'), ts: String(Date.now() - 120_000) }
    render(<TranscriptView agent={makeAgent('agent-a', 'exec-a')} events={[stale]} />)
    expect(screen.getByText(/长时间无输出/)).toBeTruthy()
  })

  it('live 期间静默：重估定时器在超过阈值后出现空闲提示；非 live 后不再出现', async () => {
    vi.useFakeTimers()
    try {
      const { TranscriptView } = await import('../src/TranscriptView')
      // 挂载时刻的新鲜事件（fake timers 下 Date.now() 即 mock 时钟）
      const fresh = { ...raw('exec-a', 0, 'line'), ts: String(Date.now()) }
      const agent = makeAgent('agent-a', 'exec-a')
      const { rerender } = render(<TranscriptView agent={agent} events={[fresh]} />)
      // 无新事件（无重渲染）：15s 重估 interval 推进时钟，60s 阈值前不提示
      act(() => { vi.advanceTimersByTime(15_000) })
      expect(screen.queryByText(/长时间无输出/)).toBeNull()
      // nowTick 按 15s 周期量化——推进到 76s（最后一次重估 t=75s）后提示出现
      act(() => { vi.advanceTimersByTime(61_000) })
      expect(screen.getByText(/长时间无输出/)).toBeTruthy()
      // live 翻转（exit）→ 提示消失且不再随时间复现
      rerender(<TranscriptView agent={makeAgent('agent-a', 'exec-a', { status: 'exited', attachment: 'none' })} events={[fresh]} />)
      expect(screen.queryByText(/长时间无输出/)).toBeNull()
      act(() => { vi.advanceTimersByTime(120_000) })
      expect(screen.queryByText(/长时间无输出/)).toBeNull()
    } finally {
      vi.useRealTimers()
    }
  })
})

// ── AgentsPanel：列表呈现 / 两段式终止 / Orphan / 空态 ────────────────────

describe('U5 AgentsPanel 列表', () => {
  it('F-DEL：终态 agent 行右上角删除 icon → 确认弹窗 → 确认调用 onDeleteAgent；取消不调用；运行中隐藏', async () => {
    const onDeleteAgent = vi.fn().mockResolvedValue(undefined)
    const idle = makeAgent('agent-d1', 'exec-d1', { status: 'exited', attachment: 'none' })
    const running = makeAgent('agent-d2', 'exec-d2', { status: 'running' })
    await renderPanel({ agents: [idle, running], onDeleteAgent })
    // 运行中无删除 icon；终态有
    expect(screen.queryByRole('button', { name: '删除 Agent 2' })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '删除 Agent agent-d1' }))
    // 确认弹窗出现（危险态文案）
    expect(screen.getByRole('alertdialog', { name: '确认删除 agent' })).toBeTruthy()
    expect(screen.getByText(/此操作不可恢复/)).toBeTruthy()
    // 取消 → 不调用
    fireEvent.click(screen.getByRole('button', { name: '取消' }))
    expect(onDeleteAgent).not.toHaveBeenCalled()
    // 再删 → 确认 → 调用并关闭
    fireEvent.click(screen.getByRole('button', { name: '删除 Agent agent-d1' }))
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }))
    await waitFor(() => expect(onDeleteAgent).toHaveBeenCalledWith(idle))
  })

  it('F-DEL：删除失败（运行守卫）→ 弹窗内就地报错不关闭', async () => {
    const onDeleteAgent = vi.fn().mockRejectedValue(new Error('agent 仍在运行——请先停止再删除'))
    const idle = makeAgent('agent-d3', 'exec-d3', { status: 'exited', attachment: 'none' })
    await renderPanel({ agents: [idle], onDeleteAgent })
    fireEvent.click(screen.getByRole('button', { name: '删除 Agent agent-d3' }))
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }))
    await waitFor(() => expect(screen.getByText(/仍在运行/)).toBeTruthy())
    expect(screen.getByRole('alertdialog', { name: '确认删除 agent' })).toBeTruthy()
  })

  beforeEach(() => invokeMock.mockReset())

  it('条目渲染 display_name：title 优先，回退记录 displayName', async () => {
    await renderPanel({
      agents: [
        makeAgent('a1', 'exec-a', { displayName: 'Agent 1', title: '重构登录' }),
        makeAgent('a2', 'exec-b', { displayName: 'Agent 2', title: null, status: 'exited' }),
      ],
    })
    expect(screen.getByText('重构登录')).toBeTruthy()
    expect(screen.getByText('Agent 2')).toBeTruthy()
  })

  it('A8 发起方标签：最新 exec launchedBy=mcp 的行带标签；desktop / 缺省不出现', async () => {
    const mcp = makeAgent('a-mcp', 'exec-m')
    mcp.execs[0].launchedBy = 'mcp'
    const desktop = makeAgent('a-dsk', 'exec-d')
    desktop.execs[0].launchedBy = 'desktop'
    const legacy = makeAgent('a-old', 'exec-o', { status: 'exited' })
    await renderPanel({ agents: [mcp, desktop, legacy] })
    expect(screen.getByLabelText('由 MCP（主控 agent）发起')).toBeTruthy()
    expect(screen.queryByLabelText('由 CLI 发起')).toBeNull()
  })

  it('origin chip names who launched the displayed exec', async () => {
    const { originLabel } = await import('../src/AgentsPanel')
    expect(originLabel('mcp')).toEqual({ text: 'MCP', title: '由 MCP（主控 agent）发起' })
    expect(originLabel('cli')).toEqual({ text: 'CLI', title: '由 CLI 发起' })
  })

  it('run item 统计（完成 N / 失败 M）走 fetchRun 渲染', async () => {
    await renderPanel({
      agents: [makeAgent('a1', 'exec-a', { execs: [makeExec('exec-a', 'RUN-1')] })],
      fetchRun: vi.fn().mockResolvedValue(runDetail([
        { state: 'done', executorActor: 'mcp:1/exec:exec-a' },
        { state: 'running', executorActor: 'mcp:1/exec:exec-a' },
        { state: 'failed', executorActor: 'cli:9' },
      ])),
    })
    await act(async () => { await Promise.resolve() })
    expect(screen.getByText(/完成 1 \/ 失败 1/)).toBeTruthy()
  })

  it('终止两段式：首击显示 N 且不调用 stop；二击调用', async () => {
    const onStopAgent = vi.fn()
    await renderPanel({
      agents: [makeAgent('a1', 'exec-a', { execs: [makeExec('exec-a', 'RUN-1')] })],
      // 不选中 agent：行内按钮与 TranscriptView 头部按钮只保留行内一个
      fetchRun: vi.fn().mockResolvedValue(runDetail([
        { state: 'running', executorActor: 'mcp:4242/exec:exec-a' },
        { state: 'running', executorActor: 'mcp:4242/exec:exec-a' },
        { state: 'done', executorActor: 'cli:1' },
      ])),
      onStopAgent,
    })
    await act(async () => { await Promise.resolve() })
    fireEvent.click(screen.getByRole('button', { name: '终止' }))
    expect(onStopAgent).not.toHaveBeenCalled()
    expect(screen.getByText('确认终止：将释放 2 个已认领项')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '确认终止' }))
    expect(onStopAgent).toHaveBeenCalledWith('exec-a')
  })

  it('确认态 3s 无操作还原', async () => {
    vi.useFakeTimers()
    try {
      await renderPanel({
        agents: [makeAgent('a1', 'exec-a', { execs: [makeExec('exec-a', 'RUN-1')] })],
        fetchRun: vi.fn().mockResolvedValue(runDetail([])),
      })
      await act(async () => { await Promise.resolve() })
      fireEvent.click(screen.getByRole('button', { name: '终止' }))
      expect(screen.getByRole('button', { name: '确认终止' })).toBeTruthy()
      act(() => { vi.advanceTimersByTime(3000) })
      expect(screen.queryByRole('button', { name: '确认终止' })).toBeNull()
      expect(screen.getByRole('button', { name: '终止' })).toBeTruthy()
    } finally {
      vi.useRealTimers()
    }
  })

  it('Orphan 行按持久化 agent/exec 身份强制终止，不向 renderer 暴露 pid 授权', async () => {
    const onKillOrphan = vi.fn()
    await renderPanel({
      agents: [makeAgent('o1', 'exec-o', { attachment: 'orphan' })],
      onKillOrphan,
    })
    expect(screen.getByText('不受控·只读')).toBeTruthy()
    expect(screen.queryByRole('button', { name: '终止' })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '强制终止' }))
    expect(onKillOrphan).toHaveBeenCalledWith('o1', 'exec-o')
  })

  it('零 claim 退出标注「未认领任何项」', async () => {
    await renderPanel({
      agents: [makeAgent('a1', 'exec-a', { status: 'exited', attachment: 'none', execs: [makeExec('exec-a', 'RUN-1')] })],
      fetchRun: vi.fn().mockResolvedValue(runDetail([{ state: 'done', executorActor: 'cli:1' }])),
    })
    await act(async () => { await Promise.resolve() })
    expect(screen.getByText('未认领任何项')).toBeTruthy()
  })

  it('空态文案 + 配置 Harness 入口；parseFailed 降级提示', async () => {
    const onOpenHarnessSettings = vi.fn()
    await renderPanel({ onOpenHarnessSettings, parseFailed: true })
    expect(screen.getByText(/尚无 Agent/)).toBeTruthy()
    // §41（B6）：parseFailed 的降级提示换成 DegradedBanner 的兜底文案
    //（旧文案「已降级为空列表」与事实不符——列表并不为空）
    expect(screen.getByText(/部分 agent 记录读取失败；绑定清理已冻结/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '配置 Harness' }))
    expect(onOpenHarnessSettings).toHaveBeenCalled()
  })

  it('当前节点名从 nodes 树解析（execs[].nodeId → title）', async () => {
    await renderPanel({
      agents: [makeAgent('a1', 'exec-a', { execs: [{ ...makeExec('exec-a'), nodeId: 'T-42' }] })],
      nodes: [{ id: 'EPIC-1', type: 'epic', title: '根 Epic', status: 'open', priority: 0, children: [{ id: 'T-42', type: 'ticket', title: '登录修复', status: 'open', priority: 0 }] }],
    })
    expect(screen.getByText(/登录修复/)).toBeTruthy()
  })
})
