/**
 * U5 (R-E): Agents 面板 —— agent 列表（运行中 + 历史，跨重启保留）+
 * 选中 agent 的 TranscriptView。列表按 status 分组（Running/Starting 在
 * 前，Exited/Failed/Killed 次之）；Orphan 行带「不受控·只读」角标：执行
 * 进程已丢失、harness 仍活，只能强制终止，不可 exec_stop，仅提供「强制
 * kill pid」（§3.2）。
 *
 * run item 统计走 run/get 单 run 拉取（§四：不挂 run/list 全量刷新），
 * 按 runId 缓存避免重复拉取；claimed 判定 =
 * executorActor.endsWith('/exec:<execId>')（嵌套命名空间，KD 定稿）。
 */
import { useEffect, useMemo, useRef, useState } from 'react'
import { omtCall, type StopAllOutcome, type TranscriptEvent } from './bridge'
import type { AgentRecord, ExecRecord, OmtNode, RunDetail, RunItemView } from './types'
import { claimedCountForExec, danglingBindingsOf, nodeTitlesById } from './store'
import {
  agentDisplayName,
  ConfirmStopAllButton,
  ConfirmStopButton,
  describeFailure,
} from './agentUi'
import { TranscriptView } from './TranscriptView'
import { DeleteIcon } from './icons'

/** 最新 exec：lastExecId 指针优先，缺失时回退 execs[] 末位。 */
function latestExecOf(agent: AgentRecord): ExecRecord | null {
  if (agent.execs.length === 0) return null
  return agent.execs.find(e => e.execId === agent.lastExecId) ?? agent.execs[agent.execs.length - 1]
}

/** A8：发起方标签——按来源分别说明（cli / mcp）；desktop 与旧记录不显示 */
export function originLabel(origin: 'cli' | 'mcp'): { text: string; title: string } {
  return origin === 'mcp'
    ? { text: 'MCP', title: '由 MCP（主控 agent）发起' }
    : { text: 'CLI', title: '由 CLI 发起' }
}

/**
 * A8 发起方小标签：只标 cli / mcp（desktop 派发是桌面自身的常规路径、旧
 * 记录无该字段——两者都不标）。标签反映的是**当前显示的那次 exec**：
 * 经 MCP 拉起、后来在桌面端续跑的 agent 标签会消失——如实反映「当前
 * 这次执行由谁发起」。
 */
function OriginChip({ origin }: { origin?: 'desktop' | 'cli' | 'mcp' | null }) {
  if (origin !== 'cli' && origin !== 'mcp') return null
  const { text, title } = originLabel(origin)
  return <span className="chip origin-chip" title={title} aria-label={title}>{text}</span>
}

/** 列表分组：活跃在前、Orphan（不受控）居中、终态殿后。 */
function groupAgents(agents: AgentRecord[]): {
  active: AgentRecord[]
  orphan: AgentRecord[]
  finished: AgentRecord[]
} {
  const active: AgentRecord[] = []
  const orphan: AgentRecord[] = []
  const finished: AgentRecord[] = []
  for (const agent of agents) {
    if (agent.attachment === 'orphan') orphan.push(agent)
    else if (agent.status === 'running' || agent.status === 'starting') active.push(agent)
    else finished.push(agent)
  }
  return { active, orphan, finished }
}

interface RunStat {
  done: number
  failed: number
  items: RunItemView[]
}

function statsOf(detail: RunDetail): RunStat {
  const items = detail.items ?? []
  return {
    done: items.filter(i => i.state === 'done').length,
    failed: items.filter(i => i.state === 'failed').length,
    items,
  }
}

