/**
 * TICKET-0138（U1 后续）：切换 / 添加工作区路径上的遗留问题——
 *
 * - CF1（§59 P3）：refreshNodes 活动守卫——A 迟到的 node/tree 不得覆盖 B。
 * - CF2（§59/§63）：timeoutNotice 结构化 {homeId, minNodesSeq}——只有同
 *   home、超时之后发出的 refreshNodes 成功才清；写入前已发出、超时后才
 *   返回的读不清；App 上的关闭按钮。
 * - CF3（§59 P3）：omt://events-status（Rust 泵健康上报）→ store 独立字段
 *   + App「外部改动不会自动刷新」提示，恢复后消失。
 * - F-TERM-8：选择序号守卫覆盖 addWorkspace（先点慢的 B、再添加 D，
 *   最终停在 D）。
 * - F-TERM-10：首次成功刷新前用户先添加工作区，不因此跳过迁移。
 * - F-TERM-11：变异测试兜底——selectHome 写入保护、focus 路径的成功门、
 *   selectWorkspace 按声明 homeId 查找、对齐 effect 吸附到 list[0]、
 *   「已打开」分支 homeId 回写（改为 effect 单一落点后的非污染语义）。
 * - §64 遗留：openRunsOfHome 不选图标栏外的 home（先收进列表）、
 *   missingPaths / selectWorkspace 的 known 查找按 homeId 双键匹配、
 *   全部条目无效的存量按首次运行处理、点图标关闭全局设置、legacy dsh
 *   提示按 home 显示。
 *
 * Bridge 层 mock 在 tauri invoke 边界（同 workspace-rail.spec 的工厂形态）；
 * store 级用例（CF1/CF2/对齐 effect/openRunsOfHome）用 renderHook 直驱。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'

afterEach(() => {
  cleanup()
})

const invokeMock = vi.fn()
const listenMock = vi.fn()
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
  listen: (...args: unknown[]) => listenMock(...args),
  emit: vi.fn().mockResolvedValue(undefined),
}))
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
  ask: vi.fn(),
}))

const STORED_KEY = 'omt-desktop-workspaces'
const homeA = { homeId: 'h_a', kind: 'workspace', path: '/tmp/ws1/.omt' }
const homeB = { homeId: 'h_b', kind: 'workspace', path: '/tmp/ws2/.omt' }
const homeX = { homeId: 'h_x', kind: 'workspace', path: '/tmp/other/.omt' }

const known = (over: Partial<{ path: string; open: boolean; missing: boolean; kind: string; lastHomeId: string }> = {}) => ({
  path: '/tmp/ws2/.omt',
  name: '.omt',
  kind: 'workspace',
  open: false,
  missing: false,
  lastHomeId: 'h_b',
  ...over,
})

/** App / store 共用的最小 daemon 环境；各用例按需覆盖个别命令。 */
function mockDaemon(
  homes: Array<{ homeId: string; kind: string; path: string }>,
  knownHomes: unknown[] = [],
  trees: Record<string, unknown[]> = {},
) {
  invokeMock.mockReset()
  invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { homeId?: string } }) => {
    if (cmd === 'daemon_homes') return Promise.resolve({ homes, homeDir: '/Users/test' })
    if (cmd === 'daemon_reconnect') return Promise.resolve({})
    if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: knownHomes })
    if (cmd === 'omt_call' && args?.method === 'node/tree') {
      return Promise.resolve({ trees: trees[args.params?.homeId ?? ''] ?? [] })
    }
    return Promise.resolve({})
  })
}

const railIcons = () => [...document.querySelectorAll<HTMLButtonElement>('.rail-icon')]

