/**
 * Shared plugin→session message payloads (TICKET-0062/0063/0065 + the RPC
 * run-start nudge): every host surface that speaks to a session sends the
 * same plugin-sourced user message shape. Delivery is best-effort — a dead
 * session or throwing channel is contained (warn), never propagated.
 */
import { randomUUID } from 'node:crypto'

/** Plugin-sourced user message (wire shape shared by every delivery site). */
export function pluginUserMessage(text: string): unknown {
  return {
    id: randomUUID(),
    role: 'user',
    content: [{ type: 'text', text }],
    source: { kind: 'plugin', plugin: 'oh-my-ticket' },
  }
}

/** Structural face of a followup target (idle/disposed hooks). */
export interface FollowupTargetLike {
  readonly id: string
  followup(message: unknown): void
}

/**
 * try/warn followup delivery shared by the idle and disposed hooks: builds
 * the plugin message and queues it, containing any throw as a warn.
 */
export function safeFollowup(
  agent: FollowupTargetLike,
  text: string,
  warn: (message: string, error: unknown) => void,
): void {
  try {
    agent.followup(pluginUserMessage(text))
  } catch (error: unknown) {
    warn(`could not queue followup for agent "${agent.id}"`, error)
  }
}
