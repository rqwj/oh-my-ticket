/** Live prompt settings + installed-skill catalog helpers. */

export const OMT_PROMPT_SETTINGS_NS = 'oh-my-ticket-prompt'

export interface PromptSettings {
  extraPrompt: string
  boundSkillNames: string[]
}

export const DEFAULT_PROMPT_SETTINGS: PromptSettings = {
  extraPrompt: '',
  boundSkillNames: [],
}

export interface SkillCatalogEntry {
  name: string
  description: string
}

export interface BoundSkillRow {
  name: string
  description: string
  bound: boolean
  missing: boolean
}

export interface SkillListRow {
  readonly name: string
  readonly description: string
  readonly invocation?: { readonly modelInvocable?: boolean }
}

/** Keep every installed skill unless it explicitly opts out of model use. */
export function selectBindableSkills(skills: readonly SkillListRow[]): SkillCatalogEntry[] {
  return skills
    .filter(skill => skill.invocation?.modelInvocable !== false)
    .map(skill => ({ name: skill.name, description: skill.description }))
}

export interface AgentCwdFace {
  readonly session?: { readonly header?: { readonly cwd?: string } }
}

export function resolveLiveAgent<T>(
  sessionId: string | undefined,
  get: (id: string) => T | undefined,
  list: () => readonly T[],
): T | undefined {
  if (sessionId !== undefined) {
    const hit = get(sessionId)
    if (hit !== undefined) return hit
  }
  return list()[0]
}

/** Session cwds first, then a no-cwd pass for runtime / user-global skills. */
export function catalogLookupsFromAgents(
  agents: readonly AgentCwdFace[],
  preferredCwd?: string,
): Array<{ cwd?: string }> {
  const cwds: string[] = []
  const add = (cwd: string | undefined): void => {
    if (cwd === undefined || cwd === '') return
    if (!cwds.includes(cwd)) cwds.push(cwd)
  }
  add(preferredCwd)
  for (const agent of agents) add(agent.session?.header?.cwd)
  return [...cwds.map(cwd => ({ cwd })), {}]
}

export async function collectBindableCatalog(
  list: (lookup: { cwd?: string }) => Promise<readonly SkillListRow[]>,
  lookups: readonly { cwd?: string }[],
): Promise<SkillCatalogEntry[]> {
  const byName = new Map<string, SkillCatalogEntry>()
  for (const lookup of lookups) {
    const skills = await list(lookup.cwd === undefined ? {} : { cwd: lookup.cwd })
    for (const skill of selectBindableSkills(skills)) {
      if (!byName.has(skill.name)) byName.set(skill.name, skill)
    }
  }
  return [...byName.values()]
}

/** Merge the live catalog with bound names so stale binds stay visible. */
export function describeBoundCatalog(catalog: readonly SkillCatalogEntry[], bound: readonly string[]): BoundSkillRow[] {
  const boundSet = new Set(bound)
  const rows: BoundSkillRow[] = catalog.map(item => ({
    name: item.name,
    description: item.description,
    bound: boundSet.has(item.name),
    missing: false,
  }))
  const seen = new Set(catalog.map(item => item.name))
  for (const name of bound) {
    if (seen.has(name)) continue
    rows.push({ name, description: '', bound: true, missing: true })
  }
  return rows
}

export class InstalledSkillCache {
  private namesInternal: string[] = []

  names(): readonly string[] {
    return this.namesInternal
  }

  async refresh(list: () => Promise<readonly { name: string; invocation?: { modelInvocable?: boolean } }[]>): Promise<void> {
    const skills = await list()
    this.namesInternal = selectBindableSkills(skills).map(skill => skill.name)
  }
}
