/**
 * U10 tree panel: hierarchical ticket forest with status colors, filters
 * bar (query + status multi-toggle), and recent strip. Data arrives as
 * the recursive node/tree projection; filtering is client-side over the
 * bag persisted under the `tauri:ui` key (KD3).
 */
import { useMemo, useState } from 'react'
import type { OmtNode, SavedFilters } from './types'
import { STATUS_COLORS } from './types'

interface Props {
  nodes: OmtNode[]
  filters: SavedFilters
  recentIds: string[]
  selectedId: string | null
  onSelect: (id: string) => void
  onFilters: (patch: SavedFilters) => void
}

const STATUS_ORDER = ['open', 'in_progress', 'done', 'blocked', 'skipped'] as const

function matches(node: OmtNode, filters: SavedFilters): boolean {
  if (filters.query && !node.title.toLowerCase().includes(filters.query.toLowerCase()) && !node.id.toLowerCase().includes(filters.query.toLowerCase())) {
    return false
  }
  if (filters.statuses && filters.statuses.length > 0 && !filters.statuses.includes(node.status)) return false
  if (!filters.showArchived && node.status === 'archived') return false
  return true
}

function NodeRow({ node, depth, filters, selectedId, onSelect }: {
  node: OmtNode
  depth: number
  filters: SavedFilters
  selectedId: string | null
  onSelect: (id: string) => void
}) {
  const children = ((node as { children?: OmtNode[] }).children ?? []).filter(c => matches(c, filters))
  if (!matches(node, filters) && children.length === 0) return null
  return (
    <>
      <button
        className={`tree-row${selectedId === node.id ? ' selected' : ''}`}
        style={{ paddingLeft: 8 + depth * 16 }}
        onClick={() => onSelect(node.id)}
      >
        <span className="status-dot" style={{ background: STATUS_COLORS[node.status] ?? '#8b949e' }} />
        <span className="node-id">{node.id}</span>
        <span className="node-title">{node.title}</span>
      </button>
      {children.map(child => (
        <NodeRow key={child.id} node={child} depth={depth + 1} filters={filters} selectedId={selectedId} onSelect={onSelect} />
      ))}
    </>
  )
}

export function TreePanel({ nodes, filters, recentIds, selectedId, onSelect, onFilters }: Props) {
  const [query, setQuery] = useState(filters.query ?? '')
  const roots = useMemo(() => nodes.filter(n => matches(n, filters) || hasMatchingDescendant(n, filters)), [nodes, filters])
  const recentNodes = useMemo(
    () => recentIds.map(id => findNode(nodes, id)).filter((n): n is OmtNode => n !== undefined),
    [recentIds, nodes],
  )
  return (
    <div className="tree-panel">
      <div className="filters-bar">
        <input
          placeholder="搜索标题或 id…"
          value={query}
          onChange={e => {
            setQuery(e.target.value)
            onFilters({ query: e.target.value })
          }}
        />
        <div className="status-toggles">
          {STATUS_ORDER.map(status => {
            const active = !filters.statuses?.length || filters.statuses.includes(status)
            return (
              <button
                key={status}
                className={`chip${active ? ' active' : ''}`}
                style={{ borderColor: STATUS_COLORS[status] }}
                onClick={() => {
                  const current = filters.statuses?.length ? filters.statuses : [...STATUS_ORDER]
                  const next = current.includes(status) ? current.filter(s => s !== status) : [...current, status]
                  onFilters({ statuses: next.length === STATUS_ORDER.length ? [] : next })
                }}
              >
                {status}
              </button>
            )
          })}
        </div>
      </div>
      {recentNodes.length > 0 && (
        <div className="recent-strip">
          <span className="recent-label">最近</span>
          {recentNodes.map(node => (
            <button key={node.id} className="chip" onClick={() => onSelect(node.id)}>
              {node.id}
            </button>
          ))}
        </div>
      )}
      <div className="tree-scroll">
        {roots.map(node => (
          <NodeRow key={node.id} node={node} depth={0} filters={filters} selectedId={selectedId} onSelect={onSelect} />
        ))}
        {roots.length === 0 && <p className="empty-hint">无匹配 ticket</p>}
      </div>
    </div>
  )
}

function hasMatchingDescendant(node: OmtNode, filters: SavedFilters): boolean {
  return ((node as { children?: OmtNode[] }).children ?? []).some(c => matches(c, filters) || hasMatchingDescendant(c, filters))
}

function findNode(nodes: OmtNode[], id: string): OmtNode | undefined {
  for (const node of nodes) {
    if (node.id === id) return node
    const hit = findNode((node as { children?: OmtNode[] }).children ?? [], id)
    if (hit) return hit
  }
  return undefined
}