/** 装配 store（renderHook 直驱，真实时钟 + waitFor 收敛）。 */
async function bootStore(
  homes: Array<{ homeId: string; kind: string; path: string }> = [homeA, homeB],
  installMock?: () => void,
) {
  listenMock.mockReset()
  listenMock.mockImplementation(async () => () => {})
  if (installMock) installMock()
  else mockDaemon(homes)
  const { useTreeStore } = await import('../src/store')
  const { renderHook } = await import('@testing-library/react')
  const rendered = renderHook(() => useTreeStore())
  await waitFor(() =>
    expect(rendered.result.current.state.activeHome?.homeId).toBe(homes[0]?.homeId ?? null),
  )
  // 等 per-home 装载链（refreshNodes 等的 await 链）落地——后续断言
  //（如 CF2 的序号门槛）不得与 boot 的异步装载竞态
  await waitFor(() =>
    expect(
      invokeMock.mock.calls.some(
        c => c[0] === 'omt_call'
          && (c[1] as { method?: string; params?: { homeId?: string } }).method === 'node/tree'
          && (c[1] as { params?: { homeId?: string } }).params?.homeId === homes[0]?.homeId,
      ),
    ).toBe(true),
  )
  await act(async () => {
    await new Promise(resolve => setTimeout(resolve, 0))
  })
  return rendered
}

const dispatchTimeout = (homeId: string, text = 'daemon 无响应（30s）：结果未知、可能已生效') =>
  window.dispatchEvent(new CustomEvent('omt:daemon-timeout', { detail: { text, homeId } }))

beforeEach(() => {
  window.localStorage.clear()
  // listen 默认可调用（store 的 tauriListen 链路不能因 mock 返回 undefined 而抛错）
  listenMock.mockReset()
  listenMock.mockImplementation(async () => () => {})
})

// ── CF1：refreshNodes 活动守卫 ──────────────────────────────────────────────

describe('CF1：refreshNodes 活动守卫', () => {
  it('从 A 切到 B 时挂住 A 的 node/tree：B 先加载完，A 迟到的结果不覆盖 B 的 nodes', async () => {
    let resolveTreeA: ((value: { trees: unknown[] }) => void) | undefined
    const rendered = await bootStore([homeA, homeB], () => {
      invokeMock.mockReset()
      invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { homeId?: string } }) => {
        if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
        if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
        if (cmd === 'omt_call' && args?.method === 'node/tree') {
          if (args.params?.homeId === 'h_a') {
            // A 的树挂住，直到外部释放
            return new Promise(resolve => {
              resolveTreeA = resolve
            })
          }
          return Promise.resolve({ trees: [{ id: 'B-1', title: 'B 的树' }] })
        }
        return Promise.resolve({})
      })
    })
    const { result } = rendered
    await waitFor(() => expect(result.current.state.activeHome?.homeId).toBe('h_a'))

    await act(async () => {
      await result.current.selectHome(homeB)
    })
    await waitFor(() => expect(result.current.state.nodes.map(n => n.id)).toEqual(['B-1']))

    // A 迟到的 node/tree 返回——不得覆盖 B 的 nodes
    await act(async () => {
      resolveTreeA?.({ trees: [{ id: 'A-1', title: 'A 的旧树' }] })
    })
    await act(async () => {}) // 微任务落地
    expect(result.current.state.nodes.map(n => n.id), 'A 迟到的结果必须被丢弃（CF1）').toEqual(['B-1'])
  })
})

// ── CF2：timeoutNotice 绑定 {homeId, seq} ───────────────────────────────────

