/**
 * U7 (R-A) 设置页 Harnesses 管理面测试：启用/禁用投影、禁用语义（存活
 * Agent 仅挡新派发）、env 保留键/空键前端拒绝（不发起 invoke）、Manual
 * path 必配提示与保存。Bridge 层 mock 沿 settings-install.spec 边界
 * （tauri invoke / dialog / app-version）。
 */
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

import { SettingsPanel } from '../src/SettingsPanel'

/** harness_list 单条投影 fixture（Rust harness_entry 键位契约 + U7 回显字段）。 */
const harnessWire = (over: Record<string, unknown> = {}) => ({
  id: 'omp',
  label: 'OMP (oh-my-pi)',
  installed: false,
  detectedPath: null,
  detectedVersion: null,
  enabled: false,
  binarySource: 'detected',
  manualPath: null,
  state: 'notDetected',
  unattendedFlag: ['--yolo'],
  extraArgs: [],
  env: {},
  ...over,
})

/** agents_list 条目 fixture（R-E wire 契约）。 */
const agentWire = (over: Record<string, unknown> = {}) => ({
  id: 'agent-1',
  displayName: 'Agent 1',
  title: null,
  harness: 'omp',
  status: 'running',
  attachment: 'live',
  lastExecId: 'exec-1',
  lastSessionId: null,
  execs: [],
  ...over,
})

const baseProps = {
  homes: [{ homeId: 'h1', name: 'demo', kind: 'workspace', path: '/tmp/demo/.omt' }] as never[],
  knownHomes: [] as never[],
  legacyDshByHome: {} as Record<string, boolean>,
  activeHome: null,
  onSelectHome: vi.fn(),
  onDeclare: vi.fn().mockResolvedValue(null),
}

/** 设置页默认 tab 是「通用」：先点进 Harnesses 内层 tab 再断言。 */
const openHarnessesTab = async () => {
  await waitFor(() => screen.getByText('Harnesses'))
  screen.getByText('Harnesses').click()
  // 详情头也渲染 label——取首个（DOM 序 = 左列 nav 行）
  await waitFor(() => expect(screen.getAllByText('OMP (oh-my-pi)').length).toBeGreaterThan(0))
}

/** 左列 nav 行（label 文本在 nav 与详情头各出现一次，取首个点击选中）。 */
const navRow = (label: string) => screen.getAllByText(label)[0]

beforeEach(() => {
  invokeMock.mockReset()
  openMock.mockReset()
  askMock.mockReset()
})

