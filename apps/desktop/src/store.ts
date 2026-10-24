/**
 * U10 store: ticket tree + selection + runs + settings state, fed by the
 * bridge. Pull-based freshness contract (plan U10 Approach): the home
 * list re-pulls on window focus (no cross-surface push signal in v1);
 * per-home ticket data streams through events/resume subscriptions.
 *
 * U5 增量：executions/agents 切片——agents_list 走 pull（tab 首开 /
 * window focus / 派发后主动拉，不进 daemon 事件流）；transcript 经
 * Channel 推送进 ingestTranscriptEvent 归并（每 execId 独立尾窗、raw
 * 50ms 合并刷新、2000 条有界）。
 *
 * U6 增量：harness 数据面（harness_list/harness_config_set 封装 +
 * HarnessEntry）、bag v2 helpers（defaultHarness/approvalAck，只动 v2
 * 字段）、startDispatch（per-dispatch Channel → exec_start，成功后防抖
 * 拉 agents_list）——执行弹窗的数据源（R-B）。
 *
 * U1b 增量：bag v2 迁移挂在 home 加载路径（refreshHomes/selectHome 每次
 * 拉 home 后顺带执行；legacy 探测与 v2 投影同一次读，不加 RPC）——旧
 * opencode 条目写 defaultHarness 并移除旧 `harness` 键；旧 dsh 条目不写
 * 不删（不静默丢弃），置 legacyDshByHome 提示态。旧 HarnessSelect 路径
 * （harnessByHome/loadHarness/saveHarness）退役。
 *
 * U9 增量：bindings 切片——bindingsByHome（bindings_get pull，契约对齐
 * agents）+ 子树继承解析纯函数（resolveBindingForNode / bindingViewsForNodes
 * / danglingBindingsOf，R-C）+ gcBindings（一键清理，Rust 侧降级冻结）。
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification'
import { Channel, invoke } from '@tauri-apps/api/core'
import {
  agentsArchiveLegacy,
  agentsList,
  bindingsGet,
  bindingsGc,
  execAttach,
  execKillOrphan,
  execStart,
  execStop,
  execStopAll,
  execTranscriptTail,
  agentsDelete,
  omtCall,
  subscribeEvents,
  type AgentsDeleteResult,
  type EventEnvelope,
  type ExecStartOutcome,
  type StopAllOutcome,
  type TranscriptEvent,
} from './bridge'
import type {
  AgentRecord,
  CorruptAgent,
  HomeInfo,
  LeaseNotice,
  LegacyUnmigrated,
  OmtNode,
  RunDetail,
  RunSummary,
  SavedFilters,
} from './types'

const FILTERS_KEY = 'tauri:ui' // KD3: surface-prefixed bag key
const LAST_HOME_KEY = 'omt-desktop-last-home' // 启动恢复：最后选择的 workspace homeId
const RECENT_KEY = 'recent' // KD3's single shared cross-surface key (R4)

// ── U5 agents/transcript 常量 ─────────────────────────────────────────────
const TRANSCRIPT_TAIL_CAP = 2000 // 每 exec 尾窗上限：丢最旧（内存上界 = agent 数 × cap）
const RAW_FLUSH_MS = 50 // raw 行合并刷新窗口（避免每行一渲染）
const EXIT_REPULL_MS = 400 // exit 后 agents_list 防抖重拉（等 Rust 侧落盘收敛）
// A8 / Q-10：窗口可见时的 agents_list 静默轮询周期（外部 CLI / MCP 发起的
// 执行由此在桌面端无需切换焦点即可见）
const AGENTS_POLL_MS = 5_000
// §51 attach-retry：exec_attach 瞬时失败后的有界重试——§49 让 silent 轮询
// 结果结构未变时跳过 setState（agents 引用稳定），失败后 AgentsPanel 键在
// [agents] 上的接回 effect 不再每 5s 重跑，运行中 agent 的实时 transcript
// 会冻结。失败后按轮询周期重试，上限 3 次（仍在列表且未 attached 才重试）。
const ATTACH_RETRY_MS = 5_000
const ATTACH_RETRY_MAX = 3

const EMPTY_TRANSCRIPT: TranscriptEvent[] = []

// ── U6 (R-B/F-U1b-1)：harness 数据面（harness_list wire 投影，U7/U1b 复用）──

/** harness_list 单条投影（Rust harness_entry 的 json! 键位契约）。 */
export interface HarnessEntry {
  id: string
  label: string
  installed: boolean
  detectedPath: string | null
  detectedVersion: string | null
  enabled: boolean
  binarySource: 'detected' | 'manual'
  manualPath: string | null
  state: 'ready' | 'notDetected' | 'pendingVerification'
  /** 无人值守放行 flag（弹窗确认文案与预览说明用） */
  unattendedFlag: string[]
  /**
   * U7 起投影携带的已存用户配置（回显编辑用，v1 env 值为字面量）；
   * 旧 fixture/降级缺省按空处理。
   */
  extraArgs?: string[]
  env?: Record<string, string>
}

/** harness_list：类型表 × 探测 × 用户配置投影。 */
export async function harnessList(): Promise<HarnessEntry[]> {
  const result = await invoke<{ harnesses?: HarnessEntry[] }>('harness_list')
  return result.harnesses ?? []
}

/** harness_config_set 的入参形状（Rust HarnessUserConfig 的 serde camelCase）。 */
export interface HarnessUserConfigInput {
  enabled: boolean
  binary: { kind: 'detected' } | { kind: 'manual'; path: string }
  extraArgs: string[]
  env: Record<string, string>
}

// ── U9（R-C）绑定解析纯函数：显式绑定只写派发节点，读侧沿祖先链取最近 ───────

/** resolveBindingForNode 的解析结果（badge/弹窗预选共用的中间形态）。 */
export type ResolvedBinding =
  | { agentId: string; source: 'direct' }
  | { agentId: string; source: 'inherited'; viaNodeId: string }

/** 全树扁平化为 byId 表（绑定解析 / 标题查找共用；O(N) 一次构建）。 */
export function nodesById(nodes: OmtNode[]): Map<string, OmtNode> {
  const byId = new Map<string, OmtNode>()
  const walk = (list: OmtNode[]) => {
    for (const node of list) {
      byId.set(node.id, node)
      if (node.children) walk(node.children)
    }
  }
  walk(nodes)
  return byId
}

/** 全树 nodeId → title 一次建表（AgentsPanel 行渲染 / ExecutionDialog 成员预览共用）。 */
export function nodeTitlesById(nodes: OmtNode[]): Map<string, string> {
  const titles = new Map<string, string>()
  for (const node of nodesById(nodes).values()) titles.set(node.id, node.title)
  return titles
}

/**
 * 子树继承解析（§U9 Approach）：沿祖先链（parentId 上溯）取最近绑定——
 * 自身命中为 direct，祖先命中为 inherited（viaNodeId = 提供绑定的最近
 * 祖先）。绑定不因 archived 失效：archived 祖先的绑定仍参与解析（badge
 * 只是跳过渲染，restore 后回归）。
 */
export function resolveBindingForNode(
  bindings: Record<string, string>,
  nodes: OmtNode[],
  nodeId: string,
): ResolvedBinding | null {
  return resolveBindingWith(nodesById(nodes), bindings, nodeId)
}

