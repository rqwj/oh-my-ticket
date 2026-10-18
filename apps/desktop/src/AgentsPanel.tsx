/**
 * U5 (R-E): Agents 面板 —— agent 列表（运行中 + 历史，跨重启保留）+
 * 选中 agent 的 TranscriptView。列表按 status 分组（Running/Starting 在
 * 前，Exited/Failed/Killed 次之）；Orphan 行带「不受控·只读」角标：只读
 * 尾随、不可 exec_stop，仅提供「强制 kill pid」（§3.2）。
 *
 * run item 统计走 run/get 单 run 拉取（§四：不挂 run/list 全量刷新），
 * 按 runId 缓存避免重复拉取；claimed 判定 =
 * executorActor.endsWith('/exec:<execId>')（嵌套命名空间，KD 定稿）。
 */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { TranscriptEvent } from './bridge'
import type { AgentRecord, ExecRecord, OmtNode, RunDetail, RunItemView } from './types'
import { claimedCountForExec, danglingBindingsOf, nodeTitlesById } from './store'
import { agentDisplayName, ConfirmStopButton } from './agentUi'
import { TranscriptView } from './TranscriptView'

/** 最新 exec：lastExecId 指针优先，缺失时回退 execs[] 末位。 */
function latestExecOf(agent: AgentRecord): ExecRecord | null {
  if (agent.execs.length === 0) return null
  return agent.execs.find(e => e.execId === agent.lastExecId) ?? agent.execs[agent.execs.length - 1]
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
  onSelectAgent: (id: string) => void
  onStopAgent: (execId: string) => void
  /** U10：空闲 agent 一键续跑（App → store.resumeAgent） */
  onResumeAgent: (agent: AgentRecord) => Promise<void>
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
  onSelectAgent,
  onStopAgent,
  onResumeAgent,
  onKillOrphan,
  onOpenHarnessSettings,
  onGcBindings,
}: Props) {
  const [stats, setStats] = useState<Record<string, RunStat>>({})
  /** U10：一键续跑失败的就地报错（成功无需提示——事件流自己会出现） */
  const [resumeError, setResumeError] = useState<string | null>(null)
  const statsRef = useRef<Record<string, RunStat>>({})
  // 当前节点标题一次建表（行渲染 O(1) 查询；免每行全树 DFS）
  const titleById = useMemo(() => nodeTitlesById(nodes), [nodes])

  // run item 统计：对列表内出现的 runId 做 run/get 单拉；runId 缓存跨
  // 重渲染复用（agents 重拉不重取）。
  useEffect(() => {
    let cancelled = false
    for (const agent of agents) {
      const exec = latestExecOf(agent)
      const runId = exec?.runId
      if (!runId || statsRef.current[runId]) continue
      void fetchRun(runId)
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
    }
    return () => {
      cancelled = true
    }
  }, [agents, fetchRun])

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
  // 选中态在 effect 内自算——hooks 须位于提前 return 之前。
  useEffect(() => {
    const selectedAgent = agents.find(a => a.id === selectedAgentId) ?? null
    const exec = selectedAgent ? latestExecOf(selectedAgent) : null
    if (!selectedAgent || !exec) return
    const live =
      selectedAgent.attachment === 'live' &&
      (selectedAgent.status === 'running' || selectedAgent.status === 'starting')
    if (live) return
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
        {gcBar}
        <button className="chip" onClick={onOpenHarnessSettings}>
          配置 Harness
        </button>
      </div>
    )
  }

  const groups = groupAgents(agents)
  const selected = agents.find(a => a.id === selectedAgentId) ?? null
  const selectedExec = selected ? latestExecOf(selected) : null
  const selectedStat = selectedExec?.runId ? stats[selectedExec.runId] : undefined
  const selectedClaimed =
    selectedExec && selectedStat ? claimedCountForExec(selectedStat.items, selectedExec.execId) : null

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
          <span className={`chip agent-status st-${agent.status}`}>{agent.status}</span>
          {agent.attachment === 'orphan' && (
            <span className="orphan-badge" title="重启后 pid 仍存活：只读尾随，不可受控终止">
              不受控·只读
            </span>
          )}
          <span className="row-spacer" />
          {stoppable && exec && (
            <ConfirmStopButton claimed={claimed} onConfirm={() => onStopAgent(exec.execId)} />
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
        {groups.active.length > 0 && <p className="agents-group-label">运行中</p>}
        {groups.active.map(renderRow)}
        {groups.orphan.length > 0 && <p className="agents-group-label">不受控（重启遗留）</p>}
        {groups.orphan.map(renderRow)}
        {groups.finished.length > 0 && <p className="agents-group-label">已结束</p>}
        {groups.finished.map(renderRow)}
      </div>
      <div className="agents-transcript">
        {resumeError && <p className="error-banner">{resumeError}</p>}
        {selected && selectedExec ? (
          <TranscriptView
            key={selectedExec.execId}
            agent={selected}
            events={eventsOf(selectedExec.execId)}
            claimed={selectedClaimed}
            onStopAgent={() => onStopAgent(selectedExec.execId)}
            onResumeAgent={
              // U10：空闲（非 running/starting）且最新 exec 带 ticket 才可续跑；
              // 结果就地报错（harness 未启用/未做知情确认/守卫拒绝等）
              selected.status !== 'running' &&
              selected.status !== 'starting' &&
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
      </div>
    </div>
  )
}