describe('CF2：timeoutNotice 绑定 home 与 refreshNodes 序号', () => {
  it('A 上写超时 → 切 B：B 的 refreshNodes 成功不清 A 的提示；切回 A 且超时后发出的刷新成功才清', async () => {
    const { result } = await bootStore([homeA, homeB])
    await act(async () => {
      dispatchTimeout('h_a')
    })
    expect(result.current.state.timeoutNotice?.homeId).toBe('h_a')

    await act(async () => {
      await result.current.selectHome(homeB)
    })
    await waitFor(() => expect(result.current.state.nodes).toBeDefined())
    expect(
      result.current.state.timeoutNotice?.text,
      'B 的成功刷新不得清掉关于 A 的写提示',
    ).toBeTruthy()

    await act(async () => {
      await result.current.selectHome(homeA)
    })
    expect(
      result.current.state.timeoutNotice,
      '切回 A 后、超时之后发出的 refreshNodes 成功（selectHome 装载）清除提示',
    ).toBeNull()
  })

  it('写入前已发出、超时后才返回的读不清除提示；超时后新发出的成功拉取才清', async () => {
    let resolveRead: ((value: { trees: unknown[] }) => void) | undefined
    const { result } = await bootStore([homeA], () => {
      invokeMock.mockReset()
      invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { homeId?: string } }) => {
        if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
        if (cmd === 'omt_call' && args?.method === 'node/tree') {
          // 此后发出的拉取挂住，直到外部释放（模拟非 FIFO 下写入前发出的读）
          return new Promise(resolve => {
            resolveRead = resolve
          })
        }
        return Promise.resolve({})
      })
    })
    // 写入前发出的读（序号 N）挂起
    let pending: Promise<void> | undefined
    act(() => {
      pending = result.current.refreshNodes(result.current.state.activeHome!)
    })
    // 写超时落地（记 minNodesSeq = N）
    await act(async () => {
      dispatchTimeout('h_a')
    })
    expect(result.current.state.timeoutNotice).not.toBeNull()
    // 挂起的读现在才成功返回——序号不大于落置值，不得清提示
    await act(async () => {
      resolveRead?.({ trees: [] })
      await pending
    })
    expect(result.current.state.timeoutNotice?.text, '写入前发出的读不清除提示（CF2）').toBeTruthy()
    // 超时之后新发出的成功拉取 → 清（换成立即成功的 mock）
    invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { homeId?: string } }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
      if (cmd === 'omt_call' && args?.method === 'node/tree') return Promise.resolve({ trees: [] })
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.refreshNodes(result.current.state.activeHome!)
    })
    expect(result.current.state.timeoutNotice).toBeNull()
  })

  it('App：超时提示可关闭（CF2 关闭按钮）', async () => {
    mockDaemon([homeA])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(document.querySelector('.timeout-notice')).toBeNull()
    await act(async () => {
      dispatchTimeout('h_a')
    })
    await waitFor(() => expect(document.querySelector('.timeout-notice')?.textContent).toContain('结果未知'))
    fireEvent.click(screen.getByRole('button', { name: '关闭超时提示' }))
    await waitFor(() => expect(document.querySelector('.timeout-notice')).toBeNull())
  })
})

// ── CF3：事件泵健康上报 ────────────────────────────────────────────────────

describe('CF3：omt://events-status → 独立字段 + 界面提示', () => {
  async function eventsStatusListener(): Promise<(payload: unknown) => void> {
    const call = listenMock.mock.calls.find((c: unknown[]) => c[0] === 'omt://events-status')
    if (!call) throw new Error('store 应订阅 omt://events-status')
    return call[1] as (event: { payload: unknown }) => void
  }

  it('store：失败上报进独立 eventsStatus 字段；ok:true 恢复清除；切 home 重置', async () => {
    const { result } = await bootStore([homeA, homeB])
    const handler = await eventsStatusListener()
    expect(result.current.state.eventsStatus).toBeNull()
    await act(async () => {
      handler({ payload: { homeId: 'h_a', ok: false, error: 'daemon 连接失败：无活 descriptor' } })
    })
    expect(result.current.state.eventsStatus).toEqual({
      homeId: 'h_a',
      error: 'daemon 连接失败：无活 descriptor',
    })
    await act(async () => {
      handler({ payload: { homeId: 'h_a', ok: true, error: null } })
    })
    expect(result.current.state.eventsStatus, '恢复后提示消失').toBeNull()
    await act(async () => {
      handler({ payload: { homeId: 'h_a', ok: false, error: 'x' } })
    })
    expect(result.current.state.eventsStatus).not.toBeNull()
    await act(async () => {
      await result.current.selectHome(homeB)
    })
    expect(result.current.state.eventsStatus, '切 home 重置（新订阅是新起点）').toBeNull()
  })

  it('App：泵失败达阈值 → 「外部改动不会自动刷新」提示；恢复后消失', async () => {
    mockDaemon([homeA])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    const handler = await eventsStatusListener()
    await act(async () => {
      handler({ payload: { homeId: 'h_a', ok: false, error: 'events/resume: connection closed' } })
    })
    const banner = await waitFor(() => {
      const el = document.querySelector('.events-status-banner')
      expect(el, '泵失败必须可见（CF3：此前静默无限重试）').toBeTruthy()
      return el!
    })
    expect(banner.textContent).toContain('外部改动不会自动刷新')
    expect(banner.textContent).toContain('events/resume: connection closed')
    await act(async () => {
      handler({ payload: { homeId: 'h_a', ok: true, error: null } })
    })
    await waitFor(() => expect(document.querySelector('.events-status-banner')).toBeNull())
  })
})