/** resolveBindingForNode 的共享内核（byId 表由调用方构建——树面批量解析免 O(N²)）。 */
function resolveBindingWith(
  byId: Map<string, OmtNode>,
  bindings: Record<string, string>,
  nodeId: string,
): ResolvedBinding | null {
  let current = byId.get(nodeId)
  while (current) {
    const agentId = bindings[current.id]
    if (agentId) {
      return current.id === nodeId
        ? { agentId, source: 'direct' }
        : { agentId, source: 'inherited', viaNodeId: current.id }
    }
    current = current.parentId ? byId.get(current.parentId) : undefined
  }
  return null
}

/** badge 渲染视图：解析结果 × agents 现况（TreePanel NodeRow 消费）。 */
export type NodeBindingView =
  | { state: 'direct'; agentId: string; agent: AgentRecord }
  | { state: 'inherited'; agentId: string; agent: AgentRecord; viaNodeId: string }
  | { state: 'dangling'; agentId: string; agent: null }

/**
 * 整树 badge 视图批量解析（TreePanel views memo 消费）：byId 与 agents 表
 * 各建一次，逐节点沿祖先链解析——单节点语义与 resolveBindingForNode 一致
 * （agents 表 first-wins，与 `Array.find` 一致），但免去每节点全树重建的
 * O(N²)。悬空判定：agentId 不在当前 agents 列表 → 失效；`degraded`
 * （parseFailed）时冻结悬空判定并返回 null（数据降级——降级空表会把全部
 * 绑定误判失效，§3.6.6 的读侧对称纪律）。
 */
export function bindingViewsForNodes(
  bindings: Record<string, string>,
  nodes: OmtNode[],
  agents: AgentRecord[],
  degraded: boolean,
): Map<string, NodeBindingView | null> {
  const byId = nodesById(nodes)
  const agentById = new Map<string, AgentRecord>()
  for (const a of agents) {
    if (!agentById.has(a.id)) agentById.set(a.id, a)
  }
  const views = new Map<string, NodeBindingView | null>()
  for (const node of byId.values()) {
    const resolved = resolveBindingWith(byId, bindings, node.id)
    if (!resolved) {
      views.set(node.id, null)
      continue
    }
    const agent = agentById.get(resolved.agentId) ?? null
    if (!agent) {
      views.set(node.id, degraded ? null : { state: 'dangling', agentId: resolved.agentId, agent: null })
      continue
    }
    views.set(
      node.id,
      resolved.source === 'direct'
        ? { state: 'direct', agentId: resolved.agentId, agent }
        : { state: 'inherited', agentId: resolved.agentId, agent, viaNodeId: resolved.viaNodeId },
    )
  }
  return views
}

/** 悬空绑定清单（Agents 面一键清理的计数与存在性依据；降级期间不消费）。 */
export function danglingBindingsOf(
  bindings: Record<string, string>,
  agents: AgentRecord[],
): Array<{ nodeId: string; agentId: string }> {
  const ids = new Set(agents.map(a => a.id))
  return Object.entries(bindings)
    .filter(([, agentId]) => !ids.has(agentId))
    .map(([nodeId, agentId]) => ({ nodeId, agentId }))
}

/** 保存一个 provider 的用户配置（校验+原子落盘），返回最新投影。U7 复用。 */
export async function harnessConfigSet(harness: string, config: HarnessUserConfigInput): Promise<HarnessEntry> {
  return invoke<HarnessEntry>('harness_config_set', { harness, config })
}

// ── U6 bag v2（§四定稿：key 仍 `tauri:harness`，跟项目走的偏好）────────────

const HARNESS_BAG_KEY = 'tauri:harness'

/**
 * bag schema v2：`{defaultHarness: <providerId>, approvalAck: {<providerId>:
 * true}}`。旧 schema（`{harness: HarnessConfig}`）不同构——写入 read-merge-write
 * 只动 v2 字段；旧数据迁移归 migrateHarnessBag（home 加载路径顺带执行）。
 */
export interface HarnessBagV2 {
  defaultHarness?: string
  approvalAck?: Record<string, boolean>
  /** per-provider 上次派发选用的模型（F-DEFAULT-MODEL；值 '' = 清除记忆） */
  models?: Record<string, string>
}

interface RawHarnessBag {
  defaultHarness?: unknown
  approvalAck?: unknown
  models?: unknown
  harness?: unknown
  [k: string]: unknown
}

/** readHarnessBag 的返回：v2 字段投影 + 旧 schema 探测（迁移与提示态复用）。 */
export interface HarnessBagRead extends HarnessBagV2 {
  /** 旧 `harness.type` 命中即如实报告；无旧字段/读失败 → null。 */
  legacy: 'opencode' | 'dsh' | null
}

/** 读 bag 原始形态：读失败 → 空 bag（投影层兜底，不抛错）。 */
async function readRawHarnessBag(homeId: string): Promise<RawHarnessBag> {
  try {
    const result = await omtCall<{ filters?: RawHarnessBag }>('ui/filters-get', {
      homeId,
      key: HARNESS_BAG_KEY,
    })
    return result.filters ?? {}
  } catch {
    return {}
  }
}

/** raw bag → v2 字段投影 + legacy 探测：纯读，不改写 bag。 */
function projectHarnessBag(bag: RawHarnessBag): HarnessBagRead {
  const read: HarnessBagRead = { legacy: null }
  if (typeof bag.defaultHarness === 'string' && bag.defaultHarness) {
    read.defaultHarness = bag.defaultHarness
  }
  if (bag.models && typeof bag.models === 'object' && !Array.isArray(bag.models)) {
    const models: Record<string, string> = {}
    for (const [k, v] of Object.entries(bag.models as Record<string, unknown>)) {
      if (typeof v === 'string' && v) models[k] = v
    }
    if (Object.keys(models).length > 0) read.models = models
  }
  if (bag.approvalAck && typeof bag.approvalAck === 'object' && !Array.isArray(bag.approvalAck)) {
    const ack: Record<string, boolean> = {}
    for (const [provider, flag] of Object.entries(bag.approvalAck as Record<string, unknown>)) {
      if (flag === true) ack[provider] = true
    }
    read.approvalAck = ack
  }
  const legacyType = (bag.harness as { type?: unknown } | undefined)?.type
  if (legacyType === 'opencode' || legacyType === 'dsh') read.legacy = legacyType
  return read
}

/** 读 bag 并投影 v2 字段 + legacy 探测：旧 shape/缺失/读失败 → 视为无默认（不抛错）。 */
export async function readHarnessBag(homeId: string): Promise<HarnessBagRead> {
  return projectHarnessBag(await readRawHarnessBag(homeId))
}

