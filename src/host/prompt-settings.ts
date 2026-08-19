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
    this.namesInternal = skills
      .filter(skill => skill.invocation?.modelInvocable !== false)
      .map(skill => skill.name)
  }
}
