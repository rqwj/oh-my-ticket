/**
 * U10 app shell: tree (left) + detail/runs (center) + settings (toggle).
 * KD4 — fresh visual design; only domain concepts and status-color
 * semantics align with the DSH surface.
 *
 * U5：Tickets 视图的 center-pane 内层 tab「详情 | Agents」（R-E）——选中
 * 树节点自动切详情；Agents 标签带未查看 Failed 计数 badge。
 */
import { useCallback, useState } from 'react'
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
    switchToAgent(outcome.agentId)
  }, [switchToAgent])

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
        {state.activeHome && (
          <div className="tree-wrap" style={{ width: treeWidth }}>
          <TreePanel
            nodes={state.nodes}
            filters={state.filters}
            recentIds={state.recentIds}
            selectedId={state.selectedId}
            onSelect={id => { store.selectNode(id); setView('detail'); setTicketTab('detail') }}
            onFilters={patch => state.activeHome && void store.saveFilters(state.activeHome, patch)}
          />
          </div>
        )}
        <div className="splitter" onPointerDown={startDrag} role="separator" aria-orientation="vertical" />
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
                    fetchRun={store.fetchRun}
                    eventsOf={store.transcriptWindow}
                    pidOf={store.sessionPidOf}
                    onSelectAgent={store.selectAgent}
                    onStopAgent={execId => void store.stopAgent(execId)}
                    onKillOrphan={pid => void store.killOrphan(pid)}
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
              harnessByHome={state.harnessByHome}
              onSaveHarness={store.saveHarness}
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
          harnessByHome={state.harnessByHome}
          onSaveHarness={store.saveHarness}
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
