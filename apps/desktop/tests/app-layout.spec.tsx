/**
 * U8/R-D layout tests: the ticket tree and splitter render only in the
 * Tickets view — Runs/设置 go full width. Bridge layer is mocked at the
 * tauri invoke boundary (same factory shape as store.spec).
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

describe('U8/R-D：树仅 Tickets tab 渲染', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') {
        return Promise.resolve({ homes: [{ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }] })
      }
      if (cmd === 'omt_call' && args?.method === 'home/list-known') return Promise.resolve({ homes: [] })
      return Promise.resolve({})
    })
  })

  it('三 tab 切换：Tickets 有树与 splitter，Runs/设置全宽隐藏，切回恢复', async () => {
    const { App } = await import('../src/App')
    render(<App />)

    // boot 完成、home 自动选中后，Tickets 视图有树与 splitter
    await waitFor(() => expect(screen.getByText('Tickets')).toBeTruthy())
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeTruthy())
    expect(document.querySelector('.splitter')).toBeTruthy()

    // Runs：树与 splitter 摘除（全宽）
    fireEvent.click(screen.getByText('Runs'))
    await waitFor(() => expect(screen.queryByText('连接 daemon…')).toBeNull())
    expect(document.querySelector('.tree-wrap')).toBeNull()
    expect(document.querySelector('.splitter')).toBeNull()

    // 设置：同样隐藏
    fireEvent.click(screen.getByText('设置'))
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeNull())
    expect(document.querySelector('.splitter')).toBeNull()

    // 切回 Tickets：树恢复
    fireEvent.click(screen.getByText('Tickets'))
    await waitFor(() => expect(document.querySelector('.tree-wrap')).toBeTruthy())
    expect(document.querySelector('.splitter')).toBeTruthy()
  })
})
