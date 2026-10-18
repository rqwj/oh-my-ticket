/**
 * U10 app shell: tree (left) + detail/runs (center) + settings (toggle).
 * KD4 — fresh visual design; only domain concepts and status-color
 * semantics align with the DSH surface.
 *
 * U5：Tickets 视图的 center-pane 内层 tab「详情 | Agents」（R-E）——选中
 * 树节点自动切详情；Agents 标签带未查看 Failed 计数 badge。
 * U9：树 badge 的绑定数据（bindingsByHome/agents）与 Agents 面一键清理
 * 接线；派发成功后 refreshBindings（R-C）。
 * U8/R-D：树与 splitter 仅 Tickets 视图渲染（Runs/设置全宽）。
 */
import { useCallback, useEffect, useState } from 'react'
import { WorkspaceSwitcher } from './WorkspaceSwitcher'
import { workspaceRootOf } from './homePath'
import { useTreeStore } from './store'
import { TreePanel } from './TreePanel'
import { DetailPanel } from './DetailPanel'
import { RunsPanel } from './RunsPanel'
import { SettingsPanel } from './SettingsPanel'
import { AgentsPanel } from './AgentsPanel'
import { ExecutionDialog } from './ExecutionDialog'
import type { ExecStartOutcome } from './bridge'
import './styles.css'

type CenterView = 'detail' | 'runs' | 'settings'
type TicketTab = 'detail' | 'agents'

const TREE_WIDTH_KEY = 'omt-desktop-tree-width'
// 无绑定的稳定空表：每渲染新建 `{}` 会击穿下游 views memo（U7 评审修复）
const EMPTY_BINDINGS: Record<string, string> = {}
const clampWidth = (px: number) => Math.min(720, Math.max(240, Math.round(px)))

function initialTreeWidth(): number {
  const stored = Number(window.localStorage.getItem(TREE_WIDTH_KEY))
  return Number.isFinite(stored) && stored > 0 ? clampWidth(stored) : 380
}

