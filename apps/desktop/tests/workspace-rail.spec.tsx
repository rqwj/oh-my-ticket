/**
 * U1 工作区图标栏测试（KTD14 / R1–R3、R5、R7、R8）：首次迁移、点击切换
 * （懒声明 + homeId 轮换回写）、同缩写异色、路径失效置灰、声明配额失败、
 * 底部 ⚙ 全局设置、localStorage 抛错降级。Bridge 层 mock 在 tauri invoke
 * 边界（同 app-layout.spec 的工厂形态）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'

afterEach(() => {
  cleanup()
  // 不用 vi.restoreAllMocks——它会把模块级 mock 工厂（event listen 等）的
  // 实现一并抹掉；用例内的 spy 自行 mockRestore
})

const invokeMock = vi.fn()
// §3.6.8：工厂内定义 Channel（vi.mock 工厂先于模块体求值，外部类声明踩 TDZ）。
vi.mock('@tauri-apps/api/core', () => {
  class ChannelMock {
    onmessage: ((message: unknown) => void) | null = null
    send(message: unknown) {
      this.onmessage?.(message)
    }
    toJSON() {
      return '__CHANNEL__'
    }
  }
  return {
    invoke: (...args: unknown[]) => invokeMock(...args),
    Channel: ChannelMock,
  }
})
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
  ask: vi.fn(),
}))

const STORED_KEY = 'omt-desktop-workspaces'
const homeA = { homeId: 'h_a', kind: 'workspace', path: '/tmp/ws1/.omt' }
const homeB = { homeId: 'h_b', kind: 'workspace', path: '/tmp/ws2/.omt' }

/** App boot 所需的最小 daemon 环境；各用例按需覆盖个别命令。 */
function mockDaemon(homes: Array<{ homeId: string; kind: string; path: string }>, known: unknown[] = []) {
  invokeMock.mockReset()
  invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
    if (cmd === 'daemon_homes') return Promise.resolve({ homes, homeDir: '/Users/test' })
    if (cmd === 'daemon_reconnect') return Promise.resolve({})
    if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: known })
    return Promise.resolve({})
  })
}

const railIcons = () => [...document.querySelectorAll<HTMLButtonElement>('.rail-icon')]

beforeEach(() => window.localStorage.clear())

describe('U1 首次迁移（KTD14）', () => {
  it('localStorage 为空：daemon 两个 home + known-homes 另一个 open 条目 → 3 个图标，顺序一致并落盘', async () => {
    mockDaemon(
      [homeA, homeB],
      [
        // 与 daemon 重复的 open 条目（去重）+ 一个额外的 open 条目
        { path: '/tmp/ws1/.omt', name: '.omt', kind: 'workspace', open: true, missing: false, lastHomeId: 'h_a' },
        { path: '/tmp/ws3/.omt', name: '.omt', kind: 'workspace', open: true, missing: false, lastHomeId: 'h_c' },
      ],
    )
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(3))
    expect(railIcons().map(b => b.title)).toEqual(['ws1（/tmp）', 'ws2（/tmp）', 'ws3（/tmp）'])
    expect(railIcons().map(b => within(b).getByText('ws'))).toHaveLength(3) // 缩写 = 显示名前两字符
    expect(railIcons()[0].className).toContain('active') // 当前工作区指示条
    // 迁移结果持久化（空列表语义留给用户增删）
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
      { path: '/tmp/ws3/.omt', homeId: 'h_c', name: 'ws3' },
    ])
  })
})

describe('U1 点击切换（懒声明）', () => {
  it('点击未开的第二个图标：declareHome → selectHome；ticket 栏顶部显示显示名与路径；homeId 轮换回写列表', async () => {
    // 存量列表的第二项 homeId 是 known-homes 的历史值（h_b_old），声明后轮换为 h_b
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b_old', name: 'ws2' },
    ]))
    let declared: string[] = []
    mockDaemon([homeA], [{ path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_b_old' }])
    invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { path?: string } }) => {
      if (cmd === 'daemon_homes') {
        // 声明成功后 home 列表带上第二项（homeId 轮换为 h_b）
        return Promise.resolve({
          homes: declared.length > 0 ? [homeA, homeB] : [homeA],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'daemon_reconnect') return Promise.resolve({})
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        declared.push(args.params?.path!)
        return Promise.resolve({})
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({
          homes: [{ path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_b_old' }],
        })
      }
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')

    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    await waitFor(() => expect(declared).toEqual(['/tmp/ws2/.omt']))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    expect(document.querySelector('.workspace-path')?.textContent).toBe('/tmp/ws2')
    // homeId 轮换回写（lastHomeId 只是历史值）
    await waitFor(() =>
      expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)[1]).toEqual({
        path: '/tmp/ws2/.omt',
        homeId: 'h_b',
        name: 'ws2',
      }),
    )
  })
})