interface Props {
  agents: AgentRecord[]
  parseFailed: boolean
  selectedAgentId: string | null
  nodes: OmtNode[]
  /** U9：显式绑定表 + 降级标志——悬空绑定计数与清理按钮的数据源 */
  bindings?: Record<string, string>
  bindingsDegraded?: boolean
  fetchRun: (runId: string) => Promise<RunDetail>
  eventsOf: (execId: string) => TranscriptEvent[]
  homeId: string
  /** F-PI-3：历史尾部回载（窗口空且非 live 时从 raw.log 拉取） */
  loadHistoryTail: (execId: string, homeId: string) => void
  /** P-10：选中仍在运行、但本会话无事件的 agent 时接回实时流（桌面重启后） */
  attachTranscript?: (execId: string, homeId: string) => void
  /** P-10：停止全部运行中 exec（替代桌面退出即终止）。返回 F-EXEC-12 的
   * `{ signaled, failed }`（部分失败供提示「M 个停止失败」）；失败为 null。 */
  onStopAll?: () => Promise<StopAllOutcome | null>
  onSelectAgent: (id: string) => void
  onStopAgent: (execId: string) => void
  /** U10：空闲 agent 一键续跑（App → store.resumeAgent） */
  onResumeAgent: (agent: AgentRecord) => Promise<void>
  /** F-DEL / F-EXEC-14(4)：删除 agent（确认弹窗在面板内；终态守卫在服务端）。
   * `force=true` = 常规删除被「待对账租约」守卫拒绝后的「放弃租约并删除」
   * （第二步确认提供该入口；force 只绕过租约守卫，其余守卫仍在服务端）。 */
  onDeleteAgent: (agent: AgentRecord, force?: boolean) => Promise<void>
  onKillOrphan: (agentId: string, execId: string) => void
  onOpenHarnessSettings: () => void
  /** U9：一键清理悬空绑定（bindings_gc，成功后 App/store 侧重拉绑定表） */
  onGcBindings?: () => void
}