// ── F-TERM-8：addWorkspace 纳入选择序号守卫 ────────────────────────────────

describe('F-TERM-8：选择序号守卫覆盖 addWorkspace', () => {
  it('先点未开的 B（声明挂起）、再经添加弹窗加 D：B 的声明完成后被丢弃，最终停在 D', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b_old', name: 'ws2' },
    ]))
    const homeD = { homeId: 'h_d', kind: 'workspace', path: '/tmp/ws3/.omt' }
    let declared2 = false
    let releaseDeclare2: (() => void) | undefined
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { path?: string } }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({
          homes: declared2 ? [homeA, homeB, homeD] : [homeA, homeD],
          homeDir: '/Users/test',
        })
      }
      if (cmd === 'daemon_reconnect') return Promise.resolve({})
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        const path = args.params?.path ?? ''
        if (path === '/tmp/ws2/.omt') {
          // 慢声明：直到 releaseDeclare2 才返回
          return new Promise(resolve => {
            releaseDeclare2 = () => {
              declared2 = true
              resolve({ homeId: 'h_b' })
            }
          })
        }
        return Promise.resolve({ homeId: 'h_d' })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({ homes: [known({ lastHomeId: 'h_b_old' })] })
      }
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')

    // 先点未开的 ws2（声明挂起），再经「＋ 添加工作区」添加 ws3（D，快）
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    fireEvent.click(screen.getByTitle('添加工作区'))
    fireEvent.click(await screen.findByText('/tmp/ws3'))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws3'))

    // ws2 的慢声明此时才完成——过期结果必须丢弃，不得覆盖 D
    await act(async () => {
      releaseDeclare2!()
    })
    await waitFor(() => expect(declared2).toBe(true))
    expect(document.querySelector('.workspace-name')?.textContent, 'F-TERM-8：最终停在 D（ws3）').toBe('ws3')
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!).map((w: { path: string }) => w.path)).toEqual([
      '/tmp/ws1/.omt',
      '/tmp/ws2/.omt',
      '/tmp/ws3/.omt',
    ])
  })
})

// ── F-TERM-10：首刷前添加工作区不得跳过迁移 ────────────────────────────────

describe('F-TERM-10：首次成功刷新前先添加工作区', () => {
  it('boot daemon 不可达 → 添加 D（declare 成功、homes 恢复）：D 收录且其他已开 home 一并迁移', async () => {
    let daemonUp = false
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        return daemonUp
          ? Promise.resolve({
            homes: [homeA, { homeId: 'h_p', kind: 'workspace', path: '/tmp/picked/.omt' }],
            homeDir: '/Users/test',
          })
          : Promise.reject(new Error('connect ECONNREFUSED'))
      }
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        daemonUp = true // daemon 恢复（声明成功 = daemon 已可达）
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
    await waitFor(() => expect(document.querySelector('.error-banner')).toBeTruthy())
    expect(window.localStorage.getItem(STORED_KEY)).toBeNull() // F-TERM-3：boot 失败不迁移

    // daemon 恢复前用户先添加 picked：经目录选择（无 known 候选可用）
    openDialogMock.mockResolvedValueOnce('/tmp/picked')
    fireEvent.click(screen.getByTitle('添加工作区'))
    fireEvent.click(screen.getByText('＋ 选择目录…'))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('picked'))

    // D 收录，且 ws1（daemon 已开的其他 home）一并迁移——不得只剩 [picked]
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/picked/.omt', homeId: 'h_p', name: 'picked' },
    ])
    expect(railIcons().length).toBe(2)
  })
})

// ── F-TERM-11：变异测试兜底 ────────────────────────────────────────────────

