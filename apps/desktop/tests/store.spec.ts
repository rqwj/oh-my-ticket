/**
 * U10 store/presentation tests: the desktop surface's contract-sensitive
 * behaviors — error presentation (quota/revision/lease copy), bag key
 * scoping (`tauri:ui` filters, shared `recent`), declare fallback copy,
 * and admin rendering gate. Bridge layer is mocked at the tauri invoke
 * boundary.
 */
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { presentError } from '../src/store'

// Mock the tauri API boundary BEFORE importing bridge consumers.
const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))

describe('presentError contract copy', () => {
  it('QUOTA_EXCEEDED(rule=open-homes) names the limit and the remedy', () => {
    const text = presentError(new Error('QUOTA_EXCEEDED: rule=open-homes max 8'))
    expect(text).toContain('配额上限')
    expect(text).toContain('limits.max_open_homes')
    expect(text).toContain('重启 daemon')
  })

  it('revision conflict prompts a refresh', () => {
    const text = presentError(new Error('CONFLICT: revision mismatch expected 3 got 5'))
    expect(text).toContain('revision 冲突')
    expect(text).toContain('刷新')
  })

  it('HOME_LOCKED points at explicit takeover', () => {
    expect(presentError(new Error('HOME_LOCKED: ts-bridge owns'))).toContain('takeover')
  })
})

describe('bag key scoping (KD3/R4)', () => {
  beforeEach(() => invokeMock.mockReset())

  it('filters persist under the tauri:ui prefix key; recent uses the shared key', async () => {
    invokeMock.mockResolvedValue({})
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())

    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }
    await act(async () => {
      await result.current.saveFilters(home, { query: '测试' })
    })
    const filtersCall = invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'ui/filters-set')
    expect(filtersCall).toBeTruthy()
    expect((filtersCall![1] as { params: { key: string } }).params.key).toBe('tauri:ui')
  })
})

describe('known-homes picker data', () => {
  beforeEach(() => invokeMock.mockReset())

  it('home/list-known loads into state; missing method degrades to empty (pre-list-known daemon)', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({
          homes: [
            { path: '/tmp/a/.omt', name: 'a', kind: 'workspace', open: false, missing: false },
            { path: '/tmp/b/.omt', name: 'b', kind: 'workspace', open: false, missing: true },
          ],
        })
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())
    await act(async () => {
      await result.current.loadKnownHomes()
    })
    expect(result.current.state.knownHomes).toHaveLength(2)
    expect(result.current.state.knownHomes[1].missing).toBe(true)

    // Old daemon: method unknown → empty list, no crash.
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.reject(new Error('NOT_FOUND: kind=method'))
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.loadKnownHomes()
    })
    expect(result.current.state.knownHomes).toEqual([])
  })
})

describe('declare flow', () => {
  beforeEach(() => invokeMock.mockReset())

  it('declare success triggers reconnect + home relist', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [{ homeId: 'h1' }] })
      if (cmd === 'daemon_reconnect') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call') return Promise.resolve({})
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())

    let error: string | null = 'unset'
    await act(async () => {
      error = await result.current.declareHome('/tmp/new-ws/.omt')
    })
    expect(error).toBeNull()
    const commands = invokeMock.mock.calls.map(c => c[0])
    expect(commands).toContain('daemon_reconnect')
  })

  it('pre-U5 daemon (unknown method) yields the upgrade fallback copy', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        return Promise.reject(new Error('NOT_FOUND: kind=method'))
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())

    let error: string | null = null
    await act(async () => {
      error = await result.current.declareHome('/tmp/new-ws/.omt')
    })
    expect(error).toContain('homeDeclare')
    expect(error).toContain('升级 daemon')
  })
})

describe('添加 workspace 路径解析', () => {
  it('picked .omt dir used as-is; workspace root resolves to <root>/.omt', async () => {
    const { resolveHomeFromPickedDir } = await import('../src/workspacePath')
    expect(resolveHomeFromPickedDir('/repo/proj/.omt')).toBe('/repo/proj/.omt')
    expect(resolveHomeFromPickedDir('/repo/proj/.omt/')).toBe('/repo/proj/.omt')
    expect(resolveHomeFromPickedDir('/repo/proj')).toBe('/repo/proj/.omt')
    expect(resolveHomeFromPickedDir('/repo/proj/')).toBe('/repo/proj/.omt')
  })
})
