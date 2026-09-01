/**
 * U10 detail panel v2 — 对齐 DSH 插件详情形态：状态/优先级下拉、相对
 * 时间戳、父链与 children 胶囊（状态点）、Execute/Add to run/Archive/
 * Copy ID/Copy path 操作行、Markdown 渲染正文。编辑保留 revision 并发门。
 *
 * 线上契约（dispatch.rs node_get）：{ node, parent, children, body, runs }
 * —— body 是顶层字段，children 是 nodeSummary 数组（nodeId 键）。
 */
import { useEffect, useState } from 'react'
import { omtCall } from './bridge'
import { MarkdownText } from './MarkdownText'
import type { HomeInfo, OmtNode } from './types'
import { STATUS_COLORS } from './types'

interface Props {
  home: HomeInfo
  nodeId: string
  onUpdated: (id: string, patch: Record<string, unknown>, revision?: number) => Promise<string | null>
  onSelect: (id: string) => void
  onChanged: () => void
}

interface NodeSummaryWire {
  nodeId: string
  type: string
  title: string
  status: string
  archived?: boolean
  priority?: number
}

interface NodeDetailWire {
  node?: (OmtNode & { nodeId?: string; createdAt?: string; updatedAt?: string; path?: string }) | undefined
  parent?: NodeSummaryWire
  children?: NodeSummaryWire[]
  body?: string
}

const STATUS_OPTIONS = ['open', 'in_progress', 'done', 'blocked', 'skipped'] as const
const PRIORITY_OPTIONS = [
  { value: 0, label: 'P0 Normal' },
  { value: 1, label: 'P1 High' },
  { value: 2, label: 'P2 Urgent' },
  { value: 3, label: 'P3 Critical' },
] as const

function relativeTime(iso?: string): string {
  if (!iso) return '—'
  const ms = Date.now() - new Date(iso).getTime()
  if (!Number.isFinite(ms) || ms < 0) return iso
  const minutes = Math.floor(ms / 60000)
  if (minutes < 1) return '刚刚'
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  if (days < 30) return `${days} 天前`
  return iso.slice(0, 10)
}

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    return false
  }
}

