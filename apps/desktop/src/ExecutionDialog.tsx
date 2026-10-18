/**
 * U6 (R-B)：执行弹窗（全局 Modal，照 WorkspaceSwitcher overlay 模式）。
 * 选择器 = 已启用 harness ∪ 空闲 Agent（渲染但禁用——会话续接 v1 未接入，
 * U11）；默认 = bag v2 defaultHarness（读取时校验 enabled，失效回退首个
 * enabled 并提示）；「详情」展开只读预览（harness_launch_preview：binary/
 * args/env 键名 + unattendedFlag 说明）与将进入 run 的成员清单（前端 DFS，
 * 语义同 daemon collectRunMembers）；权限知情确认（§九-10：显式放行 +
 * per-workspace 记住 → bag v2 approvalAck）；同节点已有运行中派发 → 三选项
 * （查看运行中 Agent / 追加到该 Agent 当前 run / 取消）——「追加」按 daemon
 * run-status-gate（仅 Pending）+ 成员差集门控，不可用即禁用并说明原因。
 * 提交态机（§九-7）：派发中禁用、>5s 展开 ①–⑧ 静态降级进度、成功关弹窗
 * 回调、失败就地内联报错不关弹窗。
 *
 * U9（R-C）绑定预选：resolveBindingForNode 命中且 agent 存在时顶部提示
 * 「将派发至 <displayName> 的绑定（直接/继承自 <节点>）」；会话续接（U11）
 * 未接入，当前诚实行为 = harness 默认值优先取绑定 agent 的 harness 字段
 * （优先级：绑定 agent.harness > bag defaultHarness > 首个 enabled）。
 * 悬空绑定（agent 不在列表）与数据降级（parseFailed）不提示、不覆盖默认。
 */
import { useEffect, useMemo, useState } from 'react'
import { omtCall, harnessLaunchPreview, harnessListModels, bindingsGet, type ExecStartOutcome, type HarnessLaunchPreview, type HarnessModelEntry } from './bridge'
import { readHarnessBag, writeHarnessBag, harnessList, resolveBindingForNode, nodesById, nodeTitlesById, type HarnessEntry, type ResolvedBinding } from './store'
import { agentDisplayName } from './agentUi'
import type { AgentRecord, HomeInfo, OmtNode } from './types'

const SLOW_DISPATCH_MS = 5000 // 超时未返回 → 展开 ①–⑧ 步骤降级清单（§九-7）

/** 成员预览：目标节点子树 DFS，跳过 archived（不下钻）与 done/skipped，
 * 只收 ticket/subticket——语义对齐 daemon 侧 collectRunMembers（executor.rs）。 */
export function collectRunMembers(nodes: OmtNode[], rootId: string): string[] {
  const byId = nodesById(nodes)
  const members: string[] = []
  const visited = new Set<string>()
  const stack = [rootId]
  while (stack.length > 0) {
    const id = stack.pop()!
    if (visited.has(id)) continue
    visited.add(id)
    const node = byId.get(id)
    if (!node || node.archived) continue
    if ((node.type === 'ticket' || node.type === 'subticket') && node.status !== 'done' && node.status !== 'skipped') {
      members.push(id)
    }
    for (const child of [...(node.children ?? [])].reverse()) stack.push(child.id)
  }
  return members
}

interface Props {
  home: HomeInfo
  nodeId: string
  nodeTitle: string
  nodes: OmtNode[]
  agents: AgentRecord[]
  startDispatch: (options: {
    home: HomeInfo
    nodeId: string
    harness: string
    unattended: boolean
    model?: string | null
  }) => Promise<ExecStartOutcome>
  onClose: () => void
  /** 派发成功：App 关弹窗、切 Agents 内层 tab 并选中返回的 agentId */
  onDispatched: (outcome: ExecStartOutcome) => void
  /** 「查看运行中 Agent」/追加成功：同样切 Agents 并选中既有 agent */
  onSelectExistingAgent: (agentId: string) => void
  onOpenHarnessSettings: () => void
}

interface NodeRunsWire {
  runs?: Array<{ runId?: string; status?: string }>
}

interface RunDetailWire {
  run?: { status?: string }
  items?: Array<{ nodeId?: string }>
}

