/**
 * U1（嵌入式终端计划）app shell：最左侧工作区图标栏 + 内容列。ticket 树
 * 常驻内容列左侧，顶部显示当前工作区的显示名、路径与「⚙ 工作区设置」
 * （R1、R5）；中间区域标签为「详情 | Agents | Runs | 工作区」（R6、R8）。
 * 顶栏与右上角的切换弹窗已移除——全局设置从图标栏底部的 ⚙ 进入，占满整
 * 个内容区（R8）。KD4：仅领域概念与状态色语义与 DSH 表面对齐。
 *
 * U5：中间区域标签的 Agents 带（R-E）未查看 Failed 计数 badge——选中树节
 * 点自动切详情。U9：树 badge 的绑定数据（bindingsByHome/agents）与 Agents
 * 面一键清理接线；派发成功后 refreshBindings（R-C）。
 */
import { useCallback, useEffect, useState } from 'react'
import { WorkspaceRail } from './WorkspaceRail'
import { workspaceRootOf } from './homePath'
import { isWorkspaceMissing, workspaceNameOf } from './workspaceList'
import { useTreeStore } from './store'
import { TreePanel } from './TreePanel'
import { DetailPanel } from './DetailPanel'
import { RunsPanel } from './RunsPanel'
import { SettingsPanel } from './SettingsPanel'
import { AgentsPanel } from './AgentsPanel'
import { LeaseNoticeBanner } from './agentUi'
import { ExecutionDialog } from './ExecutionDialog'
import type { ExecStartOutcome } from './bridge'
import './styles.css'

type TicketTab = 'detail' | 'agents' | 'runs' | 'workspace'

const TREE_WIDTH_KEY = 'omt-desktop-tree-width'
// 无绑定的稳定空表：每渲染新建 `{}` 会击穿下游 views memo（U7 评审修复）
const EMPTY_BINDINGS: Record<string, string> = {}
const clampWidth = (px: number) => Math.min(720, Math.max(240, Math.round(px)))

function initialTreeWidth(): number {
  // F-TERM-2：localStorage 读取抛错（隐私模式/配额/损坏）时用默认宽度，
  // 页面照常渲染
  try {
    const stored = Number(window.localStorage.getItem(TREE_WIDTH_KEY))
    return Number.isFinite(stored) && stored > 0 ? clampWidth(stored) : 380
  } catch {
    return 380
  }
}

