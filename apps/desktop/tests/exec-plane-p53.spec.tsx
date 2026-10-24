/**
 * §53 前端用例：
 * - in-flight-stop-all-error（store 级）：`store.stopAll` 整体失败/抛错的
 *   错误条只写回**发起时的 home**——§52 的切 home 守卫只覆盖面板提示，
 *   结果回来时已切走的话，A home 的停止错误不得写进 B home 的全局
 *   `state.error`。mock 边界与 exec-plane-p51.spec 相同。
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

/** 假时钟下等 store 的 boot 链（homes → 自动选中 → per-home 装载）收敛。 */
async function advanceUntil(
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

type StopAllResolver = {
  resolve: (value: { signaled: number; failed: string[] }) => void
  reject: (error: Error) => void
}

/** 装配 store boot mock：两个 home（自动选中 h_a）+ 受控的 exec_stop_all。 */
async function bootStoreWithControlledStopAll() {
  let stopAllDeferred: StopAllResolver | undefined
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
    if (cmd === 'exec_stop_all') {
      return new Promise((resolve, reject) => {
        stopAllDeferred = {
          resolve: resolve as StopAllResolver['resolve'],
          reject,
        }
      })
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
  const takeDeferred = (): StopAllResolver => {
    const deferred = stopAllDeferred
    expect(deferred, 'exec_stop_all 已被调用（stopAll 已发起）').toBeTruthy()
    return deferred as StopAllResolver
  }
  return { result, takeDeferred }
}

describe('§53 stop-all-error：整体失败/抛错的错误条只写回发起时的 home', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    window.localStorage.clear()
    vi.useFakeTimers()
  })

  it('全失败结果在切 home 后回来 → 不写新 home 的全局 error', async () => {
    const { result, takeDeferred } = await bootStoreWithControlledStopAll()
    let outcome: Promise<unknown> | undefined
    await act(async () => {
      outcome = result.current.stopAll()
      await vi.advanceTimersByTimeAsync(0)
    })
    const deferred = takeDeferred()
    // 发起后、结果回来前切到 B home
    await act(async () => {
      await result.current.selectHome(result.current.state.homes[1])
    })
    expect(result.current.state.activeHome?.homeId).toBe('h_b')
    // A home 的全失败结果回来（signaled=0 且有 failed → 全局 error 路径）
    await act(async () => {
      deferred.resolve({ signaled: 0, failed: ['Agent 1 (exec-1): kill EPERM'] })
      await outcome
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(result.current.state.error).toBeNull(
      '切走后 A home 的停止错误不得写进 B home 的全局 error（§53 store 级守卫）',
    )
  })

  it('invoke 抛错在切 home 后回来 → 不写新 home 的全局 error', async () => {
    const { result, takeDeferred } = await bootStoreWithControlledStopAll()
    let outcome: Promise<unknown> | undefined
    await act(async () => {
      outcome = result.current.stopAll()
      await vi.advanceTimersByTimeAsync(0)
    })
    const deferred = takeDeferred()
    await act(async () => {
      await result.current.selectHome(result.current.state.homes[1])
    })
    await act(async () => {
      deferred.reject(new Error('home lock: 已被 CLI 接管'))
      await outcome
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(result.current.state.error).toBeNull(
      '切走后 A home 的停止异常不得写进 B home 的全局 error',
    )
  })

  it('未切 home 时全失败/抛错照常写全局 error（护栏：不误吞正常路径）', async () => {
    const first = await bootStoreWithControlledStopAll()
    let outcome: Promise<unknown> | undefined
    await act(async () => {
      outcome = first.result.current.stopAll()
      await vi.advanceTimersByTimeAsync(0)
    })
    await act(async () => {
      first.takeDeferred().resolve({ signaled: 0, failed: ['Agent 1 (exec-1): kill EPERM'] })
      await outcome
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(first.result.current.state.error).toBeTruthy('未切 home：全失败必须写全局 error')

    const second = await bootStoreWithControlledStopAll()
    let outcome2: Promise<unknown> | undefined
    await act(async () => {
      outcome2 = second.result.current.stopAll()
      await vi.advanceTimersByTimeAsync(0)
    })
    await act(async () => {
      second.takeDeferred().reject(new Error('home lock: 已被 CLI 接管'))
      await outcome2
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(second.result.current.state.error).toBeTruthy('未切 home：抛错必须写全局 error')
  })
})
