/**
 * §59 前端用例（F-SP2-12 写路径超时前缀 + P3-1 跨语言锁）：
 * - write-path-timeout-clean（F-SP2-12，store 级）：node/create / exec_start
 *   这类**写路径**的 daemon 超时（Rust DAEMON_TIMEOUT_MESSAGE 原文）此前直接
 *   原样上抛——组件层 `String(err)` 显示带 `[daemon-timeout]` 机器前缀的
 *   文案，且这些写路径从不落粘性 timeoutNotice（§59 点名：前缀只在
 *   presentErrorWithKind 的调用点剥掉，TreePanel/ExecutionDialog/RunsPanel/
 *   DetailPanel/AgentsPanel 的就地错误态全在覆盖之外）。bridge 出口
 *   （omtCall / execStart）改抛类型化错误（toString = 剥前缀文案）后：
 *   展示点自动干净、写路径落 sticky timeoutNotice。
 * - create-node-timeout-clean（F-SP2-12，组件级）：新建节点超时 → 表单就地
 *   错误**不含**字面量 `[daemon-timeout]`，且 App 同款 `.timeout-notice`
 *   粘性横幅出现。
 * - 超时原文一律从 `../src-tauri/src/daemon.rs` 抽取（daemon-timeout-message.ts，
 *   §59 P3-1）——不再手抄整句（Rust 改文案，本文件跟着变，锁是真的）。
 * - cross-language-lock（§59 P3-1）：Rust `DAEMON_TIMEOUT_MARKER` /
 *   `DAEMON_TIMEOUT_MESSAGE` 与 TS 侧 `bridge.DAEMON_TIMEOUT_MARKER` 的真
 *   锁定——测试直接读 daemon.rs 源码抽字面量断言同值 / 前缀关系。此前
 *   「锁定」不存在：Rust 测试断言自己写死的字符串，前端另写一份，p55/p57
 *   手抄整句——Rust 把前缀和自己的测试一起改，前端会静默把超时退回普通
 *   error。
 * - bridge 出口类型化错误（F-SP2-12 的 seam）：omtCall / execStart 的超时
 *   拒绝 → DaemonTimeoutError（daemonTimeout=true，toString() 剥前缀）；
 *   非超时拒绝原样上抛（既有语义不变）。
 * mock 边界与 exec-plane-p57.spec 相同。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import {
  DAEMON_TIMEOUT_MARKER,
  DaemonTimeoutError,
  execStart,
  omtCall,
} from '../src/bridge'
import {
  extractDaemonTimeoutMarker,
  extractDaemonTimeoutMessage,
} from './daemon-timeout-message'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

const invokeMock = vi.fn()
const { listenMock, emitMock } = vi.hoisted(() => ({ listenMock: vi.fn(), emitMock: vi.fn() }))
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
  listen: (...args: unknown[]) => listenMock(...args),
  emit: (...args: unknown[]) => emitMock(...args),
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

const homeA = { homeId: 'h_a', kind: 'workspace', path: '/tmp/a/.omt' } as const

/** §59 P3-1：Rust DAEMON_TIMEOUT_MESSAGE 原文（含机器前缀），从源码抽取。 */
const RUST_TIMEOUT_MESSAGE = extractDaemonTimeoutMessage()

/** 假时钟下等 store 的 boot 链（homes → 自动选中 → per-home 装载）收敛。 */
async function advanceUntil(predicate: () => boolean, steps = 50): Promise<void> {
  for (let i = 0; i < steps && !predicate(); i += 1) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
  }
  if (!predicate()) throw new Error('advanceUntil: condition never met')
}

/** 微任务冲刷（promise 链落地）。 */
async function flush(): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(50)
  })
}

type BootOptions = {
  /** omt_call 的 invoke 实现（按 method 分派；缺省成功 {}）。 */
  omtCallImpl?: (method: string) => Promise<unknown>
  /** exec_start 的 invoke 实现。 */
  execStartImpl?: () => Promise<unknown>
}

