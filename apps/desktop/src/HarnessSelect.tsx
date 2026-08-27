/**
 * Per-workspace agent harness 设置（workspace 行右侧）：
 * 1. 选择 harness 类型（DeepSeek Harness / OpenCode）；
 * 2. Rust 侧探测本机安装位置（PATH + --version 探针）；
 * 3. DeepSeek Harness 多一个「开发模式」——可改选 DSH 源码 checkout
 *    （目录对话框 + 服务端校验 pnpm-workspace.yaml/apps/packages）。
 * 配置按 home 持久化在 `tauri:harness` bag（KD3 前缀约定）。
 */
import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import type { HomeInfo } from './types'

export type HarnessType = 'dsh' | 'opencode'

export interface HarnessConfig {
  type: HarnessType
  /** dsh 专属：install=用 PATH 上探测到的安装版；dev=源码 checkout。 */
  mode: 'install' | 'dev'
  installPath?: string
  version?: string
  devPath?: string
}

export const HARNESS_BAG_KEY = 'tauri:harness'

interface DetectResult {
  installed: boolean
  path?: string
  version?: string
}

interface Props {
  home: HomeInfo
  config: HarnessConfig | undefined
  onSave: (home: HomeInfo, config: HarnessConfig | null) => Promise<void>
}

export function HarnessSelect({ home, config, onSave }: Props) {
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)

  const chooseType = async (value: string) => {
    setNotice(null)
    if (value === '') {
      await onSave(home, null)
      return
    }
    const harness = value as HarnessType
    setBusy(true)
    try {
      const detected = await invoke<DetectResult>('harness_detect', { harness })
      if (!detected.installed) {
        setNotice(harness === 'dsh' ? '未在 PATH 检测到 dsh（可改用开发模式选择源码目录）' : '未在 PATH 检测到 opencode')
        if (harness === 'dsh') {
          // dsh 允许无安装版直接进入开发模式选择。
          await onSave(home, { type: 'dsh', mode: 'dev' })
        }
        return
      }
      await onSave(home, {
        type: harness,
        mode: 'install',
        installPath: detected.path,
        version: detected.version,
      })
    } finally {
      setBusy(false)
    }
  }

  const chooseDevDir = async () => {
    setBusy(true)
    setNotice(null)
    try {
      const picked = await openDialog({ directory: true, multiple: false, title: '选择 DeepSeek Harness 源码目录' })
      if (typeof picked !== 'string') return
      const result = await invoke<{ valid: boolean }>('harness_validate_checkout', { path: picked })
      if (!result.valid) {
        setNotice('所选目录不是有效的 DSH 源码 checkout（需含 pnpm-workspace.yaml / apps / packages）')
        return
      }
      await onSave(home, { type: 'dsh', mode: 'dev', devPath: picked, installPath: config?.installPath, version: config?.version })
    } finally {
      setBusy(false)
    }
  }

  return (
    <span className="harness-select" onClick={e => e.stopPropagation()}>
      <select
        className="detail-select harness-type"
        disabled={busy}
        value={config?.type ?? ''}
        onChange={e => void chooseType(e.target.value)}
      >
        <option value="">无 harness</option>
        <option value="dsh">DeepSeek Harness</option>
        <option value="opencode">OpenCode</option>
      </select>
      {config?.type === 'dsh' && (
        <>
          <button
            className={`chip${config.mode === 'install' ? ' active' : ''}`}
            disabled={busy || config.installPath === undefined}
            title={config.installPath ?? '未检测到安装版'}
            onClick={() => void onSave(home, { ...config, mode: 'install' })}
          >
            安装版
          </button>
          <button
            className={`chip${config.mode === 'dev' ? ' active' : ''}`}
            disabled={busy}
            title={config.devPath ?? '选择 DSH 源码目录'}
            onClick={() => void chooseDevDir()}
          >
            开发模式
          </button>
        </>
      )}
      {config?.mode === 'install' && config.installPath && (
        <span className="harness-path mono" title={config.installPath}>
          {config.version ?? config.installPath}
        </span>
      )}
      {config?.mode === 'dev' && config.devPath && (
        <span className="harness-path mono" title={config.devPath}>{config.devPath}</span>
      )}
      {notice && <span className="harness-notice">{notice}</span>}
    </span>
  )
}
