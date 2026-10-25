/**
 * U2（R4）：工作区图标的右键菜单——无需先切换到该工作区即可操作：
 * 工作区设置 / 在此工作区打开终端（KTD19：terminalCapable 为 false 不
 * 显示；U8 之前点击跳 Agents 页）/ 在 Finder 中显示（KTD17）/ 从列表
 * 移除（R9/R32：有运行中会话时弹「停止会话并移除」「移除但保留运行」
 * 两个选项；无论选哪个都不删除 `.omt` 目录——前端没有任何删除路径）。
 * 会话计数由调用方在打开菜单前拉取（workspace_session_counts）；拉取
 * 失败时 running 传 null——移除一律走两段确认（宁可多问一次）。
 */
import { useEffect, useRef, useState } from 'react'
import type { WorkspaceEntry } from './workspaceList'
import type { WorkspaceSessionCount } from './bridge'

/** 右键菜单的会话能力面（null = 拉取失败，按保守处理）。 */
export type WorkspaceMenuInfo = Pick<WorkspaceSessionCount, 'terminalCapable'> & {
  running: number | null
}

interface Props {
  ws: WorkspaceEntry
  info: WorkspaceMenuInfo
  position: { x: number; y: number }
  onClose: () => void
  onOpenSettings: (ws: WorkspaceEntry) => void
  onOpenTerminal: (ws: WorkspaceEntry) => void
  onReveal: (ws: WorkspaceEntry) => void
  onRemove: (ws: WorkspaceEntry, opts?: { stopSessions?: boolean }) => Promise<void> | void
}

/**
 * 移除确认块（R32）：两个选项 + 取消。菜单与工作区设置页共用——
 * 「停止会话并移除」先停该 home 的活动 exec 再移除（失败如实上抛，调用
 * 方就地展示）；「移除但保留运行」只动桌面端列表，会话照常跑完
 * （Rust 侧按 runtime 目录计数/停止，与图标栏无关）。
 */
export function WorkspaceRemoveOptions({
  running,
  busy,
  onRemove,
  onCancel,
}: {
  /** null = 无法确认会话状态（保守走两段确认） */
  running: number | null
  busy: boolean
  onRemove: (stopSessions: boolean) => void
  onCancel: () => void
}) {
  return (
    <div className="ws-remove-confirm" role="alertdialog" aria-label="移除工作区确认">
      <p className="ws-remove-text">
        {running === null
          ? '无法确认该工作区的会话状态。它可能还有运行中的 agent。'
          : `该工作区还有 ${running} 个运行中的会话。`}
        移除只把它从桌面端列表去掉，不删除磁盘上的 `.omt` 目录和 ticket。
      </p>
      <div className="chip-row">
        <button className="chip active" disabled={busy} onClick={() => onRemove(true)}>
          {busy ? '停止中…' : '停止会话并移除'}
        </button>
        <button className="chip" disabled={busy} onClick={() => onRemove(false)}>
          移除但保留运行
        </button>
        <button className="chip" disabled={busy} onClick={onCancel}>取消</button>
      </div>
    </div>
  )
}

export function WorkspaceContextMenu({
  ws,
  info,
  position,
  onClose,
  onOpenSettings,
  onOpenTerminal,
  onReveal,
  onRemove,
}: Props) {
  const [confirming, setConfirming] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // 卸载语义：菜单消失即收场（无内部定时器，ref 仅用于点击外部判定）
  const menuRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  const runRemove = async (stopSessions: boolean) => {
    setBusy(true)
    setError(null)
    try {
      await onRemove(ws, { stopSessions })
      onClose()
    } catch (err) {
      setBusy(false)
      setError(String(err))
    }
  }

  return (
    <div
      className="context-overlay"
      onClick={onClose}
      onContextMenu={event => {
        event.preventDefault()
        onClose()
      }}
    >
      <div
        ref={menuRef}
        className="ws-context-menu"
        style={{ left: position.x, top: position.y }}
        onClick={event => event.stopPropagation()}
        onContextMenu={event => event.preventDefault()}
        role="menu"
        aria-label={`${ws.name} 的操作`}
      >
        {confirming ? (
          <>
            <WorkspaceRemoveOptions
              running={info.running}
              busy={busy}
              onRemove={stop => void runRemove(stop)}
              onCancel={() => { setConfirming(false); setError(null) }}
            />
            {error && <p className="error-banner">{error}</p>}
          </>
        ) : (
          <>
            <button className="ws-menu-item" role="menuitem" onClick={() => { onOpenSettings(ws); onClose() }}>
              工作区设置
            </button>
            {info.terminalCapable && (
              <button className="ws-menu-item" role="menuitem" onClick={() => { onOpenTerminal(ws); onClose() }}>
                在此工作区打开终端
              </button>
            )}
            <button className="ws-menu-item" role="menuitem" onClick={() => { onReveal(ws); onClose() }}>
              在 Finder 中显示
            </button>
            <div className="ws-menu-sep" />
            <button
              className="ws-menu-item ws-menu-danger"
              role="menuitem"
              onClick={() => {
                if (info.running === null || info.running > 0) setConfirming(true)
                else void runRemove(false)
              }}
            >
              从列表移除
            </button>
            {error && <p className="error-banner">{error}</p>}
          </>
        )}
      </div>
    </div>
  )
}
