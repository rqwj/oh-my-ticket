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

/**
 * Execution-ending changes (shared by the tool and RPC update paths):
 * done/blocked/skipped/archive all end active execution, so the running
 * mark clears.
 */
export function endsExecution(status?: string, archived?: boolean): boolean {
  return status === 'done' || status === 'blocked' || status === 'skipped' || archived === true
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

  /** Reverse lookup (idle hook): every ticket currently running under one session. */
  forSession(sessionId: string): { id: string; info: RunningInfo }[] {
    const matches: { id: string; info: RunningInfo }[] = []
    for (const [id, info] of this.running) {
      if (info.sessionId === sessionId) matches.push({ id, info })
    }
    return matches
  }
}
