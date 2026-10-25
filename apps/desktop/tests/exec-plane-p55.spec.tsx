/**
 * §55 前端用例（F-SP2-7 TS 覆盖 + refreshHomes 错误条规则）：
 * - daemon_homes-reject（store 级，F-SP2-7 的 TS 半边）：`daemon_homes`
 *   被拒（Rust 侧 §53 起透传重建失败）时 homes / activeHome 必须保留、
 *   error 置位——此前无 TS 覆盖。
 * - enroll-friendly-copy（§55 P3）：重建失败的原始 `enroll: connection
 *   closed: …` 必须映射为友好文案（不得把原始 enroll 报错怼给用户）。
 * - refresh-homes-keeps-timeout-error（§55 P3 → §57 F-SP2-11 修订）：超时
 *   提示移入独立的粘性 timeoutNotice 字段（只有 refreshNodes 成功才清）；
 *   重建失败友好文案（读路径、无结果未知语义）照常随 refreshHomes 成功
 *   清除——§57 点名的「daemon_homes 恢复后仍显示连接重建失败」收口。
 * mock 边界与 exec-plane-p53.spec 相同。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup } from '@testing-library/react'
import { extractDaemonTimeoutMessage } from './daemon-timeout-message'

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
  // §61：bridge 出口超时整形会 emit('omt:daemon-timeout')——mock 必须提供
  emit: vi.fn().mockResolvedValue(undefined),
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

/** §55 场景里 daemon 卡死时重建失败的原始报错（state.rs → daemon.rs 文案）。 */
const RAW_ENROLL_TIMEOUT =
  'enroll: connection closed: Resource temporarily unavailable (IO)'

/** 假时钟下等 store 的 boot 链（homes → 自动选中 → per-home 装载）收敛。 */
async function advanceUntil(predicate: () => boolean, steps = 50): Promise<void> {
  for (let i = 0; i < steps && !predicate(); i += 1) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200)
    })
  }
  if (!predicate()) throw new Error('advanceUntil: condition never met')
}

/** 装配 store：两个 home（自动选中 h_a）；daemonHomes 由调用方控制成败。 */
async function bootStore() {
  invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
    if (cmd === 'daemon_homes') {
      return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
    }
    if (cmd === 'omt_call' && args?.method === 'home/list-known') {
      return Promise.resolve({ homes: [] })
    }
    if (cmd === 'agents_list') {
      return Promise.resolve({ agents: [], parseFailed: false })
    }
    return Promise.resolve({})
  })
  const { useTreeStore } = await import('../src/store')
  const { renderHook } = await import('@testing-library/react')
  const { result } = renderHook(() => useTreeStore())
  await advanceUntil(
    () =>
      result.current.state.homes.length === 2
      && result.current.state.activeHome?.homeId === 'h_a',
  )
  return result
}

describe('§55 daemon_homes 拒绝 / 超时错误条', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('daemon_homes 被拒 → homes/activeHome 保留 + error 置位（F-SP2-7 TS 覆盖）', async () => {
    const result = await bootStore()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') return Promise.reject(new Error(RAW_ENROLL_TIMEOUT))
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(
      result.current.state.homes.map(h => h.homeId),
      '重建失败不得清空 home 列表（切换器必须保住既有 homes）',
    ).toEqual(['h_a', 'h_b'])
    expect(result.current.state.activeHome?.homeId, '重建失败不得清掉活动 home').toBe('h_a')
    expect(result.current.state.error, '重建失败必须置全局 error').toBeTruthy()
  })

  it('重建失败映射为友好文案（不得原样透出 enroll: connection closed）', async () => {
    const result = await bootStore()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') return Promise.reject(new Error(RAW_ENROLL_TIMEOUT))
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.error, '超时形态的重建失败要映射为友好文案（含 daemon 无响应标记）').toContain(
      'daemon 无响应',
    )
    expect(result.current.state.error, '原始 enroll 报错不得直接怼给用户（§55 P3）').not.toContain(
      'enroll: connection closed',
    )
  })

  it('超时提示走独立 timeoutNotice（refreshHomes 成功不清）；重建失败文案照常清除；节点拉取成功才清粘性提示', async () => {
    const result = await bootStore()
    // ① 重建失败（enroll 超时形态）→ 友好文案进普通 error
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') return Promise.reject(new Error(RAW_ENROLL_TIMEOUT))
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.error).toContain('daemon 无响应')
    // ② daemon 恢复：homes 拉取成功——重建失败文案是读路径错误，照常
    // 清除（§57：不再按「daemon 无响应」子串粘住）；本场景无超时写，
    // 粘性 timeoutNotice 应为 null
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.homes.length, 'homes 照常刷新').toBe(2)
    expect(
      result.current.state.error,
      '§57：重建失败文案随 homes 恢复照常清除（它没有结果未知语义）',
    ).toBeNull()
    expect(result.current.state.timeoutNotice).toBeNull()
    // ③ 真正的超时写提示（Rust DAEMON_TIMEOUT_MESSAGE 原文，带机器前缀；
    // §59 P3-1 起从 daemon.rs 源码抽取，不再手抄句）→ 独立粘性字段；
    // refreshHomes / 非静默 refreshAgents 的成功路径都不清。
    // CF2：node/tree 是读——读超时照常进普通 error，不进粘性提示
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'omt_call' && args?.method === 'node/tree') {
        return Promise.reject(new Error(extractDaemonTimeoutMessage()))
      }
      if (cmd === 'agents_list') return Promise.resolve({ agents: [], parseFailed: false })
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.error, '读超时照常进普通 error').toContain('结果未知')
    expect(result.current.state.timeoutNotice, 'CF2：读超时不进粘性提示').toBeNull()
    // 写超时（bridge 出口事件）→ 结构化粘性提示
    await act(async () => {
      window.dispatchEvent(
        new CustomEvent('omt:daemon-timeout', {
          detail: { text: 'daemon 无响应（30s）：结果未知、可能已生效', homeId: 'h_a' },
        }),
      )
    })
    expect(result.current.state.timeoutNotice?.text).toContain('结果未知')
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(
      result.current.state.timeoutNotice?.text,
      '§55→§57：refreshHomes 成功不得抹掉超时提示——树并未随之刷新',
    ).toContain('结果未知')
    await act(async () => {
      await result.current.refreshAgents()
    })
    expect(
      result.current.state.timeoutNotice?.text,
      '§57 F-SP2-11：非静默 refreshAgents（agents_list 只读本地文件）成功不得抹掉超时提示',
    ).toContain('结果未知')
    // ④ 成功的节点拉取清掉粘性提示（确认过实际状态后回到正常）
    invokeMock.mockImplementation(() => Promise.resolve({}))
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.timeoutNotice, '成功的节点拉取清除超时提示').toBeNull()
    expect(result.current.state.error).toBeNull()
  })

  it('非超时类错误照常被 refreshHomes 成功清除（护栏：不把错误条卡死）', async () => {
    const result = await bootStore()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') {
        return Promise.reject(new Error('QUOTA_EXCEEDED(rule=open-homes)'))
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.error, '配额错误照常置 error').toBeTruthy()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.error, '非超时类错误在 refreshHomes 成功后照常清除').toBeNull()
  })
})
