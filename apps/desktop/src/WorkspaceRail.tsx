/**
 * U1 工作区图标栏（R1–R3、R8；KTD14）：最左侧一列，每个已收录工作区一个
 * 图标——缩写取显示名前两字符、底色按路径哈希区分同缩写、悬停提示显示
 * 全名与上级目录、当前工作区左侧有指示条、路径失效的图标置灰带警告。
 * 点击切换（懒声明与错误态由 store 的 selectWorkspace 处理）。底部「＋」
 * 添加工作区（沿用目录选择 + 已知未开流程，R3）、「⚙」全局设置（R8）。
 * 运行中终端的角标（R2）数据源在 U2/U8 接入。
 */
import { useState } from 'react'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import type { KnownHome } from './store'
import { workspaceRootOf } from './homePath'
import { NOT_A_WORKSPACE_HINT, resolveHomeFromPickedDir } from './workspacePath'
import {
  abbreviationOf,
  parentDirOf,
  workspaceColor,
  type WorkspaceEntry,
} from './workspaceList'

interface Props {
  workspaces: WorkspaceEntry[]
  knownHomes: KnownHome[]
  activeHomeId: string | null
  homeDir?: string
  /** 图标错误态（homeId → 原因）：懒声明失败等 */
  errors: Record<string, string>
  /** 路径已失效的工作区（known-homes 的 missing 探测）——置灰带警告 */
  missingPaths: Set<string>
  settingsOpen: boolean
  /** 添加弹窗由 App 托管（空状态的「添加工作区」按钮共用同一入口） */
  addOpen: boolean
  onSelect: (ws: WorkspaceEntry) => void
  onAdd: (homePath: string) => Promise<string | null>
  onAddOpen: () => void
  onAddClose: () => void
  onToggleSettings: () => void
}

export function WorkspaceRail({
  workspaces,
  knownHomes,
  activeHomeId,
  homeDir,
  errors,
  missingPaths,
  settingsOpen,
  addOpen,
  onSelect,
  onAdd,
  onAddOpen,
  onAddClose,
  onToggleSettings,
}: Props) {
  const [addMessage, setAddMessage] = useState<string | null>(null)
  const [picking, setPicking] = useState(false)

  const add = async (homePath: string) => {
    const error = await onAdd(homePath)
    if (error === null) {
      setAddMessage(null)
      onAddClose()
      return
    }
    setAddMessage(
      error.includes('不存在') || error.includes('unreadable') || error.includes('not a directory') || error.includes('INVALID_INPUT')
        ? NOT_A_WORKSPACE_HINT
        : error,
    )
  }

  const pickDirectory = async () => {
    setPicking(true)
    try {
      const picked = await openDialog({ directory: true, multiple: false, title: '选择要添加的 workspace 目录' })
      if (typeof picked !== 'string') return // 用户取消
      await add(resolveHomeFromPickedDir(picked))
    } finally {
      setPicking(false)
    }
  }

  const knownClosed = knownHomes.filter(k => !k.open)

  return (
    <>
      <nav className="workspace-rail" aria-label="工作区">
        <div className="rail-icons">
          {workspaces.map(ws => {
            const missing = missingPaths.has(ws.path)
            const error = errors[ws.homeId]
            const cls = [
              'rail-icon',
              ws.homeId === activeHomeId ? 'active' : '',
              missing ? 'missing' : '',
              error ? 'error' : '',
            ].filter(Boolean).join(' ')
            const tooltip = [
              `${ws.name}（${parentDirOf(ws.path, homeDir)}）`,
              missing ? '目录已不存在' : '',
              error ?? '',
            ].filter(Boolean).join(' — ')
            return (
              <button
                key={ws.homeId}
                className={cls}
                style={{ background: workspaceColor(ws.path) }}
                title={tooltip}
                onClick={() => onSelect(ws)}
              >
                <span className="rail-abbr">{abbreviationOf(ws.name)}</span>
                {(missing || error) && <span className="rail-warn" aria-hidden>⚠</span>}
              </button>
            )
          })}
        </div>
        <div className="rail-footer">
          <button className="rail-action" title="添加工作区" onClick={onAddOpen}>＋</button>
          <button
            className={`rail-action${settingsOpen ? ' active' : ''}`}
            title="全局设置"
            aria-pressed={settingsOpen}
            onClick={onToggleSettings}
          >
            ⚙
          </button>
        </div>
      </nav>
      {addOpen && (
        <div className="modal-overlay" onClick={onAddClose}>
          <div className="modal-panel" onClick={e => e.stopPropagation()}>
            <header className="modal-header">
              <h3>添加工作区</h3>
              <div className="modal-header-actions">
                <button className="chip active" disabled={picking} onClick={() => void pickDirectory()}>
                  {picking ? '选择中…' : '＋ 选择目录…'}
                </button>
                <button className="chip" onClick={() => { setAddMessage(null); onAddClose() }}>✕</button>
              </div>
            </header>
            {knownClosed.length > 0 ? (
              <div className="known-section">
                <h4>已知未开（点击即添加）</h4>
                {knownClosed.map(known => (
                  <button
                    key={known.path}
                    className="home-row"
                    disabled={known.missing}
                    title={known.missing ? '目录已不存在（被移动/删除）' : known.path}
                    onClick={() => void add(known.path)}
                  >
                    <span className="chip">{known.kind}</span>
                    <span className="mono">{workspaceRootOf(known.path, homeDir, known.name)}</span>
                    {known.missing && <span className="chip">已缺失</span>}
                  </button>
                ))}
              </div>
            ) : (
              <p className="empty-hint">选择一个目录添加工作区；在本机初始化过的其他工作区也会出现在这里。</p>
            )}
            {addMessage && <p className="info-banner">{addMessage}</p>}
          </div>
        </div>
      )}
    </>
  )
}
