/**
 * U2 / R6：工作区设置标签页（与详情 / Agents / Runs 并列）——当前工作区
 * 的：路径、重建索引（沿用 SettingsPanel 的 admin 探测：home/reindex
 * dryRun 探测，无权限不显示）、在 Finder 中显示（KTD17）、从列表移除
 * （R9/R32：有运行中会话时两段确认，不删除 `.omt` 目录）。
 */
import { useEffect, useState } from 'react'
import { omtCall, workspaceReveal, workspaceSessionCounts } from './bridge'
import type { HomeInfo } from './types'
import { workspaceRootOf } from './homePath'
import type { WorkspaceEntry } from './workspaceList'
import { WorkspaceRemoveOptions } from './WorkspaceContextMenu'

interface Props {
  workspace: WorkspaceEntry
  home: HomeInfo
  homeDir?: string
  onRemove: (ws: WorkspaceEntry, opts?: { stopSessions?: boolean }) => Promise<void>
}

export function WorkspaceSettingsTab({ workspace, home, homeDir, onRemove }: Props) {
  const [isAdmin, setIsAdmin] = useState(false)
  const [reindexing, setReindexing] = useState(false)
  const [message, setMessage] = useState<string | null>(null)
  const [confirming, setConfirming] = useState(false)
  const [busy, setBusy] = useState(false)
  const [running, setRunning] = useState<number | null>(null)

  // Admin 探测（同 SettingsPanel）：FORBIDDEN → 无 admin 授权，按钮不显示
  useEffect(() => {
    setIsAdmin(false)
    void adminProbe(home.homeId).then(setIsAdmin)
  }, [home.homeId])

  // 会话计数（移除确认的判据；拉取失败 → null，移除一律走两段确认）
  useEffect(() => {
    setRunning(null)
    let disposed = false
    void workspaceSessionCounts()
      .then(homes => {
        if (disposed) return
        const entry = homes.find(h => h.homeId === home.homeId)
        // 找不到条目按未知处理（null）——移除走两段确认，不当作 0
        setRunning(entry ? entry.terminals + entry.others : null)
      })
      .catch(() => {/* 保持 null——保守路径 */ })
    return () => {
      disposed = true
    }
  }, [home.homeId])

  const reveal = async () => {
    setMessage(null)
    try {
      await workspaceReveal(workspace.homeId)
    } catch (error) {
      setMessage(String(error))
    }
  }

  const reindex = async () => {
    setReindexing(true)
    setMessage(null)
    try {
      const result = await omtCall('home/reindex', { homeId: home.homeId })
      const view = result as { nodes?: number; edges?: number; skipped?: number }
      setMessage(
        `已重建索引：${view.nodes ?? '?'} 节点 / ${view.edges ?? '?'} 边`
        + (view.skipped ? `（跳过 ${view.skipped} 条无法解析的条目）` : ''),
      )
    } catch (error) {
      setMessage(`重建索引失败：${String(error)}`)
    } finally {
      setReindexing(false)
    }
  }

  const remove = async (stopSessions: boolean) => {
    setBusy(true)
    try {
      await onRemove(workspace, { stopSessions })
      // 移除成功后选中已切换/清空，本标签页随之卸载
    } catch (error) {
      setBusy(false)
      setMessage(String(error))
    }
  }

  return (
    <div className="workspace-settings-page">
      <h2>工作区设置</h2>
      <dl className="status-grid">
        <dt>名称</dt><dd>{workspace.name}</dd>
        <dt>路径</dt><dd className="mono">{workspaceRootOf(workspace.path, homeDir, workspace.path)}</dd>
        <dt>homeId</dt><dd className="mono">{home.homeId}</dd>
      </dl>
      <section className="ws-settings-actions">
        {isAdmin && (
          <button className="chip active" disabled={reindexing} onClick={() => void reindex()}>
            {reindexing ? '重建索引中…' : '重建索引'}
          </button>
        )}
        <button className="chip" onClick={() => void reveal()}>在 Finder 中显示</button>
        <button
          className="chip ws-menu-danger"
          onClick={() => {
            if (running === null || running > 0) setConfirming(true)
            else void remove(false)
          }}
        >
          从列表移除
        </button>
      </section>
      {message && <p className="info-banner">{message}</p>}
      {confirming && (
        <WorkspaceRemoveOptions
          running={running}
          busy={busy}
          onRemove={stop => void remove(stop)}
          onCancel={() => setConfirming(false)}
        />
      )}
    </div>
  )
}

/** admin 探测（与 SettingsPanel 同款：FORBIDDEN → 无授权）。 */
async function adminProbe(homeId: string): Promise<boolean> {
  try {
    await omtCall('home/reindex', { homeId, dryRun: true })
    return true
  } catch (error) {
    return !String(error).includes('FORBIDDEN')
  }
}