/** 同节点冲突的运行上下文：daemon `run_add_members` 仅接受 Pending run
 * （dispatch.rs run-status-gate）且拒绝已在 run 中的成员（DUPLICATE_MEMBER）
 * ——「追加」可用性必须按 status + 成员差集诚实呈现。 */
interface ConflictInfo {
  agent: AgentRecord
  runId: string
  /** run 状态原文；run/get 失败时为空串（按未知处理 → 追加禁用） */
  status: string
  existingIds: Set<string>
}

export function ExecutionDialog({
  home,
  nodeId,
  nodeTitle,
  nodes,
  agents,
  startDispatch,
  onClose,
  onDispatched,
  onSelectExistingAgent,
  onOpenHarnessSettings,
}: Props) {
  const [harnesses, setHarnesses] = useState<HarnessEntry[] | null>(null)
  const [selected, setSelected] = useState('')
  const [fallbackNotice, setFallbackNotice] = useState<string | null>(null)
  // U9：命中的绑定（直接/继承）与对应 agent（存在才提示/预选）
  const [bound, setBound] = useState<ResolvedBinding | null>(null)
  const [boundAgent, setBoundAgent] = useState<AgentRecord | null>(null)
  const [ackMap, setAckMap] = useState<Record<string, boolean>>({})
  const [confirmChecked, setConfirmChecked] = useState(false)
  const [rememberChecked, setRememberChecked] = useState(false)
  const [detailOpen, setDetailOpen] = useState(false)
  // 模型下拉（F-ACC-14）：selected harness 变化时拉取；失败置 null 降级隐藏
  const [models, setModels] = useState<HarnessModelEntry[] | null>(null)
  const [modelSel, setModelSel] = useState('')
  const [preview, setPreview] = useState<HarnessLaunchPreview | null>(null)
  const [previewError, setPreviewError] = useState<string | null>(null)
  const [conflict, setConflict] = useState<ConflictInfo | null>(null)
  const [appending, setAppending] = useState(false)
  const [submitting, setSubmitting] = useState(false)
  const [slow, setSlow] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const enabledHarnesses = useMemo(() => (harnesses ?? []).filter(h => h.enabled), [harnesses])
  const selectedEntry = enabledHarnesses.find(h => h.id === selected) ?? null
  const needsAck = selected !== '' && !ackMap[selected]
  const members = useMemo(() => collectRunMembers(nodes, nodeId), [nodes, nodeId])
  // 「追加」门控（daemon run-status-gate + DUPLICATE_MEMBER）：仅
  // pending 且存在未入 run 的新成员时可用；否则禁用并给出可读原因。
  const newMembers = useMemo(
    () => (conflict ? members.filter(id => !conflict.existingIds.has(id)) : []),
    [conflict, members],
  )
  const appendReason =
    conflict === null
      ? null
      : conflict.status !== 'pending'
        ? `daemon 仅允许向 Pending run 追加成员（该 run 当前为 ${conflict.status || '未知'}）`
        : newMembers.length === 0
          ? '成员已全部在 run 中'
          : null

  // 挂载：harness 投影 + bag v2（default/approvalAck）+ 活跃 run 检测
  // + 绑定表（U9；拉取失败/降级 → 无绑定预选，不阻断弹窗）
  useEffect(() => {
    let disposed = false
    void (async () => {
      try {
        const [list, bag, detail, bindings] = await Promise.all([
          harnessList(),
          readHarnessBag(home.homeId),
          omtCall<NodeRunsWire>('node/get', { homeId: home.homeId, nodeId }),
          bindingsGet(home.homeId).catch(() => null),
        ])
        if (disposed) return
        setHarnesses(list)
        setAckMap(bag.approvalAck ?? {})
        // 默认选中（U9 优先级）：绑定 agent.harness > bag defaultHarness >
        // 首个 enabled。绑定悬空（agent 不在列表）或数据降级（parseFailed）
        // 时按无绑定处理——不做悬空判定、不覆盖默认链路。
        const enabled = list.filter(h => h.enabled)
        if (enabled.length === 0) return
        let boundHit: ResolvedBinding | null = null
        let boundHitAgent: AgentRecord | null = null
        if (bindings && !bindings.parseFailed) {
          const resolved = resolveBindingForNode(bindings.bindings ?? {}, nodes, nodeId)
          const agent = resolved ? agents.find(a => a.id === resolved.agentId) ?? null : null
          if (resolved && agent) {
            boundHit = resolved
            boundHitAgent = agent
          }
        }
        const def = bag.defaultHarness
        const boundEntry = boundHitAgent ? enabled.find(h => h.id === boundHitAgent.harness) : undefined
        if (boundEntry && boundHitAgent) {
          setSelected(boundEntry.id)
          setBound(boundHit)
          setBoundAgent(boundHitAgent)
        } else if (def && enabled.some(h => h.id === def)) {
          setSelected(def)
        } else {
          setSelected(enabled[0].id)
          if (def) setFallbackNotice(`workspace 默认 harness ${def} 当前未启用，已回退到 ${enabled[0].label}`)
        }
        // 同节点已有运行中派发：node/get.runs × live agent 的 execs[].runId 交叉
        const runIds = new Set((detail.runs ?? []).map(r => r.runId).filter((id): id is string => typeof id === 'string'))
        const agent = agents.find(
          a =>
            (a.status === 'running' || a.status === 'starting') &&
            a.attachment === 'live' &&
            a.execs.some(e => typeof e.runId === 'string' && runIds.has(e.runId)),
        )
        if (agent) {
          const runId = agent.execs.find(e => typeof e.runId === 'string' && runIds.has(e.runId))!.runId!
          let status = ''
          const existingIds = new Set<string>()
          try {
            const runDetail = await omtCall<RunDetailWire>('run/get', { homeId: home.homeId, runId })
            status = runDetail.run?.status ?? ''
            for (const item of runDetail.items ?? []) {
              if (typeof item.nodeId === 'string') existingIds.add(item.nodeId)
            }
          } catch {
            // run/get 失败：按未知状态降级——「追加」禁用，不盲发会被
            // run-status-gate 拒绝的请求
          }
          setConflict({ agent, runId, status, existingIds })
        }
      } catch (err) {
        if (!disposed) setError(String(err))
      }
    })()
    return () => {
      disposed = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [home.homeId, nodeId])

  // 模型列表（selected 变化即拉；失败/超时降级为不显示下拉——不阻断派发）
  useEffect(() => {
    if (!selected) return
    let disposed = false
    setModels(null)
    setModelSel('')
    harnessListModels(selected)
      .then(list => {
        if (!disposed) setModels(list)
      })
      .catch(() => {
        if (!disposed) setModels(null)
      })
    return () => {
      disposed = true
    }
  }, [selected])

  // 详情展开（或选择变化）：只读启动预览（env 只回键名——secrets 纪律 §3.1）
  useEffect(() => {
    if (!detailOpen || !selected) return
    let disposed = false
    setPreview(null)
    setPreviewError(null)
    harnessLaunchPreview(selected)
      .then(p => {
        if (!disposed) setPreview(p)
      })
      .catch(err => {
        if (!disposed) setPreviewError(String(err))
      })
    return () => {
      disposed = true
    }
  }, [detailOpen, selected])

  // 标题查找一次建表（成员预览/绑定提示行 O(1) 查询）
  const titleById = useMemo(() => nodeTitlesById(nodes), [nodes])
  const memberTitle = (id: string) => titleById.get(id) ?? id

  const dispatch = async () => {
    if (!selected || submitting) return
    setSubmitting(true)
    setSlow(false)
    setError(null)
    const slowTimer = setTimeout(() => setSlow(true), SLOW_DISPATCH_MS)
    try {
      const outcome = await startDispatch({
        home,
        nodeId,
        harness: selected,
        unattended: true,
        model: modelSel || null,
      })
      clearTimeout(slowTimer)
      // 偏好记忆（bag v2，只动 v2 字段）：defaultHarness 随成功派发记为
      // 本次选择；勾「记住」时再写 approvalAck（另一 workspace 仍要求确认）
      void writeHarnessBag(home.homeId, {
        defaultHarness: selected,
        ...(rememberChecked ? { approvalAck: { [selected]: true } } : {}),
      })
      onDispatched(outcome)
      onClose()
    } catch (err) {
      clearTimeout(slowTimer)
      setSubmitting(false)
      setSlow(false)
      setError(String(err))
    }
  }

  const viewRunningAgent = () => {
    if (!conflict) return
    onSelectExistingAgent(conflict.agent.id)
    onClose()
  }

  const appendToRunningRun = async (nodeIds: string[]) => {
    if (!conflict || appending || nodeIds.length === 0) return
    setAppending(true)
    setError(null)
    try {
      // 只发差集：已在 run 中的成员重发会触发 DUPLICATE_MEMBER
      await omtCall('run/add-members', {
        homeId: home.homeId,
        runId: conflict.runId,
        nodeIds,
      })
      onSelectExistingAgent(conflict.agent.id)
      onClose()
    } catch (err) {
      setError(String(err))
      setAppending(false)
    }
  }

  const idleAgents = agents.filter(a => a.status !== 'running' && a.status !== 'starting')

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-panel" onClick={e => e.stopPropagation()}>
        <header className="modal-header">
          <h3>派发执行 — {nodeTitle || nodeId}</h3>
          <div className="modal-header-actions">
            <button className="chip" onClick={onClose}>✕</button>
          </div>
        </header>

        {error && <p className="error-banner">{error}</p>}
        {fallbackNotice && <p className="info-banner">{fallbackNotice}</p>}
        {bound && boundAgent && (
          // U9（R-C）：绑定预选提示——agent 目标派发延后 U11（会话续接未
          // 接入），当前诚实行为 = 默认 harness 取绑定 agent 的 harness。
          <p className="info-banner binding-hint">
            将派发至 <strong>{agentDisplayName(boundAgent)}</strong> 的绑定
            （{bound.source === 'direct' ? '直接' : `继承自 ${memberTitle(bound.viaNodeId)}`}）。
            会话续接（U11）接入后可直接派发至该 Agent。
          </p>
        )}

        {harnesses !== null && enabledHarnesses.length === 0 ? (
          // 零可用 → 引导前往设置（U7 落地 Harnesses tab 后即达管理面）
          <div className="dispatch-guidance">
            <p className="empty-hint">尚无已启用的 harness——先到 设置 → Harnesses 完成检测并启用。</p>
            <button
              className="chip active"
              onClick={() => {
                onOpenHarnessSettings()
                onClose()
              }}
            >
              配置 Harness
            </button>
          </div>
        ) : conflict ? (
          // 同节点已有运行中派发：三选项（§3.3 ①复用分支的用户呈现）
          <div className="conflict-box">
            <p>
              该节点已有运行中的派发——{agentDisplayName(conflict.agent)} 正在 run{' '}
              <span className="mono">{conflict.runId}</span> 上执行。
            </p>
            <div className="conflict-actions">
              <button className="chip active" onClick={viewRunningAgent}>查看运行中 Agent</button>
              <button
                className="chip"
                disabled={appending || appendReason !== null}
                title={appendReason ?? undefined}
                onClick={() => void appendToRunningRun(newMembers)}
              >
                {appending ? '追加中…' : '追加到该 Agent 当前 run'}
              </button>
              <button className="chip" onClick={onClose}>取消</button>
            </div>
            {appendReason && <p className="empty-hint conflict-reason">{appendReason}</p>}
          </div>
        ) : (
          <>
            <div className="dispatch-row">
              <select
                className="detail-select"
                value={selected}
                onChange={e => {
                  const value = e.target.value
                  if (!value.startsWith('agent:')) {
                    setSelected(value)
                    // 知情确认的对象是「这个 provider 将无人值守执行」——跨
                    // harness 切换必须重置（含「记住」勾选态），A 的放行不
                    // 隐含对 B 的放行（§九-10）。
                    setConfirmChecked(false)
                    setRememberChecked(false)
                  }
                }}
              >
                {enabledHarnesses.length > 0 && (
                  <optgroup label="Harness">
                    {enabledHarnesses.map(h => (
                      <option key={h.id} value={h.id}>
                        {h.label}
                        {h.state === 'pendingVerification' ? '（待验证）' : ''}
                      </option>
                    ))}
                  </optgroup>
                )}
                {idleAgents.length > 0 && (
                  // 空闲 Agent 渲染但禁用：会话续接 v1 未接入（U11）
                  <optgroup label="空闲 Agent">
                    {idleAgents.map(a => (
                      <option key={a.id} value={`agent:${a.id}`} disabled title="会话续接 v1 未接入（U11）">
                        {agentDisplayName(a)}
                      </option>
                    ))}
                  </optgroup>
                )}
              </select>
              <button className="chip" onClick={() => setDetailOpen(v => !v)}>{detailOpen ? '收起详情' : '详情'}</button>
            </div>

            {models !== null && models.length > 0 && (
              // 模型下拉（F-ACC-14）：默认不指定（用 harness 自身默认模型）；
              // 列表拉取失败时整个区块降级隐藏，不阻断派发
              <div className="dispatch-row">
                <select
                  className="detail-select"
                  aria-label="执行模型"
                  value={modelSel}
                  onChange={e => setModelSel(e.target.value)}
                >
                  <option value="">模型：默认（不指定）</option>
                  {Array.from(new Set(models.map(m => m.group).filter((g): g is string => g !== null))).map(group => (
                    <optgroup key={group} label={group}>
                      {models.filter(m => m.group === group).map(m => (
                        <option key={m.id} value={m.id}>{m.id}</option>
                      ))}
                    </optgroup>
                  ))}
                  {models.filter(m => m.group === null).map(m => (
                    <option key={m.id} value={m.id}>{m.id}</option>
                  ))}
                </select>
              </div>
            )}

            {detailOpen && (
              <div className="dispatch-detail">
                {previewError && <p className="error-banner">{previewError}</p>}
                {preview && (
                  <dl className="preview-grid">
                    <dt>binary</dt>
                    <dd className="mono">{preview.program}</dd>
                    <dt>args</dt>
                    <dd className="mono">{preview.args.length > 0 ? preview.args.join(' ') : '（无附加参数）'}</dd>
                    <dt>env 键</dt>
                    <dd className="mono">{preview.envKeys.length > 0 ? preview.envKeys.join(' ') : '（无）'}</dd>
                    <dt>无人值守</dt>
                    <dd>
                      确认派发将追加 <span className="mono">{preview.unattendedFlag.join(' ')}</span>，harness 将
                      无人值守执行文件/命令操作
                    </dd>
                  </dl>
                )}
                <p className="dispatch-members-label">{`将进入 run 的成员（${members.length}）`}</p>
                {members.length > 0 ? (
                  <div className="children-list">
                    {members.map(id => (
                      <span key={id} className="child-chip">
                        {id} {memberTitle(id)}
                      </span>
                    ))}
                  </div>
                ) : (
                  <p className="empty-hint">无未完成可执行成员——子树内 ticket/subticket 均已 done/skipped/archived。</p>
                )}
              </div>
            )}

            {selectedEntry && needsAck && (
              // 权限知情确认（§九-10）：显式放行 + per-workspace 记住（bag v2）
              <div className="ack-box">
                <label className="ack-line">
                  <input
                    type="checkbox"
                    checked={confirmChecked}
                    onChange={e => setConfirmChecked(e.target.checked)}
                  />
                  我已知悉 {selectedEntry.label} 将无人值守执行文件/命令操作
                </label>
                <label className="ack-line muted">
                  <input
                    type="checkbox"
                    checked={rememberChecked}
                    onChange={e => setRememberChecked(e.target.checked)}
                  />
                  本 workspace 记住（另一 workspace 仍会要求确认）
                </label>
              </div>
            )}

            <div className="dispatch-actions">
              <button className="chip" onClick={onClose} disabled={submitting}>取消</button>
              <button
                className="chip active"
                disabled={submitting || !selected || (needsAck && !confirmChecked)}
                title={needsAck && !confirmChecked ? '请先勾选无人值守知情确认' : undefined}
                onClick={() => void dispatch()}
              >
                {submitting ? '派发中…' : '派发'}
              </button>
            </div>

            {slow && (
              // >5s 未返回：①–⑧ 步骤静态降级展示（§3.3 派发链）
              <div className="dispatch-steps">
                <p>派发仍在进行（超过 5s）——链路步骤：</p>
                <ol>
                  <li>① 预检子树成员与活跃 run</li>
                  <li>② node/execute（in_progress + 审计）</li>
                  <li>③ ensure-run（复用或新建）</li>
                  <li>④ run/control start</li>
                  <li>⑤ Agent 记录落盘</li>
                  <li>⑥ 绑定写入</li>
                  <li>⑦ MCP 注入准备</li>
                  <li>⑧ spawn harness</li>
                </ol>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  )
}
