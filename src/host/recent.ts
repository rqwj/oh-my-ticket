/**
 * RecentRegistry: per-session "recently touched" ticket ids, fed by RPC
 * reads/writes (the @ codec's get, UI actions) and by omt_* tool executes.
 * Powers the conversation turn-tail "相关 ticket" list.
 *
 * Persistence: an optional delegate mirrors every touch into daemon-owned
 * storage (`ui/recent-get|set`, global-home scoped, key = session id) and
 * lazily reloads it after a host restart, so the list survives process
 * restarts as well as page refreshes (TICKET-0019).
 */
export interface RecentPersistence {
  /** Load the persisted id list (most-recent-first); undefined = none. */
  load(sessionId: string): Promise<string[] | undefined>
  /** Persist the current list (fire-and-forget safe). */
  save(sessionId: string, ids: readonly string[]): Promise<void>
}

export class RecentRegistry {
  private readonly bySession = new Map<string, string[]>()
  private persistence: RecentPersistence | undefined

  /** Cap per session; the tail list is a glance, not a history. */
  private static readonly CAP = 10

  /** Attach disk persistence (called once at plugin wiring). */
  attachPersistence(persistence: RecentPersistence): void {
    this.persistence = persistence
  }

  touch(sessionId: string | undefined, id: string): void {
    if (sessionId === undefined || sessionId === '') return
    const list = (this.bySession.get(sessionId) ?? []).filter(existing => existing !== id)
    list.push(id)
    while (list.length > RecentRegistry.CAP) list.shift()
    this.bySession.set(sessionId, list)
    if (this.persistence !== undefined) {
      const ids = [...list].reverse()
      void this.persistence.save(sessionId, ids).catch(() => {})
    }
  }

  /** Most-recent-first ids for one session (in-memory view). */
  list(sessionId: string): string[] {
    return [...(this.bySession.get(sessionId) ?? [])].reverse()
  }

  /**
   * Resolve a session's list, falling back to the persisted copy on first
   * access after a host restart (and caching it back into memory).
   */
  async resolve(sessionId: string): Promise<string[]> {
    const cached = this.bySession.get(sessionId)
    if (cached !== undefined) return [...cached].reverse()
    const persisted = await this.persistence?.load(sessionId).catch(() => undefined)
    if (persisted === undefined) return []
    this.bySession.set(sessionId, [...persisted].reverse())
    return persisted
  }
}
