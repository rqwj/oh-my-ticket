/**
 * U9/U10 frontend bridge wrapper: the ONLY daemon path from the renderer
 * (KTD6 — the socket lives Rust-side). Calls forward through
 * `omt_call`; daemon event envelopes arrive over the `omt://event`
 * Tauri channel.
 */
import { Channel, invoke } from '@tauri-apps/api/core'
import { getVersion } from '@tauri-apps/api/app'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { AgentsListResult } from './types'

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

export interface SessionStartEvent {
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

export interface RawTranscriptEvent {
  execId: string
  seq: number
  ts: string
  kind: 'raw'
  stream: 'stdout' | 'stderr'
  text: string
}

export interface ExitTranscriptEvent {
  execId: string
  seq: number
  ts: string
  kind: 'exit'
  status: 'exited' | 'failed' | 'killed'
  code: number | null
  killReason: string | null
  durationMs: number
}

export type TranscriptEvent = SessionStartEvent | RawTranscriptEvent | ExitTranscriptEvent

/** agents_list（F-U4a-8）：pull 契约 = tab 首开 / window focus / 派发后主动拉。 */
export async function agentsList(homeId: string): Promise<AgentsListResult> {
  return invoke<AgentsListResult>('agents_list', { homeId })
}

/** 终止一个 exec（幂等：已终态/不存在的 execId 返回成功）。 */
export async function execStop(execId: string): Promise<void> {
  return invoke<void>('exec_stop', { execId })
}

/** Orphan 强制 kill（§3.2：Orphan 只读尾随、不可 exec_stop）。pid 取自 transcript 的 session_start 事件。 */
export async function execKillOrphan(pid: number): Promise<void> {
  return invoke<void>('exec_kill_orphan', { pid })
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
export async function execStart<T = unknown>(options: {
  homeId: string
  homePath: string
  nodeId: string
  harness: string
  unattended: boolean
  channel: Channel<T>
}): Promise<ExecStartOutcome> {
  return invoke<ExecStartOutcome>('exec_start', {
    homeId: options.homeId,
    homePath: options.homePath,
    nodeId: options.nodeId,
    harness: options.harness,
    unattended: options.unattended,
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
