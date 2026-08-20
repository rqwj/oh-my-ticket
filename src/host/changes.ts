/**
 * ChangeHub: in-process broadcast of OMT data changes (tool executes, RPC
 * mutations). Carried to the browser over the `/omt/events` SSE route so the
 * drawer tree / doc panel refresh immediately after any change. Payloads are
 * minimal — the client refetches; this is a notification, not a data channel.
 *
 * Run dimension (TICKET-0071): bumps caused by a run/item transition carry
 * an optional `run` hint so run views can refresh (or filter) without a
 * second handshake. The field is additive — older clients ignore it.
 */
import type { OmtCore } from './core.ts'

/** Run-dimension hint on a change event (which run / item moved). */
export interface OmtRunChangeHint {
  readonly id: string
  readonly kind: 'run' | 'item'
  /** Item-level changes: the member node that transitioned. */
  readonly nodeId?: string
}

export interface OmtChangeEvent {
  readonly version: number
  /** Home whose data changed (informational; clients refetch their view). */
  readonly home: string
  readonly run?: OmtRunChangeHint
}

export class ChangeHub {
  private readonly listeners = new Set<(event: OmtChangeEvent) => void>()
  private version = 0

  bump(home: string, run?: OmtRunChangeHint): void {
    this.version += 1
    const event: OmtChangeEvent = { version: this.version, home, ...(run !== undefined ? { run } : {}) }
    for (const listener of this.listeners) {
      try {
        listener(event)
      } catch {
        // A broken subscriber must not break the mutating call.
      }
    }
  }

  subscribe(listener: (event: OmtChangeEvent) => void): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }
}

/**
 * Bridge one core's run events into hub bumps (TICKET-0071): any run/item
 * transition — from tools, RPC, or the hooks — refreshes run views over SSE.
 * Attach once per opened core (pool onCoreOpened); returns the unsubscribe.
 */
export function bridgeRunEvents(core: OmtCore, hub: ChangeHub): () => void {
  return core.onRunEvent(event => {
    hub.bump(core.home, {
      id: event.run.id,
      kind: event.kind,
      ...(event.item !== undefined ? { nodeId: event.item.node_id } : {}),
    })
  })
}
