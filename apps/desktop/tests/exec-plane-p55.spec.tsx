/**
 * §55 前端用例（F-SP2-7 TS 覆盖 + refreshHomes 错误条规则）：
 * - daemon_homes-reject（store 级，F-SP2-7 的 TS 半边）：`daemon_homes`
 *   被拒（Rust 侧 §53 起透传重建失败）时 homes / activeHome 必须保留、
 *   error 置位——此前无 TS 覆盖。
 * - enroll-friendly-copy（§55 P3）：重建失败的原始 `enroll: connection
 *   closed: …` 必须映射为友好文案（不得把原始 enroll 报错怼给用户）。
 * - refresh-homes-keeps-timeout-error（§55 P3）：refreshHomes 成功不得
 *   清除「超时类」错误（树并未随 homes 刷新，用户还没确认上次写入是否
 *   生效）；成功的节点拉取（refreshNodes）才清。
 * mock 边界与 exec-plane-p53.spec 相同。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup } from '@testing-library/react'

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
    expect(result.current.state.homes.map(h => h.homeId)).toEqual(
      ['h_a', 'h_b'],
      '重建失败不得清空 home 列表（切换器必须保住既有 homes）',
    )
    expect(result.current.state.activeHome?.homeId).toBe(
      'h_a',
      '重建失败不得清掉活动 home',
    )
    expect(result.current.state.error).toBeTruthy('重建失败必须置全局 error')
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
    expect(result.current.state.error).toContain(
      'daemon 无响应',
      '超时形态的重建失败要映射为友好文案（含 daemon 无响应标记）',
    )
    expect(result.current.state.error).not.toContain(
      'enroll: connection closed',
      '原始 enroll 报错不得直接怼给用户（§55 P3）',
    )
  })

  it('refreshHomes 成功不清除超时类错误（树仍是旧的）；成功的节点拉取才清', async () => {
    const result = await bootStore()
    // 先让 refreshHomes 失败一次 → 超时类错误条在场（友好文案含「daemon 无响应」）
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') return Promise.reject(new Error(RAW_ENROLL_TIMEOUT))
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.error).toContain('daemon 无响应')
    // daemon 恢复：homes 拉取成功——错误条必须保留（节点树并未随之刷新）
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.homes.length).toBe(2, 'homes 照常刷新')
    expect(
      result.current.state.error,
      '§55 P3：refreshHomes 成功不得抹掉超时类错误——用户还没确认上次写入是否生效',
    ).toContain('daemon 无响应')
    // 成功的节点拉取清掉它（确认过实际状态后回到正常）
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.error).toBeNull('成功的节点拉取清除超时错误')
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
    expect(result.current.state.error).toBeTruthy('配额错误照常置 error')
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshHomes()
    })
    expect(result.current.state.error).toBeNull(
      '非超时类错误在 refreshHomes 成功后照常清除',
    )
  })
})
