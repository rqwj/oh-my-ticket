/**
 * U9/U10 frontend bridge wrapper: the ONLY daemon path from the renderer
 * (KTD6 — the socket lives Rust-side). Calls forward through
 * `omt_call`; daemon event envelopes arrive over the `omt://event`
 * Tauri channel.
 */
import { Channel, invoke } from '@tauri-apps/api/core'
import { getVersion } from '@tauri-apps/api/app'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { AgentsListResult, BindingsGcResult, BindingsResult } from './types'

export interface DaemonStatus {
  running: boolean
  pid: number | null
  generation: number | null
  endpoint: string | null
  spawned_by_us: boolean
}

export async function omtCall<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return invoke<T>('omt_call', { method, params })
}

export async function daemonStatus(): Promise<DaemonStatus> {
  return invoke<DaemonStatus>('daemon_status')
}

export async function daemonEnsure(): Promise<DaemonStatus> {
  return invoke<DaemonStatus>('daemon_ensure')
}

// ── DSH 集成（设置页「安装 DSH 插件」流程）─────────────────────────────

export interface HarnessInfo {
  installed: boolean
  path?: string
  version?: string | null
}

/** PATH（含常见前缀）上的 harness 安装探测。 */
export async function harnessDetect(harness: 'dsh' | 'opencode'): Promise<HarnessInfo> {
  return invoke<HarnessInfo>('harness_detect', { harness })
}

/** 目录是否为 dsh 源码 checkout（pnpm-workspace.yaml + apps + packages）。 */
export async function checkoutValid(path: string): Promise<{ valid: boolean }> {
  return invoke<{ valid: boolean }>('harness_validate_checkout', { path })
}

export interface DshInstallResult {
  ok: boolean
  output: string
}

/**
 * 安装 oh-my-ticket DSH 插件（版本 lockstep：version = 当前 app 版本）。
 * - global：PATH 上的 dsh（`dsh plugin --profile <profile> add <pkg>@<ver>`）
 * - dev：dsh 源码 checkout（`pnpm --filter @deepseek-ai/dsh exec dsh plugin …`）
 */
export async function installDshPlugin(options: {
  mode: 'global' | 'dev'
  checkoutDir?: string
  profile?: string
  packageName: string
  version: string
}): Promise<DshInstallResult> {
  return invoke<DshInstallResult>('dsh_plugin_install', {
    mode: options.mode,
    checkoutDir: options.checkoutDir,
    profile: options.profile ?? 'web',
    packageName: options.packageName,
    version: options.version,
  })
}

/** 当前桌面 app 版本（tauri.conf.json，与插件版本 lockstep）。 */
export async function appVersion(): Promise<string> {
  return getVersion()
}

export interface EventEnvelope {
  homeId?: string
  cursor?: number
  kind?: string
  [k: string]: unknown
}

/** Subscribe to one home's event stream; returns an unlisten disposer. */
export async function subscribeEvents(
  homeId: string,
  onEnvelope: (envelope: EventEnvelope) => void,
): Promise<UnlistenFn> {
  await invoke('events_subscribe', { homeId, since: 0 })
  return listen<EventEnvelope>('omt://event', event => {
    const envelope = event.payload
    if (envelope?.homeId === homeId) onEnvelope(envelope)
  })
}

// ── U5 Agents / transcript（R-E）──────────────────────────────────────────
// TranscriptEvent 经 Tauri Channel 推送（随子进程生命周期，KD-D3：绝不进
// daemon 事件流）。三类事件统一 §5.3 envelope：{execId, seq, ts, kind, ...}。

interface SessionStartEvent {
  execId: string
  seq: number
  /** epoch 毫秒字符串（Rust 侧 agents::now_epoch_ms，字典序 = 时间序） */
  ts: string
  kind: 'session_start'
  harness: string
  program: string
  args: string[]
  cwd: string
  pid: number
}

interface RawTranscriptEvent {
  execId: string
  seq: number
  ts: string
  kind: 'raw'
  stream: 'stdout' | 'stderr'
  text: string
}

interface ExitTranscriptEvent {
  execId: string
  seq: number
  ts: string
  kind: 'exit'
  status: 'exited' | 'failed' | 'killed'
  code: number | null
  killReason: string | null
  durationMs: number
}

/** U12 结构化事件（§5.3 折叠为平铺字段；payload 由 adapters.rs 产出） */
interface AssistantTextEvent {
  execId: string
  seq: number
  ts: string
  kind: 'assistant_text'
  text: string
}

interface AssistantThinkingEvent {
  execId: string
  seq: number
  ts: string
  kind: 'assistant_thinking'
  text: string
}

interface ToolCallEvent {
  execId: string
  seq: number
  ts: string
  kind: 'tool_call'
  name: string
  input: unknown
}

interface ToolResultEvent {
  execId: string
  seq: number
  ts: string
  kind: 'tool_result'
  summary: string
  isError: boolean
}

interface UsageEvent {
  execId: string
  seq: number
  ts: string
  kind: 'usage'
  tokens?: number | null
  cost?: number | null
}

interface ErrorEvent {
  execId: string
  seq: number
  ts: string
  kind: 'error'
  message: string
}

export type TranscriptEvent =
  | SessionStartEvent
  | RawTranscriptEvent
  | ExitTranscriptEvent
  | AssistantTextEvent
  | AssistantThinkingEvent
  | ToolCallEvent
  | ToolResultEvent
  | UsageEvent
  | ErrorEvent

