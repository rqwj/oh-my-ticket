/**
 * U1 工作区图标栏（R1–R3、R8；KTD14）：最左侧一列，每个已收录工作区一个
 * 图标——缩写取显示名前两字符、底色按路径哈希区分同缩写、悬停提示显示
 * 全名与上级目录、当前工作区左侧有指示条、路径失效的图标置灰带警告。
 * 点击切换（懒声明与错误态由 store 的 selectWorkspace 处理）。底部「＋」
 * 添加工作区（沿用目录选择 + 已知未开流程，R3）、「⚙」全局设置（R8）。
 * 运行中终端的角标（R2）数据源在 U2/U8 接入。
 * U2（R4）：右键弹工作区菜单——会话计数在打开菜单前经
 * workspace_session_counts 拉取（失败 → running=null，移除走两段确认）。
 */
import { useState } from 'react'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import type { HomeInfo } from './types'
import type { KnownHome } from './store'
import { workspaceRootOf } from './homePath'
import { NOT_A_WORKSPACE_HINT, resolveHomeFromPickedDir } from './workspacePath'
import {
  addCandidates,
  abbreviationOf,
  parentDirOf,
  workspaceColor,
  type WorkspaceEntry,
} from './workspaceList'
import { workspaceSessionCounts, workspaceReveal } from './bridge'
import { WorkspaceContextMenu, type WorkspaceMenuInfo } from './WorkspaceContextMenu'

interface Props {
  workspaces: WorkspaceEntry[]
  knownHomes: KnownHome[]
  /** daemon 已打开的 home——添加弹窗候选来源之一（F-TERM-5） */
  homes: HomeInfo[]
  activeHomeId: string | null
  homeDir?: string
  /** 图标错误态（homeId → 原因）：懒声明失败等 */
  errors: Record<string, string>
  /** 路径已失效的工作区（known-homes 的 missing 探测，按 path/homeId 双键）——置灰带警告 */
  isMissingWorkspace: (ws: WorkspaceEntry) => boolean
  settingsOpen: boolean
  /** 添加弹窗由 App 托管（空状态的「添加工作区」按钮共用同一入口） */
  addOpen: boolean
  onSelect: (ws: WorkspaceEntry) => void
  onAdd: (homePath: string) => Promise<string | null>
  onAddOpen: () => void
  onAddClose: () => void
  onToggleSettings: () => void
  /** U2/R4：右键菜单的入口（非当前工作区也可操作，无需先切换） */
  onOpenSettings: (ws: WorkspaceEntry) => void
  onOpenTerminal: (ws: WorkspaceEntry) => void
  onRemove: (ws: WorkspaceEntry, opts?: { stopSessions?: boolean }) => Promise<void>
}

export function WorkspaceRail({
  workspaces,
  knownHomes,
  homes,
  activeHomeId,
  homeDir,
  errors,
  isMissingWorkspace,
  settingsOpen,
  addOpen,
  onSelect,
  onAdd,
  onAddOpen,
  onAddClose,
  onToggleSettings,
  onOpenSettings,
  onOpenTerminal,
  onRemove,
}: Props) {
  const [addMessage, setAddMessage] = useState<string | null>(null)
  const [picking, setPicking] = useState(false)
  // U2/R4：右键菜单（打开前先拉会话计数，拉到才弹——terminalCapable 与
  // running 一次拿齐；失败也弹，running=null 走保守两段确认）
  const [menu, setMenu] = useState<{
    ws: WorkspaceEntry
    position: { x: number; y: number }
    info: WorkspaceMenuInfo
  } | null>(null)

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

  const openContextMenu = (ws: WorkspaceEntry, x: number, y: number) => {
    void workspaceSessionCounts()
      .then(homes => {
        const entry = homes.find(h => h.homeId === ws.homeId)
        setMenu({
          ws,
          position: { x, y },
          info: {
            terminalCapable: entry?.terminalCapable ?? false,
            // 找不到条目（homeId 轮换 / home 无 agents 目录）按未知处理——
            // 移除走两段确认，绝不因“没找到”而当作 0 直接移除
            running: entry ? entry.terminals + entry.others : null,
          },
        })
      })
      .catch(() => {
        setMenu({
          ws,
          position: { x, y },
          info: { terminalCapable: false, running: null },
        })
      })
  }

  // F-TERM-5：候选 = known-homes 条目 + daemon 已打开的 home，排除已在
  // 图标栏里的（path 或 homeId 命中）——已在列表中的不再显示为「已知未开」，
  // 由 DSH/CLI 打开的 home 也能从这里收进图标栏
  const candidates = addCandidates(workspaces, knownHomes, homes, homeDir)

  return (
    <>
      <nav className="workspace-rail" aria-label="工作区">
        <div className="rail-icons">
          {workspaces.map(ws => {
            const missing = isMissingWorkspace(ws)
            const error = errors[ws.homeId]
            const cls = [
              'rail-icon',
              ws.homeId === activeHomeId ? 'active' : '',
              missing ? 'missing' : '',
              error ? 'error' : '',
            ].filter(Boolean).join(' ')
            // P3（§64）：全局 home 的根被压缩成 ~，没有上级目录——悬停提示
            // 只显示显示名，不出现「~（~）」
            const parent = parentDirOf(ws.path, homeDir)
            const tooltip = [
              parent !== null ? `${ws.name}（${parent}）` : ws.name,
              missing ? '目录已不存在' : '',
              error ?? '',
            ].filter(Boolean).join(' — ')
            return (
              <button
                key={`${ws.homeId}:${ws.path}`}
                className={cls}
                style={{ background: workspaceColor(ws.path) }}
                title={tooltip}
                aria-label={ws.name}
                aria-current={ws.homeId === activeHomeId ? 'true' : undefined}
                onClick={() => onSelect(ws)}
                onContextMenu={event => {
                  event.preventDefault()
                  openContextMenu(ws, event.clientX, event.clientY)
                }}
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
      {menu && (
        <WorkspaceContextMenu
          ws={menu.ws}
          info={menu.info}
          position={menu.position}
          onClose={() => setMenu(null)}
          onOpenSettings={onOpenSettings}
          onOpenTerminal={onOpenTerminal}
          onReveal={ws => {
            void workspaceReveal(ws.homeId).catch(() => {/* 错误在 Rust 侧已拒、菜单关闭 */ })
          }}
          onRemove={onRemove}
        />
      )}
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
            {candidates.length > 0 ? (
              <div className="known-section">
                <h4>可添加的工作区（点击即添加）</h4>
                {candidates.map(c => (
                  <button
                    key={c.path}
                    className="home-row"
                    disabled={c.missing}
                    title={c.missing ? '目录已不存在（被移动/删除）' : c.path}
                    onClick={() => void add(c.path)}
                  >
                    <span className="chip">{c.kind}</span>
                    <span className="mono">{workspaceRootOf(c.path, homeDir, c.name)}</span>
                    {c.missing && <span className="chip">已缺失</span>}
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
