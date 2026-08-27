/**
 * U10 detail/editor panel: node view + guarded edit flow. Revision
 * conflicts surface as an explicit refresh prompt (presentError already
 * translates CONFLICT/revision failures); the body editor posts with
 * expectedRevision so a stale submit is rejected server-side.
 */
import { useEffect, useState } from 'react'
import { omtCall } from './bridge'
import type { HomeInfo, OmtNode } from './types'
import { STATUS_COLORS } from './types'

interface Props {
  home: HomeInfo
  nodeId: string
  onUpdated: (id: string, patch: Record<string, unknown>, revision?: number) => Promise<string | null>
}

interface NodeDetail extends OmtNode {
  body?: string
  revision?: number
}

export function DetailPanel({ home, nodeId, onUpdated }: Props) {
  const [detail, setDetail] = useState<NodeDetail | null>(null)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const [message, setMessage] = useState<string | null>(null)

  const load = async () => {
    try {
      const result = await omtCall<{ node?: NodeDetail & { nodeId?: string } }>('node/get', { homeId: home.homeId, nodeId })
      const wire = result.node
      // Same boundary rule as the tree: wire views use nodeId.
      const detail = wire ? { ...wire, id: wire.nodeId ?? wire.id } : null
      setDetail(detail)
      setDraft(detail?.body ?? '')
      setMessage(null)
    } catch (error) {
      setMessage(String(error))
    }
  }

  useEffect(() => {
    setEditing(false)
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [home.homeId, nodeId])

  if (!detail) return <div className="detail-panel"><p className="empty-hint">{message ?? '加载中…'}</p></div>

  const save = async () => {
    const error = await onUpdated(nodeId, { body: draft }, detail.revision)
    if (error) {
      setMessage(error)
      return
    }
    setEditing(false)
    await load()
  }

  const cycleStatus = async (status: string) => {
    const error = await onUpdated(nodeId, { status }, detail.revision)
    if (error) setMessage(error)
    else await load()
  }

  return (
    <div className="detail-panel">
      <header className="detail-header">
        <span className="status-dot" style={{ background: STATUS_COLORS[detail.status] ?? '#8b949e' }} />
        <h2>{detail.id} · {detail.title}</h2>
        <span className="node-type-tag">{detail.type}</span>
      </header>
      <div className="status-actions">
        {['open', 'in_progress', 'done', 'blocked', 'skipped'].map(status => (
          <button key={status} className="chip" disabled={detail.status === status} onClick={() => void cycleStatus(status)}>
            {status}
          </button>
        ))}
      </div>
      {message && <p className="error-banner">{message}</p>}
      {editing ? (
        <div className="editor">
          <textarea value={draft} onChange={e => setDraft(e.target.value)} rows={16} />
          <div className="editor-actions">
            <button className="chip active" onClick={() => void save()}>保存</button>
            <button className="chip" onClick={() => { setEditing(false); setDraft(detail.body ?? '') }}>取消</button>
          </div>
        </div>
      ) : (
        <div className="body-view">
          <pre>{detail.body || '（无正文）'}</pre>
          <button className="chip" onClick={() => setEditing(true)}>编辑正文</button>
        </div>
      )}
    </div>
  )
}