export function AgentsPanel({
  agents,
  parseFailed,
  selectedAgentId,
  nodes,
  bindings = {},
  bindingsDegraded = false,
  fetchRun,
  eventsOf,
  homeId,
  loadHistoryTail,
  attachTranscript,
  onStopAll,
  onSelectAgent,
  onStopAgent,
  onResumeAgent,
  onDeleteAgent,
  onKillOrphan,
  onOpenHarnessSettings,
  onGcBindings,
}: Props) {
  const [stats, setStats] = useState<Record<string, RunStat>>({})
  /** U10：一键续跑失败的就地报错（成功无需提示——事件流自己会出现） */
  const [resumeError, setResumeError] = useState<string | null>(null)
  /** U14：信任门弹窗动作报错 */
  const [gateError, setGateError] = useState<string | null>(null)
  /** F-DEL：待确认删除的 agent（弹窗）；删除中/失败就地呈现 */
  const [deleteTarget, setDeleteTarget] = useState<AgentRecord | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)
  /** F-EXEC-14(4)：删除弹窗处于第二步（常规删除被待对账租约守卫拒绝后的
   * 「放弃租约并删除」确认）。
   * §45 P3-1：第二步是**不可逆**的两行警告——不做 3s 自动复位（旧实现
   * CONFIRM_RESET_MS 无操作还原第一步，既不适合不可逆操作，屏幕阅读器也
   * 不播报内容切换，违反 WCAG 2.2.1）；改为出现即聚焦确认按钮 +
   * aria-describedby 关联后果说明。CONFIRM_RESET_MS 仍用于停止全部/单个
   * 终止的 arming（agentUi 内部），那是可逆的防误点。 */
  const [forceArming, setForceArming] = useState(false)
  const forceConfirmRef = useRef<HTMLButtonElement | null>(null)
  // 进入第二步即聚焦确认按钮。`deleting` 也入 deps：进入第二步的首帧按钮
  // 可能仍是 disabled（deleting 在稍后的微任务里才复位——disabled 元素的
  // focus() 是 no-op），复位后补聚焦。
  useEffect(() => {
    if (forceArming && !deleting) forceConfirmRef.current?.focus()
  }, [forceArming, deleting])
  /** review F14（P-10）：停止全部的进行中状态与结果提示（5 秒后消失） */
  const [stoppingAll, setStoppingAll] = useState(false)
  const [stopAllNotice, setStopAllNotice] = useState<string | null>(null)
  useEffect(() => {
    if (!stopAllNotice) return
    const timer = setTimeout(() => setStopAllNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [stopAllNotice])
  // U14：全面板待裁决项（底部弹窗）——跨所有已拉取的 run 聚合。普通计算
  //（不得用 useMemo——须位于提前 return 之前且避免 hooks 位点抖动；stats 规模小）
  const awaitingItems: Array<{ runId: string; nodeId: string; title?: string }> = []
  for (const [runId, stat] of Object.entries(stats)) {
    for (const item of stat.items) {
      if (item.state === 'awaiting_confirmation') {
        awaitingItems.push({ runId, nodeId: item.nodeId, title: item.title })
      }
    }
  }

  /** U14 弹窗动作后重拉对应 run（statsRef 缓存会使旧值滞留） */
  const refreshStat = async (runId: string) => {
    try {
      const detail = await fetchRun(runId)
      statsRef.current[runId] = statsOf(detail)
      setStats(s => ({ ...s, [runId]: statsOf(detail) }))
    } catch {
      // 刷新失败保留旧态——下次挂载/焦点重拉
    }
  }

  /** U14 信任门裁决（底部弹窗）：确认 = admin report done */
  const confirmGateItem = async (runId: string, nodeId: string) => {
    setGateError(null)
    try {
      await omtCall('run/report', {
        homeId, runId, nodeId, outcome: 'done', note: '人工确认（信任门弹窗）',
      })
      await refreshStat(runId)
    } catch (err) {
      setGateError(String(err))
    }
  }

  /** U14 信任门裁决（底部弹窗）：打回 = ticket 重开（daemon 联动 interrupted） */
  const rejectGateItem = async (runId: string, nodeId: string) => {
    setGateError(null)
    try {
      await omtCall('node/update', { homeId, nodeId, changes: { status: 'open' } })
      await refreshStat(runId)
    } catch (err) {
      setGateError(String(err))
    }
  }

  // U14：信任门裁决弹窗（空列表/外部执行者场景也要可见——两分支共享元素）
  const gatePopup = awaitingItems.length > 0 && (
    <div className="trust-gate-popup" role="alertdialog" aria-label="待人工确认的完成">
      <p className="trust-gate-popup-title">
        ⚠ {awaitingItems.length} 项完成待人工确认（agent 绕过了 run 纪律的上报）
      </p>
      {gateError && <p className="error-banner">{gateError}</p>}
      {awaitingItems.map(item => (
        <div className="trust-gate-popup-row" key={`${item.runId}:${item.nodeId}`}>
          <span className="mono">{item.nodeId}</span>
          <span className="trust-gate-popup-title-text">{item.title ?? ''}</span>
          <button
            className="chip active"
            title="认可完成：item → done，ticket 保持 done"
            onClick={() => void confirmGateItem(item.runId, item.nodeId)}
          >
            确认
          </button>
          <button
            className="chip"
            title="打回：ticket 重开 → item 转 interrupted，可重新派发"
            onClick={() => void rejectGateItem(item.runId, item.nodeId)}
          >
            打回
          </button>
        </div>
      ))}
    </div>
  )
  const statsRef = useRef<Record<string, RunStat>>({})
  // 当前节点标题一次建表（行渲染 O(1) 查询；免每行全树 DFS）
  const titleById = useMemo(() => nodeTitlesById(nodes), [nodes])

  // run item 统计：对列表内出现的 runId 做 run/get 单拉；runId 缓存跨
  // 重渲染复用（agents 重拉不重取）。
  useEffect(() => {
    let cancelled = false
    const pull = (runId: string) =>
      fetchRun(runId)
        .then(detail => {
          // 缓存写入不受 cancelled 限制：ref 跨 effect 轮次存续，同 runId
          // 的已完成请求结果永远有效（仅渲染 setState 受过期保护）
          statsRef.current = { ...statsRef.current, [runId]: statsOf(detail) }
          if (cancelled) return
          setStats(statsRef.current)
        })
        .catch(() => {
          /* 统计是展示增强——失败留白 */
        })
    for (const agent of agents) {
      const exec = latestExecOf(agent)
      const runId = exec?.runId
      if (!runId || statsRef.current[runId]) continue
      void pull(runId)
    }
    // U14 兜底扫描（F-U14-5）：信任门可由外部执行者（CLI/其他 MCP 客户端）
    // 触发，agents 列表无对应记录——对 home 的 running run 再拉一遍详情
    //（信任门 item 只出现在 running run；失败静默）。
    void omtCall<{ runs?: Array<{ runId?: string; status: string }> }>('run/list', { homeId })
      .then(list => {
        if (cancelled) return
        // F-U14-6：daemon run/list 的键名是 `runId`（views.rs run_view 契约）——
        // 首版读 `run.id` 恒 undefined，测试 mock 同错自洽绿、真实数据静默失败
        for (const run of list.runs ?? []) {
          const runId = run.runId
          if (run.status !== 'running' || !runId || statsRef.current[runId]) continue
          void pull(runId)
        }
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [agents, fetchRun, homeId])

  // U9：悬空绑定（agentId 不在当前 agents 列表）。降级期间冻结悬空判定
  // （bindingsDegraded / parseFailed——降级空表会把全部绑定误判失效）。
  const dangling = bindingsDegraded || parseFailed ? [] : danglingBindingsOf(bindings, agents)
  const gcBar =
    onGcBindings && dangling.length > 0 ? (
      <div className="gc-bar">
        <span className="empty-hint">{dangling.length} 条绑定指向不存在的 Agent</span>
        <button className="chip" onClick={onGcBindings}>
          清理失效绑定 ({dangling.length})
        </button>
      </div>
    ) : null

  // F-PI-3：选中非 live 的 exec 且本会话无事件 → 从 raw.log 尾部回载一次
  //（跨重启的历史 agent；live exec 的事件由 Channel 实时推送，不走此路）。
  // P-10：live 且本会话无事件 = 重启后的存活 exec → 接回实时流（exec_attach）。
  // 选中态在 effect 内自算——hooks 须位于提前 return 之前。
  useEffect(() => {
    const selectedAgent = agents.find(a => a.id === selectedAgentId) ?? null
    const exec = selectedAgent ? latestExecOf(selectedAgent) : null
    if (!selectedAgent || !exec) return
    const live =
      selectedAgent.attachment === 'live' &&
      (selectedAgent.status === 'running' || selectedAgent.status === 'starting')
    if (live) {
      // P-10：本会话没有事件 = 重启后的存活 exec，接回实时流
      if (eventsOf(exec.execId).length === 0) attachTranscript?.(exec.execId, homeId)
      return
    }
    if (eventsOf(exec.execId).length > 0) return
    loadHistoryTail(exec.execId, homeId)
    // 每个 execId 只需触发一次；eventsOf 为尾窗快照读
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedAgentId, agents, homeId])

  if (agents.length === 0) {
    return (
      <div className="agents-empty">
        {parseFailed && (
          <p className="error-banner">agents.json 解析失败——已降级为空列表（损坏文件已留证 .corrupt-*）</p>
        )}
        <p className="empty-hint">尚无 Agent——在 ticket 详情点『运行』派发</p>
        {gatePopup}
        {gcBar}
        <button className="chip" onClick={onOpenHarnessSettings}>
          配置 Harness
        </button>
      </div>
    )
  }

  const groups = groupAgents(agents)
  // review F7（P-10）：确认框里显示的已认领项总数（拉不到 run 详情的 agent 不计入）
  const activeClaimed = groups.active.reduce<number | null>((sum, a) => {
    const exec = latestExecOf(a)
    const stat = exec?.runId ? stats[exec.runId] : undefined
    if (!exec || !stat) return sum
    return (sum ?? 0) + claimedCountForExec(stat.items, exec.execId)
  }, null)
  /** §43 P3：停止失败明细的截断呈现——最多列前 2 条，余者以「等 N 个」带过
   * （Rust 失败串形如 `agent <agentId>: <err>`，agent id 是用户定位线索）。 */
  const describeStopAllFailures = (failed: string[]): string =>
    failed.length <= 2 ? failed.join('；') : `${failed[0]}；${failed[1]}；等 ${failed.length} 个`
  const handleStopAll = async () => {
    if (!onStopAll) return
    setStoppingAll(true)
    try {
      const outcome = await onStopAll()
      const orphanNote =
        groups.orphan.length > 0 ? `；${groups.orphan.length} 个不受控 agent 需单独强制终止` : ''
      // review R2-7 / F-EXEC-12 / §43 P3：全失败、无事可做、成功、部分失败
      // 分开提示，且失败要带明细（此前只报计数，用户无从定位是哪个 agent）。
      // §43 P3：outcome 携带 signaled=0 && failed>0 时也按失败处理——不得
      // 只依赖 store 的 null 折算（面板对 outcome 原形状自卫）。
      setStopAllNotice(
        outcome === null
          ? '停止失败，请重试'
          : outcome.signaled === 0
            ? outcome.failed.length > 0
              ? `停止失败：${describeStopAllFailures(outcome.failed)}`
              : '当前没有运行中的 agent'
            : `已向 ${outcome.signaled} 个 agent 发出停止信号${
                outcome.failed.length > 0
                  ? `，${outcome.failed.length} 个停止失败：${describeStopAllFailures(outcome.failed)}`
                  : ''
              }${orphanNote}`,
      )
    } finally {
      setStoppingAll(false)
    }
  }
  const selected = agents.find(a => a.id === selectedAgentId) ?? null
  const selectedExec = selected ? latestExecOf(selected) : null
  const selectedStat = selectedExec?.runId ? stats[selectedExec.runId] : undefined
  const selectedClaimed =
    selectedExec && selectedStat ? claimedCountForExec(selectedStat.items, selectedExec.execId) : null
  // U14：选中 agent 的 run 中待信任门裁决的 item 数（流尾提示条）
  const selectedAwaiting = selectedStat
    ? selectedStat.items.filter(i => i.state === 'awaiting_confirmation').length
    : 0

  const renderRow = (agent: AgentRecord) => {
    const exec = latestExecOf(agent)
    const stat = exec?.runId ? stats[exec.runId] : undefined
    const claimed = exec && stat ? claimedCountForExec(stat.items, exec.execId) : null
    const nodeTitle = exec?.nodeId ? titleById.get(exec.nodeId) ?? null : null
    const stoppable =
      agent.attachment === 'live' && (agent.status === 'running' || agent.status === 'starting')
    const selectedClass = agent.id === selectedAgentId ? ' selected' : ''
    return (
      <div
        key={agent.id}
        className={`agent-row${selectedClass}`}
        role="button"
        tabIndex={0}
        onClick={() => onSelectAgent(agent.id)}
        onKeyDown={event => {
          if (event.key === 'Enter') onSelectAgent(agent.id)
        }}
      >
        <div className="agent-row-main">
          <span className="agent-name">{agentDisplayName(agent)}</span>
          {/* A8：当前显示的最新 exec 的发起方（cli / mcp 才显示） */}
          <OriginChip origin={exec?.launchedBy} />
          <span className={`chip agent-status st-${agent.status}`}>{agent.status}</span>
          {agent.attachment === 'orphan' && (
            <span
              className="orphan-badge"
              title="执行进程已丢失，harness 进程仍在运行：不会再有新输出，只能强制终止"
            >
              不受控·只读
            </span>
          )}
          <span className="row-spacer" />
          {stoppable && exec && (
            <ConfirmStopButton claimed={claimed} onConfirm={() => onStopAgent(exec.execId)} />
          )}
          {/* F-DEL：右上角小删除 icon（运行中/不受控隐藏——先处理存活态） */}
          {!stoppable && agent.attachment !== 'orphan' && (
            <button
              className="agent-delete-icon"
              title="删除该 agent"
              aria-label={`删除 ${agentDisplayName(agent)}`}
              onClick={event => {
                event.stopPropagation()
                setDeleteError(null)
                setForceArming(false)
                setDeleteTarget(agent)
              }}
            >
              <DeleteIcon />
            </button>
          )}
          {agent.attachment === 'orphan' && exec && (
            <button
              className="chip agent-kill-btn"
              onClick={event => {
                event.stopPropagation()
                onKillOrphan(agent.id, exec.execId)
              }}
            >
              强制终止
            </button>
          )}
        </div>
        <div className="agent-row-sub">
          <span className="mono">{agent.harness}</span>
          {nodeTitle && <span className="node-title">· {nodeTitle}</span>}
          {stat && <span className="run-progress">完成 {stat.done} / 失败 {stat.failed}</span>}
          {agent.status === 'failed' && exec?.failureDetail && (
            <span className="agent-failure" title={exec.failureDetail}>
              {/* F-EXEC-5 residue：orphan（不受控、pid 存活）不可续跑——失败
                  说明不得承诺「可续跑」，改指路强制终止（agentUi 按此分流） */}
              {describeFailure(exec.failureDetail, agent.attachment !== 'orphan')}
            </span>
          )}
          {agent.status === 'exited' && stat && claimed === 0 && (
            <span className="agent-unclaimed">未认领任何项</span>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className="agents-layout">
      <div className="agents-list">
        {parseFailed && (
          <p className="error-banner">agents.json 解析失败——已降级为空列表（损坏文件已留证 .corrupt-*）</p>
        )}
        {gcBar}
        {onStopAll && groups.active.length > 0 && (
          <ConfirmStopAllButton
            running={groups.active.length}
            claimed={activeClaimed}
            busy={stoppingAll}
            onConfirm={() => void handleStopAll()}
          />
        )}
        {stopAllNotice && <p className="agents-notice">{stopAllNotice}</p>}
        {groups.active.length > 0 && <p className="agents-group-label">运行中</p>}
        {groups.active.map(renderRow)}
        {groups.orphan.length > 0 && <p className="agents-group-label">不受控（执行进程已丢失）</p>}
        {groups.orphan.map(renderRow)}
        {groups.finished.length > 0 && <p className="agents-group-label">已结束</p>}
        {groups.finished.map(renderRow)}
      </div>
      <div className="agents-transcript">
        {resumeError && <p className="error-banner">{resumeError}</p>}
        {/* A8：transcript 头部——当前显示的那次 exec 的发起方（cli / mcp 才显示） */}
        {selected && selectedExec && <OriginChip origin={selectedExec.launchedBy} />}
        {selected && selectedExec ? (
          <TranscriptView
            key={selectedExec.execId}
            agent={selected}
            events={eventsOf(selectedExec.execId)}
            claimed={selectedClaimed}
            onStopAgent={() => onStopAgent(selectedExec.execId)}
            awaitingCount={selectedAwaiting}
            onDeleteAgent={
              selected.status !== 'running' &&
              selected.status !== 'starting' &&
              selected.attachment !== 'orphan'
                ? () => {
                    setDeleteError(null)
                    setForceArming(false)
                    setDeleteTarget(selected)
                  }
                : undefined
            }
            onResumeAgent={
              // U10：空闲（非 running/starting）且最新 exec 带 ticket 才可续跑；
              // F-EXEC-5：orphan（不受控、pid 存活）不可续跑——服务端守卫
              // 拒绝，UI 直接不给入口（改指路强制终止，见 TranscriptView）；
              // 结果就地报错（harness 未启用/未做知情确认/守卫拒绝等）
              selected.status !== 'running' &&
              selected.status !== 'starting' &&
              selected.attachment !== 'orphan' &&
              selectedExec.nodeId
                ? async () => {
                    setResumeError(null)
                    try {
                      await onResumeAgent(selected)
                    } catch (err) {
                      setResumeError(String(err))
                    }
                  }
                : undefined
            }
          />
        ) : (
          <TranscriptView agent={null} events={[]} />
        )}
        {gatePopup}
        {deleteTarget && (
          // F-DEL：删除确认弹窗——红色危险态，明确不可恢复与影响面。
          // F-EXEC-14(4)：常规删除被「待对账租约」守卫拒绝时进入第二步
          // 「放弃租约并删除」确认；其余错误照旧就地报错。
          // F-EXEC-16 ③（§45）：后果如实说明——服务端会先尽力释放一次，
          // 释放失败的租约被真正放弃（daemon 不回收租约），对应 run item
          // 可能一直显示运行中，需在 Runs 面板手动取消。
          // §45 P3-1：第二步不自动复位；确认按钮带展示名、聚焦 +
          // aria-describedby（见 force-delete-consequence）。
          <div className="trust-gate-popup delete-confirm" role="alertdialog" aria-label="确认删除 agent">
            {forceArming ? (
              <>
                <p className="trust-gate-popup-title">该 agent 有未释放的租约。放弃租约并删除？</p>
                <p className="hm-hint">
                  将删除该 agent 的记录、执行历史和 transcript 文件。此操作不可恢复。
                </p>
                <p className="hm-hint" id="force-delete-consequence">
                  强制删除前会先尝试释放租约；若释放失败，该 run item 可能一直显示运行中，需到
                  Runs 面板手动取消该 run。
                </p>
              </>
            ) : (
              <>
                <p className="trust-gate-popup-title">
                  删除 {agentDisplayName(deleteTarget)}（{deleteTarget.harness}）？
                </p>
                <p className="hm-hint">将删除该 agent 的记录、执行历史和 transcript 文件。此操作不可恢复。</p>
              </>
            )}
            {deleteError && <p className="error-banner">{deleteError}</p>}
            <div className="chip-row">
              <button
                className="chip active"
                ref={forceConfirmRef}
                aria-describedby={forceArming ? 'force-delete-consequence' : undefined}
                disabled={deleting}
                onClick={() => {
                  setDeleting(true)
                  // F-EXEC-14(4)：第二步（放弃租约）以 force=true 重发；第一步
                  // 保持 onDeleteAgent(agent) 的既有单参契约（store/bridge
                  // 侧缺省按 false 走常规守卫）
                  const attempt = forceArming
                    ? onDeleteAgent(deleteTarget, true)
                    : onDeleteAgent(deleteTarget)
                  attempt
                    .then(() => {
                      setDeleteTarget(null)
                      setForceArming(false)
                    })
                    .catch(err => {
                      if (String(err).includes('存在待对账的租约')) {
                        // F-EXEC-14(4)：租约守卫拒绝 → 第二步「放弃租约并删除」
                        //（守卫原因由第二步文案说明，不重复刷原始长错误）
                        setDeleteError(null)
                        setForceArming(true)
                      } else {
                        setDeleteError(String(err))
                      }
                    })
                    .finally(() => setDeleting(false))
                }}
              >
                {/* §45 P3-1：第二步按钮不再只写「确认」——动作 + agent 展示名 */}
                {deleting
                  ? '删除中…'
                  : forceArming
                    ? `放弃租约并删除 ${agentDisplayName(deleteTarget)}`
                    : '确认删除'}
              </button>
              <button
                className="chip"
                disabled={deleting}
                onClick={() => {
                  setDeleteTarget(null)
                  setForceArming(false)
                }}
              >
                取消
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
