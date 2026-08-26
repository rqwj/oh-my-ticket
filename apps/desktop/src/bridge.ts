/**
 * U9/U10 frontend bridge wrapper: the ONLY daemon path from the renderer
 * (KTD6 — the socket lives Rust-side). Calls forward through
 * `omt_call`; daemon event envelopes arrive over the `omt://event`
 * Tauri channel.
 */
import { invoke } from '@tauri-apps/api/core'
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
