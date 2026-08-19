/**
 * RunningRegistry: which session is executing which ticket. A ticket is
 * "running" from the moment execution starts (the 执行 button's execute RPC,
 * or a tool call setting it in_progress — the latter arrives with the model
 * round-trip, which is acceptable) until it is done or archived. In-memory:
 * execution is transient by nature.
 */
export interface RunningInfo {
  readonly sessionId: string
  readonly sessionLabel: string
  readonly since: string
}

export class RunningRegistry {
  private readonly running = new Map<string, RunningInfo>()

  start(id: string, sessionId: string, sessionLabel: string): void {
    this.running.set(id, { sessionId, sessionLabel, since: new Date().toISOString() })
  }

  stop(id: string): void {
    this.running.delete(id)
  }

  get(id: string): RunningInfo | undefined {
    return this.running.get(id)
  }
}
