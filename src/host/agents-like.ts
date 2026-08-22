/**
 * Shared structural face of ctx.agents (the real registry type lives in the
 * host app; this external plugin casts structurally). Parameterized by the
 * agent shape each surface needs — from a bare session header up to a
 * followup/inject delivery target.
 */
export interface AgentsLike<TAgent> {
  get(id: string): TAgent | undefined
  list(): { id: string }[]
}
