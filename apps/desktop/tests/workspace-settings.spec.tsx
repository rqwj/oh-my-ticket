/**
 * U2（R4/R6/R9/R32；KTD17/KTD19）：右键菜单与工作区设置页——
 * - AE1（菜单部分）：全局 home 的右键菜单里没有「在此工作区打开终端」；
 * - 右键非当前工作区选「工作区设置」：切换过去并打开设置标签页；
 * - 「在此工作区打开终端」（U8 前）点击跳 Agents 页；
 * - KTD17：「在 Finder 中显示」调 workspace_reveal（homeId，不传路径）；
 * - R9：没有运行中会话时「从列表移除」直接移除（无 exec_stop_all、
 *   known-homes 不动、无任何删除性调用）；
 * - AE7/R32：有运行中会话 → 两段确认；「移除但保留运行」只动桌面端列表
 *   （workspace_session_counts 仍统计该会话）；「停止会话并移除」先
 *   exec_stop_all 再移除；
 * - KTD14：移除当前 → 选中上一个；移除最后一个 → 空状态 + 添加工作区；
 * - R6：设置页（路径 / 重建索引（FORBIDDEN 不显示）/ Finder / 移除）。
 * Bridge 层 mock 在 tauri invoke 边界（同 workspace-rail.spec 的工厂形态）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'

afterEach(() => {
  cleanup()
})

const invokeMock = vi.fn()
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
const homeGlobal = { homeId: 'h_g', kind: 'global', path: '/Users/test/.omt' }

/** per-home 会话计数（右键菜单 / 设置页的判据） */
let sessionCounts: Array<{ homeId: string; terminals: number; others: number; terminalCapable: boolean }> = []

function mockDaemon(
  homes: Array<{ homeId: string; kind: string; path: string }>,
  known: unknown[] = [],
  trees: Record<string, unknown[]> = {},
) {
  invokeMock.mockReset()
  invokeMock.mockImplementation((cmd: string, args?: { method?: string; homeId?: string; params?: { homeId?: string } }) => {
    if (cmd === 'daemon_homes') return Promise.resolve({ homes, homeDir: '/Users/test' })
    if (cmd === 'daemon_reconnect') return Promise.resolve({})
    if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: known })
    if (cmd === 'omt_call' && args?.method === 'node/tree') {
      return Promise.resolve({ trees: trees[args.params?.homeId ?? ''] ?? [] })
    }
    if (cmd === 'workspace_session_counts') return Promise.resolve({ homes: sessionCounts })
    if (cmd === 'workspace_reveal') return Promise.resolve({})
    if (cmd === 'exec_stop_all') return Promise.resolve({ signaled: 1, failed: [] })
    return Promise.resolve({})
  })
}

const railIcons = () => [...document.querySelectorAll<HTMLButtonElement>('.rail-icon')]

const contextMenuOf = (icon: HTMLElement) => {
  fireEvent.contextMenu(icon, { clientX: 20, clientY: 30 })
  return waitFor(() => {
    const menu = document.querySelector('.ws-context-menu') as HTMLElement | null
    expect(menu, '右键应弹出工作区菜单（R4）').toBeTruthy()
    return menu!
  })
}

beforeEach(() => {
  window.localStorage.clear()
  sessionCounts = []
})

describe('U2 右键菜单（R4）', () => {
  it('AE1（菜单部分）：普通工作区有「在此工作区打开终端」；全局 home 没有', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/Users/test/.omt', homeId: 'h_g', name: '~' },
    ]))
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_g', terminals: 0, others: 0, terminalCapable: false },
    ]
    mockDaemon([homeA, homeGlobal])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))

    const wsMenu = await contextMenuOf(railIcons()[0])
    expect(within(wsMenu).getByRole('menuitem', { name: '在此工作区打开终端' })).toBeTruthy()
    expect(within(wsMenu).getByRole('menuitem', { name: '工作区设置' })).toBeTruthy()
    expect(within(wsMenu).getByRole('menuitem', { name: '在 Finder 中显示' })).toBeTruthy()
    expect(within(wsMenu).getByRole('menuitem', { name: '从列表移除' })).toBeTruthy()
    fireEvent.click(document.querySelector('.context-overlay')!)

    const globalMenu = await contextMenuOf(railIcons()[1])
    expect(
      within(globalMenu).queryByRole('menuitem', { name: '在此工作区打开终端' }),
      'KTD19/R12：没有项目目录的工作区不提供终端入口',
    ).toBeNull()
  })

  it('右键非当前工作区选「工作区设置」：切换到该工作区并打开设置标签页', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_b', terminals: 0, others: 0, terminalCapable: true },
    ]
    mockDaemon([homeA, homeB])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')

    const menu = await contextMenuOf(railIcons()[1])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '工作区设置' }))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    await waitFor(() => expect(document.querySelector('.workspace-settings-page')).toBeTruthy())
    expect(within(document.querySelector('.workspace-settings-page') as HTMLElement).getByText('工作区设置')).toBeTruthy()
  })

  it('「在此工作区打开终端」（U8 之前）：切换到该工作区并跳 Agents 页', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_b', terminals: 0, others: 0, terminalCapable: true },
    ]
    mockDaemon([homeA, homeB])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))

    const menu = await contextMenuOf(railIcons()[1])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '在此工作区打开终端' }))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))
    await waitFor(() => expect(document.querySelector('.center-pane')?.className).toContain('agents-pane'))
  })

  it('KTD17：「在 Finder 中显示」调 workspace_reveal(homeId)，不传路径', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ]))
    sessionCounts = [{ homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true }]
    mockDaemon([homeA])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))

    const menu = await contextMenuOf(railIcons()[0])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '在 Finder 中显示' }))
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(c => c[0] === 'workspace_reveal' && (c[1] as { homeId?: string }).homeId === 'h_a')).toBe(true),
    )
    expect(
      invokeMock.mock.calls.filter(c => c[0] === 'workspace_reveal').every(c => Object.keys((c[1] as object) ?? {}).join(',') === 'homeId'),
      'KTD17：路径由服务端解析，前端只传 homeId',
    ).toBe(true)
  })
})