describe('F-TERM-11：变异兜底测试', () => {
  it('selectHome 写入保护：localStorage.setItem 抛错时切换照常完成（per-home 装载仍发出）', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    mockDaemon([homeA, homeB])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1'))
    const setItemSpy = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('QuotaExceededError')
    })
    try {
      fireEvent.click(screen.getByTitle('ws2（/tmp）'))
      await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
      // 关键断言：写入失败不得中断 selectHome——per-home 装载（node/tree）
      // 必须仍对 h_b 发出（去掉 try/catch 时 setItem 抛错会跳过整个装载）
      await waitFor(() =>
        expect(
          invokeMock.mock.calls.some(
            c => c[0] === 'omt_call'
              && (c[1] as { method?: string; params?: { homeId?: string } }).method === 'node/tree'
              && (c[1] as { params?: { homeId?: string } }).params?.homeId === 'h_b',
          ),
          'selectHome 的 localStorage 写入保护：装载不得被写入失败打断',
        ).toBe(true),
      )
    } finally {
      setItemSpy.mockRestore()
    }
  })

  it('focus 路径的成功门：daemon 仍不可达时 focus 不把「空列表」当用户清空落盘', async () => {
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.reject(new Error('connect ECONNREFUSED'))
      if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(document.querySelector('.error-banner')).toBeTruthy())
    fireEvent(window, new Event('focus'))
    await waitFor(() =>
      expect(invokeMock.mock.calls.filter(c => c[0] === 'daemon_homes').length).toBeGreaterThanOrEqual(2),
    )
    expect(
      window.localStorage.getItem(STORED_KEY),
      'daemon 不可达的 focus 不得持久化空列表（成功门：仅 ok 时才初始化）',
    ).toBeNull()
  })

  it('selectWorkspace 按声明 homeId 找回：daemon 回报路径写法不同（/private 前缀）也能选中并回写', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b_old', name: 'ws2' },
    ]))
    let declared = false
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { path?: string } }) => {
      if (cmd === 'daemon_homes') {
        // 声明后 daemon 回报的路径是首个声明者的原始写法（/private 前缀）
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
        return Promise.resolve({ homeId: 'h_b' })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({ homes: [known({ lastHomeId: 'h_b_old' })] })
      }
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    // 按声明返回的 homeId（h_b）找回——路径写法不一致时兜底旧 homeId / 路径
    // 会误报「工作区已声明，但 home 列表中没有找到它」
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    expect(document.querySelector('.workspace-error-banner')).toBeNull()
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)[1]).toEqual({
      path: '/tmp/ws2/.omt',
      homeId: 'h_b',
      name: 'ws2',
    })
  })

  it('对齐 effect 吸附到 list[0]：活动 home 离开列表（非首次）时弹回首项，不悬空', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    const rendered = await bootStore([homeA, homeB, homeX])
    const { result } = rendered
    await waitFor(() => expect(result.current.state.activeHome?.homeId).toBe('h_a'))
    // 直接 selectHome 一个不在图标栏的 home（如旧 openRunsOfHome 的裸切换）
    await act(async () => {
      await result.current.selectHome(homeX)
    })
    await waitFor(() =>
      expect(result.current.state.activeHome?.homeId, '非首次激活且不在列表 → 吸附回首项 list[0]').toBe('h_a'),
    )
    expect(
      invokeMock.mock.calls.some(c => c[0] === 'omt_call' && (c[1] as { method?: string }).method === 'home/declare'),
      '吸附走「已打开」分支，不发起声明',
    ).toBe(false)
  })

  it('「已打开」分支 homeId 回写（effect 单一落点）：只按 path 回写本条，不波及共享陈旧 id 的其他条目', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_shared', name: 'ws2' },
      { path: '/tmp/ws3/.omt', homeId: 'h_shared', name: 'ws3' },
    ]))
    // daemon 以新 id（h_new）开着 ws2；ws2/ws3 两条存量共享陈旧 id（h_shared）
    mockDaemon([homeA, { homeId: 'h_new', kind: 'workspace', path: '/tmp/ws2/.omt' }])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(3))
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    const stored = JSON.parse(window.localStorage.getItem(STORED_KEY)!)
    expect(stored[1].homeId, '点击的条目按轮换后的 id 回写').toBe('h_new')
    expect(stored[2].homeId, '共享陈旧 id 的其他条目不被一并改写（按 path 定位本条）').toBe('h_shared')
    // 选中稳定，不被弹回首项
    await waitFor(() =>
      expect(
        railIcons().some(b => b.title.startsWith('ws2') && b.className.includes('active')),
        '点击的图标点亮（不被弹回首项）',
      ).toBe(true),
    )
    // 状态列表仍只有三条（key 用 homeId:path，重复陈旧 id 不再破坏渲染）
    expect(railIcons().length).toBe(3)
  })
})