/** 装配 store（一个 home，自动选中）；boot 链收敛后返回 result。 */
async function bootStore(opts?: BootOptions) {
  invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
    if (cmd === 'daemon_homes') {
      return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
    }
    if (cmd === 'home/list-known') return Promise.resolve({ homes: [] })
    if (cmd === 'agents_list') return Promise.resolve({ agents: [], parseFailed: false })
    if (cmd === 'events_subscribe') return Promise.resolve({})
    if (cmd === 'omt_call' && opts?.omtCallImpl) return opts.omtCallImpl(args?.method ?? '')
    if (cmd === 'exec_start' && opts?.execStartImpl) return opts.execStartImpl()
    return Promise.resolve({})
  })
  listenMock.mockImplementation(async () => () => {})
  const { useTreeStore } = await import('../src/store')
  const { renderHook } = await import('@testing-library/react')
  const rendered = renderHook(() => useTreeStore())
  await advanceUntil(
    () =>
      rendered.result.current.state.homes.length === 1
      && rendered.result.current.state.activeHome?.homeId === 'h_a',
  )
  return rendered
}

describe('§59 F-SP2-12：写路径超时 → 干净文案 + 粘性 timeoutNotice', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    listenMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('createNode（node/create 无 commandId，重复提交风险最高）超时 → 落粘性 timeoutNotice、剥机器前缀', async () => {
    const { result } = await bootStore({
      omtCallImpl: method => {
        if (method === 'node/create') return Promise.reject(new Error(RUST_TIMEOUT_MESSAGE))
        return Promise.resolve({})
      },
    })
    let caught: unknown = null
    await act(async () => {
      await result.current
        .createNode({ type: 'ticket', title: 'x', parentId: null })
        .catch(error => {
          caught = error
        })
    })
    // 写路径必须落粘性提示（结果未知、可能已生效）——此前 createNode 原样
    // 上抛、从不进 timeoutNotice（§59 F-SP2-12）；CF2 起提示是结构化的
    // {homeId, text, minNodesSeq}（绑定写入的 home）
    expect(result.current.state.timeoutNotice?.text, '写路径超时要落粘性 timeoutNotice').toContain('结果未知')
    expect(result.current.state.timeoutNotice?.text, '机器前缀是分流契约，不得进展示文案').not.toContain(
      '[daemon-timeout]',
    )
    expect(result.current.state.timeoutNotice?.homeId, 'CF2：提示绑定写入的 home').toBe('h_a')
    // 拒绝值本身也要干净（组件层 String(err) 就地展示）
    expect(String(caught), 'String(err) 展示点拿到剥前缀文案').not.toContain('[daemon-timeout]')
    expect(String(caught)).toContain('daemon 无响应')
  })

  it('startDispatch（exec_start）超时 → 落粘性 timeoutNotice、拒绝值剥前缀', async () => {
    const { result } = await bootStore({
      execStartImpl: () => Promise.reject(new Error(RUST_TIMEOUT_MESSAGE)),
    })
    let caught: unknown = null
    await act(async () => {
      await result.current
        .startDispatch({
          home: result.current.state.activeHome!,
          nodeId: 'n_1',
          harness: 'omp',
          unattended: true,
        })
        .catch(error => {
          caught = error
        })
    })
    expect(result.current.state.timeoutNotice?.text, '派发写路径超时要落粘性 timeoutNotice').toContain('结果未知')
    expect(result.current.state.timeoutNotice?.homeId, 'CF2：提示绑定写入的 home').toBe('h_a')
    expect(String(caught), 'ExecutionDialog 的 setError(String(err)) 不得显示机器前缀').not.toContain('[daemon-timeout]')
  })

  it('组件级：新建节点超时 → 表单就地错误无字面量前缀，`.timeout-notice` 粘性横幅出现', async () => {
    const { useTreeStore } = await import('../src/store')
    const { TreePanel } = await import('../src/TreePanel')
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
      }
      if (cmd === 'home/list-known') return Promise.resolve({ homes: [] })
      if (cmd === 'agents_list') return Promise.resolve({ agents: [], parseFailed: false })
      if (cmd === 'events_subscribe') return Promise.resolve({})
      if (cmd === 'omt_call' && args?.method === 'node/create') {
        return Promise.reject(new Error(RUST_TIMEOUT_MESSAGE))
      }
      return Promise.resolve({})
    })
    listenMock.mockImplementation(async () => () => {})
    /** App 同款装配：粘性横幅 + TreePanel（onCreateNode 直连 store.createNode） */
    function Harness() {
      const store = useTreeStore()
      return (
        <div>
          {store.state.timeoutNotice && (
            <p className="error-banner timeout-notice">{store.state.timeoutNotice.text}</p>
          )}
          <TreePanel
            nodes={store.state.nodes}
            filters={store.state.filters}
            recentIds={store.state.recentIds}
            selectedId={store.state.selectedId}
            onSelect={store.selectNode}
            onFilters={patch => {
              const home = store.state.activeHome
              if (home) void store.saveFilters(home, patch)
            }}
            onCreateNode={store.createNode}
          />
        </div>
      )
    }
    render(<Harness />)
    // 假时钟下推进 boot 链收敛（waitFor 与 fake timers 不兼容，用推进轮询）。
    // 收敛判据用「per-home 装载已发出 node/tree」——「＋ 新建 Epic」按钮首帧
    // 就在，等不到 boot；而 boot 未收敛时点创建，装载路径的 refreshNodes
    // （在超时之后发出）会按 CF2 语义合法地清掉刚落的提示
    await advanceUntil(() =>
      invokeMock.mock.calls.some(
        c => c[0] === 'omt_call' && (c[1] as { method?: string })?.method === 'node/tree',
      ),
    )
    await flush()
    // 打开根级 Epic 表单、填标题、提交 → node/create 超时拒绝
    fireEvent.click(screen.getByRole('button', { name: /新建 Epic/ }))
    fireEvent.change(screen.getByLabelText('节点标题'), { target: { value: '超时票据' } })
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    // 推进到拒绝落地 + setState 生效
    await flush()
    await flush()
    // 就地错误出现且不含机器前缀（§59 F-SP2-12：此前显示
    // 「[daemon-timeout] daemon 无响应（30s）：…」）
    expect(
      document.querySelector('.error-banner')?.textContent ?? '',
      '表单就地错误要显示超时文案',
    ).toContain('daemon 无响应')
    for (const banner of Array.from(document.querySelectorAll('.error-banner'))) {
      expect(
        banner.textContent,
        '写路径错误展示面（就地错误 / 粘性横幅）都不得出现机器前缀',
      ).not.toContain('[daemon-timeout]')
    }
    // 粘性横幅出现（写路径结果未知语义）——App 以 .timeout-notice 渲染
    expect(
      document.querySelector('.timeout-notice')?.textContent ?? '',
      '写路径超时必须落粘性 timeoutNotice（此前 createNode 从不进粘性提示）',
    ).toContain('结果未知')
    // 表单仍在（失败不关表单），标题保留
    expect(screen.getByLabelText('节点标题')).toBeTruthy()
  })
})