describe('U2 从列表移除（R9/R32/AE7）', () => {
  function seedTwo() {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_b', terminals: 0, others: 0, terminalCapable: true },
    ]
    mockDaemon([homeA, homeB])
  }

  it('没有运行中会话：直接移除——不调 exec_stop_all、known-homes 不变、无删除性调用', async () => {
    seedTwo()
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))

    const menu = await contextMenuOf(railIcons()[1])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '从列表移除' }))
    await waitFor(() => expect(railIcons().length).toBe(1))
    // R9：只从桌面端列表移除——.omt 目录与 known-homes 条目不动（前端没有
    // 任何删除路径）；没有会话所以也不发停止
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toEqual([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ])
    expect(invokeMock.mock.calls.some(c => c[0] === 'exec_stop_all')).toBe(false)
    const methods = invokeMock.mock.calls.filter(c => c[0] === 'omt_call').map(c => (c[1] as { method?: string }).method)
    expect(methods.every(m => m !== 'home/remove' && m !== 'node/archive'), '无删除性 daemon 调用').toBe(true)
  })

  it('AE7：有 1 个运行中会话 → 两段确认；「移除但保留运行」只动列表，会话计数仍统计它', async () => {
    seedTwo()
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_b', terminals: 0, others: 1, terminalCapable: true },
    ]
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))

    const menu = await contextMenuOf(railIcons()[1])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '从列表移除' }))
    const confirm = await waitFor(() => {
      const dialog = document.querySelector('.ws-remove-confirm') as HTMLElement | null
      expect(dialog, '有运行中会话时必须两段确认（R32）').toBeTruthy()
      return dialog!
    })
    expect(within(confirm).getByText(/还有 1 个运行中的会话/)).toBeTruthy()
    expect(within(confirm).getByRole('button', { name: '停止会话并移除' })).toBeTruthy()
    expect(within(confirm).getByRole('button', { name: '移除但保留运行' })).toBeTruthy()

    fireEvent.click(within(confirm).getByRole('button', { name: '移除但保留运行' }))
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(invokeMock.mock.calls.some(c => c[0] === 'exec_stop_all'), '保留运行：不发停止信号').toBe(false)
    // 图标消失后 workspace_session_counts 的返回不受影响（会话仍在跑）——
    // Rust 侧按 runtime 目录计数，与桌面端列表无关；此处再拉一次仍统计 h_b
    const countsAgain = await (await import('../src/bridge')).workspaceSessionCounts()
    expect(countsAgain.find(h => h.homeId === 'h_b')?.others).toBe(1)
  })

  it('「停止会话并移除」：先对该 home 调 exec_stop_all，再移除图标', async () => {
    seedTwo()
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_b', terminals: 0, others: 2, terminalCapable: true },
    ]
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))

    const menu = await contextMenuOf(railIcons()[1])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '从列表移除' }))
    const confirm = await waitFor(() => document.querySelector('.ws-remove-confirm') as HTMLElement)
    fireEvent.click(within(confirm).getByRole('button', { name: '停止会话并移除' }))
    await waitFor(() => expect(railIcons().length).toBe(1))
    expect(
      invokeMock.mock.calls.some(c => c[0] === 'exec_stop_all' && (c[1] as { homeId?: string }).homeId === 'h_b'),
      '停止会话并移除：先停该 home 的活动 exec',
    ).toBe(true)
    expect(JSON.parse(window.localStorage.getItem(STORED_KEY)!)).toHaveLength(1)
  })

  it('KTD14：移除当前工作区 → 选中上一个；移除最后一个 → 空状态 + 添加工作区', async () => {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
      { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' },
    ]))
    sessionCounts = [
      { homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true },
      { homeId: 'h_b', terminals: 0, others: 0, terminalCapable: true },
    ]
    mockDaemon([homeA, homeB])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(2))
    expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1')

    // 移除当前（ws2 是当前吗？当前是 ws1——先切到 ws2 再移除它）
    fireEvent.click(screen.getByTitle('ws2（/tmp）'))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws2'))

    let menu = await contextMenuOf(railIcons()[1])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '从列表移除' }))
    await waitFor(() => expect(railIcons().length).toBe(1))
    await waitFor(() => expect(document.querySelector('.workspace-name')?.textContent).toBe('ws1'), )
    expect(railIcons()[0].className).toContain('active')

    // 移除最后一个 → 空状态
    menu = await contextMenuOf(railIcons()[0])
    fireEvent.click(within(menu).getByRole('menuitem', { name: '从列表移除' }))
    await waitFor(() => expect(railIcons().length).toBe(0))
    await waitFor(() => expect(screen.getByText('还没有打开的工作区')).toBeTruthy())
    expect(screen.getByRole('button', { name: '＋ 添加工作区' })).toBeTruthy()
    expect(document.querySelector('.workspace-name')).toBeNull()
  })
})

