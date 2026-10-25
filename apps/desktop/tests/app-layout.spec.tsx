/**
 * U1（嵌入式终端计划）布局测试：左侧工作区图标栏取代顶栏与切换弹窗
 * （R1/R8）——ticket 树在中间区域各标签下常驻，全局设置从图标栏底部 ⚙
 * 进入并占满内容区。Bridge 层 mock 在 tauri invoke 边界（同 store.spec 的
 * 工厂形态）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'

afterEach(cleanup)

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

beforeEach(() => {
  invokeMock.mockReset()
  window.localStorage.clear()
  invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
    if (cmd === 'daemon_homes') {
      return Promise.resolve({ homes: [{ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }] })
    }
    if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
    return Promise.resolve({})
  })
})

describe('U1：图标栏布局与全局设置', () => {
  it('树与 splitter 在详情/Agents/Runs/工作区各标签下常驻；⚙ 全局设置占满内容区，关闭恢复', async () => {
    const { App } = await import('../src/App')
    render(<App />)

    // boot 完成、home 自动选中后：图标栏 + 树 + splitter
    await waitFor(() => expect(document.querySelector('.workspace-rail')).toBeTruthy())
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeTruthy())
    expect(document.querySelector('.splitter')).toBeTruthy()

    // 中间区域四个标签（R6/R8），逐个切换树都常驻
    for (const tab of ['Agents', 'Runs', '工作区', '详情']) {
      fireEvent.click(screen.getByText(tab))
      await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeTruthy())
      expect(document.querySelector('.splitter')).toBeTruthy()
    }

    // 图标栏底部 ⚙：全局设置占满内容区——树与 splitter 摘除（R8）
    fireEvent.click(screen.getByTitle('全局设置'))
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeNull())
    expect(document.querySelector('.splitter')).toBeNull()
    expect(screen.getByText('Daemon')).toBeTruthy()
    expect(screen.getByText('DSH 集成')).toBeTruthy()
    expect(screen.getByText('Harnesses')).toBeTruthy()
    expect(screen.getByText('派发策略')).toBeTruthy()

    // 再点一次 ⚙ 收起：树恢复
    fireEvent.click(screen.getByTitle('全局设置'))
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeTruthy())
    expect(screen.queryByText('DSH 集成')).toBeNull()
  })

  it('顶栏不再渲染：没有「切换 workspace」按钮，也没有 Tickets/设置顶栏标签', async () => {
    const { App } = await import('../src/App')
    render(<App />)
    await waitFor(() => expect(document.querySelector('.workspace-rail')).toBeTruthy())
    expect(screen.queryByTitle('切换 workspace')).toBeNull()
    expect(document.querySelector('.top-bar')).toBeNull()
    expect(screen.queryByText('Tickets')).toBeNull()
    expect(screen.queryByText('设置')).toBeNull()
    // 中间区域的标签包含 Runs（R8：Runs 从顶栏移入中间区域标签）
    expect(screen.getByText('Runs')).toBeTruthy()
  })
})