/** agents_list（F-U4a-8）：pull 契约 = tab 首开 / window focus / 派发后主动拉。 */
export async function agentsList(homeId: string): Promise<AgentsListResult> {
  return invoke<AgentsListResult>('agents_list', { homeId })
}

// ── U9 绑定（R-C）─────────────────────────────────────────────────────────

/**
 * bindings_get（§3.4）：显式绑定表——前端做子树继承解析与 badge 渲染。
 * pull 契约对齐 agents：selectHome / window focus / 派发后主动拉（不进
 * daemon 事件流，§四）。parseFailed 粘性语义同 agents_list 且叠加
 * agents.json 降级态：为 true 时前端不得做悬空判定（数据降级）。
 */
export async function bindingsGet(homeId: string): Promise<BindingsResult> {
  return invoke<BindingsResult>('bindings_get', { homeId })
}

/**
 * bindings_gc（§3.4，用户触发的一键清理）：移除 agentId 不存在于
 * agents.json 的绑定项。Rust 侧在 agents.json 解析失败/粘性 corrupt 残留
 * 时拒绝执行（§3.6.6 硬规则：解析失败期间冻结 GC）。
 */
export async function bindingsGc(homeId: string): Promise<BindingsGcResult> {
  return invoke<BindingsGcResult>('bindings_gc', { homeId })
}

/** 终止一个 exec（幂等：已终态/不存在的 execId 返回成功）。 */
export async function execStop(execId: string): Promise<void> {
  return invoke<void>('exec_stop', { execId })
}

/** Orphan 强制 kill：Rust 侧从持久化 agent/exec 身份解析并验证 pid。 */
export async function execKillOrphan(homeId: string, agentId: string, execId: string): Promise<void> {
  return invoke<void>('exec_kill_orphan', { homeId, agentId, execId })
}

// ── U6 执行派发（R-B）─────────────────────────────────────────────────────

/** exec_start 成功返回（Rust ExecStartOutcome 的 camelCase 投影）。 */
export interface ExecStartOutcome {
  execId: string
  agentId: string
  runId: string
  /** 非阻断警告（如绑定写失败）——成功后作为 Agents 面持久警告条 */
  warnings: string[]
}

/**
 * 派发执行（§3.3 链统一入口）。`channel` 归本函数所有：Rust 事件泵经它推送
 * transcript envelope（KD-D3：绝不进 daemon 事件流），随子进程生命周期。
 * `homePath` 是 `.omt` 目录（activeHome.path）；`unattended` 显式放行
 * （headless 无人应答，flag 由 provider descriptor 提供）。
 */
export async function execStart(options: {
  homeId: string
  homePath: string
  nodeId: string
  harness: string
  unattended: boolean
  /** 指定模型（Rust 侧校验字符集；null = 不指定，用 harness 默认） */
  model?: string | null
  /** U11：续跑的 harness session id（null = 新开会话） */
  resumeSession?: string | null
  /** U11：派发到既有 agent（null = 新 agent） */
  agentId?: string | null
  channel: Channel<TranscriptEvent>
}): Promise<ExecStartOutcome> {
  return invoke<ExecStartOutcome>('exec_start', {
    homeId: options.homeId,
    homePath: options.homePath,
    nodeId: options.nodeId,
    harness: options.harness,
    unattended: options.unattended,
    model: options.model ?? null,
    resumeSession: options.resumeSession ?? null,
    agentId: options.agentId ?? null,
    channel: options.channel,
  })
}

/**
 * 弹窗「详情」的只读启动预览。secrets 纪律（§3.1）：env 只回键名不回值；
 * args 是用户 extra_args，unattendedFlag 是勾选放行后将追加的 flag。
 */
export interface HarnessLaunchPreview {
  program: string
  args: string[]
  envKeys: string[]
  unattendedFlag: string[]
}

export async function harnessLaunchPreview(harness: string): Promise<HarnessLaunchPreview> {
  return invoke<HarnessLaunchPreview>('harness_launch_preview', { harness })
}

/** 历史 transcript 尾部（F-PI-3）：raw.log 尾部重建的展示事件——
 * app 重启后选中历史 agent 时惰性加载；无日志返回空数组。 */
export async function execTranscriptTail(
  execId: string,
  homeId: string,
  maxLines = 500,
): Promise<TranscriptEvent[]> {
  return invoke<TranscriptEvent[]>('exec_transcript_tail', { execId, homeId, maxLines })
}

/** 模型列表条目（`<binary> models` 解析产物；group 供下拉分组）。 */
export interface HarnessModelEntry {
  id: string
  group: string | null
}

/** 执行弹窗「模型」下拉数据面：失败/超时抛可读错误（调用方降级隐藏）。 */
export async function harnessListModels(harness: string): Promise<HarnessModelEntry[]> {
  const result = await invoke<{ models?: HarnessModelEntry[] }>('harness_list_models', { harness })
  return result.models ?? []
}

/** 终端登录（F-ACC-13）：在本机 Terminal 里启动 harness 的交互登录命令
 * （omp 裸跑 TUI /login；opencode 走 auth login）。凭据落在 harness 自己
 * 的全局存储，OMT 不经手。 */
export async function harnessOpenLogin(harness: string): Promise<void> {
  await invoke<void>('harness_open_login', { harness })
}