describe('SettingsPanel Harnesses 管理面 (U7)', () => {
  it('启用开关 → harness_config_set 以 enabled 调用，返回的最新投影被渲染', async () => {
    const off = harnessWire({})
    const on = harnessWire({
      enabled: true,
      installed: true,
      detectedPath: '/opt/homebrew/bin/omp',
      detectedVersion: '1.2.4',
      state: 'ready',
    })
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_list') return { harnesses: [off] }
      if (cmd === 'harness_config_set') {
        expect(args).toMatchObject({
          harness: 'omp',
          config: { enabled: true, binary: { kind: 'detected' }, extraArgs: [], env: {} },
        })
        return on
      }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<SettingsPanel {...baseProps} />)
    await openHarnessesTab()
    const switchControl = screen.getByRole('switch', { name: '启用 OMP (oh-my-pi)' })
    expect(switchControl.getAttribute('aria-checked')).toBe('false')
    switchControl.click()
    // 返回的最新投影被渲染：探测版本回显 + 状态徽章 + 开关态
    await waitFor(() => expect(screen.getByText('1.2.4')).toBeTruthy())
    await waitFor(() =>
      expect(screen.getByRole('switch', { name: '启用 OMP (oh-my-pi)' }).getAttribute('aria-checked')).toBe('true'),
    )
    // 左列与详情头各渲染一枚状态徽章
    await waitFor(() => expect(screen.getAllByText('就绪').length).toBeGreaterThan(0))
  })

  it('禁用且有存活 Agent → 显示「N 个运行中 Agent…仅阻止新派发」并引导去 Agents 面板', async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_list') return { harnesses: [harnessWire({ id: 'opencode', label: 'OpenCode' }), harnessWire({})] }
      if (cmd === 'agents_list') return { agents: [agentWire({ status: 'running' }), agentWire({ id: 'agent-2', status: 'starting' })], agentSeq: 2, parseFailed: false }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<SettingsPanel {...baseProps} />)
    await openHarnessesTab()
    // 默认选中首个类型（opencode，无存活 Agent）；选中 omp 才能看到禁用语义提示
    navRow('OMP (oh-my-pi)').click()
    await waitFor(() => expect(screen.getByText(/2 个运行中 Agent/)).toBeTruthy())
    expect(screen.getByText(/2 个运行中 Agent/).textContent).toContain('终止请前往 Agents 面板')
  })

  it('env 保留键 → 前端拒绝保存（不发起 invoke）+ 可读错误', async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_list') return { harnesses: [harnessWire({ state: 'ready', installed: true, enabled: true })] }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<SettingsPanel {...baseProps} />)
    await openHarnessesTab()
    screen.getByText('＋ 添加变量').click()
    await waitFor(() => screen.getByLabelText('环境变量名 1'))
    fireEvent.change(screen.getByLabelText('环境变量名 1'), { target: { value: 'PATH' } })
    fireEvent.change(screen.getByLabelText('环境变量值 1'), { target: { value: '/evil' } })
    screen.getByText('保存').click()
    await waitFor(() => expect(screen.getByText(/保留键/)).toBeTruthy())
    expect(invokeMock.mock.calls.some(c => c[0] === 'harness_config_set')).toBe(false)
  })

  it('env 空键 → 前端拒绝保存（不发起 invoke）+ 可读错误', async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_list') return { harnesses: [harnessWire({ state: 'ready', installed: true, enabled: true })] }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<SettingsPanel {...baseProps} />)
    await openHarnessesTab()
    screen.getByText('＋ 添加变量').click()
    await waitFor(() => screen.getByLabelText('环境变量值 1'))
    fireEvent.change(screen.getByLabelText('环境变量值 1'), { target: { value: '1' } })
    screen.getByText('保存').click()
    await waitFor(() => expect(screen.getByText(/环境变量名不能为空/)).toBeTruthy())
    expect(invokeMock.mock.calls.some(c => c[0] === 'harness_config_set')).toBe(false)
  })

  it('Manual path：检测不到时提示必配；填路径保存成功', async () => {
    const saved = harnessWire({
      binarySource: 'manual',
      manualPath: '/opt/tools/omp',
      installed: true,
      detectedPath: '/opt/tools/omp',
      detectedVersion: '3.0.1',
      enabled: true,
      state: 'ready',
    })
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'daemon_status') return { running: true, pid: 1, generation: 1, endpoint: 'x', spawned_by_us: false }
      if (cmd === 'omt_call') throw new Error('FORBIDDEN')
      if (cmd === 'harness_list') return { harnesses: [harnessWire({})] }
      if (cmd === 'harness_config_set') {
        expect(args).toMatchObject({
          harness: 'omp',
          config: { enabled: false, binary: { kind: 'manual', path: '/opt/tools/omp' } },
        })
        return saved
      }
      throw new Error(`unexpected command: ${cmd}`)
    })
    render(<SettingsPanel {...baseProps} />)
    await openHarnessesTab()
    // 检测不到 → Manual 模式的「必配」提示
    screen.getByText('手动指定').click()
    await waitFor(() => expect(screen.getByText(/必须手动指定/)).toBeTruthy())
    // Manual 空路径保存 → 前端镜像 Rust validate_config 拒绝，不发起 invoke
    screen.getByText('保存').click()
    await waitFor(() => expect(screen.getByText(/路径不能为空/)).toBeTruthy())
    expect(invokeMock.mock.calls.some(c => c[0] === 'harness_config_set')).toBe(false)
    // 填路径 → 保存成功，返回的最新投影被渲染（版本回显）
    fireEvent.change(screen.getByLabelText('手动 binary 路径'), { target: { value: '/opt/tools/omp' } })
    screen.getByText('保存').click()
    await waitFor(() => expect(screen.getByText('3.0.1')).toBeTruthy())
  })
})
