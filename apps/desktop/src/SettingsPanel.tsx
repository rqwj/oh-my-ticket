/**
 * U10 settings panel: daemon status projection + home selector/declare
 * flow. Admin-only actions render ONLY when the credential's operations
 * include the admin family (AE12: no grant → no render).
 */
import { useEffect, useState } from 'react'
import { daemonStatus, omtCall, type DaemonStatus } from './bridge'
import type { HomeInfo } from './types'
import type { KnownHome } from './store'

interface Props {
  homes: HomeInfo[]
  knownHomes: KnownHome[]
  activeHome: HomeInfo | null
  onSelectHome: (home: HomeInfo) => void
  onDeclare: (path: string) => Promise<string | null>
}

export function SettingsPanel({ homes, knownHomes, activeHome, onSelectHome, onDeclare }: Props) {
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [declarePath, setDeclarePath] = useState('')
  const [declareMessage, setDeclareMessage] = useState<string | null>(null)
  const [isAdmin, setIsAdmin] = useState(false)

  useEffect(() => {
    void daemonStatus().then(setStatus).catch(() => {})
    // Capability probe: an admin-family call that fails FORBIDDEN tells us
    // the credential lacks the grant — admin actions stay hidden (AE12).
    omtCall('home/reindex', { homeId: activeHome?.homeId ?? '', dryRun: true })
      .then(() => setIsAdmin(true))
      .catch(error => setIsAdmin(!String(error).includes('FORBIDDEN')))
  }, [activeHome?.homeId])

  const declare = async () => {
    const path = declarePath.trim()
    if (!path) return
    const error = await onDeclare(path)
    setDeclareMessage(error ?? `已收录 ${path}`)
    if (!error) setDeclarePath('')
  }

  return (
    <div className="settings-panel">
      <section>
        <h3>Daemon</h3>
        {status ? (
          <dl className="status-grid">
            <dt>状态</dt><dd>{status.running ? '运行中' : '未运行'}</dd>
            <dt>pid</dt><dd>{status.pid ?? '—'}</dd>
            <dt>代际</dt><dd>{status.generation ?? '—'}</dd>
            <dt>endpoint</dt><dd className="mono">{status.endpoint ?? '—'}</dd>
          </dl>
        ) : (
          <p className="empty-hint">读取中…</p>
        )}
      </section>
      <section>
        <h3>Homes</h3>
        <div className="home-list">
          {homes.map(home => (
            <button
              key={home.homeId}
              className={`home-row${activeHome?.homeId === home.homeId ? ' selected' : ''}`}
              onClick={() => onSelectHome(home)}
            >
              <span className="chip">{home.kind ?? 'workspace'}</span>
              <span>{home.name ?? home.path ?? home.homeId}</span>
            </button>
          ))}
        </div>
        {knownHomes.filter(k => !k.open).length > 0 && (
          <div className="known-section">
            <h4>已知未开（点击收录）</h4>
            {knownHomes.filter(k => !k.open).map(known => (
              <button
                key={known.path}
                className="home-row"
                disabled={known.missing}
                title={known.missing ? '目录已不存在（被移动/删除）' : known.path}
                onClick={() => void onDeclare(known.path).then(err => setDeclareMessage(err ?? `已收录 ${known.name}`))}
              >
                <span className="chip">{known.kind}</span>
                <span>{known.name}</span>
                {known.missing && <span className="chip">已缺失</span>}
              </button>
            ))}
          </div>
        )}
        <div className="declare-form">
          <input
            placeholder="收录新 home 目录路径（含 .omt）…"
            value={declarePath}
            onChange={e => setDeclarePath(e.target.value)}
          />
          <button className="chip active" onClick={() => void declare()}>收录</button>
        </div>
        {declareMessage && <p className="info-banner">{declareMessage}</p>}
      </section>
      {isAdmin && (
        <section>
          <h3>Admin</h3>
          <p className="empty-hint">管理操作区（凭据已授权 admin 族）</p>
        </section>
      )}
    </div>
  )
}
