/**
 * OMT host plugin (node half). Wires the data core (OMT home resolution:
 * plugin config > OMT_HOME env > ~/.omt) and registers the omt_* tool family.
 */
import { homedir } from 'node:os'
import { join } from 'node:path'
import type { Context } from '@deepseek-ai/cordis'
import Schema from '@deepseek-ai/schemastery'
import { ChangeHub } from './host/changes.ts'
import { registerOmtEvents } from './host/events.ts'
import { OmtCorePool } from './host/pool.ts'
import { RecentRegistry } from './host/recent.ts'
import { RunningRegistry } from './host/running.ts'
import { registerOmtRpc } from './host/rpc.ts'
import { registerOmtSkill } from './host/skill.ts'
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

export function apply(ctx: Context, config: Config): void {
  const globalHome = resolveHome(config.home)
  // Workspace-aware pool: a workspace root with its own `.omt/` wins, the
  // global home is the fallback. Cores open lazily per home (lazy
  // node:sqlite import, possible first-run reindex).
  const pool = new OmtCorePool(globalHome)
  console.log(`[omt] host plugin loaded (global home: ${globalHome}; workspace .omt/ wins when present)`)
  const recent = new RecentRegistry()
  const running = new RunningRegistry()
  recent.attachPersistence({
    load: async sessionId => (await pool.coreFor(undefined)).getSessionRecent(sessionId),
    save: async (sessionId, ids) => {
      ;(await pool.coreFor(undefined)).setSessionRecent(sessionId, ids)
    },
  })
  const changes = new ChangeHub()
  registerOmtTools(ctx, pool, (sessionId, id) => recent.touch(sessionId, id), home => changes.bump(home), running)
  registerOmtSkill(ctx)
  registerOmtRpc(ctx, pool, recent, changes, running)
  registerOmtEvents(ctx, changes)
}
