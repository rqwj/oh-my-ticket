/**
 * U1 工作区图标栏测试（KTD14 / R1–R3、R5、R7、R8）：首次迁移、点击切换
 * （懒声明 + homeId 轮换回写）、同缩写异色、路径失效置灰、声明配额失败、
 * 底部 ⚙ 全局设置、localStorage 抛错降级。Bridge 层 mock 在 tauri invoke
 * 边界（同 app-layout.spec 的工厂形态）。U1 返工（§64）：F-TERM-2..7 的
 * 回归测试与 P3 顺带项（aria、空态文案、对齐 effect）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'

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
    // P3（§64）：图标可访问性——aria-label 全名、aria-current 标记当前项
    expect(railIcons()[0].getAttribute('aria-label')).toBe('ws1')
    expect(railIcons()[0].getAttribute('aria-current')).toBe('true')
    expect(railIcons()[1].getAttribute('aria-current')).toBeNull()
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

describe('U1 localStorage 抛错降级（F-TERM-2）', () => {
  it('所有 key 的 getItem 都抛错（Storage.prototype spy）：图标栏退回 daemon 的 homes，页面照常渲染', async () => {
    // jsdom 下 spy localStorage 实例不生效——必须 spy Storage.prototype
    const getItemSpy = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('SecurityError')
    })
    try {
      mockDaemon([homeA, homeB])
      const { App } = await import('../src/App')
      render(<App />)
      await waitFor(() => expect(railIcons().length).toBe(2))
      expect(railIcons().map(b => b.title)).toEqual(['ws1（/tmp）', 'ws2（/tmp）'])
      expect(document.querySelector('.tree-wrap')).toBeTruthy() // 页面照常渲染
    } finally {
      getItemSpy.mockRestore()
    }
  })
})

describe('U1 daemon 不可达时不迁移（F-TERM-3）', () => {
  it('daemon_homes 失败：不执行首次迁移、不持久化空列表；daemon 恢复（focus 重拉）后再迁移', async () => {
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.reject(new Error('connect ECONNREFUSED'))
      if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(document.querySelector('.error-banner')).toBeTruthy())
    // 不得把「空列表」当成「用户清空过」落盘——否则首启 daemon 不可达
    // 会永久失去已迁移的工作区列表
    expect(window.localStorage.getItem(STORED_KEY)).toBeNull()
    // daemon 恢复：focus 重拉成功后补首次迁移
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
      return Promise.resolve({})
    })
    fireEvent(window, new Event('focus'))
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ])
  })
})

describe('U1 添加工作区（F-TERM-4 / F-TERM-7）', () => {
  it('经「已知未开」添加：按声明返回的 homeId 找回——daemon 回报的路径写法不同（符号链接/首个声明者）也能收录并切换', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ]))
    let declared = false
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        // 声明后 daemon 回报的路径是首次声明者的原始写法（/private 前缀），
        // 与声明的 /tmp 写法不同——按路径字符串比对会找不到
        return Promise.resolve({
          homes: declared
            ? [homeA, { homeId: 'h_b', kind: 'workspace', path: '/private/tmp/ws2/.omt' }]
            : [homeA],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'daemon_reconnect') return Promise.resolve({})
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        declared = true
        return Promise.resolve({ homeId: 'h_b', requiresRehandshake: true })
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
    await waitFor(() => expect(railIcons().length).toBe(1))
    fireEvent.click(screen.getByTitle('添加工作区'))
    fireEvent.click(await screen.findByText('/tmp/ws2'))
    await waitFor(() => expect(railIcons().length).toBe(2))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    // 收录条目按声明返回的 homeId（不是路径字符串比对）
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ])
    expect(document.querySelector('.workspace-error-banner')).toBeNull()
  })
})

describe('U1 添加弹窗候选（F-TERM-5）', () => {
  it('候选 = known-homes 未收录条目 + daemon 已开但不在图标栏的 home；已在列表里的不重复显示', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws3/.omt', homeId: 'h_c', name: 'ws3' },
    ]))
    mockDaemon(
      [homeA, { homeId: 'h_d', kind: 'workspace', path: '/tmp/ws4/.omt' }],
      [
        { path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_b' },
        // ws3 已在图标栏里（path/homeId 命中）——不得再显示为可添加
        { path: '/tmp/ws3/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_c' },
      ],
    )
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    fireEvent.click(screen.getByTitle('添加工作区'))
    await waitFor(() => expect(screen.getByText('可添加的工作区（点击即添加）')).toBeTruthy())
    // 断言限定在弹窗内（workspace 头部也会显示当前工作区的根路径）
    const dialogPanel = within(document.querySelector('.modal-panel') as HTMLElement)
    expect(dialogPanel.getByText('/tmp/ws4')).toBeTruthy() // daemon 已开、不在图标栏（如 DSH/CLI 打开）
    expect(dialogPanel.getByText('/tmp/ws2')).toBeTruthy() // known 未开、未收录
    expect(dialogPanel.queryByText('/tmp/ws1')).toBeNull() // 已在图标栏
    expect(dialogPanel.queryByText('/tmp/ws3')).toBeNull() // 已在图标栏
    // 点 daemon 已开的 ws4：声明幂等返回现有 home，收录并切换
    fireEvent.click(dialogPanel.getByText('/tmp/ws4'))
    await waitFor(() => expect(railIcons().length).toBe(3))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws4'))
  })
})

describe('U1 选择序号守卫（F-TERM-6）', () => {
  it('先点慢的未开 B、再点快的已开 C：B 的声明完成后被丢弃，最终停在 C', async () => {
    const homeC = { homeId: 'h_c', kind: 'workspace', path: '/tmp/ws3/.omt' }
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
      { path: '/tmp/ws3/.omt', homeId: 'h_c', name: 'ws3' },
    ]))
    let declared = false
    let releaseDeclare: (() => void) | undefined
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({
          homes: declared ? [homeA, homeB, homeC] : [homeA, homeC],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'daemon_reconnect') return Promise.resolve({})
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        // 慢声明：直到 releaseDeclare 才返回
        return new Promise(resolve => {
          releaseDeclare = () => {
            declared = true
            resolve({ homeId: 'h_b' })
          }
        })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({
          homes: [{ path: '/tmp/ws2/.omt', name: '.omt', kind: 'workspace', open: false, missing: false, lastHomeId: 'h_b' }],
        })
      }
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(3))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')

    // 先点未开的 ws2（声明挂起），再点已开的 ws3（立即选中）
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    fireEvent.click(screen.getByTitle('ws3（/tmp）'))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws3'))

    // ws2 的声明此时才完成——过期结果必须丢弃，不得覆盖 ws3
    await act(async () => {
      releaseDeclare!()
    })
    await waitFor(() => expect(declared).toBe(true))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws3')
    expect(railIcons()[2].className).toContain('active')
    expect(document.querySelector('.workspace-error-banner')).toBeNull()
  })
})

describe('U1 经目录选择添加（F-TERM-7）', () => {
  it('选择目录：resolveHomeFromPickedDir → 声明 → 收录并切换；取消选择不声明、弹窗保留', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ]))
    let declaredCount = 0
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({
          homes: declaredCount > 0
            ? [homeA, { homeId: 'h_p', kind: 'workspace', path: '/tmp/picked/.omt' }]
            : [homeA],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'daemon_reconnect') return Promise.resolve({})
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        declaredCount += 1
        return Promise.resolve({ homeId: 'h_p' })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    const dialog = await import('@tauri-apps/plugin-dialog')
    const openDialogMock = vi.mocked(dialog.open)
    openDialogMock.mockReset()
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    fireEvent.click(screen.getByTitle('添加工作区'))

    // 取消：openDialog 返回 null → 不声明、弹窗保留
    openDialogMock.mockResolvedValueOnce(null)
    fireEvent.click(screen.getByText('＋ 选择目录…'))
    await waitFor(() => expect(openDialogMock).toHaveBeenCalledTimes(1))
    expect(invokeMock.mock.calls.some(c => c[0] === 'omt_call' && (c[1] as { method?: string }).method === 'home/declare')).toBe(false)
    expect(screen.getByText('添加工作区')).toBeTruthy() // 弹窗仍在

    // 选择 /tmp/picked（工作区根目录）→ 声明 <root>/.omt
    openDialogMock.mockResolvedValueOnce('/tmp/picked')
    fireEvent.click(screen.getByText('＋ 选择目录…'))
    await waitFor(() => expect(railIcons().length).toBe(2))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('picked'))
    expect(
      invokeMock.mock.calls.some(
        c => c[0] === 'omt_call'
          && (c[1] as { method?: string; params?: { path?: string } }).method === 'home/declare'
          && (c[1] as { params?: { path?: string } }).params?.path === '/tmp/picked/.omt',
      ),
    ).toBe(true)
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/picked/.omt', homeId: 'h_p', name: 'picked' },
    ])
  })
})

describe('U1 对齐 effect（F-TERM-7 / P3）', () => {
  it('boot 自动选中的 home 不在列表里：不主动声明、不弹回首项（等用户点图标）', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    mockDaemon([{ homeId: 'h_x', kind: 'workspace', path: '/tmp/other/.omt' }])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('other'))
    // boot 不主动开 home（KTD14）：不懒声明 list[0]
    expect(invokeMock.mock.calls.some(c => c[0] === 'omt_call' && (c[1] as { method?: string }).method === 'home/declare')).toBe(false)
    expect(railIcons().length).toBe(2)
    expect(railIcons().some(b => b.className.includes('active'))).toBe(false)
  })

  it('点击存量 homeId 已过期的图标（daemon 已按新 id 开着同一目录）：对齐 effect 按 path 回写，选中不被弹回首项', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b_old', name: 'ws2' },
    ]))
    mockDaemon([homeA, { homeId: 'h_b', kind: 'workspace', path: '/tmp/ws2/.omt' }])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    // 轮换后的 homeId 回写列表（P3），不再被对齐 effect 弹回 ws1
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)[1]).toEqual({
      path: '/tmp/ws2/.omt',
      homeId: 'h_b',
      name: 'ws2',
    })
  })

  it('boot 选中的 home 与存量条目同路径但 homeId 已轮换：effect 兑底回写，图标点亮', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a_old', name: 'ws1' },
    ]))
    mockDaemon([homeA])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    await waitFor(() => expect(railIcons()[0].className).toContain('active'))
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ])
  })
})

describe('U1 空态与悬停提示（P3）', () => {
  it('图标栏有工作区但 daemon 未开任何 home：提示去点图标，不显示「还没有打开的工作区」空态', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ]))
    mockDaemon([]) // daemon 一个 home 都没开
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(screen.getByText('从左侧图标栏选择一个工作区')).toBeTruthy()
    expect(screen.queryByText('还没有打开的工作区')).toBeNull()
  })

  it('全局 home 悬停提示只有 ~，不是「~（~）」', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/Users/test/.omt', homeId: 'h_g', name: '~' },
    ]))
    mockDaemon([{ homeId: 'h_g', kind: 'global', path: '/Users/test/.omt' }])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(railIcons()[0].title).toBe('~')
  })
})
