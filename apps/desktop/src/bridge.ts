/**
 * U9/U10 frontend bridge wrapper: the ONLY daemon path from the renderer
 * (KTD6 — the socket lives Rust-side). Calls forward through
 * `omt_call`; daemon event envelopes arrive over the `omt://event`
 * Tauri channel.
 */
import { invoke } from '@tauri-apps/api/core'
import { getVersion } from '@tauri-apps/api/app'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

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