/** 写 v2 字段：read-merge-write，只动传入的 v2 字段（旧 `harness` 键原样保留）。 */
export async function writeHarnessBag(homeId: string, patch: HarnessBagV2): Promise<void> {
  const current = await readRawHarnessBag(homeId)
  const next: RawHarnessBag = { ...current }
  if (patch.defaultHarness !== undefined) next.defaultHarness = patch.defaultHarness
  if (patch.approvalAck !== undefined) {
    const existing =
      current.approvalAck && typeof current.approvalAck === 'object' && !Array.isArray(current.approvalAck)
        ? (current.approvalAck as Record<string, unknown>)
        : {}
    next.approvalAck = { ...existing, ...patch.approvalAck }
  }
  if (patch.models !== undefined) {
    const existing =
      current.models && typeof current.models === 'object' && !Array.isArray(current.models)
        ? (current.models as Record<string, unknown>)
        : {}
    const merged: Record<string, string> = {}
    for (const [k, v] of Object.entries(existing)) {
      if (typeof v === 'string' && v) merged[k] = v
    }
    for (const [provider, model] of Object.entries(patch.models)) {
      if (model === '') delete merged[provider]
      else merged[provider] = model
    }
    if (Object.keys(merged).length > 0) next.models = merged
    else delete next.models
  }
  await omtCall('ui/filters-set', { homeId, key: HARNESS_BAG_KEY, filters: next }).catch(() => {})
}

export interface KnownHome {
  path: string
  name: string
  kind: string
  open: boolean
  missing: boolean
}

export interface TreeState {
  homes: HomeInfo[]
  knownHomes: KnownHome[]
  homeDir?: string
  /** U1b：旧 dsh harness 条目的 per-home 提示态（迁移不静默丢弃，设置页提示条） */
  legacyDshByHome: Record<string, boolean>
  activeHome: HomeInfo | null
  nodes: OmtNode[]
  selectedId: string | null
  filters: SavedFilters
  recentIds: string[]
  runs: RunSummary[]
  /** U5：当前 home 的 agent 列表（运行中 + 历史，跨重启保留） */
  agents: AgentRecord[]
  /** agents.json 解析失败（前端提示降级；U9 据此冻结 GC） */
  parseFailed: boolean
  /** §41：旧版存储未迁移的说明（横幅 + 「确认并归档」） */
  legacyUnmigrated: LegacyUnmigrated | null
  /** §41：agent.json 已损坏的 agent（目录 + 留证文件名） */
  corruptAgents: CorruptAgent[]
  /** §47：强制删除时被放弃的租约通知（应用级，逐条关闭；换 home 不清） */
  leaseNotices: LeaseNotice[]
  selectedAgentId: string | null
  /** 本 home 未查看的 Failed agent id（tab badge；选中即清除） */
  unviewedFailedIds: string[]
  /** U9：各 home 的显式绑定表（nodeId → agentId；读侧做继承解析） */
  bindingsByHome: Record<string, Record<string, string>>
  /** 当前 home 绑定数据降级（bindings_get parseFailed，粘性）——悬空判定冻结 */
  bindingsParseFailed: boolean
  /** 尾窗渲染信号：一次 raw 合并 flush 递增 1（高频事件不逐条渲染） */
  transcriptVersion: number
  loading: boolean
  error: string | null
}

const INITIAL: TreeState = {
  homes: [],
  knownHomes: [],
  legacyDshByHome: {},
  activeHome: null,
  nodes: [],
  selectedId: null,
  filters: {},
  recentIds: [],
  runs: [],
  agents: [],
  parseFailed: false,
  legacyUnmigrated: null,
  corruptAgents: [],
  leaseNotices: [],
  selectedAgentId: null,
  unviewedFailedIds: [],
  bindingsByHome: {},
  bindingsParseFailed: false,
  transcriptVersion: 0,
  loading: true,
  error: null,
}

/** QUOTA_EXCEEDED(rule=open-homes) must name the limit and the remedy. */
export function presentError(error: unknown): string {
  const text = String(error)
  if (text.includes('QUOTA_EXCEEDED') && text.includes('open-homes')) {
    return `同时打开的 home 数量已达 daemon 配额上限。解法：调大 daemon.json 的 limits.max_open_homes，或重启 daemon 释放空闲 home。(${text})`
  }
  if (text.includes('revision') || text.includes('CONFLICT')) {
    return `该 ticket 已被其他端修改（revision 冲突）— 请刷新后重试。(${text})`
  }
  if (text.includes('HOME_LOCKED')) {
    return `该 home 被 ts-bridge 占用，需显式 takeover（omt takeover）。(${text})`
  }
  return text
}

/**
 * exec 的 claimed 判定（KD 定稿）：daemon 对 kind=mcp 的显式 actor 做基
 * 命名空间嵌套，实际授予形如 `mcp:<pid>/exec:<execId>`，MCP 进程重启会换
 * pid——必须 endsWith，不能精确相等。裸 `exec:<execId>`（无命名空间）不算
 * desktop exec 的持有项。
 */
export function claimedCountForExec(
  items: ReadonlyArray<{ executorActor?: string }>,
  execId: string,
): number {
  const suffix = `/exec:${execId}`
  return items.filter(i => typeof i.executorActor === 'string' && i.executorActor.endsWith(suffix)).length
}

/**
 * §47 release-results-hidden：被放弃租约的通知文案。Rust 侧明细形如
 * `exec <execId>: <err>`（force_release_agent_leases）；run id 从被删 agent
 * 的 execs 反查；形状不符的明细原样列出。agent 记录已不在列表时退回 id。
 */
export function leaseNoticeOf(
  agent: AgentRecord | undefined,
  agentId: string,
  homeId: string,
  failures: string[],
): LeaseNotice {
  const runIds: string[] = []
  const parts = failures.map(failure => {
    const execId = /^exec (\S+): /.exec(failure)?.[1]
    if (!execId) return failure
    const runId = agent?.execs.find(e => e.execId === execId)?.runId
    if (runId && !runIds.includes(runId)) runIds.push(runId)
    return runId ? `${execId}（${runId}）` : execId
  })
  const name = agent ? (agent.title ?? agent.displayName) : agentId
  return {
    agentId,
    homeId,
    runIds,
    text: `已删除 ${name}，但 ${failures.length} 个租约释放失败、已被放弃：${parts.join('；')}。对应 run item 可能一直显示运行中，请到 Runs 页手动取消该 run。`,
  }
}

/** 尾窗入窗（有界：丢最旧；与选中无关，全部活跃 exec 都保）。 */
function appendTail(
  windows: Map<string, TranscriptEvent[]>,
  execId: string,
  events: TranscriptEvent[],
): void {
  const tail = windows.get(execId) ?? []
  for (const ev of events) tail.push(ev)
  if (tail.length > TRANSCRIPT_TAIL_CAP) tail.splice(0, tail.length - TRANSCRIPT_TAIL_CAP)
  windows.set(execId, tail)
}

