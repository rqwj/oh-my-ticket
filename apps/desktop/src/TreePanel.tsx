/**
 * U10 tree panel v2 — 视觉对齐 DSH 插件设计系统：类型徽章（E/S/SS/T/ST
 * 彩色方块）、可折叠树（chevron）、完整过滤器行（类型/状态/优先级/
 * Archived 胶囊）与排序行（# ID / No sorting / Priority desc/asc）。
 * 数据为递归 node/tree 投影；过滤排序客户端执行，filter bag 持久化在
 * `tauri:ui` 键（KD3）。
 *
 * U9（R-C）：NodeRow 绑定 badge——直接绑定高亮、继承灰显（title 标注
 * 祖先节点）、悬空「绑定已失效」；archived 节点跳过渲染（不清理，
 * restore 后回归）；数据降级（bindingsDegraded）时不做悬空判定。
 */
import { useEffect, useMemo, useState } from 'react'
import type { AgentRecord, OmtNode, SavedFilters } from './types'
import { STATUS_COLORS } from './types'
import { bindingViewsForNodes, nodesById, type NodeBindingView } from './store'
import { agentDisplayName } from './agentUi'

/** 层级规则（daemon 契约镜像）：根=epic；epic→story；story/substory→
 * substory|ticket（ticket 只能挂 story 系下，不能直接挂 epic）；ticket/
 * subticket→subticket。纯函数供测试钉住。 */
export function allowedChildTypes(parentType: string | null): string[] {
  switch (parentType) {
    case null:
      return ['epic']
    case 'epic':
      return ['story']
    case 'story':
    case 'substory':
      return ['substory', 'ticket']
    case 'ticket':
    case 'subticket':
      return ['subticket']
    default:
      return []
  }
}

interface Props {
  nodes: OmtNode[]
  filters: SavedFilters
  recentIds: string[]
  selectedId: string | null
  /** U9：显式绑定表（nodeId → agentId；App 从 store.bindingsByHome 取当前 home） */
  bindings?: Record<string, string>
  agents?: AgentRecord[]
  /** U9：绑定数据降级（bindings_get parseFailed）——悬空判定冻结 */
  bindingsDegraded?: boolean
  onSelect: (id: string) => void
  onFilters: (patch: SavedFilters) => void
  /** 新建子节点（返回新节点 id；store 侧负责刷新树并选中新节点） */
  onCreateNode: (input: { type: string; title: string; body?: string; parentId: string | null }) => Promise<string>
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
  if (!filters.showArchived && node.archived) return false
  return true
}

function sortSiblings(nodes: OmtNode[], sortOrder?: string): OmtNode[] {
  if (sortOrder === 'priority-desc') return [...nodes].sort((a, b) => b.priority - a.priority)
  if (sortOrder === 'priority-asc') return [...nodes].sort((a, b) => a.priority - b.priority)
  return nodes
}

function BindingBadge({ view }: { view: NodeBindingView }) {
  if (view.state === 'dangling') {
    return (
      <span className="binding-badge dangling" title={`绑定指向的 Agent 已不存在（${view.agentId}）`}>
        绑定已失效
      </span>
    )
  }
  const name = agentDisplayName(view.agent)
  return (
    <span
      className={`binding-badge ${view.state}`}
      title={
        view.state === 'direct'
          ? `派发绑定：${name}`
          : `继承自 ${view.viaNodeId} 的绑定：${name}`
      }
    >
      ⭦ {name}
    </span>
  )
}

