/**
 * U10 tree panel v2 — 视觉对齐 DSH 插件设计系统：类型徽章（E/S/SS/T/ST
 * 彩色方块）、可折叠树（chevron）、完整过滤器行（类型/状态/优先级/
 * Archived 胶囊）与排序行（# ID / No sorting / Priority desc/asc）。
 * 数据为递归 node/tree 投影；过滤排序客户端执行，filter bag 持久化在
 * `tauri:ui` 键（KD3）。
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

const TYPE_META: Record<string, { badge: string; color: string }> = {
  epic: { badge: 'E', color: '#a371f7' },
  story: { badge: 'S', color: '#58a6ff' },
  substory: { badge: 'SS', color: '#79c0ff' },
  ticket: { badge: 'T', color: '#3fb950' },
  subticket: { badge: 'ST', color: '#56d364' },
}

const STATUS_ORDER = ['open', 'in_progress', 'done', 'blocked', 'skipped'] as const
const STATUS_LABEL: Record<string, string> = {
  open: 'Open',
  in_progress: 'In progress',
  done: 'Done',
  blocked: 'Blocked',
  skipped: 'Skipped',
}
const PRIORITIES = [0, 1, 2, 3] as const
const SORTS = [
  { key: 'none', label: 'No sorting' },
  { key: 'priority-desc', label: 'Priority descending' },
  { key: 'priority-asc', label: 'Priority ascending' },
] as const

function childrenOf(node: OmtNode): OmtNode[] {
  return (node as { children?: OmtNode[] }).children ?? []
}

function matches(node: OmtNode, filters: SavedFilters): boolean {
  if (filters.query && !node.title.toLowerCase().includes(filters.query.toLowerCase()) && !node.id.toLowerCase().includes(filters.query.toLowerCase())) {
    return false
  }
  if (filters.types && filters.types.length > 0 && !filters.types.includes(node.type)) return false
  if (filters.statuses && filters.statuses.length > 0 && !filters.statuses.includes(node.status)) return false
  if (filters.priorities && filters.priorities.length > 0 && !filters.priorities.includes(node.priority)) return false
  if (!filters.showArchived && node.status === 'archived') return false
  return true
}

function sortSiblings(nodes: OmtNode[], sortOrder?: string): OmtNode[] {
  if (sortOrder === 'priority-desc') return [...nodes].sort((a, b) => b.priority - a.priority)
  if (sortOrder === 'priority-asc') return [...nodes].sort((a, b) => a.priority - b.priority)
  return nodes
}

function NodeRow({ node, depth, filters, selectedId, collapsed, onToggle, onSelect }: {
  node: OmtNode
  depth: number
  filters: SavedFilters
  selectedId: string | null
  collapsed: Set<string>
  onToggle: (id: string) => void
  onSelect: (id: string) => void
}) {
  const children = childrenOf(node)
  const visible = matches(node, filters) || children.some(c => matches(c, filters) || hasMatchingDescendant(c, filters))
  if (!visible) return null
  const meta = TYPE_META[node.type] ?? { badge: '?', color: '#8b949e' }
  const isCollapsed = collapsed.has(node.id)
  return (
    <>
      <div
        className={`tree-row${selectedId === node.id ? ' selected' : ''}`}
        style={{ paddingLeft: 6 + depth * 18 }}
        onClick={() => onSelect(node.id)}
      >
        <span
          className={`chevron${children.length === 0 ? ' empty' : ''}`}
          onClick={e => {
            if (children.length > 0) {
              e.stopPropagation()
              onToggle(node.id)
            }
          }}
        >
          {children.length > 0 ? (isCollapsed ? '▸' : '▾') : ''}
        </span>
        <span className="type-badge" style={{ background: meta.color }}>{meta.badge}</span>
        {filters.showId && <span className="node-id">{node.id}</span>}
        <span className="node-title">{node.title}</span>
        {node.priority > 0 && <span className={`prio-tag p${node.priority}`}>P{node.priority}</span>}
      </div>
      {!isCollapsed &&
        sortSiblings(children, filters.sortOrder).map(child => (
          <NodeRow key={child.id} node={child} depth={depth + 1} filters={filters} selectedId={selectedId} collapsed={collapsed} onToggle={onToggle} onSelect={onSelect} />
        ))}
    </>
  )
}

function hasMatchingDescendant(node: OmtNode, filters: SavedFilters): boolean {
  return childrenOf(node).some(c => matches(c, filters) || hasMatchingDescendant(c, filters))
}

function toggleInList<T>(list: T[] | undefined, value: T, all: readonly T[]): T[] {
  const current = list && list.length > 0 ? [...list] : [...all]
  const next = current.includes(value) ? current.filter(v => v !== value) : [...current, value]
  return next.length === all.length ? [] : next
}

export function TreePanel({ nodes, filters, recentIds, selectedId, onSelect, onFilters }: Props) {
  const [query, setQuery] = useState(filters.query ?? '')
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const roots = useMemo(
    () => sortSiblings(nodes.filter(n => matches(n, filters) || hasMatchingDescendant(n, filters)), filters.sortOrder),
    [nodes, filters],
  )
  const recentNodes = useMemo(
    () => recentIds.map(id => findNode(nodes, id)).filter((n): n is OmtNode => n !== undefined),
    [recentIds, nodes],
  )
  const onToggle = (id: string) =>
    setCollapsed(prev => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  return (
    <div className="tree-panel">
      <div className="filters-bar">
        <input
          className="search-input"
          placeholder="Search id / title…"
          value={query}
          onChange={e => {
            setQuery(e.target.value)
            onFilters({ query: e.target.value })
          }}
        />
        <div className="chip-row">
          {Object.entries(TYPE_META).map(([type, meta]) => {
            const active = !filters.types?.length || filters.types.includes(type)
            return (
              <button
                key={type}
                className={`chip type-chip${active ? ' active' : ''}`}
                onClick={() => onFilters({ types: toggleInList(filters.types, type, Object.keys(TYPE_META)) })}
              >
                <span className="type-badge mini" style={{ background: meta.color }}>{meta.badge}</span>
              </button>
            )
          })}
          <span className="chip-sep" />
          {STATUS_ORDER.map(status => {
            const active = !filters.statuses?.length || filters.statuses.includes(status)
            return (
              <button
                key={status}
                className={`chip status-chip${active ? ' active' : ''}`}
                onClick={() => onFilters({ statuses: toggleInList(filters.statuses, status, STATUS_ORDER) })}
              >
                <span className="status-dot" style={{ background: STATUS_COLORS[status] }} />
                {STATUS_LABEL[status]}
              </button>
            )
          })}
          <span className="chip-sep" />
          {PRIORITIES.map(priority => {
            const active = !filters.priorities?.length || filters.priorities.includes(priority)
            return (
              <button
                key={priority}
                className={`chip prio-chip p${priority}${active ? ' active' : ''}`}
                onClick={() => onFilters({ priorities: toggleInList(filters.priorities, priority, PRIORITIES) })}
              >
                P{priority}
              </button>
            )
          })}
          <span className="chip-sep" />
          <button
            className={`chip${filters.showArchived ? ' active' : ''}`}
            onClick={() => onFilters({ showArchived: !filters.showArchived })}
          >
            🗃 Archived
          </button>
        </div>
        <div className="chip-row sort-row">
          <button
            className={`chip sort-anchor${filters.showId ? ' active' : ''}`}
            title="显示 ticket ID"
            onClick={() => onFilters({ showId: !filters.showId })}
          >
            # ID
          </button>
          <span className="chip-sep" />
          {SORTS.map(sort => (
            <button
              key={sort.key}
              className={`chip${(filters.sortOrder ?? 'none') === sort.key ? ' active' : ''}`}
              onClick={() => onFilters({ sortOrder: sort.key === 'none' ? undefined : sort.key })}
            >
              {sort.label}
            </button>
          ))}
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
          <NodeRow key={node.id} node={node} depth={0} filters={filters} selectedId={selectedId} collapsed={collapsed} onToggle={onToggle} onSelect={onSelect} />
        ))}
        {roots.length === 0 && <p className="empty-hint">无匹配 ticket</p>}
      </div>
    </div>
  )
}

function findNode(nodes: OmtNode[], id: string): OmtNode | undefined {
  for (const node of nodes) {
    if (node.id === id) return node
    const hit = findNode(childrenOf(node), id)
    if (hit) return hit
  }
  return undefined
}
