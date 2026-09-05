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
  private readonly running = new Map<string, { id: string; homeId?: string; info: RunningInfo }>()

  private key(id: string, homeId?: string): string {
    return JSON.stringify([homeId ?? null, id])
  }

  // Optional home supports legacy registry-only callers. Host paths must pass
  // the resolved home, never cwd or an ID-derived guess.
  start(id: string, sessionId: string, sessionLabel: string, lineage: RunningLineage = {}, homeId?: string): void {
    this.running.set(this.key(id, homeId), {
      id,
      ...(homeId !== undefined ? { homeId } : {}),
      info: {
        sessionId,
        sessionLabel,
        since: new Date().toISOString(),
        ...(lineage.parentSessionId !== undefined ? { parentSessionId: lineage.parentSessionId } : {}),
        ...(lineage.isSubagent === true ? { isSubagent: true } : {}),
      },
    })
  }

  // Unscoped cleanup only owns legacy entries; it cannot erase scoped work.
  stop(id: string, homeId?: string): void {
    this.running.delete(this.key(id, homeId))
  }

  get(id: string, homeId?: string): RunningInfo | undefined {
    if (homeId !== undefined) return this.running.get(this.key(id, homeId))?.info
    // Preserve unambiguous legacy reads without selecting a colliding home.
    const matches = [...this.running.values()].filter(entry => entry.id === id)
    return matches.length === 1 ? matches[0]!.info : undefined
  }

  /** Reverse lookup (idle/disposed hooks): retain home identity for cleanup. */
  forSession(sessionId: string): { id: string; homeId?: string; info: RunningInfo }[] {
    return [...this.running.values()].filter(entry => entry.info.sessionId === sessionId)
  }
}
