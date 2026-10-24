/**
 * §57 前端用例（F-SP2-10 / F-SP2-11 + P3）：
 * - subscribe-failure-surfaced（F-SP2-10 TS 半边）：`events_subscribe` 被拒
 *   不再 `.catch(() => {})` 静默——泵停在旧 home 时外部改动不再刷新树，
 *   必须进错误状态可见。
 * - unlisten-not-leaked（§57 P3）：effect 在 subscribeEvents 完成前被销毁
 *   （快速切 home）时，返回的 unlisten 必须立即执行——旧的 disposers 数组
 *   只 push 从不执行，每次切 home 泄漏一个 omt://event 监听器。
 * - sticky-timeout-notice（F-SP2-11）：超时类失败（Rust
 *   DAEMON_TIMEOUT_MESSAGE 原文，带 `[daemon-timeout]` 机器前缀）分流到
 *   独立的粘性 `timeoutNotice` 字段——非静默 refreshAgents()（agents_list
 *   只读本地文件、daemon 卡死也成功）与 refreshHomes() 的成功路径都不清
 *   它，成功的节点拉取（refreshNodes）才清；展示文案剥掉机器前缀。
 * - plain-error-still-cleared（护栏）：非超时 error 的既有清除语义不变。
 * - enroll-request-write（§57 P3）：重建失败的 `enroll: request write: …
 *   timed out` 写超时形态也映射友好文案（此前只匹配 connection closed）。
 * mock 边界与 exec-plane-p55.spec 相同。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup } from '@testing-library/react'
import { extractDaemonTimeoutMessage } from './daemon-timeout-message'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

const invokeMock = vi.fn()
const { listenMock } = vi.hoisted(() => ({ listenMock: vi.fn() }))
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
const homeB = { homeId: 'h_b', kind: 'workspace', path: '/tmp/b/.omt' } as const

/** §57 F-SP2-11：Rust DAEMON_TIMEOUT_MESSAGE 原文（含机器前缀）。
 * §59 P3-1：从 daemon.rs 源码抽取（不再手抄——Rust 改前缀/文案时由
 * exec-plane-p59 的跨语言锁定断言兜红，本文件随抽取值同步）。 */
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
  /** events_subscribe 的 invoke 实现（默认成功）。 */
  eventsSubscribe?: (homeId: string) => Promise<unknown>
}

/** 装配 store：两个 home（自动选中 h_a）。返回 result 与 unmount 句柄。 */
async function bootStore(opts?: BootOptions) {
  invokeMock.mockImplementation((cmd: string, args?: { homeId?: string }) => {
    if (cmd === 'daemon_homes') {
      return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
    }
    if (cmd === 'home/list-known') return Promise.resolve({ homes: [] })
    if (cmd === 'agents_list') return Promise.resolve({ agents: [], parseFailed: false })
    if (cmd === 'events_subscribe') {
      return opts?.eventsSubscribe
        ? opts.eventsSubscribe(args?.homeId ?? '')
        : Promise.resolve({})
    }
    return Promise.resolve({})
  })
  listenMock.mockImplementation(async () => () => {})
  const { useTreeStore } = await import('../src/store')
  const { renderHook } = await import('@testing-library/react')
  const rendered = renderHook(() => useTreeStore())
  await advanceUntil(
    () =>
      rendered.result.current.state.homes.length === 2
      && rendered.result.current.state.activeHome?.homeId === 'h_a',
  )
  return rendered
}

