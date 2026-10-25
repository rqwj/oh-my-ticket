// @vitest-environment jsdom
/**
 * A8 派发策略设置页（ExecPolicySection）测试：生效策略与来源展示、无人
 * 值守 / 威胁模型如实文案（Q-10 同源的「能执行 shell 的 agent」）、保存
 * 经 exec_policy_set、数值校验拒绝（不发 invoke）、空 harness 警示、
 * POLICY_INVALID 预填文档默认值 + 恢复默认值、读取失败重试。Mock 边界照
 * harness-manager.spec.tsx（tauri invoke / Channel / dialog / app-version）。
 */
import '@testing-library/jest-dom/vitest'
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, render, screen, waitFor, fireEvent } from '@testing-library/react'

afterEach(cleanup)

const invokeMock = vi.fn()
// §3.6.8：mock 工厂必须导出 Channel（凡 import Channel 的模块在只导出
// invoke 的旧 mock 下会崩）。类定义收在工厂内：vi.mock 工厂先于模块体
// 求值，外部类声明会踩 TDZ。
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
vi.mock('@tauri-apps/api/app', () => ({
  getVersion: vi.fn().mockResolvedValue('0.6.4'),
}))
const openMock = vi.fn()
const askMock = vi.fn()
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: (...args: unknown[]) => openMock(...args),
  ask: (...args: unknown[]) => askMock(...args),
}))

import { ExecPolicySection } from '../src/ExecPolicySection'

const defaults = {
  allowFromCli: true, allowFromMcp: false, harnesses: ['pi', 'omp', 'opencode'],
  workspaces: 'homeWorkspaceOnly', maxConcurrent: 4, maxDurationSec: 1800, approvalMode: 'inherit',
}
const ok = (extra: object = {}) => ({ policy: defaults, defaults, source: 'default', path: '/p', ...extra })

describe('ExecPolicySection', () => {
  beforeEach(() => invokeMock.mockReset())

  it('shows the effective policy, its source and the unattended/CLI warnings', async () => {
    // 注：`cmd ?` 守卫——vitest 收尾时会以零参调一次实现（其他 spec 的兜底
    // mock 全是 resolve 所以从未显现）；拒掉未知命令的语义保持不变。
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok()) : cmd ? Promise.reject(new Error(cmd)) : Promise.resolve(undefined))
    render(<ExecPolicySection />)
    expect(await screen.findByLabelText('允许经 MCP 派发')).not.toBeChecked()
    expect(screen.getByLabelText('允许经 CLI 派发')).toBeChecked()
    // §49 策略设置测试守「默认来源」提示：文案已改为「出厂默认」，旧断言
    // /默认值/ 只匹配到「恢复默认值」按钮——提示被删掉测试也绿。
    expect(screen.getByText(/出厂默认/)).toBeInTheDocument()
    expect(screen.getByText(/无人值守/)).toBeInTheDocument()
    expect(screen.getByText(/能执行 shell 的 agent/)).toBeInTheDocument()
    expect(screen.getByText(/只影响之后的派发/)).toBeInTheDocument()
  })

  it('§49 source=file 时不显示「出厂默认」提示', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok({ source: 'file' })) : cmd ? Promise.reject(new Error(cmd)) : Promise.resolve(undefined))
    render(<ExecPolicySection />)
    expect(await screen.findByLabelText('允许经 MCP 派发')).not.toBeChecked()
    expect(screen.queryByText(/出厂默认/)).toBeNull()
  })

  it('saves toggles and numeric limits through exec_policy_set', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { policy: typeof defaults }) => {
      if (cmd === 'exec_policy_get') return Promise.resolve(ok())
      if (cmd === 'exec_policy_set') return Promise.resolve(ok({ policy: args!.policy, source: 'file' }))
      return cmd ? Promise.reject(new Error(cmd)) : Promise.resolve(undefined)
    })
    render(<ExecPolicySection />)
    fireEvent.click(await screen.findByLabelText('允许经 MCP 派发'))
    fireEvent.change(screen.getByLabelText('最大并发'), { target: { value: '2' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('exec_policy_set', {
      policy: { ...defaults, allowFromMcp: true, maxConcurrent: 2 },
    }))
    expect(await screen.findByRole('status')).toHaveTextContent('已保存')
  })

  it('reports validation errors as an alert tied to the field, without invoking', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok()) : cmd ? Promise.reject(new Error(cmd)) : Promise.resolve(undefined))
    render(<ExecPolicySection />)
    const field = await screen.findByLabelText('最大并发')
    fireEvent.change(field, { target: { value: '0' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/最大并发须在 1–32 之间/)
    expect(field).toHaveAttribute('aria-invalid', 'true')
    expect(invokeMock).not.toHaveBeenCalledWith('exec_policy_set', expect.anything())
  })

  it('warns inline when no harness is allowed', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok({ policy: { ...defaults, harnesses: [] } })) : cmd ? Promise.reject(new Error(cmd)) : Promise.resolve(undefined))
    render(<ExecPolicySection />)
    expect(await screen.findByText(/一个 harness 都没选/)).toBeInTheDocument()
  })

  it('prefills documented defaults when the file is invalid and offers restore-defaults', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve({ error: 'POLICY_INVALID: 无法解析', defaults, path: '/p' }) : cmd ? Promise.reject(new Error(cmd)) : Promise.resolve(undefined))
    render(<ExecPolicySection />)
    expect(await screen.findByText(/策略文件无效/)).toBeInTheDocument()
    expect(screen.getByText(/CLI 和 MCP 派发已全部拒绝/)).toBeInTheDocument()
    expect(screen.getByText(/下面已填入默认值/)).toBeInTheDocument()
    expect(screen.getByLabelText('允许经 CLI 派发')).toBeChecked()
    fireEvent.click(screen.getByLabelText('允许经 CLI 派发'))
    fireEvent.click(screen.getByRole('button', { name: '恢复默认值' }))
    expect(screen.getByLabelText('允许经 CLI 派发')).toBeChecked()
  })

  it('shows the load error with a retry button instead of loading forever', async () => {
    invokeMock.mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce(ok())
    render(<ExecPolicySection />)
    expect(await screen.findByRole('alert')).toHaveTextContent(/读取派发策略失败/)
    fireEvent.click(screen.getByRole('button', { name: '重试' }))
    expect(await screen.findByLabelText('允许经 MCP 派发')).toBeInTheDocument()
  })
})
