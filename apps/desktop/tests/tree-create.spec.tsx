/**
 * 树面板新建节点（U14 验收诉求）：层级规则纯函数 + 创建表单交互。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, render, screen, waitFor, fireEvent } from '@testing-library/react'

afterEach(cleanup)

const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => {
  class ChannelMock {
    onmessage: ((message: unknown) => void) | null = null
    send(message: unknown) { this.onmessage?.(message) }
    toJSON() { return '__CHANNEL__' }
  }
  return {
    invoke: (...args: unknown[]) => invokeMock(...args),
    Channel: ChannelMock,
  }
})
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }))

import { TreePanel, allowedChildTypes } from '../src/TreePanel'
import type { OmtNode } from '../src/types'

const node = (id: string, type: string, title = id): OmtNode =>
  ({ id, type, title, status: 'open', priority: 0 }) as never

beforeEach(() => { invokeMock.mockReset() })

describe('层级规则 allowedChildTypes', () => {
  it('镜像 daemon 契约：根=epic；ticket 不能直接挂 epic 下', () => {
    expect(allowedChildTypes(null)).toEqual(['epic'])
    expect(allowedChildTypes('epic')).toEqual(['story'])
    expect(allowedChildTypes('story')).toEqual(['substory', 'ticket'])
    expect(allowedChildTypes('substory')).toEqual(['substory', 'ticket'])
    expect(allowedChildTypes('ticket')).toEqual(['subticket'])
    expect(allowedChildTypes('subticket')).toEqual(['subticket'])
  })
})

describe('树面板新建表单', () => {
  const baseProps = (over: Partial<Parameters<typeof TreePanel>[0]> = {}) => ({
    // 嵌套树形状（真实 node/tree 数据）：story 藏在 epic 的 children 里——
    // 顶层 find 会漏（F-U14-3 回归）
    nodes: [{ ...node('E-1', 'epic', 'Epic'), children: [node('S-1', 'story', 'Story')] }],
    filters: {},
    recentIds: [],
    selectedId: null as string | null,
    onSelect: vi.fn(),
    onFilters: vi.fn(),
    onCreateNode: vi.fn().mockResolvedValue('T-9'),
    ...over,
  })

  it('未选中 → 根级 Epic 表单；提交透传 parentId:null 与 body', async () => {
    const props = baseProps()
    render(<TreePanel {...(props as never)} />)
    fireEvent.click(screen.getByRole('button', { name: /新建 Epic/ }))
    fireEvent.change(screen.getByLabelText('节点标题'), { target: { value: '新验收 Epic' } })
    fireEvent.change(screen.getByLabelText('节点正文（可选）'), { target: { value: '正文' } })
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    await waitFor(() => expect(props.onCreateNode).toHaveBeenCalledWith({
      type: 'epic', title: '新验收 Epic', body: '正文', parentId: null,
    }))
  })

  it('选中 story → 类型只允许 substory/ticket，parentId 带选中 id；空标题禁用', async () => {
    const props = baseProps({ selectedId: 'S-1' })
    render(<TreePanel {...(props as never)} />)
    fireEvent.click(screen.getByRole('button', { name: /story 下/ }))
    // 类型 chips 只有两种
    fireEvent.click(screen.getByRole('button', { name: 'ticket' }))
    const create = screen.getByRole('button', { name: '创建' }) as HTMLButtonElement
    expect(create.disabled).toBe(true) // 空标题
    fireEvent.change(screen.getByLabelText('节点标题'), { target: { value: '信任门触发票' } })
    expect(create.disabled).toBe(false)
    fireEvent.click(create)
    await waitFor(() => expect(props.onCreateNode).toHaveBeenCalledWith({
      type: 'ticket', title: '信任门触发票', body: undefined, parentId: 'S-1',
    }))
  })

  it('创建失败 → 就地报错不关表单', async () => {
    const props = baseProps({ onCreateNode: vi.fn().mockRejectedValue(new Error('title 必填')) })
    render(<TreePanel {...(props as never)} />)
    fireEvent.click(screen.getByRole('button', { name: /新建 Epic/ }))
    fireEvent.change(screen.getByLabelText('节点标题'), { target: { value: 'x' } })
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    await waitFor(() => expect(screen.getByText(/title 必填/)).toBeTruthy())
    expect(screen.getByLabelText('节点标题')).toBeTruthy() // 表单仍在
  })
})
