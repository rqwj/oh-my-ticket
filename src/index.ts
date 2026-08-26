/**
 * OMT host plugin (node half). U7a: a thin DSH adapter over the omt-daemon
 * runtime — no direct SQLite/Markdown access remains. The plugin wires the
 * OmtService (@omt/client-ts lifecycle: discover-or-spawn via OMT_DAEMON or
 * PATH, handshake kind:"dsh", capped-backoff reconnect, disposal), exposes it
 * as the optional-inject Cordis service `omt` (ctx.get('omt')), and registers
 * the omt_* tool family, the /omt RPC channel, SSE change push, and the three
 * session hooks — every data op routed through the service.
 */
import { homedir } from 'node:os'
import { join } from 'node:path'
import type { Context } from '@deepseek-ai/cordis'
import Schema from '@deepseek-ai/schemastery'
import type { AgentsLike } from './host/agents-like.ts'
import { registerOmtDisposedHook } from './host/disposed-hook.ts'
import { registerOmtEvents } from './host/events.ts'
import { registerOmtIdleHook } from './host/idle-hook.ts'
import { createOmtRunNotifier } from './host/notify-hook.ts'
import { RecentRegistry } from './host/recent.ts'
import { RunningRegistry } from './host/running.ts'
import { registerOmtRpc } from './host/rpc.ts'
import { registerOmtRunsSkill, registerOmtSkill } from './host/skill.ts'
import { OmtService, RECENT_SHARED_KEY } from './host/service.ts'
import { registerOmtTools } from './host/tools.ts'

export const name = 'oh-my-ticket'
export const inject = ['tools', 'skills', 'connection', 'agents', 'userQuestions', 'webServer']

export interface Config {
  /** OMT home directory; empty falls back to OMT_HOME env, then ~/.omt. */
  home: string
}

export const Config: Schema<Config> = Schema.object({
  home: Schema.string().default('').description('OMT home directory (defaults to OMT_HOME env or ~/.omt)'),
})

/** Resolution order: plugin config > OMT_HOME env > ~/.omt. */
export function resolveHome(configHome: string, env: NodeJS.ProcessEnv = process.env): string {
  const configured = configHome.trim()
  if (configured !== '') return configured
  const fromEnv = env.OMT_HOME?.trim()
  if (fromEnv !== undefined && fromEnv !== '') return fromEnv
  return join(homedir(), '.omt')
}

/** Structural agent shape used here: just the session header (see rpc.ts). */
type HeaderAgent = { session: { header: { cwd?: string } } }

export function apply(ctx: Context, config: Config): void {
  const globalHome = resolveHome(config.home)
  const agents = (ctx as unknown as { agents?: AgentsLike<HeaderAgent> }).agents

  // Runtime boundary (U7a): one client lifecycle per plugin instance. The
  // spawned/discovered daemon learns the global home from OMT_HOME (or its
  // own --home args); this adapter resolves workspace homes against the
  // registry learned at handshake.
  const service = new OmtService({ name: 'oh-my-ticket-dsh' })

  // Optional-inject Cordis service `omt` (ctx.get('omt')); tied to this
  // fiber via reflect.provide and disposed with it.
  const withReflect = ctx as unknown as { reflect?: { provide(name: string, value: unknown): () => void } }
  const disposeService = withReflect.reflect?.provide('omt', service)

  // Run-notification closure (TICKET-0065): subscribes to the service's
  // run-event stream (daemon envelopes), so notifications fire for tools,
  // RPC, and CLI mutations alike.
  const notifier = createOmtRunNotifier(ctx)

  console.log(`[omt] host adapter loaded (global home: ${globalHome}; data ops route through omt-daemon)`)
  const recent = new RecentRegistry()
  const running = new RunningRegistry()
  // Persistence rides ui/recent-get|set on the GLOBAL home (TICKET-0019);
  // bare ids are re-resolved by ownership on read. U4/R4: every surface
  // persists under the ONE shared key 'recent' (KD3's sole cross-surface
  // exception) — the in-memory registry stays per-session, but storage is
  // converged; legacy per-session keys are left as orphans by design.
  recent.attachPersistence({
    load: () => service.recentGet(RECENT_SHARED_KEY),
    save: async (_sessionId, ids) => {
      await service.recentSet(RECENT_SHARED_KEY, ids)
    },
  })
  // SSE refresh is fed by daemon event envelopes through the service-owned
  // hub (single source of truth); only reindex — which rebuilds silently —
  // bumps explicitly via the RPC layer.
  registerOmtTools(ctx, service, (sessionId, id) => recent.touch(sessionId, id), undefined, running)
  registerOmtSkill(ctx)
  registerOmtRunsSkill(ctx)
  registerOmtRpc(ctx, service, recent, service.hub, running)
  registerOmtEvents(ctx, service.hub)
  registerOmtIdleHook(ctx, service, running)
  registerOmtDisposedHook(ctx, service, running)

  const detachNotifier = notifier.attach(service)

  // Disposal binds to the fiber: detach listeners, drop the Cordis
  // registration, close the client (credentials die server-side).
  const withEffect = ctx as unknown as { effect?: (body: () => Generator<() => void, void, unknown>) => void }
  withEffect.effect?.call(ctx, function* () {
    yield () => {
      detachNotifier()
      disposeService?.()
      void service.close().catch(() => {})
    }
  })
}