// ── §64 遗留 ───────────────────────────────────────────────────────────────

describe('§64 遗留：openRunsOfHome / missing 双键 / 全无效存量 / 设置关闭 / legacy dsh', () => {
  it('openRunsOfHome：home 不在图标栏 → 先收进列表再选中（不被对齐 effect 弹回）；daemon 未开 → 不动 home', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ]))
    const rendered = await bootStore([homeA, homeB])
    const { result } = rendered
    await waitFor(() => expect(result.current.state.activeHome?.homeId).toBe('h_a'))

    // h_b 已开但不在图标栏：先收进列表再选中——裸 selectHome 会被对齐
    // effect 弹回首项（ws1）
    await act(async () => {
      result.current.openRunsOfHome('h_b')
    })
    await waitFor(() => expect(result.current.state.activeHome?.homeId).toBe('h_b'))
    await act(async () => {}) // 让对齐 effect 跑完
    expect(result.current.state.activeHome?.homeId, '选中不被弹回首项').toBe('h_b')
    expect(result.current.state.workspaces.map(w => w.path)).toEqual(['/tmp/ws1/.omt', '/tmp/ws2/.omt'])
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!).length).toBe(2)

    // daemon 未开的 home：不动活动 home
    await act(async () => {
      result.current.openRunsOfHome('h_zzz')
    })
    await act(async () => {})
    expect(result.current.state.activeHome?.homeId).toBe('h_b')
  })

  it('missing 双键匹配：known-homes 报规范路径（/private 前缀）、条目记原始写法 → 图标仍置灰，点击报因不声明', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    mockDaemon([homeA], [
      // known-homes 探测到的失效路径是规范化写法，与条目的 /tmp 写法不同，
      // 但 lastHomeId 与条目 homeId 一致（§64：按 homeId 或规范化路径比对）
      known({ path: '/private/tmp/ws2/.omt', missing: true, lastHomeId: 'h_b' }),
    ])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    const missing = railIcons()[1]
    expect(missing.className).toContain('missing')
    expect(within(missing).getByText('⚠')).toBeTruthy()
    fireEvent.click(missing)
    await waitFor(() => expect(screen.getByText(/目录已不存在/)).toBeTruthy())
    // selectWorkspace 的 known 失效查找同样按 homeId 命中 → 不发起声明
    expect(
      invokeMock.mock.calls.some(c => c[0] === 'omt_call' && (c[1] as { method?: string }).method === 'home/declare'),
      '失效路径按 homeId 命中，不发起声明',
    ).toBe(false)
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')
  })

  it('全部条目无效的存量 → 按首次运行处理（迁移照常执行），不是「用户清空了列表」', async () => {
    // 原始数组非空但全部条目形状无效——不是用户清空（那会存 []），
    // 而是历史写入损坏；按首次运行迁移，daemon 已开 home 收进图标栏
    window.localStorage.setItem(STORED_KEY, JSON.stringify([{ path: 42 }, 'oops']))
    mockDaemon([homeA])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ])
  })

  it('点图标时关闭全局设置（回到内容区）', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    mockDaemon([homeA, homeB])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    fireEvent.click(screen.getByTitle('全局设置'))
    await waitFor(() => expect(screen.getByText('Daemon')).toBeTruthy())
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeTruthy())
    expect(screen.queryByText('Daemon')).toBeNull()
  })

  it('legacy dsh 提示按 home 显示（不只看当前 home）', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: { homeId?: string } }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA, homeB], homeDir: '/Users/test' })
      if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call' && args?.method === 'ui/filters-get') {
        // 两个 home 的 bag 都是旧 dsh harness 条目
        return Promise.resolve({ filters: { harness: { type: 'dsh' } } })
      }
      return Promise.resolve({})
    })
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    fireEvent.click(screen.getByTitle('全局设置'))
    // 当前 home 是 ws1；ws2 的 dsh 提示也必须可见（按 home 逐条）
    await waitFor(() => expect(screen.getByText(/「ws1」：dsh harness 后续版本支持/)).toBeTruthy())
    await waitFor(() => expect(screen.getByText(/「ws2」：dsh harness 后续版本支持/)).toBeTruthy())
  })
})
