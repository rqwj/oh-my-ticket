/**
 * 「安装 DSH 插件」设置页流程测试：检测→弹窗→选目录→校验→安装的分支
 * 矩阵。Bridge 层 mock 在 tauri invoke / dialog / app-version 边界。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, render, screen, waitFor } from '@testing-library/react'

afterEach(cleanup)

const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
vi.mock('@tauri-apps/api/app', () => ({
  getVersion: vi.fn().mockResolvedValue('0.5.2'),
}))
const openMock = vi.fn()
const askMock = vi.fn()
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: (...args: unknown[]) => openMock(...args),
  ask: (...args: unknown[]) => askMock(...args),
}))

import { SettingsPanel } from '../src/SettingsPanel'

const baseProps = {
  homes: [] as never[],
  knownHomes: [] as never[],
  harnessByHome: {},
  onSaveHarness: vi.fn(),
  activeHome: null,
  onSelectHome: vi.fn(),
  onDeclare: vi.fn().mockResolvedValue(null),
}

const baseInvokeEnvironment = () => {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
    if (cmd === 'omt_call') throw new Error('FORBIDDEN')
    throw new Error(`unexpected command: ${cmd}`)
  })
}

beforeEach(() => {
  invokeMock.mockReset()
  openMock.mockReset()
  askMock.mockReset()
})

describe('SettingsPanel 安装 DSH 插件', () => {
  it('检测到全局 dsh → 直接以 global 模式安装 dsh-oh-my-ticket@<app 版本>', async () => {
    baseInvokeEnvironment()
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_detect') return { installed: true, path: '/opt/homebrew/bin/dsh', version: '0.1.2' }
      if (cmd === 'dsh_plugin_install') {
        expect(args).toMatchObject({
          mode: 'global',
          profile: 'web',
          packageName: 'dsh-oh-my-ticket',
          version: '0.5.2',
        })
        return { ok: true, output: 'Done' }
      }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<SettingsPanel {...baseProps} />)
    await waitFor(() => screen.getByText('安装 DSH 插件…'))
    screen.getByText('安装 DSH 插件…').click()
    await waitFor(() => expect(screen.getByText(/已安装 dsh-oh-my-ticket@0\.5\.2/)).toBeTruthy())
    expect(askMock).not.toHaveBeenCalled()
  })

  it('未检测到 dsh 且用户选择「否」→ 关闭弹窗，不发起安装', async () => {
    baseInvokeEnvironment()
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'daemon_status') return { running: false, pid: null, generation: null, endpoint: null, spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_detect') return { installed: false }
      throw new Error(`unexpected command: ${cmd}`)
    })
    askMock.mockResolvedValue(false)
    render(<SettingsPanel {...baseProps} />)
    await waitFor(() => screen.getByText('安装 DSH 插件…'))
    screen.getByText('安装 DSH 插件…').click()
    await waitFor(() => expect(askMock).toHaveBeenCalled())
    // 弹窗文案点明「使用 dsh 开发环境安装」
    expect(String(askMock.mock.calls[0][0])).toContain('开发环境')
    await new Promise(resolve => setTimeout(resolve, 20))
    expect(invokeMock.mock.calls.some(c => c[0] === 'dsh_plugin_install')).toBe(false)
    expect(openMock).not.toHaveBeenCalled()
  })

  it('用户选择「是」→ 选目录 → 校验通过 → 以 dev 模式安装', async () => {
    baseInvokeEnvironment()
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'daemon_status') return { running: false, pid: null, generation: null, endpoint: null, spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_detect') return { installed: false }
      if (cmd === 'harness_validate_checkout') return { valid: true }
      if (cmd === 'dsh_plugin_install') {
        expect(args).toMatchObject({
          mode: 'dev',
          checkoutDir: '/Users/me/tools/deepseek-harness',
          profile: 'web',
          packageName: 'dsh-oh-my-ticket',
          version: '0.5.2',
        })
        return { ok: true, output: 'Done' }
      }
      throw new Error(`unexpected command: ${cmd}`)
    })
    askMock.mockResolvedValue(true)
    openMock.mockResolvedValue('/Users/me/tools/deepseek-harness')
    render(<SettingsPanel {...baseProps} />)
    await waitFor(() => screen.getByText('安装 DSH 插件…'))
    screen.getByText('安装 DSH 插件…').click()
    await waitFor(() => expect(screen.getByText(/已安装 dsh-oh-my-ticket@0\.5\.2/)).toBeTruthy())
    expect(invokeMock.mock.calls.some(c => c[0] === 'harness_validate_checkout')).toBe(true)
  })

  it('所选目录不是 dsh checkout → 提示并中止', async () => {
    baseInvokeEnvironment()
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'daemon_status') return { running: false, pid: null, generation: null, endpoint: null, spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_detect') return { installed: false }
      if (cmd === 'harness_validate_checkout') return { valid: false }
      throw new Error(`unexpected command: ${cmd}`)
    })
    askMock.mockResolvedValue(true)
    openMock.mockResolvedValue('/not/a/checkout')
    render(<SettingsPanel {...baseProps} />)
    await waitFor(() => screen.getByText('安装 DSH 插件…'))
    screen.getByText('安装 DSH 插件…').click()
    await waitFor(() => expect(screen.getByText(/不是 dsh 开发环境/)).toBeTruthy())
    expect(invokeMock.mock.calls.some(c => c[0] === 'dsh_plugin_install')).toBe(false)
  })
})
