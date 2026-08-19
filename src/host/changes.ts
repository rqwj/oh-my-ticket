/**
 * ChangeHub: in-process broadcast of OMT data changes (tool executes, RPC
 * mutations). Carried to the browser over the `/omt/events` SSE route so the
 * drawer tree / doc panel refresh immediately after any change. Payloads are
 * minimal — the client refetches; this is a notification, not a data channel.
 */
export interface OmtChangeEvent {
  readonly version: number
  /** Home whose data changed (informational; clients refetch their view). */
  readonly home: string
}

export class ChangeHub {
  private readonly listeners = new Set<(event: OmtChangeEvent) => void>()
  private version = 0

  bump(home: string): void {
    this.version += 1
    const event: OmtChangeEvent = { version: this.version, home }
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
