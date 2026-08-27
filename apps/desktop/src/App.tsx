/**
 * U10 app shell: tree (left) + detail/runs (center) + settings (toggle).
 * KD4 — fresh visual design; only domain concepts and status-color
 * semantics align with the DSH surface.
 */
import { useCallback, useState } from 'react'
import { WorkspaceSwitcher } from './WorkspaceSwitcher'
import { workspaceRootOf } from './homePath'
import { useTreeStore } from './store'
import { TreePanel } from './TreePanel'
import { DetailPanel } from './DetailPanel'
import { RunsPanel } from './RunsPanel'
import { SettingsPanel } from './SettingsPanel'
import './styles.css'

type CenterView = 'detail' | 'runs' | 'settings'

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
  const [treeWidth, setTreeWidth] = useState(initialTreeWidth)
  const [switcherOpen, setSwitcherOpen] = useState(false)

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
            onSelect={id => { store.selectNode(id); setView('detail') }}
            onFilters={patch => state.activeHome && void store.saveFilters(state.activeHome, patch)}
          />
          </div>
        )}
        <div className="splitter" onPointerDown={startDrag} role="separator" aria-orientation="vertical" />
        <section className="center-pane">
          {view === 'detail' && state.activeHome && state.selectedId && (
            <DetailPanel home={state.activeHome} nodeId={state.selectedId} onUpdated={store.updateNode} />
          )}
          {view === 'detail' && !state.selectedId && <p className="empty-hint">从左侧选择一个 ticket</p>}
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
    </main>
  )
}