function NodeRow({ node, depth, filters, selectedId, collapsed, views, onToggle, onSelect }: {
  node: OmtNode
  depth: number
  filters: SavedFilters
  selectedId: string | null
  collapsed: Set<string>
  /** U9：nodeId → 绑定 badge 视图（TreePanel 预解析） */
  views: Map<string, NodeBindingView>
  onToggle: (id: string) => void
  onSelect: (id: string) => void
}) {
  const children = childrenOf(node)
  const visible = matches(node, filters) || children.some(c => matches(c, filters) || hasMatchingDescendant(c, filters))
  if (!visible) return null
  const meta = TYPE_META[node.type] ?? { badge: '?', color: '#8b949e' }
  const isCollapsed = collapsed.has(node.id)
  const binding = views.get(node.id) ?? null
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
        <span
          className="status-dot"
          title={node.archived ? 'archived' : node.status}
          style={{ background: node.archived ? '#6e7681' : (STATUS_COLORS[node.status] ?? '#8b949e') }}
        />
        {filters.showId && <span className="node-id">{node.id}</span>}
        <span className={`node-title${node.archived ? ' archived' : ''}`}>{node.title}</span>
        {/* U9：绑定 badge（type-badge 与 prio-tag 之间）；archived 跳过渲染
            ——绑定不清理，restore 后回归（§U9 Approach） */}
        {binding && !node.archived && <BindingBadge view={binding} />}
        {node.priority > 0 && <span className={`prio-tag p${node.priority}`}>P{node.priority}</span>}
      </div>
      {!isCollapsed &&
        sortSiblings(children, filters.sortOrder).map(child => (
          <NodeRow key={child.id} node={child} depth={depth + 1} filters={filters} selectedId={selectedId} collapsed={collapsed} views={views} onToggle={onToggle} onSelect={onSelect} />
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

export function TreePanel({ nodes, filters, recentIds, selectedId, bindings = {}, agents = [], bindingsDegraded = false, onSelect, onFilters, onCreateNode }: Props) {
  const [query, setQuery] = useState(filters.query ?? '')
  // 新建子节点表单（U14 验收诉求）：父节点 = 当前选中（无选中 = 根 → epic）
  const [createOpen, setCreateOpen] = useState(false)
  const [createType, setCreateType] = useState('')
  const [createTitle, setCreateTitle] = useState('')
  const [createBody, setCreateBody] = useState('')
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)
  // 深索引查找：树是嵌套结构（story 在 epic 的 children 里），顶层 find
  // 永远找不到子节点（F-U14-3——曾致选中 story 按钮仍显示「新建 Epic」）
  const selectedNode = useMemo(
    () => (selectedId ? nodesById(nodes).get(selectedId) ?? null : null),
    [nodes, selectedId],
  )
  const allowedTypes = useMemo(() => allowedChildTypes(selectedNode?.type ?? null), [selectedNode?.type])
  // Persisted bag loads asynchronously after mount — adopt its query.
  useEffect(() => setQuery(filters.query ?? ''), [filters.query])
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  // U9：全树绑定 badge 视图一次预解析（直接/继承/悬空三态，降级冻结悬空判定）；
  // 批量入口共享一次 byId/agents 建表，避免每节点全树重建的 O(N²)
  const views = useMemo(() => {
    const resolved = bindingViewsForNodes(bindings, nodes, agents, bindingsDegraded)
    const map = new Map<string, NodeBindingView>()
    for (const [id, view] of resolved) {
      if (view) map.set(id, view)
    }
    return map
  }, [nodes, bindings, agents, bindingsDegraded])
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
        <div className="chip-row">
          <button
            className="chip"
            title={selectedNode ? `在 ${selectedNode.id} 下新建子节点` : '新建顶层 Epic'}
            onClick={() => {
              setCreateOpen(v => !v)
              setCreateType(allowedTypes[0] ?? '')
              setCreateError(null)
            }}
          >
            ＋ 新建{selectedNode ? `（${selectedNode.type} 下）` : ' Epic'}
          </button>
        </div>
        {createOpen && (
          <div className="create-node-form">
            <p className="hm-hint">
              父节点：{selectedNode ? `${selectedNode.id} ${selectedNode.title}` : '（根）'}
            </p>
            <div className="chip-row">
              {allowedTypes.map(t => (
                <button
                  key={t}
                  className={`chip${createType === t ? ' active' : ''}`}
                  onClick={() => setCreateType(t)}
                >
                  {t}
                </button>
              ))}
              {allowedTypes.length === 0 && <span className="hm-hint">该节点类型不支持子节点</span>}
            </div>
            <input
              className="hm-input"
              aria-label="节点标题"
              placeholder="标题（必填）"
              value={createTitle}
              onChange={e => setCreateTitle(e.target.value)}
            />
            <textarea
              className="hm-textarea"
              rows={3}
              aria-label="节点正文（可选）"
              placeholder="正文（可选）——如 ticket 的任务说明"
              value={createBody}
              onChange={e => setCreateBody(e.target.value)}
            />
            {createError && <p className="error-banner">{createError}</p>}
            <div className="chip-row">
              <button
                className="chip active"
                disabled={creating || !createType || createTitle.trim() === ''}
                onClick={() => {
                  setCreating(true)
                  setCreateError(null)
                  onCreateNode({
                    type: createType,
                    title: createTitle.trim(),
                    body: createBody.trim() === '' ? undefined : createBody,
                    parentId: selectedNode?.id ?? null,
                  })
                    .then(() => {
                      setCreateOpen(false)
                      setCreateTitle('')
                      setCreateBody('')
                    })
                    .catch(err => setCreateError(String(err)))
                    .finally(() => setCreating(false))
                }}
              >
                {creating ? '创建中…' : '创建'}
              </button>
              <button className="chip" onClick={() => setCreateOpen(false)}>取消</button>
            </div>
          </div>
        )}
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
          <NodeRow key={node.id} node={node} depth={0} filters={filters} selectedId={selectedId} collapsed={collapsed} views={views} onToggle={onToggle} onSelect={onSelect} />
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