describe('§61 P3 / CF2·CF4：bridge 出口派发的 omt:daemon-timeout → 粘性 timeoutNotice', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    listenMock.mockReset()
    emitMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('事件携带去前缀文案与 homeId → timeoutNotice 结构化置位（组件本地 omtCall 的写路径全覆盖）', async () => {
    const { result } = await bootStore()
    expect(result.current.state.timeoutNotice).toBeNull()
    const stripped = RUST_TIMEOUT_MESSAGE.slice(
      extractDaemonTimeoutMarker().length,
    ).trim()
    // §63 P3 / CF4：出口改用同步 DOM CustomEvent——不再经 tauri emit，
    // 直接派发事件验证接收端
    await act(async () => {
      window.dispatchEvent(
        new CustomEvent('omt:daemon-timeout', { detail: { text: stripped, homeId: 'h_a' } }),
      )
    })
    expect(result.current.state.timeoutNotice).toMatchObject({
      homeId: 'h_a',
      text: stripped,
    })
    expect(Number.isInteger(result.current.state.timeoutNotice?.minNodesSeq)).toBe(true)
  })

  it('事件载荷缺 homeId → 绑定当前活动 home；空文案不置位', async () => {
    const { result } = await bootStore()
    await act(async () => {
      window.dispatchEvent(
        new CustomEvent('omt:daemon-timeout', { detail: { text: '写超时：结果未知' } }),
      )
    })
    expect(result.current.state.timeoutNotice?.homeId, '缺 homeId 时回退当前活动 home').toBe('h_a')
    await act(async () => {
      window.dispatchEvent(new CustomEvent('omt:daemon-timeout', { detail: {} }))
    })
    expect(result.current.state.timeoutNotice, '空文案不覆盖已有提示').not.toBeNull()
    act(() => {
      result.current.dismissTimeoutNotice()
    })
    expect(result.current.state.timeoutNotice, 'CF2：关闭按钮出口清掉提示').toBeNull()
    await act(async () => {
      window.dispatchEvent(new CustomEvent('omt:daemon-timeout', { detail: { text: '' } }))
    })
    expect(result.current.state.timeoutNotice, '空文案不置位').toBeNull()
  })

  it('CF4：omtCall 写方法超时拒会派发事件；读方法超时与非超时拒绝不派发', async () => {
    const seen: Array<{ text: string; homeId: string | null }> = []
    const listener = (event: Event) => {
      seen.push((event as CustomEvent<{ text: string; homeId: string | null }>).detail)
    }
    window.addEventListener('omt:daemon-timeout', listener)
    try {
      const stripped = RUST_TIMEOUT_MESSAGE.slice(extractDaemonTimeoutMarker().length).trim()
      invokeMock.mockRejectedValue(RUST_TIMEOUT_MESSAGE)
      await omtCall('node/create', { homeId: 'h_a', type: 'ticket', title: 'x' }).catch(() => {})
      expect(seen, '写方法超时必须派发（载荷为去前缀文案 + homeId）').toEqual([
        { text: stripped, homeId: 'h_a' },
      ])
      seen.length = 0
      await omtCall('node/tree', { homeId: 'h_a' }).catch(() => {})
      expect(seen, 'CF2：读方法超时不派发（无「可能已生效」语义）').toEqual([])
      invokeMock.mockRejectedValue(new Error('QUOTA_EXCEEDED(rule=open-homes)'))
      await omtCall('node/create', { homeId: 'h_a' }).catch(() => {})
      expect(seen, '非超时拒绝不派发').toEqual([])
    } finally {
      window.removeEventListener('omt:daemon-timeout', listener)
    }
  })
})