describe('§57 订阅通道（F-SP2-10 / 监听器泄漏）', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    listenMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('events_subscribe 被拒 → 进错误状态（不再 .catch(() => {}) 静默）', async () => {
    const { result } = await bootStore()
    // 切 home 触发新订阅；node/tree 与 agents_list 挂起（手动 resolve）——
    // selectHome 的 Promise.all 不落地，避免 refreshNodes/refreshAgents 的
    // 成功路径（error: null）在断言前覆盖
    let resolveTree!: (value: unknown) => void
    const treePending = new Promise(resolve => {
      resolveTree = resolve
    })
    let resolveAgents!: (value: unknown) => void
    const agentsPending = new Promise(resolve => {
      resolveAgents = resolve
    })
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'events_subscribe') return Promise.reject(new Error('订阅通道建立失败'))
      if (cmd === 'omt_call' && args?.method === 'node/tree') return treePending
      if (cmd === 'agents_list') return agentsPending
      return Promise.resolve({})
    })
    await act(async () => {
      void result.current.selectHome(result.current.state.homes[1])
    })
    await flush()
    expect(
      result.current.state.error,
      'F-SP2-10：订阅失败必须可见——泵停在旧 home 时外部 CLI 改动不再刷新树',
    ).toContain('订阅通道建立失败')
    // 后续刷新成功——普通 error 的既有清除语义照常（订阅失败是常规
    // error，非粘性提示）
    await act(async () => {
      resolveTree({})
      resolveAgents({ agents: [], parseFailed: false })
    })
    await flush()
    expect(result.current.state.error).toBeNull()
  })

  it('effect 销毁后才 resolve 的订阅 → unlisten 立即执行（不泄漏 omt://event 监听器）', async () => {
    // h_a 的订阅 invoke 由测试控制（挂起）；切到 h_b 让订阅 effect
    // teardown——此时 h_a 的 invoke 仍未 resolve，它的 unlisten 到达时
    // effect 已销毁（§56 改 async 后这个窗口最宽 30s）。
    let resolveFirst!: (value: unknown) => void
    const firstInvoke = new Promise(resolve => {
      resolveFirst = resolve
    })
    let subscribeCalls = 0
    const unlistens: Array<ReturnType<typeof vi.fn>> = []
    const { result, unmount } = await bootStore({
      eventsSubscribe: () => {
        subscribeCalls += 1
        return subscribeCalls === 1 ? firstInvoke : Promise.resolve({})
      },
    })
    expect(subscribeCalls).toBe(1)
    // bootStore 装配的是无跟踪 listen——订阅 invoke#1 仍挂起（listen 尚未
    // 被调），现在换成逐次记录 unlisten 的实现
    listenMock.mockImplementation(async () => {
      const fn = vi.fn()
      unlistens.push(fn)
      return fn
    })
    // 切 home：旧 effect 销毁（disposed=true）、新 effect 订阅 h_b
    await act(async () => {
      await result.current.selectHome(result.current.state.homes[1])
    })
    expect(subscribeCalls).toBe(2)
    await flush()
    expect(unlistens, 'h_b 的 invoke 已 resolve，其 listen 已返回').toHaveLength(1)
    // h_a 的 invoke 此刻才 resolve → 其 listen 返回的 unlisten（下标 1）
    // 到达时 effect 已销毁——必须被立即执行
    await act(async () => {
      resolveFirst({})
    })
    await flush()
    expect(unlistens, 'h_a 的迟到订阅也调用了 listen').toHaveLength(2)
    expect(
      unlistens[1],
      '§57 P3：effect 销毁后才到达的 unlisten 必须立即执行（不得丢进只进不出的 disposers）',
    ).toHaveBeenCalledTimes(1)
    // 活跃 effect（h_b）的 unlisten（下标 0）尚未执行——仍在订阅中
    expect(unlistens[0]).not.toHaveBeenCalled()
    // 卸载 → cleanup 调用保存的 unlisten
    unmount()
    expect(unlistens[0]).toHaveBeenCalledTimes(1)
  })
})

describe('§57 超时提示独立字段（F-SP2-11）', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    listenMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('超时类失败 → 粘性 timeoutNotice（剥机器前缀）；refreshAgents/refreshHomes 不清；refreshNodes 成功才清', async () => {
    const { result } = await bootStore()
    // node/tree 拉取超时：Rust 原文（带机器前缀）上抛
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'omt_call' && args?.method === 'node/tree') {
        return Promise.reject(new Error(RUST_TIMEOUT_MESSAGE))
      }
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.timeoutNotice).toContain('结果未知')
    expect(result.current.state.timeoutNotice, '机器前缀是分流契约，不得展示给用户').not.toContain(
      '[daemon-timeout]',
    )
    expect(
      result.current.state.error,
      '超时类只进粘性 timeoutNotice——error 会被常规刷新的 error:null 抹掉',
    ).toBeNull()
    // F-SP2-11 的触发点：窗口聚焦路径的非静默 refreshAgents()（agents_list
    // 只读本地文件，daemon 卡死时也成功）
    await act(async () => {
      await result.current.refreshAgents()
    })
    expect(
      result.current.state.timeoutNotice,
      'F-SP2-11：非静默 refreshAgents 成功不得抹掉超时提示',
    ).toContain('结果未知')
    // homes 刷新成功同样不清
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(
      result.current.state.timeoutNotice,
      'homes 拉回来了不等于节点树刷新过——用户还没确认上次写入是否生效',
    ).toContain('结果未知')
    // 成功的节点拉取（用户确认过实际状态）才清
    invokeMock.mockImplementation(() => Promise.resolve({}))
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.timeoutNotice, '成功的节点拉取清除粘性提示').toBeNull()
    expect(result.current.state.error).toBeNull()
  })

  it('护栏：非超时 error 照常被非静默 refreshAgents 成功清除', async () => {
    const { result } = await bootStore()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'omt_call' && args?.method === 'node/tree') {
        return Promise.reject(new Error('QUOTA_EXCEEDED(rule=open-homes)'))
      }
      if (cmd === 'agents_list') {
        return Promise.resolve({ agents: [], parseFailed: false })
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.error).toContain('配额上限')
    expect(result.current.state.timeoutNotice, '非超时失败不进粘性提示').toBeNull()
    await act(async () => {
      await result.current.refreshAgents()
    })
    expect(result.current.state.error, '非超时 error 的既有清除语义不变').toBeNull()
  })

  it('enroll: request write 超时形态也映射友好文案（§57 P3 补齐）', async () => {
    const { result } = await bootStore()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') {
        return Promise.reject(new Error('enroll: request write: Operation timed out (IO)'))
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(
      result.current.state.error,
      '写超时形态的重建失败同样要映射友好文案（此前只匹配 connection closed）',
    ).toContain('daemon 无响应')
    expect(result.current.state.error).not.toContain('enroll: request write')
  })
})
