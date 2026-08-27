/**
 * Workspace 切换弹窗：顶栏点击当前 workspace 触发。列出 open
 * workspaces（显示 ROOT 路径而非 .omt 目录名）、known-homes 的
 * 「已知未开」一键添加，以及目录对话框添加新 workspace。
 */
import { useState } from 'react'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import type { HomeInfo } from './types'
import type { KnownHome } from './store'
import { workspaceRootOf } from './homePath'
import { NOT_A_WORKSPACE_HINT, resolveHomeFromPickedDir } from './workspacePath'
import { HarnessSelect, type HarnessConfig } from './HarnessSelect'

interface Props {
  homes: HomeInfo[]
  knownHomes: KnownHome[]
  activeHome: HomeInfo | null
  homeDir?: string
  harnessByHome: Record<string, HarnessConfig>
  onSaveHarness: (home: HomeInfo, config: HarnessConfig | null) => Promise<void>
  onSelect: (home: HomeInfo) => void
  onDeclare: (path: string) => Promise<string | null>
  onClose: () => void
}

export function WorkspaceSwitcher({ homes, knownHomes, activeHome, homeDir, harnessByHome, onSaveHarness, onSelect, onDeclare, onClose }: Props) {
  const [message, setMessage] = useState<string | null>(null)
  const [picking, setPicking] = useState(false)

  const rootOf = (home: HomeInfo) => workspaceRootOf(home.path, homeDir, home.name ?? home.homeId)

  const declare = async (homePath: string, thenSelect?: HomeInfo) => {
    const error = await onDeclare(homePath)
    if (error !== null) {
      setMessage(
        error.includes('不存在') || error.includes('unreadable') || error.includes('not a directory') || error.includes('INVALID_INPUT')
          ? NOT_A_WORKSPACE_HINT
          : error,
      )
      return
    }
    if (thenSelect) onSelect(thenSelect)
    onClose()
  }

  const pickWorkspace = async () => {
    setPicking(true)
    try {
      const picked = await openDialog({ directory: true, multiple: false, title: '选择要添加的 workspace 目录' })
      if (typeof picked !== 'string') return
      await declare(resolveHomeFromPickedDir(picked))
    } finally {
      setPicking(false)
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-panel" onClick={e => e.stopPropagation()}>
        <header className="modal-header">
          <h3>切换 workspace</h3>
          <div className="modal-header-actions">
            <button className="chip active" disabled={picking} onClick={() => void pickWorkspace()}>
              {picking ? '选择中…' : '＋ 添加 workspace…'}
            </button>
            <button className="chip" onClick={onClose}>✕</button>
          </div>
        </header>
        <div className="home-list">
          {homes.map(home => (
            <button
              key={home.homeId}
              className={`home-row${activeHome?.homeId === home.homeId ? ' selected' : ''}`}
              title={home.path}
              onClick={() => { onSelect(home); onClose() }}
            >
              <span className="home-row-main">
                <span className="chip">{home.kind ?? 'workspace'}</span>
                <span className="mono home-path">{rootOf(home)}</span>
                {activeHome?.homeId === home.homeId && <span className="row-check">✓</span>}
              </span>
              <span className="home-row-sub">
                <HarnessSelect home={home} config={harnessByHome[home.homeId]} onSave={onSaveHarness} />
              </span>
            </button>
          ))}
        </div>
        {knownHomes.filter(k => !k.open).length > 0 && (
          <div className="known-section">
            <h4>已知未开（点击即添加）</h4>
            {knownHomes.filter(k => !k.open).map(known => (
              <button
                key={known.path}
                className="home-row"
                disabled={known.missing}
                title={known.missing ? '目录已不存在（被移动/删除）' : known.path}
                onClick={() => void declare(known.path)}
              >
                <span className="chip">{known.kind}</span>
                <span className="mono">{workspaceRootOf(known.path, homeDir, known.name)}</span>
                {known.missing && <span className="chip">已缺失</span>}
              </button>
            ))}
          </div>
        )}
        {message && <p className="info-banner">{message}</p>}
      </div>
    </div>
  )
}