export function App() {
  const store = useTreeStore()
  const { state } = store
  const [view, setView] = useState<CenterView>('detail')
  const [ticketTab, setTicketTab] = useState<TicketTab>('detail')
  const [treeWidth, setTreeWidth] = useState(initialTreeWidth)
  const [switcherOpen, setSwitcherOpen] = useState(false)
  // U6：执行弹窗全局门控（R-B）——DetailPanel 的运行按钮只开弹窗；outcome
  // warnings 作为 Agents 面持久警告条（下次派发或换 workspace 前不清除）。
  const [dispatchTarget, setDispatchTarget] = useState<{ nodeId: string; title: string } | null>(null)
  const [dispatchWarnings, setDispatchWarnings] = useState<string[]>([])

  const openDispatch = useCallback((title: string) => {
    if (!state.selectedId) return
    setDispatchWarnings([])
    setDispatchTarget({ nodeId: state.selectedId, title })
  }, [state.selectedId])

  const switchToAgent = useCallback((agentId: string) => {
    setDispatchTarget(null)
    setView('detail')
    setTicketTab('agents')
    store.selectAgent(agentId)
    void store.refreshAgents()
  }, [store])

  const handleDispatched = useCallback((outcome: ExecStartOutcome) => {
    setDispatchWarnings(outcome.warnings ?? [])
    // U9 pull 契约：派发成功后重拉绑定表（⑥ 写入已先于 exec_start 返回）
    void store.refreshBindings()
    switchToAgent(outcome.agentId)
  }, [switchToAgent, store])

  // 警告条不跨 workspace 存活（与上方 openDispatch 注释的承诺一致）：
  // 换 home 即清除；「下次派发前清除」由 openDispatch 负责。
  useEffect(() => {
    setDispatchWarnings([])
  }, [state.activeHome?.homeId])

  // Drag-to-resize the tree/center splitter: pointer captured on the
  // divider, width clamped 240–720px, persisted across launches.
  const startDrag = useCallback((down: React.PointerEvent) => {
    down.preventDefault()
    const startX = down.clientX
    const startWidth = treeWidth
    const onMove = (move: PointerEvent) => {
      setTreeWidth(clampWidth(startWidth + (move.clientX - startX)))
    }
    const onUp = (up: PointerEvent) => {
      const finalWidth = clampWidth(startWidth + (up.clientX - startX))
      window.localStorage.setItem(TREE_WIDTH_KEY, String(finalWidth))
      window.removeEventListener('pointermove', onMove)
      window.removeEventListener('pointerup', onUp)
    }
    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp)
  }, [treeWidth])

  if (state.loading) return <main className="app-shell"><p className="empty-hint">连接 daemon…</p></main>

  return (
    <main className="app-shell">
      <nav className="top-bar">
        <div className="tab-strip">
          <button className={`tab${view === 'detail' ? ' active' : ''}`} onClick={() => setView('detail')}>Tickets</button>
          <button className={`tab${view === 'runs' ? ' active' : ''}`} onClick={() => setView('runs')}>Runs</button>
          <button className={`tab${view === 'settings' ? ' active' : ''}`} onClick={() => setView('settings')}>设置</button>
        </div>
        <button className="home-name home-switch-btn" title="切换 workspace" onClick={() => setSwitcherOpen(true)}>
          {state.activeHome ? workspaceRootOf(state.activeHome.path, state.homeDir, state.activeHome.name ?? '') : '选择 workspace'}
        </button>
      </nav>
      {state.error && <p className="error-banner">{state.error}</p>}
      <div className="content">
        {/* U8/R-D：树与 splitter 仅 Tickets 视图渲染——Runs/设置自然全宽 */}
        {view === 'detail' && state.activeHome && (
          <div className="tree-wrap" style={{ width: treeWidth }}>
          <TreePanel
            nodes={state.nodes}
            filters={state.filters}
            recentIds={state.recentIds}
            selectedId={state.selectedId}
            bindings={state.bindingsByHome[state.activeHome.homeId] ?? EMPTY_BINDINGS}
            agents={state.agents}
            bindingsDegraded={state.bindingsParseFailed}
            onSelect={id => { store.selectNode(id); setView('detail'); setTicketTab('detail') }}
            onFilters={patch => state.activeHome && void store.saveFilters(state.activeHome, patch)}
            onCreateNode={store.createNode}
          />
          </div>
        )}
        {view === 'detail' && <div className="splitter" onPointerDown={startDrag} role="separator" aria-orientation="vertical" />}
        <section className={`center-pane${view === 'detail' && ticketTab === 'agents' ? ' agents-pane' : ''}`}>
          {view === 'detail' && (
            <>
              <div className="inner-tabs">
                <button
                  className={`inner-tab${ticketTab === 'detail' ? ' active' : ''}`}
                  onClick={() => setTicketTab('detail')}
                >
                  详情
                </button>
                <button
                  className={`inner-tab${ticketTab === 'agents' ? ' active' : ''}`}
                  onClick={() => { setTicketTab('agents'); void store.refreshAgents() }}
                >
                  Agents
                  {state.unviewedFailedIds.length > 0 && (
                    <span className="tab-badge">{state.unviewedFailedIds.length}</span>
                  )}
                </button>
              </div>
              {ticketTab === 'detail' && (
                <>
                  {state.activeHome && state.selectedId && (
                    <DetailPanel
                      home={state.activeHome}
                      nodeId={state.selectedId}
                      onUpdated={store.updateNode}
                      onSelect={store.selectNode}
                      onChanged={() => state.activeHome && void store.refreshNodes(state.activeHome)}
                      onDispatch={openDispatch}
                    />
                  )}
                  {!state.selectedId && <p className="empty-hint">从左侧选择一个 ticket</p>}
                </>
              )}
              {ticketTab === 'agents' && state.activeHome && (
                <>
                  {dispatchWarnings.length > 0 && (
                    <div className="warn-banner" role="alert">
                      {dispatchWarnings.map((w, i) => (
                        <span key={i}>{w}{i < dispatchWarnings.length - 1 ? '；' : ''}</span>
                      ))}
                    </div>
                  )}
                  <AgentsPanel
                    agents={state.agents}
                    parseFailed={state.parseFailed}
                    selectedAgentId={state.selectedAgentId}
                    nodes={state.nodes}
                    bindings={state.bindingsByHome[state.activeHome.homeId] ?? EMPTY_BINDINGS}
                    bindingsDegraded={state.bindingsParseFailed}
                    fetchRun={store.fetchRun}
                    eventsOf={store.transcriptWindow}
                    homeId={state.activeHome.homeId}
                    loadHistoryTail={store.loadHistoryTail}
                    onSelectAgent={store.selectAgent}
                    onStopAgent={execId => void store.stopAgent(execId)}
                    onResumeAgent={store.resumeAgent}
                    onKillOrphan={(agentId, execId) => void store.killOrphan(agentId, execId)}
                    onGcBindings={() => void store.gcBindings()}
                    onOpenHarnessSettings={() => setView('settings')}
                  />
                </>
              )}
            </>
          )}
          {view === 'runs' && state.activeHome && (
            <RunsPanel
              home={state.activeHome}
              runs={state.runs}
              fetchRun={store.fetchRun}
              onChanged={() => state.activeHome && void store.refreshNodes(state.activeHome)}
            />
          )}
          {view === 'settings' && (
            <SettingsPanel
              homes={state.homes}
              knownHomes={state.knownHomes}
              homeDir={state.homeDir}
              legacyDshByHome={state.legacyDshByHome}
              activeHome={state.activeHome}
              onSelectHome={home => { void store.selectHome(home); setView('detail') }}
              onDeclare={store.declareHome}
            />
          )}
        </section>
      </div>
      {switcherOpen && (
        <WorkspaceSwitcher
          homes={state.homes}
          knownHomes={state.knownHomes}
          activeHome={state.activeHome}
          homeDir={state.homeDir}
          onSelect={home => void store.selectHome(home)}
          onDeclare={store.declareHome}
          onClose={() => setSwitcherOpen(false)}
        />
      )}
      {dispatchTarget && state.activeHome && (
        <ExecutionDialog
          home={state.activeHome}
          nodeId={dispatchTarget.nodeId}
          nodeTitle={dispatchTarget.title}
          nodes={state.nodes}
          agents={state.agents}
          startDispatch={store.startDispatch}
          onClose={() => setDispatchTarget(null)}
          onDispatched={handleDispatched}
          onSelectExistingAgent={switchToAgent}
          onOpenHarnessSettings={() => setView('settings')}
        />
      )}
    </main>
  )
}
