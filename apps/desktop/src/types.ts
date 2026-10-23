/**
 * U10 domain types + store: ticket tree state over the daemon JSON-RPC.
 * Types mirror the shared protocol shapes (schema/common.schema.json —
 * node_type/status enums); the desktop defines them locally per KD4
 * (fresh surface; no cross-surface imports).
 */

export type NodeType = 'epic' | 'story' | 'substory' | 'ticket' | 'subticket'
export type NodeStatus = 'open' | 'in_progress' | 'done' | 'blocked' | 'skipped' | 'archived'

export interface OmtNode {
  /** 本地模型键；线上 TreeNode 的对应字段叫 `nodeId`——store 在桥接
   *  边界归一化（见 store.refreshNodes 的 normalize）。 */
  id: string
  type: NodeType
  title: string
  status: NodeStatus
  priority: number
  parentId?: string
  revision?: number
  path?: string
  /** 线上契约：archived 是独立布尔（status 保持 open/done 等生命周期值，
   *  不存在 status:'archived'）——归档过滤与呈现必须以本字段为准。 */
  archived?: boolean
  /** node/tree 投影的子树（归一化后保留）。 */
  children?: OmtNode[]
}

export interface HomeInfo {
  homeId: string
  name?: string
  kind?: string
  path?: string
}

export interface SavedFilters {
  query?: string
  statuses?: string[]
  types?: string[]
  priorities?: number[]
  showArchived?: boolean
  showId?: boolean
  sortOrder?: string
}

export interface RunProgress {
  total: number
  pending: number
  running: number
  done: number
  failed: number
  blocked: number
  skipped: number
  interrupted: number
  awaitingConfirmation: number
}

export interface RunSummary {
  /** 本地归一化键；线上 RunView 用 `runId`（store 在桥接边界归一）。 */
  id: string
  title?: string
  status: string
  progress?: RunProgress
  stalledCount?: number
  createdAt?: string
  finishedAt?: string
  [k: string]: unknown
}

export interface RunItemView {
  nodeId: string
  /** 线上键名（不是 status）。 */
  state: string
  attempts: number
  executorActor?: string
  lastError?: string
  stalled?: boolean
  title?: string
  startedAt?: string
  finishedAt?: string
  [k: string]: unknown
}

export interface RunDetail {
  run: RunSummary
  items: RunItemView[]
}

export const STATUS_COLORS: Record<string, string> = {
  open: '#8b949e',
  in_progress: '#58a6ff',
  done: '#3fb950',
  blocked: '#f85149',
  skipped: '#d29922',
  archived: '#6e7681',
}

export const NEXT_TYPES: Record<NodeType, NodeType | null> = {
  epic: 'story',
  story: 'ticket',
  substory: 'ticket',
  ticket: 'subticket',
  subticket: null,
}

// ── U5 Agents（R-E；wire 契约 = agents_list 的 serde camelCase 投影）──────

export type AgentStatus = 'starting' | 'running' | 'exited' | 'failed' | 'killed'

/** 可控性正交字段（§3.2）：orphan = 重启后 pid 仍活（只读尾随、不可 stop） */
export type AgentAttachment = 'none' | 'live' | 'orphan'

export interface ExecRecord {
  execId: string
  nodeId: string | null
  runId: string | null
  /** epoch 毫秒字符串（字典序 = 时间序；展示格式化由前端负责） */
  startedAt: string
  exitCode: number | null
  killReason: string | null
  failureDetail: string | null
  harnessSessionId: string | null
  transcriptFile: string | null
  binaryPath: string | null
  binaryArgs: string[]
  /** F-U12-4：终态归档的会话累计用量（无 usage 事件为 null） */
  usageTokens?: number | null
  usageCost?: number | null
  /** F-BENCH-20：usage 归档口径。'message-sum' = 按消息/步求和、
   * 'cumulative-last' = 会话累计取末值（e87a99a 起）；null/缺省 = 旧口径
   * （只含末条消息，少报）——历史记录未重算，消费方据此区分。 */
  usageCaliber?: 'message-sum' | 'cumulative-last' | null
  /** 本次派发选用的模型（F-VIEW3：弹窗空闲 Agent 列表展示；未指定为 null） */
  model?: string | null
  /** Rust-captured process identity; never accepted from the renderer for kill authorization. */
  pid: number | null
  processStartId: string | null
  /** exec-shim 进程身份（Rust 侧写入，仅展示/诊断；停止授权不信任 renderer） */
  shimPid?: number | null
  shimStartId?: string | null
  /** shim 收尾时租约释放失败、待对账重试 */
  leaseReleasePending?: boolean
  /** A8 / Q-7：本次派发的发起方（desktop 派发 / CLI / MCP 主控）；旧记录为
   *  null/缺省。Agents 面板据此显示发起方标签（只标 cli / mcp）。 */
  launchedBy?: 'desktop' | 'cli' | 'mcp' | null
}

export interface AgentRecord {
  id: string
  displayName: string
  /** harness 返回前为 null——display 名回退 displayName（P0 即 Agent N） */
  title: string | null
  harness: string
  status: AgentStatus
  attachment: AgentAttachment
  lastExecId: string | null
  lastSessionId: string | null
  execs: ExecRecord[]
  /** A8 / Q-7：新建该 agent 的显式派发参数写入的父 agent id；旧记录缺省。 */
  parentAgentId?: string | null
}

export interface AgentsListResult {
  agents: AgentRecord[]
  agentSeq: number
  parseFailed: boolean
}

// ── U9 Bindings（R-C；wire 契约 = bindings_get / bindings_gc 的 json! 投影）──

/** bindings_get：显式绑定 nodeId → agentId。parseFailed 粘性语义同
 *  agents_list（F-U4a-1），且叠加 agents.json 降级态——悬空判定冻结依据。 */
export interface BindingsResult {
  bindings: Record<string, string>
  parseFailed: boolean
}

/** bindings_gc：移除 agentId 不存在于 agents.json 的绑定项。 */
export interface BindingsGcResult {
  removed: number
  bindings: Record<string, string>
}

// ── A8 派发策略（设置页「派发策略」；wire = exec_policy_get/set 投影）─────

/** A8：exec/policy.json（设计 §3.6）。只约束 CLI / MCP 发起的派发。 */
export interface ExecPolicy {
  allowFromCli: boolean
  allowFromMcp: boolean
  harnesses: string[]
  workspaces: 'homeWorkspaceOnly'
  maxConcurrent: number
  maxDurationSec: number
  approvalMode: 'inherit'
}

export interface ExecPolicyView {
  policy?: ExecPolicy
  /** ExecPolicy::default()（Rust 侧唯一来源） */
  defaults: ExecPolicy
  source?: 'file' | 'default'
  path: string
  error?: string
}