export function DetailPanel({ home, nodeId, onUpdated, onSelect, onChanged }: Props) {
  const [wire, setWire] = useState<NodeDetailWire | null>(null)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const [message, setMessage] = useState<string | null>(null)
  const [confirmArchive, setConfirmArchive] = useState(false)

  const load = async () => {
    try {
      const result = await omtCall<NodeDetailWire>('node/get', { homeId: home.homeId, nodeId })
      setWire(result)
      setDraft(result.body ?? '')
      setMessage(null)
    } catch (error) {
      setMessage(String(error))
    }
  }

  useEffect(() => {
    setEditing(false)
    setConfirmArchive(false)
    setWire(null)
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [home.homeId, nodeId])

  if (!wire?.node) return <div className="detail-panel"><p className="empty-hint">{message ?? '加载中…'}</p></div>

  const detail = { ...wire.node, id: wire.node.nodeId ?? wire.node.id }
  const children = wire.children ?? []

  const applyPatch = async (patch: Record<string, unknown>) => {
    const error = await onUpdated(detail.id, patch, detail.revision)
    if (error) {
      setMessage(error)
      return false
    }
    await load()
    return true
  }

  const saveBody = async () => {
    if (await applyPatch({ body: draft })) setEditing(false)
  }

  const execute = async () => {
    try {
      await omtCall('node/execute', { homeId: home.homeId, nodeId: detail.id })
      setMessage(null)
      await load()
      onChanged()
    } catch (error) {
      setMessage(String(error))
    }
  }

  const addToRun = async () => {
    try {
      const result = await omtCall<{ run?: { runId?: string } }>('run/create', {
        homeId: home.homeId,
        nodeIds: [detail.id],
        title: detail.title,
      })
      setMessage(`已创建 run ${result.run?.runId ?? ''}（含 ${detail.id}）`)
      onChanged()
    } catch (error) {
      setMessage(String(error))
    }
  }

  const archive = async () => {
    try {
      await omtCall('node/archive', { homeId: home.homeId, nodeId: detail.id })
      setConfirmArchive(false)
      await load()
      onChanged()
    } catch (error) {
      setMessage(String(error))
    }
  }

  return (
    <div className="detail-panel">
      <div className="detail-toolbar">
        <span className="node-id">{detail.id}</span>
        <select
          className="detail-select"
          value={detail.status}
          onChange={e => void applyPatch({ status: e.target.value })}
        >
          {STATUS_OPTIONS.map(status => (
            <option key={status} value={status}>● {status}</option>
          ))}
        </select>
        <select
          className="detail-select"
          value={detail.priority}
          onChange={e => void applyPatch({ priority: Number(e.target.value) })}
        >
          {PRIORITY_OPTIONS.map(option => (
            <option key={option.value} value={option.value}>{option.label}</option>
          ))}
        </select>
        <span className="node-type-tag">{detail.type}</span>
        {detail.archived && <span className="chip">已归档</span>}
      </div>

      <h2 className="detail-title">{detail.title}</h2>
      <p className="detail-meta">
        Created {relativeTime(detail.createdAt)} · Updated {relativeTime(detail.updatedAt)}
      </p>

      {wire.parent && (
        <div className="detail-line">
          <span className="detail-label">Parent</span>
          <button className="child-chip" onClick={() => onSelect(wire.parent!.nodeId)}>
            <span className="status-dot" style={{ background: STATUS_COLORS[wire.parent.status] ?? '#8b949e' }} />
            {wire.parent.nodeId} {wire.parent.title}
          </button>
        </div>
      )}
      {children.length > 0 && (
        <div className="detail-line">
          <span className="detail-label">Children</span>
          <div className="children-list">
            {children.map(child => (
              <button key={child.nodeId} className="child-chip" onClick={() => onSelect(child.nodeId)}>
                <span className="status-dot" style={{ background: STATUS_COLORS[child.status] ?? '#8b949e' }} />
                {child.nodeId} {child.title}
              </button>
            ))}
          </div>
        </div>
      )}

      <div className="detail-actions">
        <button className="chip primary" onClick={() => void execute()}>Execute</button>
        <button className="chip" onClick={() => void addToRun()}>Add to run</button>
        {confirmArchive ? (
          <span className="confirm-group">
            确认归档？
            <button className="chip active" onClick={() => void archive()}>是</button>
            <button className="chip" onClick={() => setConfirmArchive(false)}>否</button>
          </span>
        ) : (
          <button className="chip" onClick={() => setConfirmArchive(true)}>Archive</button>
        )}
        <button className="chip" onClick={() => void copyText(detail.id).then(ok => setMessage(ok ? `已复制 ${detail.id}` : '复制失败'))}>Copy ID</button>
        <button className="chip" onClick={() => void copyText(detail.path ?? '').then(ok => setMessage(ok ? '已复制路径' : '复制失败'))}>Copy path</button>
      </div>

      {message && <p className={message.startsWith('已') ? 'info-banner' : 'error-banner'}>{message}</p>}

      {editing ? (
        <div className="editor">
          <textarea value={draft} onChange={e => setDraft(e.target.value)} rows={16} />
          <div className="editor-actions">
            <button className="chip active" onClick={() => void saveBody()}>保存</button>
            <button className="chip" onClick={() => { setEditing(false); setDraft(wire.body ?? '') }}>取消</button>
          </div>
        </div>
      ) : (
        <div className="body-view">
          {wire.body ? <MarkdownText text={wire.body} /> : <p className="empty-hint">（无正文）</p>}
          <button className="chip" onClick={() => setEditing(true)}>编辑正文</button>
        </div>
      )}
    </div>
  )
}