describe('§59 P3-1：跨语言前缀锁定（读 daemon.rs 源码断言）', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    listenMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('Rust DAEMON_TIMEOUT_MARKER 字面量 === TS 侧 bridge.DAEMON_TIMEOUT_MARKER', () => {
    expect(
      extractDaemonTimeoutMarker(),
      '两边 marker 必须同值——Rust 侧改前缀时本断言红（而非前端静默退回普通 error）',
    ).toBe(DAEMON_TIMEOUT_MARKER)
  })

  it('Rust DAEMON_TIMEOUT_MESSAGE 以 TS 侧 marker 开头（Rust 改前缀即红）', () => {
    const message = extractDaemonTimeoutMessage()
    expect(
      message.startsWith(DAEMON_TIMEOUT_MARKER),
      `Rust 超时文案必须以 TS marker ${DAEMON_TIMEOUT_MARKER} 开头，实际：${message}`,
    ).toBe(true)
  })

  it('抽取的 Rust 原文喂 store：读超时进普通 error、写超时进结构化粘性提示（非手抄串驱动）', async () => {
    // §59 P3-1：本用例的输入就是从 daemon.rs 抽出的字面量——Rust 侧改
    // 文案 / 前缀，本用例消费新值（前缀变形时上面的锁定断言红）。
    // CF2：node/tree 是读——超时不再进粘性提示（对读请求「可能已生效」
    // 是误导），照常进可被常规刷新清除的 error
    const { result } = await bootStore({
      omtCallImpl: method => {
        if (method === 'node/tree') return Promise.reject(new Error(RUST_TIMEOUT_MESSAGE))
        return Promise.resolve({})
      },
    })
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    // F-TERM-14：读超时用读路径自己的文案（可安全重试，无「可能已生效」）
    expect(result.current.state.error).toContain('可安全重试')
    expect(result.current.state.error).not.toContain('可能已生效')
    expect(result.current.state.timeoutNotice, 'CF2：读超时不进粘性提示').toBeNull()
    // 写路径（node/update，无 commandId）超时 → 粘性提示（bridge 出口事件）
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
      }
      if (cmd === 'omt_call' && args?.method === 'node/update') {
        return Promise.reject(new Error(RUST_TIMEOUT_MESSAGE))
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.updateNode('TICKET-0001', { title: 'x' })
    })
    expect(result.current.state.timeoutNotice?.text).toContain('结果未知')
    expect(result.current.state.timeoutNotice?.homeId).toBe('h_a')
  })
})