describe('U1 同缩写异色（KTD14）', () => {
  it('两个目录都叫 app：缩写相同、颜色不同、悬停提示各带上级目录', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/one/app/.omt', homeId: 'h_1', name: 'app' },
      { path: '/tmp/two/app/.omt', homeId: 'h_2', name: 'app' },
    ]))
    mockDaemon([
      { homeId: 'h_1', kind: 'workspace', path: '/tmp/one/app/.omt' },
      { homeId: 'h_2', kind: 'workspace', path: '/tmp/two/app/.omt' },
    ])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    const [one, two] = railIcons()
    expect(within(one).getByText('ap')).toBeTruthy()
    expect(within(two).getByText('ap')).toBeTruthy()
    expect(one.title).toBe('app（/tmp/one）')
    expect(two.title).toBe('app（/tmp/two）')
    expect(one.style.background).not.toBe(two.style.background) // 路径哈希色
  })
})

describe('U1 路径失效（KTD14）', () => {
  it('known-homes 标记 missing：图标置灰带警告；点击显示原因，不清空当前选中、不声明', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b_old', name: 'ws2' },
    ]))
    mockDaemon([homeA], [{ path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: true, lastHomeId: 'h_b_old' }])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    const missing = railIcons()[1]
    expect(missing.className).toContain('missing')
    expect(within(missing).getByText('⚠')).toBeTruthy()

    fireEvent.click(missing)
    await waitFor(() => expect(screen.getByText(/目录已不存在/)).toBeTruthy())
    // 不清空当前选中、不发起声明
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')
    expect(invokeMock.mock.calls.some(c => c[0] === 'omt_call' && (c[1] as { method?: string }).method === 'home/declare')).toBe(false)
  })
})

describe('U1 懒声明失败（QUOTA_EXCEEDED）', () => {
  it('声明被配额拒绝：图标错误态，提示文案点名配额；当前选中不变', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    mockDaemon([homeA], [{ path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_b' }])
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        return Promise.reject(new Error('QUOTA_EXCEEDED(rule=open-homes): daemon 已达 8 个上限'))
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({ homes: [{ path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_b' }] })
      }
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('配额'))
    expect(railIcons()[1].className).toContain('error')
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')
    // 提示可关闭
    fireEvent.click(screen.getByRole('button', { name: '关闭工作区提示' }))
    await waitFor(() => expect(document.querySelector('.workspace-error-banner')).toBeNull())
  })
})

describe('U1 全局设置（R8）', () => {
  it('底部 ⚙：内容区显示 Daemon/DSH 集成/Harnesses/派发策略，没有 Workspaces 列表', async () => {
    mockDaemon([homeA])
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
      if (cmd === 'daemon_status') {
        return Promise.resolve({ running: true, pid: 42, generation: 3, endpoint: 'unix:///x', spawned_by_us: false })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    fireEvent.click(screen.getByTitle('全局设置'))
    await waitFor(() => expect(screen.getByText('Daemon')).toBeTruthy())
    expect(screen.getByText('DSH 集成')).toBeTruthy()
    expect(screen.getByText('Harnesses')).toBeTruthy()
    expect(screen.getByText('派发策略')).toBeTruthy()
    expect(screen.queryByText('Workspaces')).toBeNull()
    expect(screen.queryByText('已知未开')).toBeNull()
  })
})

describe('U1 localStorage 抛错降级', () => {
  it('getItem 抛错：图标栏退回 daemon 的 homes，页面照常渲染', async () => {
    const getItemSpy = vi.spyOn(window.localStorage, 'getItem').mockImplementation(key => {
      if (key === STORED_KEY) throw new Error('SecurityError')
      return window.localStorage.constructor.prototype.getItem.call(window.localStorage, key)
    })
    mockDaemon([homeA, homeB])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    expect(railIcons().map(b => b.title)).toEqual(['ws1（/tmp）', 'ws2（/tmp）'])
    expect(document.querySelector('.tree-wrap')).toBeTruthy() // 页面照常渲染
    getItemSpy.mockRestore()
  })
})
