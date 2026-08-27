/**
 * U10 app shell: tree (left) + detail/runs (center) + settings (toggle).
 * KD4 — fresh visual design; only domain concepts and status-color
 * semantics align with the DSH surface.
 */
import { useState } from 'react'
import { useTreeStore } from './store'
import { TreePanel } from './TreePanel'
import { DetailPanel } from './DetailPanel'
import { RunsPanel } from './RunsPanel'
import { SettingsPanel } from './SettingsPanel'
import './styles.css'

type CenterView = 'detail' | 'runs' | 'settings'

export function App() {
  const store = useTreeStore()
  const { state } = store
  const [view, setView] = useState<CenterView>('detail')

  if (state.loading) return <main className="app-shell"><p className="empty-hint">连接 daemon…</p></main>

  return (
    <main className="app-shell">
      <nav className="top-bar">
        <div className="tab-strip">
          <button className={`tab${view === 'detail' ? ' active' : ''}`} onClick={() => setView('detail')}>Tickets</button>
          <button className={`tab${view === 'runs' ? ' active' : ''}`} onClick={() => setView('runs')}>Runs</button>
          <button className={`tab${view === 'settings' ? ' active' : ''}`} onClick={() => setView('settings')}>设置</button>
        </div>
        <span className="home-name">{state.activeHome?.name ?? state.activeHome?.path ?? ''}</span>
      </nav>
      {state.error && <p className="error-banner">{state.error}</p>}
      <div className="content">
        {state.activeHome && (
          <TreePanel
            nodes={state.nodes}
            filters={state.filters}
            recentIds={state.recentIds}
            selectedId={state.selectedId}
            onSelect={id => { store.selectNode(id); setView('detail') }}
            onFilters={patch => state.activeHome && void store.saveFilters(state.activeHome, patch)}
          />
        )}
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
              activeHome={state.activeHome}
              onSelectHome={home => { void store.selectHome(home); setView('detail') }}
              onDeclare={store.declareHome}
            />
          )}
        </section>
      </div>
    </main>
  )
}