export function App() {
  const store = useTreeStore()
  const { state } = store
  const [ticketTab, setTicketTab] = useState<TicketTab>('detail')
  // U1/R8：全局设置由图标栏底部 ⚙ 进入，占满内容区（关闭回到原标签）
  const [settingsOpen, setSettingsOpen] = useState(false)
  // 添加弹窗由 App 托管：图标栏「＋」与空状态的「添加工作区」共用入口
  const [addOpen, setAddOpen] = useState(false)
  const [treeWidth, setTreeWidth] = useState(initialTreeWidth)
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
    setSettingsOpen(false)
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

  // §49 lease-notice-switch-home：通知跨 home 保留——「前往 Runs」先切到该条
  // 所属 home 再打开 Runs。§64 遗留：home 不在图标栏里时不直接选中（对齐
  // effect 会把它弹回首项）——store.openRunsOfHome 先把它收进列表；daemon
  // 未开该 home 时不动 home，仅切视图。
  const openRunsOfHome = useCallback(
    (homeId: string) => {
      store.openRunsOfHome(homeId)
      setSettingsOpen(false)
      setTicketTab('runs')
    },
    [store],
  )

  // R5：ticket 栏顶部的 ⚙ 打开当前工作区的设置标签页（内容在 U2 落地）
  const openWorkspaceSettings = useCallback(() => {
    setSettingsOpen(false)
    setTicketTab('workspace')
  }, [])

  const toggleGlobalSettings = useCallback(() => setSettingsOpen(open => !open), [])

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
      // F-TERM-2：持久化不可用时仅本次会话生效
      try {
        window.localStorage.setItem(TREE_WIDTH_KEY, String(finalWidth))
      } catch {
        /* localStorage 不可用：跳过持久化 */
      }
      window.removeEventListener('pointermove', onMove)
      window.removeEventListener('pointerup', onUp)
    }
    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp)
  }, [treeWidth])

  // KTD14：known-homes 的 missing 探测 → 图标置灰带警告。F-TERM-12：路径
  // 精确匹配优先、homeId 仅作写法不一致时的回退（见 isWorkspaceMissing）
  const isMissingWorkspace = useCallback(
    (ws: { path: string; homeId: string }) => isWorkspaceMissing(ws, state.knownHomes),
    [state.knownHomes],
  )
  const workspaceErrorText = Object.keys(state.workspaceErrors).length > 0
    ? Object.values(state.workspaceErrors).join('；')
    : null
  const activeWorkspaceName = state.activeHome ? workspaceNameOf(state.activeHome.path ?? '', state.homeDir) : null
  const activeWorkspaceRoot = state.activeHome
    ? workspaceRootOf(state.activeHome.path, state.homeDir, state.activeHome.name ?? state.activeHome.homeId)
    : null

  if (state.loading) return <main className="app-shell"><p className="empty-hint">连接 daemon…</p></main>

  return (
    <main className="app-shell">
      <WorkspaceRail
        workspaces={state.workspaces}
        knownHomes={state.knownHomes}
        homes={state.homes}
        activeHomeId={state.activeHome?.homeId ?? null}
        homeDir={state.homeDir}
        errors={state.workspaceErrors}
        isMissingWorkspace={isMissingWorkspace}
        settingsOpen={settingsOpen}
        addOpen={addOpen}
        onSelect={ws => {
          // §64 遗留：点击图标时关闭全局设置（回到内容区）
          setSettingsOpen(false)
          void store.selectWorkspace(ws)
        }}
        onAdd={store.addWorkspace}
        onAddOpen={() => setAddOpen(true)}
        onAddClose={() => setAddOpen(false)}
        onToggleSettings={toggleGlobalSettings}
      />
      <div className="app-main">
        <div className="notice-stack">
          {state.error && <p className="error-banner">{state.error}</p>}
          {/* §57 F-SP2-11 / CF2：**写**超时（结果未知、可能已生效）的粘性
              横幅——独立于 error（focus 的 agents/homes 刷新不清它），只有
              同 home、超时后发出的 refreshNodes 成功才清；可手动关闭
              （CF2：无活动 home 等无法自动清除的场景的出口） */}
          {state.timeoutNotice && (
            <div className="error-banner timeout-notice" role="alert">
              <span className="workspace-error-text">{state.timeoutNotice.text}</span>
              <button className="banner-close" aria-label="关闭超时提示" onClick={store.dismissTimeoutNotice}>✕</button>
            </div>
          )}
          {/* CF3：事件泵连续失败——外部 CLI 改动不会自动刷新；恢复后消失 */}
          {state.eventsStatus && (
            <div className="warn-banner events-status-banner" role="status">
              外部改动不会自动刷新：事件订阅连接失败
              {state.eventsStatus.error ? `（${state.eventsStatus.error}）` : ''}，正在自动重试…
            </div>
          )}
          {/* U1：工作区图标的错误态（懒声明失败 / 路径失效）逐条列出，可关闭 */}
          {workspaceErrorText && (
            <div className="error-banner workspace-error-banner" role="alert">
              <span className="workspace-error-text">{workspaceErrorText}</span>
              <button className="banner-close" aria-label="关闭工作区提示" onClick={store.clearWorkspaceErrors}>✕</button>
            </div>
          )}
        </div>
        <LeaseNoticeBanner
          notices={state.leaseNotices}
          onDismiss={store.dismissLeaseNotice}
          onOpenRuns={openRunsOfHome}
        />
        {settingsOpen ? (
          // U1/R8：全局设置占满整个内容区（Daemon、DSH 集成、Harnesses、派发策略）
          <div className="content">
            <section className="center-pane">
              <SettingsPanel homes={state.homes} legacyDshByHome={state.legacyDshByHome} activeHome={state.activeHome} />
            </section>
          </div>
        ) : (
          <div className="content">
            <div className="tree-wrap" style={{ width: treeWidth }}>
              {state.activeHome ? (
                <>
                  {/* R5：ticket 栏顶部——当前工作区的显示名、路径与设置入口 */}
                  <header className="workspace-header">
                    <div className="workspace-title">
                      <span className="workspace-name">{activeWorkspaceName}</span>
                      <span className="mono workspace-path">{activeWorkspaceRoot}</span>
                    </div>
                    <button className="icon-btn" title="工作区设置" onClick={openWorkspaceSettings}>⚙</button>
                  </header>
                  <TreePanel
                    nodes={state.nodes}
                    filters={state.filters}
                    recentIds={state.recentIds}
                    selectedId={state.selectedId}
                    bindings={state.bindingsByHome[state.activeHome.homeId] ?? EMPTY_BINDINGS}
                    agents={state.agents}
                    bindingsDegraded={state.bindingsParseFailed}
                    onSelect={id => { store.selectNode(id); setTicketTab('detail') }}
                    onFilters={patch => state.activeHome && void store.saveFilters(state.activeHome, patch)}
                    onCreateNode={store.createNode}
                  />
                </>
              ) : (
                <div className="workspace-empty">
                  {/* P3（§64）：图标栏有工作区时只提示去点图标，「还没有打开
                      的工作区」的空态（含添加按钮）只在图标栏也为空时显示 */}
                  {state.workspaces.length > 0 ? (
                    <p className="empty-hint">从左侧图标栏选择一个工作区</p>
                  ) : (
                    <>
                      <p className="empty-hint">还没有打开的工作区</p>
                      <button className="chip active" onClick={() => setAddOpen(true)}>＋ 添加工作区</button>
                    </>
                  )}
                </div>
              )}
            </div>
            <div className="splitter" onPointerDown={startDrag} role="separator" aria-orientation="vertical" />
            <section className={`center-pane${ticketTab === 'agents' ? ' agents-pane' : ''}`}>
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
                <button
                  className={`inner-tab${ticketTab === 'runs' ? ' active' : ''}`}
                  onClick={() => setTicketTab('runs')}
                >
                  Runs
                </button>
                <button
                  className={`inner-tab${ticketTab === 'workspace' ? ' active' : ''}`}
                  onClick={openWorkspaceSettings}
                >
                  工作区
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
                    legacyUnmigrated={state.legacyUnmigrated}
                    corruptAgents={state.corruptAgents}
                    onArchiveLegacy={store.archiveLegacyAgents}
                    selectedAgentId={state.selectedAgentId}
                    nodes={state.nodes}
                    bindings={state.bindingsByHome[state.activeHome.homeId] ?? EMPTY_BINDINGS}
                    bindingsDegraded={state.bindingsParseFailed}
                    fetchRun={store.fetchRun}
                    eventsOf={store.transcriptWindow}
                    homeId={state.activeHome.homeId}
                    loadHistoryTail={store.loadHistoryTail}
                    attachTranscript={store.attachTranscript}
                    onStopAll={store.stopAll}
                    onSelectAgent={store.selectAgent}
                    onStopAgent={execId => void store.stopAgent(execId)}
                    onResumeAgent={store.resumeAgent}
                    onDeleteAgent={async (agent, force) => { await store.deleteAgent(agent.id, force) }}
                    onKillOrphan={(agentId, execId) => void store.killOrphan(agentId, execId)}
                    onGcBindings={() => void store.gcBindings()}
                    onOpenHarnessSettings={() => setSettingsOpen(true)}
                  />
                </>
              )}
              {ticketTab === 'runs' && state.activeHome && (
                <RunsPanel
                  home={state.activeHome}
                  runs={state.runs}
                  fetchRun={store.fetchRun}
                  onChanged={() => state.activeHome && void store.refreshNodes(state.activeHome)}
                />
              )}
              {ticketTab === 'workspace' && (
                // R6：工作区设置标签页与详情/Agents/Runs 并列；内容（路径、
                // 重建索引、Finder 显示、移除）在 U2 落地
                <div className="workspace-settings-page">
                  <h2>工作区设置</h2>
                  {state.activeHome ? (
                    <p className="empty-hint">路径、重建索引、在 Finder 中显示与从列表移除等设置项将在后续版本提供。</p>
                  ) : (
                    <p className="empty-hint">当前没有打开的工作区。</p>
                  )}
                </div>
              )}
            </section>
          </div>
        )}
      </div>
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
          onOpenHarnessSettings={() => setSettingsOpen(true)}
        />
      )}
    </main>
  )
}
