/**
 * RunningRegistry: which session is executing which ticket. A ticket is
 * "running" from the moment execution starts (the 执行 button's execute RPC,
 * or a tool call setting it in_progress — the latter arrives with the model
 * round-trip, which is acceptable) until it is done or archived. In-memory:
 * execution is transient by nature.
 */
/**
 * Executor lineage snapshot (TICKET-0066): read once from the session
 * header at start() time (parentSession + origin === 'subagent'), so the
 * UI can render 「父会话 ↳ subagent」 even after the executor ends.
 */
export interface RunningLineage {
  readonly parentSessionId?: string
  readonly isSubagent?: boolean
}

export interface RunningInfo {
  readonly sessionId: string
  readonly sessionLabel: string
  readonly since: string
  readonly parentSessionId?: string
  readonly isSubagent?: boolean
}

/**
 * Execution-ending changes (shared by the tool and RPC update paths):
 * done/blocked/skipped/archive all end active execution, so the running
 * mark clears.
 */
export function endsExecution(status?: string, archived?: boolean): boolean {
  return status === 'done' || status === 'blocked' || status === 'skipped' || archived === true
}

/** Lineage snapshot from a session-header-shaped object (TICKET-0066). */
export function lineageOfHeader(header: { parentSession?: string; origin?: string } | undefined): RunningLineage {
  return {
    ...(header?.parentSession !== undefined ? { parentSessionId: header.parentSession } : {}),
    ...(header?.origin === 'subagent' ? { isSubagent: true } : {}),
  }
}

export class RunningRegistry {
  private readonly running = new Map<string, RunningInfo>()

  start(id: string, sessionId: string, sessionLabel: string, lineage: RunningLineage = {}): void {
    this.running.set(id, {
      sessionId,
      sessionLabel,
      since: new Date().toISOString(),
      ...(lineage.parentSessionId !== undefined ? { parentSessionId: lineage.parentSessionId } : {}),
      ...(lineage.isSubagent === true ? { isSubagent: true } : {}),
    })
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
