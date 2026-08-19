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
import { DEFAULT_PROMPT_SETTINGS, InstalledSkillCache, type PromptSettings } from './host/prompt-settings.ts'
import { registerOmtPrompt } from './host/prompt.ts'
import { RecentRegistry } from './host/recent.ts'
import { registerOmtRpc } from './host/rpc.ts'
import { RunningRegistry } from './host/running.ts'
import { registerOmtSkill } from './host/skill.ts'
import { registerOmtTools } from './host/tools.ts'

export const name = 'oh-my-ticket'
export const inject = ['tools', 'skills', 'connection', 'agents', 'userQuestions', 'webServer', 'systemPrompt']

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

function normalizePromptSettings(value: unknown): PromptSettings {
  const raw = value as Partial<PromptSettings> | undefined
  const extra = typeof raw?.extraPrompt === 'string' ? raw.extraPrompt : ''
  const bound = Array.isArray(raw?.boundSkillNames) ? raw.boundSkillNames.filter((name): name is string => typeof name === 'string') : []
  return { extraPrompt: extra, boundSkillNames: bound }
}

function attachPromptSettings(ctx: Context, setValue: (value: PromptSettings) => void): void {
  const inject = (ctx as unknown as { inject?: (deps: string[], cb: (scoped: Context) => void) => void }).inject
  if (typeof inject !== 'function') return
  inject.call(ctx, ['settings'], scoped => {
    const settings = (scoped as unknown as { settings?: { register: (ns: string, schema: unknown, opts?: unknown) => { get(): unknown; watch?: (fn: () => void) => void } } }).settings
    if (settings === undefined) return
    const schema = Schema.object({
      extraPrompt: Schema.string().default(''),
      boundSkillNames: Schema.array(Schema.string()).default([]),
    })
    const scope = settings.register('oh-my-ticket-prompt', schema, { base: DEFAULT_PROMPT_SETTINGS, applies: 'live' })
    const pull = () => setValue(normalizePromptSettings(scope.get()))
    pull()
    scope.watch?.(pull)
  })
}

export function apply(ctx: Context, config: Config): void {
  const globalHome = resolveHome(config.home)
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
  const installed = new InstalledSkillCache()
  let promptSettings: PromptSettings = { ...DEFAULT_PROMPT_SETTINGS }
  const promptInputs = () => ({
    extraPrompt: promptSettings.extraPrompt,
    boundSkillNames: promptSettings.boundSkillNames,
    installedNames: [...installed.names()],
  })
  registerOmtTools(ctx, pool, (sessionId, id) => recent.touch(sessionId, id), home => changes.bump(home), running)
  registerOmtSkill(ctx)
  registerOmtPrompt(ctx, promptInputs)
  registerOmtRpc(ctx, pool, recent, changes, running, {
    getSettings: () => promptSettings,
    listCatalog: async () => {
      const skills = await ctx.skills.list()
      return skills
        .filter(skill => skill.invocation.modelInvocable)
        .map(skill => ({ name: skill.name, description: skill.description }))
    },
  })
  registerOmtEvents(ctx, changes)
  const refreshInstalled = () => installed.refresh(() => ctx.skills.list())
  void refreshInstalled()
  ctx.on('skills/change', () => { void refreshInstalled() })
  attachPromptSettings(ctx, value => { promptSettings = value })
}