export function useTreeStore() {
  const [state, setState] = useState<TreeState>(INITIAL)
  const disposers = useRef<Array<() => void>>([])
  // Synchronous mirror of state.filters: saveFilters must compute the
  // merged bag WITHOUT relying on React's async setState updater.
  const filtersRef = useRef<SavedFilters>({})
  // Same stale-closure guard for activeHome: imperative actions
  // (updateNode/fetchRun) must read the LATEST home, not the render
  // frame their closure captured.
  const activeHomeRef = useRef<HomeInfo | null>(null)
  // U16：agents 镜像——通知等异步回调读最新列表（避免闭包过期）
  const agentsRef = useRef<AgentRecord[]>([])
  // Pulls may resolve out of order while the user switches homes (or when a
  // focus/dispatch refresh overlaps another refresh). Only the newest request
  // for the still-active home may update current-home state.
  const agentsRequestSeqRef = useRef(0)
  const bindingsRequestSeqRef = useRef(0)
  useEffect(() => {
    filtersRef.current = state.filters
    activeHomeRef.current = state.activeHome
    agentsRef.current = state.agents
  }, [state.filters, state.activeHome, state.agents])

  // ── U5：executions/agents 切片 ──────────────────────────────────────────
  // 尾窗与 pending buffer 留在 ref：高频 raw 事件不进 React 调度，
  // transcriptVersion 是唯一的渲染信号（50ms 合并 flush 一次 +1）。
  const transcriptsRef = useRef<Map<string, TranscriptEvent[]>>(new Map())
  const pendingRawRef = useRef<Map<string, TranscriptEvent[]>>(new Map())
  const flushTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const repullTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  // 已查看过的 Failed agent（选中即记入）：badge 重算时不复活
  const viewedFailedRef = useRef<Set<string>>(new Set())
  // §49 5s-poll-side-effects：上一次 refreshAgents 落账结果的结构签名——
  // silent 轮询结果结构未变时跳过 setState（保持 agents 引用稳定，
  // AgentsPanel 键在 [agents] 上的 effect——run/list 兜底扫描、历史尾部
  // 回载、exec_attach 重试——不再每 5s 重跑）
  const agentsSigRef = useRef<string | null>(null)
  // P-10：本会话已接上实时流的 execId（startDispatch 的 Channel 也算）——防重复 attach
  const attachedRef = useRef<Set<string>>(new Set())
  // §51 attach-retry：失败重试的计数与在途定时器（execId 键控）
  const attachRetryCountsRef = useRef<Map<string, number>>(new Map())
  const attachRetryTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map())
  // §51 attach-retry：定时器回调里取「最新」的 attachTranscript（闭包不重建）
  const attachTranscriptRef = useRef<(execId: string, homeId: string) => void>(() => {})
  // review R2-9：派发进行中的计数（含续跑的就绪握手，最长 30s）——进行中不接回
  // 实时流，否则 exec_start 返回后会出现第二个跟随者，事件全部显示两次
  const dispatchingRef = useRef(0)
  // §39 fe-resume-fail-reattach：派发进行中被推迟的接回请求（execId → homeId），
  // 派发结束（成功或失败）后补接——此前失败路径不重拉列表，被跳过的 live
  // agent 要等下一次列表重拉才接上
  const deferredAttachRef = useRef<Map<string, string>>(new Map())

  const bumpTranscriptVersion = useCallback(() => {
    setState(s => ({ ...s, transcriptVersion: s.transcriptVersion + 1 }))
  }, [])

  const flushPendingRaw = useCallback(() => {
    flushTimerRef.current = undefined
    if (pendingRawRef.current.size === 0) return
    for (const [execId, events] of pendingRawRef.current) {
      appendTail(transcriptsRef.current, execId, events)
    }
    pendingRawRef.current.clear()
    bumpTranscriptVersion()
  }, [bumpTranscriptVersion])

  const scheduleFlush = useCallback(() => {
    if (flushTimerRef.current !== undefined) return
    flushTimerRef.current = setTimeout(flushPendingRaw, RAW_FLUSH_MS)
  }, [flushPendingRaw])

  // agents_list pull（§四）：tab 首开 / window focus / 派发后主动拉；
  // agents 是机器本地数据，不进 daemon 事件流。Failed badge 集合随 pull
  // 重算（viewedFailedRef 保证已查看的不复活）。
  // A8 / Q-10：`silent` 时不清除全局错误——5s 定时轮询用它，避免错误
  // 提示一闪而过；显式拉取（focus / 派发后）仍按原语义清错误。
  const refreshAgents = useCallback(async (opts?: { silent?: boolean }) => {
    const home = activeHomeRef.current
    if (!home) return
    const requestSeq = ++agentsRequestSeqRef.current
    try {
      const result = await agentsList(home.homeId)
      if (
        activeHomeRef.current?.homeId !== home.homeId
        || agentsRequestSeqRef.current !== requestSeq
      ) return
      // 归一化放 setState 之外：updater 在 React 渲染期执行，落到本函数
      // try/catch 之外（测试 mock 返回 {} 时不得炸渲染）
      const agents = result.agents ?? []
      // §49 5s-poll-side-effects：silent 轮询结果与上次落账结构相同 → 跳过
      // setState（引用稳定压住 AgentsPanel 的 effect 重跑）。非 silent 调用
      // 仍走原路径（focus / 派发后重拉要清全局 error）。
      const signature = JSON.stringify({
        agents,
        parseFailed: result.parseFailed ?? false,
        legacyUnmigrated: result.legacyUnmigrated ?? null,
        corruptAgents: result.corruptAgents ?? [],
      })
      if (opts?.silent && signature === agentsSigRef.current) return
      agentsSigRef.current = signature
      setState(s => ({
        ...s,
        agents,
        parseFailed: result.parseFailed ?? false,
        legacyUnmigrated: result.legacyUnmigrated ?? null,
        corruptAgents: result.corruptAgents ?? [],
        unviewedFailedIds: agents
          .filter(a => a.status === 'failed' && !viewedFailedRef.current.has(a.id))
          .map(a => a.id),
        ...(opts?.silent ? {} : { error: null }),
      }))
    } catch {
      /* agents 面是补充面——拉取失败不打断主流程 */
    }
  }, [])

  // bindings_get pull（§四，契约对齐 agents）：selectHome / window focus /
  // 派发后主动拉；按 home 键入 bindingsByHome。失败不打断主流程（badge 与
  // 弹窗预选静默缺席）。
  const refreshBindings = useCallback(async () => {
    const home = activeHomeRef.current
    if (!home) return
    const requestSeq = ++bindingsRequestSeqRef.current
    try {
      const result = await bindingsGet(home.homeId)
      if (
        activeHomeRef.current?.homeId !== home.homeId
        || bindingsRequestSeqRef.current !== requestSeq
      ) return
      const bindings = result.bindings ?? {}
      setState(s => ({
        ...s,
        bindingsByHome: { ...s.bindingsByHome, [home.homeId]: bindings },
        bindingsParseFailed: result.parseFailed ?? false,
      }))
    } catch {
      /* 绑定是补充面——拉取失败不打断主流程 */
    }
  }, [])

  // U9 一键清理（Agents 面）：bindings_gc，成功后重拉绑定表。Rust 侧在
  // agents.json 降级时拒绝（§3.6.6 GC 冻结），错误如实进全局 error。
  const gcBindings = useCallback(async () => {
    const home = activeHomeRef.current
    if (!home) return
    try {
      await bindingsGc(home.homeId)
      await refreshBindings()
    } catch (error) {
      setState(s => ({ ...s, error: presentError(error) }))
    }
  }, [refreshBindings])

  // §41：「确认并归档」旧版存储——成功后重拉 agents（降级解除）与绑定表
  //（GC 冻结解除后悬空计数才可信），返回移走的文件名供面板显示。失败**抛出**、
  // 不写 state.error：全局错误会被其他刷新清掉，归档失败由横幅就地呈现。
  const archiveLegacyAgents = useCallback(async (): Promise<string[]> => {
    const home = activeHomeRef.current
    if (!home) throw new Error('无活动 workspace')
    const { archived } = await agentsArchiveLegacy(home.homeId)
    await refreshAgents()
    await refreshBindings()
    return archived
  }, [refreshAgents, refreshBindings])

  // exit 后防抖重拉：本地乐观更新让 UI 即时反应，重拉收敛 Rust 落盘后的
  // 权威形态（exit code / failure detail）。
  const scheduleAgentsRepull = useCallback(() => {
    clearTimeout(repullTimerRef.current)
    repullTimerRef.current = setTimeout(() => { void refreshAgents() }, EXIT_REPULL_MS)
  }, [refreshAgents])

  // exit 事件的本地联动：更新对应 agent 的 status/attachment（乐观），
  // Failed 计入未查看 badge。
  const applyExitLocal = useCallback((ev: Extract<TranscriptEvent, { kind: 'exit' }>) => {
    // §51：本地乐观更新后重置轮询签名——序号竞争下下一次 silent 轮询结果
    // 不得被误判为「与上次相同」而跳过落账
    agentsSigRef.current = null
    setState(s => {
      let failedAgentId: string | null = null
      const agents = s.agents.map(a => {
        if (!a.execs.some(e => e.execId === ev.execId)) return a
        failedAgentId = a.id
        return { ...a, status: ev.status, attachment: 'none' as const }
      })
      const unviewedFailedIds =
        failedAgentId !== null &&
        ev.status === 'failed' &&
        !viewedFailedRef.current.has(failedAgentId) &&
        !s.unviewedFailedIds.includes(failedAgentId)
          ? [...s.unviewedFailedIds, failedAgentId]
          : s.unviewedFailedIds
      return { ...s, agents, unviewedFailedIds }
    })
  }, [])

  /** F-PI-3：历史尾部回载——窗口为空且非 live 时从 raw.log 惰性拉取。
   * 直接 appendTail（绕过 raw 合并批处理——一次性整段，无需再批）。 */
  const loadHistoryTail = useCallback(
    (execId: string, homeId: string) => {
      if (transcriptsRef.current.get(execId)?.length) return
      void execTranscriptTail(execId, homeId)
        .then(events => {
          if (!events.length) return
          // 竞态护栏：期间本会话已有事件（不可能出现的新派发复用同 execId
          // 不会发生——execId 全局唯一），直接并入
          appendTail(transcriptsRef.current, execId, events)
          bumpTranscriptVersion()
        })
        .catch(() => {/* raw.log 读取失败 → 保持空窗，不阻断 */})
    },
    [appendTail, bumpTranscriptVersion],
  )

  /** U16：失败系统通知——agent 名尽力而为（事件流不带 display name，
   * agents 重拉前的降级文案），失败详情截尾。通知失败静默。 */
  const notifyFailure = async (ev: Extract<TranscriptEvent, { kind: 'exit' }>) => {
    try {
      let granted = await isPermissionGranted()
      if (!granted) {
        granted = (await requestPermission()) === 'granted'
      }
      if (!granted) return
      // execId → agent 展示名（就地建表——低频事件不值得常驻索引）
      let agentName: string | undefined
      for (const agent of agentsRef.current) {
        if (agent.execs.some(e => e.execId === ev.execId)) {
          agentName = agent.title ?? agent.displayName
          break
        }
      }
      const detail =
        ev.killReason ? `终止：${ev.killReason}`
        : `exit ${ev.code ?? '?'}`
      sendNotification({
        title: `OMT Agent 失败${agentName ? ` — ${agentName}` : ''}`,
        body: detail,
      })
    } catch {
      // 通知是增强——任何失败静默
    }
  }

  const ingestTranscriptEvent = useCallback(
    (ev: TranscriptEvent) => {
      if (!ev || typeof ev.execId !== 'string') return
      if (ev.kind === 'raw') {
        // 50ms 合并：raw 行进 pending buffer，一次 flush 一次渲染
        const pending = pendingRawRef.current.get(ev.execId) ?? []
        pending.push(ev)
        pendingRawRef.current.set(ev.execId, pending)
        scheduleFlush()
        return
      }
      // session_start / exit 低频且影响状态：立即入窗
      appendTail(transcriptsRef.current, ev.execId, [ev])
      if (ev.kind === 'exit') {
        applyExitLocal(ev)
        scheduleAgentsRepull()
        // U16：失败可见性——Failed 终态发系统通知（用户可能在别的窗口；
        // exited/killed 不扰民）。权限按需申请，失败静默（通知是增强）。
        if (ev.status === 'failed') {
          void notifyFailure(ev)
        }
      }
      bumpTranscriptVersion()
    },
    [scheduleFlush, applyExitLocal, scheduleAgentsRepull, bumpTranscriptVersion],
  )

  /** P-10：重启后接回仍在运行的 exec。afterSeq 取本窗口最后一个事件的 seq。
   * §51 attach-retry：瞬时失败不再永久放弃——按轮询周期有界重试（上限
   * ATTACH_RETRY_MAX 次；exec 仍在当前 agents 列表且未 attached 才重试），
   * 否则轮询引用稳定化后无人再触发，运行中的实时 transcript 冻结。 */
  const attachTranscript = useCallback(
    (execId: string, homeId: string) => {
      if (attachedRef.current.has(execId)) return
      // review R2-9：派发进行中（续跑的就绪握手最长 30s）先不接——新 exec 的
      // 跟随者会由 exec_start 建立。§39：记下来，派发结束后补接
      if (dispatchingRef.current > 0) {
        deferredAttachRef.current.set(execId, homeId)
        return
      }
      attachedRef.current.add(execId)
      const channel = new Channel<TranscriptEvent>()
      channel.onmessage = ev => ingestTranscriptEvent(ev)
      const existing = transcriptsRef.current.get(execId)
      const last = existing && existing.length > 0 ? existing[existing.length - 1] : undefined
      const afterSeq = last ? last.seq : null
      void execAttach({ homeId, execId, afterSeq, channel })
        .then(() => {
          // 成功才清重试计数（尝试开始时不清——否则重试链永远从 0 数起）
          attachRetryCountsRef.current.delete(execId)
        })
        .catch(error => {
        attachedRef.current.delete(execId)
        setState(s => ({ ...s, error: presentError(error) }))
        // §51 attach-retry：失败重试（有界；间隔一个轮询周期）
        const attempts = (attachRetryCountsRef.current.get(execId) ?? 0) + 1
        if (attempts > ATTACH_RETRY_MAX) return
        attachRetryCountsRef.current.set(execId, attempts)
        const timer = setTimeout(() => {
          attachRetryTimersRef.current.delete(execId)
          if (agentsRef.current.some(a => a.execs.some(e => e.execId === execId))) {
            attachTranscriptRef.current(execId, homeId)
          } else {
            attachRetryCountsRef.current.delete(execId)
          }
        }, ATTACH_RETRY_MS)
        attachRetryTimersRef.current.set(execId, timer)
      })
    },
    [ingestTranscriptEvent],
  )
  attachTranscriptRef.current = attachTranscript

  // §51 attach-retry：卸载时清掉在途的重试定时器
  useEffect(
    () => () => {
      for (const timer of attachRetryTimersRef.current.values()) clearTimeout(timer)
      attachRetryTimersRef.current.clear()
    },
    [],
  )

  /** §39：所有派发结束后补接被推迟的 exec（新 exec 已在 attachedRef 里，自然跳过）。 */
  const flushDeferredAttach = useCallback(() => {
    if (dispatchingRef.current > 0) return
    const pending = [...deferredAttachRef.current]
    deferredAttachRef.current.clear()
    for (const [execId, homeId] of pending) attachTranscript(execId, homeId)
  }, [attachTranscript])

  // U6：Channel 由 startDispatch 按次创建（Rust 泵随子进程生命周期，KD-D3），
  // onmessage 直入归并管线；onEvent 兜底供调用方旁路观察。
  const startDispatch = useCallback(
    async (options: {
      home: HomeInfo
      nodeId: string
      harness: string
      unattended: boolean
      model?: string | null
      resumeSession?: string | null
      agentId?: string | null
      onEvent?: (ev: TranscriptEvent) => void
    }): Promise<ExecStartOutcome> => {
      const channel = new Channel<TranscriptEvent>()
      channel.onmessage = ev => {
        ingestTranscriptEvent(ev)
        options.onEvent?.(ev)
      }
      // review R2-9：派发进行中（就绪握手最长 30s）不计接回实时流——新 exec
      // 的跟随者由 exec_start 建立
      dispatchingRef.current += 1
      let outcome: ExecStartOutcome
      try {
        outcome = await execStart({
          homeId: options.home.homeId,
          homePath: options.home.path ?? '',
          nodeId: options.nodeId,
          harness: options.harness,
          unattended: options.unattended,
          model: options.model ?? null,
          resumeSession: options.resumeSession ?? null,
          agentId: options.agentId ?? null,
          channel,
        })
      } catch (error) {
        dispatchingRef.current -= 1
        flushDeferredAttach()
        throw error
      }
      dispatchingRef.current -= 1
      // P-10：startDispatch 的 Channel 即本会话的实时跟随者——记入防重复 attach
      attachedRef.current.add(outcome.execId)
      attachRetryCountsRef.current.delete(outcome.execId)
      flushDeferredAttach()
      // pull 契约：派发后拉 agents_list（防抖等 Rust 侧 ⑤ 落盘收敛）；
      // 绑定表重拉由 App 的派发成功回调统一触发（单一落点，避免双拉）
      scheduleAgentsRepull()
      return outcome
    },
    [ingestTranscriptEvent, scheduleAgentsRepull, flushDeferredAttach],
  )

  const selectAgent = useCallback((agentId: string) => {
    viewedFailedRef.current.add(agentId)
    setState(s => ({
      ...s,
      selectedAgentId: agentId,
      unviewedFailedIds: s.unviewedFailedIds.filter(id => id !== agentId),
    }))
  }, [])

  /** U10：空闲 agent 一键续跑——定向派发其关联 ticket（U11 续跑语义：
   *  有 lastSessionId 则续会话），走与弹窗同款的知情确认门（bag v2 ack
   *  未确认 → 拒绝并引导先走一次弹窗）与记忆模型。 */
  const resumeAgent = useCallback(
    async (agent: AgentRecord) => {
      const exec = agent.execs[agent.execs.length - 1]
      const home = activeHomeRef.current
      if (!exec?.nodeId) throw new Error('该 agent 无关联 ticket，无法续跑')
      if (!home) throw new Error('无活动 workspace')
      const bag = await readHarnessBag(home.homeId)
      if (!bag.approvalAck?.[agent.harness]) {
        throw new Error(
          `请先对 ${agent.harness} 完成一次无人值守知情确认（任意 ticket 派发弹窗勾选）后再使用一键续跑`,
        )
      }
      await startDispatch({
        home,
        nodeId: exec.nodeId,
        harness: agent.harness,
        unattended: true,
        model: bag.models?.[agent.harness] ?? null,
        resumeSession: agent.lastSessionId,
        agentId: agent.id,
      })
    },
    [startDispatch],
  )

  /** F-DEL / F-EXEC-14(4)：删除 agent（终态守卫在服务端）。`force=true` =
   * 「放弃租约并删除」——只绕过待对账租约守卫（面板在常规删除被租约守卫
   * 拒绝后以两段式确认提供该入口）；成功后清选中态并重拉列表。
   * §47 release-results-hidden：返回 agents_delete 的释放结果，释放失败的
   * 租约登记为应用级通知（App 呈现，任何页面可见）。 */
  const deleteAgent = useCallback(
    async (agentId: string, force?: boolean): Promise<AgentsDeleteResult> => {
      const home = activeHomeRef.current
      if (!home) throw new Error('无活动 workspace')
      const result = await agentsDelete(home.homeId, agentId, force ?? false)
      const failures = result?.leaseReleaseFailures ?? []
      setState(s => ({
        ...s,
        selectedAgentId: s.selectedAgentId === agentId ? null : s.selectedAgentId,
        unviewedFailedIds: s.unviewedFailedIds.filter(id => id !== agentId),
        // §47：被放弃的租约进应用级通知列表——按被删 agent 追加，不覆盖先前的
        // 通知（updater 在 refreshAgents 之前执行，s.agents 里还有这条记录）
        leaseNotices:
          failures.length > 0
            ? [
              ...s.leaseNotices.filter(n => n.agentId !== agentId),
              leaseNoticeOf(
                s.agents.find(a => a.id === agentId),
                agentId,
                home.homeId,
                failures,
              ),
            ]
            : s.leaseNotices,
      }))
      await refreshAgents()
      return result
    },
    [refreshAgents],
  )

  /** §47：关闭一条被放弃租约的通知。 */
  const dismissLeaseNotice = useCallback((agentId: string) => {
    setState(s => ({ ...s, leaseNotices: s.leaseNotices.filter(n => n.agentId !== agentId) }))
  }, [])

  const stopAgent = useCallback(async (execId: string) => {
    try {
      await execStop(execId)
    } catch (error) {
      setState(s => ({ ...s, error: presentError(error) }))
    }
  }, [])

  /** P-10 / review F14 / R2-7 / F-EXEC-12 / §43 P3：停止当前 home 的全部运行中
   * exec。返回 `{ signaled, failed }`（部分失败如实上抛供面板提示「N 个停止
   * 失败：明细」）；失败返回 null（不能与「本来就没有运行中的」0 混淆）：
   * - invoke 拒绝（home 级失败）→ 全局 error + null（既有语义）。
   * - 全部失败（signaled=0 且有 failed）→ 全局 error（合并明细）+ null
   *   （R2-7 的「停止失败」语义保留——F-EXEC-12 在 Rust 侧以 Ok 数据携带
   *   明细而非 Err，此处折算回等价的可观察行为）。
   * - 部分失败（signaled>0 且有 failed）→ 返回结果本身，面板拼接
   *   「已向 N 个 agent 发出停止信号，M 个停止失败」。
   * §43 P3：repull 在映射前无条件执行——全失败路径此前跳过重拉，行内残留
   * 的运行态不会被刷新（对账收尾后 agents.json 已变，UI 需要跟上）。 */
  const stopAll = useCallback(async (): Promise<StopAllOutcome | null> => {
    const home = activeHomeRef.current
    if (!home) return null
    try {
      const outcome = await execStopAll(home.homeId)
      scheduleAgentsRepull()
      if (outcome.failed.length > 0 && outcome.signaled === 0) {
        setState(s => ({ ...s, error: presentError(outcome.failed.join('；')) }))
        return null
      }
      return outcome
    } catch (error) {
      setState(s => ({ ...s, error: presentError(error) }))
      return null
    }
  }, [scheduleAgentsRepull])

  const killOrphan = useCallback(
    async (agentId: string, execId: string) => {
      const home = activeHomeRef.current
      if (!home) return
      try {
        await execKillOrphan(home.homeId, agentId, execId)
        scheduleAgentsRepull()
      } catch (error) {
        setState(s => ({ ...s, error: presentError(error) }))
      }
    },
    [scheduleAgentsRepull],
  )

  const refreshHomes = useCallback(async () => {
    try {
      // Handshake projection of open homes (pull-based freshness, v1).
      const result = await invoke<{ homes?: HomeInfo[]; homeDir?: string }>('daemon_homes')
      const homes = result.homes ?? []
      void loadKnownHomes()
      // U1b：每个 home 拉到后顺带执行 bag v2 迁移（幂等，旧键清理后为 no-op）
      for (const home of homes) void migrateHarnessBag(home.homeId)
      if (result.homeDir !== undefined) setState(s => ({ ...s, homeDir: result.homeDir }))
      setState(s => {
        // A homes refresh must NEVER unset an existing selection — a
        // transiently incomplete list (in-flight boot, declare still
        // landing) would otherwise wipe the user's active home. Only
        // auto-pick when nothing is selected yet; the boot pick prefers
        // the LAST-USED workspace (localStorage) over the first home.
        const remembered = window.localStorage.getItem(LAST_HOME_KEY)
        const next = s.activeHome
          ?? (remembered ? homes.find(h => h.homeId === remembered) : undefined)
          ?? homes[0]
          ?? null
        const autoPicking = s.activeHome === null && next !== null
        if (next !== s.activeHome) activeHomeRef.current = next
        return {
          ...s,
          homes,
          activeHome: next,
          // Fresh auto-pick starts with an empty bag; the load below
          // adopts the home's persisted filters.
          filters: autoPicking ? {} : s.filters,
          loading: false,
          error: null,
        }
      })
    } catch (error) {
      setState(s => ({ ...s, loading: false, error: presentError(error) }))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const refreshNodes = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ trees?: OmtNode[] }>('node/tree', { homeId: home.homeId })
      // Wire TreeNode uses `nodeId` (generated common.ts); the desktop's
      // local model calls it `id`. Normalize AT THE BRIDGE BOUNDARY so
      // every downstream consumer (row render, selection, recent, query
      // filter) sees a real id.
      const normalize = (wire: OmtNode & { nodeId?: string }): OmtNode => ({
        ...wire,
        id: wire.nodeId ?? wire.id,
        children: ((wire as { children?: OmtNode[] }).children ?? []).map(child =>
          normalize(child as OmtNode & { nodeId?: string }),
        ),
      })
      setState(s => ({ ...s, nodes: (result.trees ?? []).map(normalize), error: null }))
    } catch (error) {
      setState(s => ({ ...s, error: presentError(error) }))
    }
  }, [])

  const refreshRuns = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ runs?: Array<RunSummary & { runId?: string }> }>('run/list', { homeId: home.homeId })
      // 线上 RunView 用 runId 键——与 nodeId 同一类边界归一化。
      const runs = (result.runs ?? []).map(wire => ({ ...wire, id: wire.runId ?? wire.id }))
      setState(s => ({ ...s, runs }))
    } catch {
      /* runs are supplementary — tree stays usable */
    }
  }, [])

  const loadFilters = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ filters?: SavedFilters }>('ui/filters-get', {
        homeId: home.homeId,
        key: FILTERS_KEY,
      })
      // Merge OVER current state: a toggle the user made while this load
      // was in flight (or whose set RPC is still landing) must survive.
      setState(s => ({ ...s, filters: { ...(result.filters ?? {}), ...s.filters } }))
    } catch {
      /* keep current filters on load failure */
    }
  }, [])

  const saveFilters = useCallback(async (home: HomeInfo, patch: SavedFilters) => {
    // Merged bag drives BOTH the optimistic render and the wire payload
    // (ui/filters-set replaces the whole bag); the ref avoids depending
    // on React's async updater timing.
    const merged = { ...filtersRef.current, ...patch }
    filtersRef.current = merged
    setState(s => ({ ...s, filters: merged }))
    await omtCall('ui/filters-set', { homeId: home.homeId, key: FILTERS_KEY, filters: merged }).catch(() => {})
  }, [])

  const loadKnownHomes = useCallback(async () => {
    try {
      const result = await omtCall<{ homes?: KnownHome[] }>('home/list-known', {})
      setState(s => ({ ...s, knownHomes: result.homes ?? [] }))
    } catch {
      // Pre-list-known daemon (method NOT_FOUND) degrades to an empty
      // known list — the picker keeps working with open homes only.
      setState(s => ({ ...s, knownHomes: [] }))
    }
  }, [])

  /**
   * bag v2 迁移（§四定稿，U1b）：挂在 home 加载路径——refreshHomes 拉完
   * home 列表 / selectHome 装载 per-home 数据时顺带执行；legacy 探测与 v2
   * 投影同一次读（readRawHarnessBag，不加 RPC）。语义：
   * - 旧 opencode 条目且无 v2 defaultHarness → 写 defaultHarness='opencode'
   *   并移除旧 `harness` 键（迁移完成后旧键已清，重放为 no-op）；
   * - 旧 dsh 条目 → 不写不删（不静默丢弃），置 legacyDshByHome 提示态
   *   （设置页提示「dsh harness 后续版本支持，当前配置已保留」）；
   * - v2 已有 defaultHarness 或无旧字段 → no-op。
   */
  const migrateHarnessBag = useCallback(async (homeId: string) => {
    const raw = await readRawHarnessBag(homeId)
    const bag = projectHarnessBag(raw)
    if (bag.legacy === 'dsh') {
      setState(s =>
        s.legacyDshByHome[homeId]
          ? s
          : { ...s, legacyDshByHome: { ...s.legacyDshByHome, [homeId]: true } },
      )
      return
    }
    if (bag.legacy !== 'opencode' || bag.defaultHarness) return
    // read-merge-write（writeHarnessBag 同语义）：除移除旧 `harness` 键外，
    // 其余键（approvalAck 等）原样保留
    const next: RawHarnessBag = { ...raw }
    delete next.harness
    next.defaultHarness = 'opencode'
    await omtCall('ui/filters-set', { homeId, key: HARNESS_BAG_KEY, filters: next }).catch(() => {})
  }, [])

  const loadRecent = useCallback(async () => {
    try {
      const result = await omtCall<{ refs?: Array<{ homeId: string; nodeId: string }> }>('ui/recent-get', { key: RECENT_KEY })
      setState(s => ({ ...s, recentIds: (result.refs ?? []).map(r => r.nodeId) }))
    } catch {
      /* recent list is cosmetic */
    }
  }, [])

  const touchRecent = useCallback(async (nodeId: string) => {
    setState(s => {
      const next = [nodeId, ...s.recentIds.filter(id => id !== nodeId)].slice(0, 10)
      void omtCall('ui/recent-set', {
        key: RECENT_KEY,
        refs: next.map(id => ({ homeId: '', nodeId: id })),
      }).catch(() => {})
      return { ...s, recentIds: next }
    })
  }, [])

  const selectHome = useCallback(
    async (home: HomeInfo) => {
      setState(s => ({
        ...s,
        activeHome: home,
        selectedId: null,
        nodes: [],
        runs: [],
        filters: {},
        // U5：切换 home 重置 agents 视图态（尾窗按 execId 键控，跨 home 保留）
        agents: [],
        parseFailed: false,
        legacyUnmigrated: null,
        corruptAgents: [],
        selectedAgentId: null,
        unviewedFailedIds: [],
        // U9：绑定降级标志随 home 切换重置（表按 home 键保留）
        bindingsParseFailed: false,
      }))
      viewedFailedRef.current.clear()
      filtersRef.current = {}
      // §51：切 home 重置轮询签名——上一 home 的签名不得让新 home 的首个
      // silent 轮询结果被误判为「结构未变」而跳过落账（空列表签名尤其常见）
      agentsSigRef.current = null
      activeHomeRef.current = home
      window.localStorage.setItem(LAST_HOME_KEY, home.homeId)
      // U1b：切 home 装载 per-home 数据时顺带执行 bag v2 迁移（幂等）
      await Promise.all([refreshNodes(home), refreshRuns(home), loadFilters(home), refreshAgents(), refreshBindings(), migrateHarnessBag(home.homeId)])
    },
    [refreshNodes, refreshRuns, loadFilters, refreshAgents, refreshBindings, migrateHarnessBag],
  )

  /** 新建节点（树面板入口）：node/create → 刷新树 → 选中新节点。 */
  const createNode = useCallback(
    async (input: { type: string; title: string; body?: string; parentId: string | null }) => {
      const home = activeHomeRef.current
      if (!home) throw new Error('无活动 workspace')
      const result = await omtCall<{ node?: { nodeId?: string } }>('node/create', {
        homeId: home.homeId,
        type: input.type,
        title: input.title,
        ...(input.parentId ? { parentId: input.parentId } : {}),
        ...(input.body !== undefined ? { body: input.body } : {}),
      })
      await refreshNodes(home)
      const newId = result.node?.nodeId
      if (newId) selectNode(newId)
      return newId ?? ''
    },
    [refreshNodes],
  )

  // Declare flow (U5 consumer): register an unregistered home directory,
  // then re-pull the home list. Pre-U5 daemons surface the feature gap
  // as a readable fallback message.
  const declareHome = useCallback(
    async (path: string) => {
      try {
        await omtCall('home/declare', { path })
        // requiresRehandshake: a fresh session folds the new home into the
        // credential's scoped grant before we re-list.
        await invoke('daemon_reconnect')
        await refreshHomes()
        await loadKnownHomes()
        return null
      } catch (error) {
        const text = String(error)
        if (text.includes('NOT_FOUND') || text.includes('method')) {
          return '当前 daemon 版本不支持动态收录 home（缺少 homeDeclare 能力位）— 请升级 daemon 后重试。'
        }
        return presentError(error)
      }
    },
    [refreshHomes],
  )

  const selectNode = useCallback(
    (id: string | null) => {
      setState(s => ({ ...s, selectedId: id }))
      if (id) touchRecent(id)
    },
    [touchRecent],
  )

  // Event-driven refresh (external CLI edits → second-level consistency).
  useEffect(() => {
    const home = state.activeHome
    if (!home) return
    let disposed = false
    let timer: ReturnType<typeof setTimeout> | undefined
    subscribeEvents(home.homeId, (_envelope: EventEnvelope) => {
      if (disposed) return
      // Debounce bursts (a run claim storms dozens of envelopes).
      clearTimeout(timer)
      timer = setTimeout(() => {
        void refreshNodes(home)
        void refreshRuns(home)
      }, 300)
    })
      .then(unlisten => {
        if (!disposed) disposers.current.push(unlisten)
      })
      .catch(() => {})
    return () => {
      disposed = true
      clearTimeout(timer)
    }
  }, [state.activeHome, refreshNodes, refreshRuns])

  // Boot + pull-based home freshness on window focus (v1 contract).
  // U5：agents_list 同享 focus 重拉；U9：bindings_get 同（§四 pull 契约）。
  useEffect(() => {
    void refreshHomes()
    void loadKnownHomes()
    void loadRecent()
    const onFocus = () => {
      void refreshHomes()
      void refreshAgents()
      void refreshBindings()
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [refreshHomes, loadRecent, refreshAgents, refreshBindings])

  // Q-10：CLI / MCP 发起的执行在桌面端无需切换焦点即可见——窗口可见时每 5s
  // 静默重拉 agents（不清除全局错误）。外部 agent 由此出现在列表中；选中后由
  // AgentsPanel 的 P-10 effect 接回实时流。
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') void refreshAgents({ silent: true })
    }, AGENTS_POLL_MS)
    return () => clearInterval(timer)
  }, [refreshAgents])

  // Load per-home data whenever the active home changes.
  useEffect(() => {
    if (state.activeHome) void selectHome(state.activeHome)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.activeHome?.homeId])

  // 稳定标识（[] deps，只读 activeHomeRef）：消费方把 fetchRun 放进 effect
  // 依赖时，raw flush 引起的重渲染不得反复 teardown 请求（U7 评审修复）。
  const fetchRun = useCallback(async (runId: string) => {
    const home = activeHomeRef.current
    if (!home) throw new Error('no active home')
    const detail = await omtCall<RunDetail & { run: RunSummary & { runId?: string } }>('run/get', {
      homeId: home.homeId,
      runId,
    })
    return { ...detail, run: { ...detail.run, id: detail.run.runId ?? detail.run.id } }
  }, [])

  return {
    state,
    loadFilters,
    loadKnownHomes,
    selectHome,
    selectNode,
    saveFilters,
    declareHome,
    refreshHomes,
    refreshNodes,
    // ── U5：agents / transcript 切片 ─────────────────────────────────────
    refreshAgents,
    selectAgent,
    stopAgent,
    killOrphan,
    ingestTranscriptEvent,
    loadHistoryTail,
    attachTranscript,
    stopAll,
    resumeAgent,
    createNode,
    deleteAgent,
    dismissLeaseNotice,
    // ── U9：绑定切片（R-C）───────────────────────────────────────────────
    refreshBindings,
    gcBindings,
    archiveLegacyAgents,
    /** 某 exec 的尾窗（无则空数组）；数组原地追加，UI 以 transcriptVersion 感知 */
    transcriptWindow: (execId: string): TranscriptEvent[] =>
      transcriptsRef.current.get(execId) ?? EMPTY_TRANSCRIPT,
    startDispatch,
    fetchRun,
    updateNode: async (id: string, patch: Record<string, unknown>, revision?: number) => {
      const home = activeHomeRef.current
      if (!home) return 'no active home'
      try {
        // Wire contract: nodeId + changes object; expectedRevision is a
        // top-level optimistic-concurrency gate (dispatch.rs).
        await omtCall('node/update', {
          homeId: home.homeId,
          nodeId: id,
          changes: patch,
          ...(revision !== undefined ? { expectedRevision: revision } : {}),
        })
        await refreshNodes(home)
        return null
      } catch (error) {
        return presentError(error)
      }
    },
  }
}