describe('§59 F-SP2-12：bridge 出口类型化错误（omtCall / execStart）', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    vi.useRealTimers()
  })

  it('omtCall 超时拒绝（tauri 字符串形态）→ DaemonTimeoutError：daemonTimeout=true、toString() 剥前缀', async () => {
    invokeMock.mockRejectedValue(RUST_TIMEOUT_MESSAGE)
    const error = await omtCall('node/create', {}).then(
      () => null,
      (e: unknown) => e,
    )
    expect(error).toBeInstanceOf(DaemonTimeoutError)
    expect((error as DaemonTimeoutError).daemonTimeout).toBe(true)
    expect((error as DaemonTimeoutError).text).not.toContain('[daemon-timeout]')
    expect((error as DaemonTimeoutError).text).toContain('daemon 无响应')
    // 组件层 String(err) 展示点拿到的就是剥前缀文案（TreePanel/RunsPanel/
    // DetailPanel/AgentsPanel/ExecutionDialog 的就地错误态）
    expect(String(error)).toBe((error as DaemonTimeoutError).text)
    expect(String(error)).not.toContain('[daemon-timeout]')
  })

  it('omtCall 超时拒绝（Error 包装形态）→ 同样命中（`Error: ` 前缀容忍）', async () => {
    invokeMock.mockRejectedValue(new Error(RUST_TIMEOUT_MESSAGE))
    const error = await omtCall('run/create', {}).then(
      () => null,
      (e: unknown) => e,
    )
    expect(error).toBeInstanceOf(DaemonTimeoutError)
    expect(String(error)).not.toContain('[daemon-timeout]')
  })

  it('omtCall 非超时拒绝 → 原样上抛（不包装、文案语义不变）', async () => {
    const raw = new Error('QUOTA_EXCEEDED(rule=open-homes)')
    invokeMock.mockRejectedValue(raw)
    const error = await omtCall('home/declare', {}).then(
      () => null,
      (e: unknown) => e,
    )
    expect(error, '非超时错误保持原引用（既有就地展示/匹配语义不变）').toBe(raw)
    expect(String(error)).toContain('QUOTA_EXCEEDED')
  })

  it('execStart 超时拒绝（rpc_of → launch::start 上抛 DAEMON_TIMEOUT_MESSAGE）→ DaemonTimeoutError', async () => {
    invokeMock.mockRejectedValue(RUST_TIMEOUT_MESSAGE)
    const error = await execStart({
      homeId: 'h_a',
      homePath: '/tmp/a/.omt',
      nodeId: 'n_1',
      harness: 'omp',
      unattended: true,
      channel: { onmessage: null, toJSON: () => '__CHANNEL__' } as never,
    }).then(
      () => null,
      (e: unknown) => e,
    )
    expect(error).toBeInstanceOf(DaemonTimeoutError)
    expect(String(error)).not.toContain('[daemon-timeout]')
  })

  it('execStart 非超时拒绝 → 原样上抛', async () => {
    const raw = new Error('node not in credential scope (FORBIDDEN)')
    invokeMock.mockRejectedValue(raw)
    const error = await execStart({
      homeId: 'h_a',
      homePath: '/tmp/a/.omt',
      nodeId: 'n_1',
      harness: 'omp',
      unattended: false,
      channel: { onmessage: null, toJSON: () => '__CHANNEL__' } as never,
    }).then(
      () => null,
      (e: unknown) => e,
    )
    expect(error).toBe(raw)
  })
})