describe('U2 工作区设置标签页（R6）', () => {
  async function openSettingsTab(installMock?: () => void) {
    window.localStorage.setItem(STORED_KEY, JSON.stringify([
      { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' },
    ]))
    sessionCounts = [{ homeId: 'h_a', terminals: 0, others: 0, terminalCapable: true }]
    if (installMock) installMock()
    else mockDaemon([homeA])
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(railIcons().length).toBe(1))
    // R5：ticket 栏顶部的 ⚙ 打开工作区设置标签页
    fireEvent.click(screen.getByTitle('工作区设置'))
    await waitFor(() => expect(document.querySelector('.workspace-settings-page')).toBeTruthy())
    return document.querySelector('.workspace-settings-page') as HTMLElement
  }

  it('显示路径；重建索引无 admin 权限（FORBIDDEN）不显示；有权限时可点并显示结果', async () => {
    let reindexForbidden = true
    const install = () => {
      invokeMock.mockReset()
      invokeMock.mockImplementation((cmd: string, args?: { method?: string; params?: Record<string, unknown> }) => {
        if (cmd === 'daemon_homes') return Promise.resolve({ homes: [homeA], homeDir: '/Users/test' })
        if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
        if (cmd === 'workspace_session_counts') return Promise.resolve({ homes: sessionCounts })
        if (cmd === 'omt_call' && args?.method === 'home/reindex') {
          if ((args.params as { dryRun?: boolean })?.dryRun === true) {
            return reindexForbidden
              ? Promise.reject(new Error('FORBIDDEN: credential lacks admin'))
              : Promise.resolve({})
          }
          return Promise.resolve({ nodes: 12, edges: 7, skipped: 1 })
        }
        return Promise.resolve({})
      })
    }
    let page = await openSettingsTab(install)
    expect(within(page).getByText('/tmp/ws1')).toBeTruthy()
    expect(within(page).queryByRole('button', { name: '重建索引' }), '无 admin 权限不显示（AE12）').toBeNull()
    expect(within(page).getByRole('button', { name: '在 Finder 中显示' })).toBeTruthy()
    expect(within(page).getByRole('button', { name: '从列表移除' })).toBeTruthy()

    // 有权限：按钮显示，点击跑真实 reindex 并显示结果
    reindexForbidden = false
    fireEvent.click(screen.getByTitle('全局设置'))
    await waitFor(() => expect(screen.getByText('派发策略')).toBeTruthy())
    fireEvent.click(railIcons()[0]) // 回到内容区（关闭全局设置）
    await waitFor(() => expect(document.querySelector('.settings-panel')).toBeNull())
    fireEvent.click(screen.getByTitle('工作区设置'))
    page = await waitFor(() => {
      const el = document.querySelector('.workspace-settings-page') as HTMLElement
      expect(within(el).getByRole('button', { name: '重建索引' })).toBeTruthy()
      return el
    })
    fireEvent.click(within(page).getByRole('button', { name: '重建索引' }))
    await waitFor(() => expect(within(page).getByText(/已重建索引：12 节点 \/ 7 边/)).toBeTruthy())
    expect(
      invokeMock.mock.calls.some(c =>
        c[0] === 'omt_call'
        && (c[1] as { method?: string; params?: { dryRun?: boolean } }).method === 'home/reindex'
        && (c[1] as { params?: { dryRun?: boolean } }).params?.dryRun === undefined,
      ),
      '点击按钮跑的是真实 reindex（不带 dryRun）',
    ).toBe(true)
  })

  it('设置页「在 Finder 中显示」与「从列表移除」可用（remove 走当前工作区）', async () => {
    const page = await openSettingsTab()
    fireEvent.click(within(page).getByRole('button', { name: '在 Finder 中显示' }))
    await waitFor(() =>
      expect(invokeMock.mock.calls.some(c => c[0] === 'workspace_reveal' && (c[1] as { homeId?: string }).homeId === 'h_a')).toBe(true),
    )
    fireEvent.click(within(page).getByRole('button', { name: '从列表移除' }))
    await waitFor(() => expect(railIcons().length).toBe(0))
    await waitFor(() => expect(screen.getByText('还没有打开的工作区')).toBeTruthy())
  })
})
