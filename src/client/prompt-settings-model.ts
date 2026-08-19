/** Settings-page model for extra prompt + bound skills. No OmtController. */
import type { BoundSkillRow } from '../host/prompt-settings.ts'

export type CatalogStatus = 'loading' | 'ready' | 'empty' | 'error'

export interface PromptSettingsView {
  extraPrompt: string
  skills: BoundSkillRow[]
  catalogStatus: CatalogStatus
  catalogError: string
  writeError: string
}

export const INITIAL_PROMPT_SETTINGS_VIEW: PromptSettingsView = {
  extraPrompt: '',
  skills: [],
  catalogStatus: 'loading',
  catalogError: '',
  writeError: '',
}

export function boundNamesOf(skills: readonly BoundSkillRow[]): string[] {
  return skills.filter(row => row.bound).map(row => row.name)
}

export function toggleBound(skills: readonly BoundSkillRow[], name: string): BoundSkillRow[] {
  return skills.map(row => row.name === name ? { ...row, bound: !row.bound } : row)
}

export function catalogStatusOf(skills: readonly BoundSkillRow[]): Exclude<CatalogStatus, 'loading' | 'error'> {
  return skills.length === 0 ? 'empty' : 'ready'
}

export class PromptSettingsModel {
  private saved: PromptSettingsView = { ...INITIAL_PROMPT_SETTINGS_VIEW }
  view: PromptSettingsView = { ...INITIAL_PROMPT_SETTINGS_VIEW }

  constructor(
    private readonly loadCatalog: () => Promise<{ extraPrompt: string; skills: BoundSkillRow[] }>,
    private readonly persist: (next: { extraPrompt: string; boundSkillNames: string[] }) => Promise<void>,
    private readonly emit: (view: PromptSettingsView) => void = () => {},
  ) {}

  private commit(view: PromptSettingsView): void {
    this.view = view
    this.emit(view)
  }

  async load(): Promise<void> {
    this.commit({ ...this.view, catalogStatus: 'loading', catalogError: '', writeError: '' })
    try {
      const data = await this.loadCatalog()
      const next: PromptSettingsView = {
        extraPrompt: data.extraPrompt,
        skills: data.skills,
        catalogStatus: catalogStatusOf(data.skills),
        catalogError: '',
        writeError: '',
      }
      this.saved = next
      this.commit(next)
    } catch (error) {
      this.commit({
        ...this.view,
        catalogStatus: 'error',
        catalogError: error instanceof Error ? error.message : String(error),
      })
    }
  }

  setDraftExtra(extraPrompt: string): void {
    this.commit({ ...this.view, extraPrompt, writeError: '' })
  }

  async setExtraPrompt(extraPrompt: string): Promise<void> {
    const previous = this.view
    this.commit({ ...this.view, extraPrompt, writeError: '' })
    try {
      await this.persist({ extraPrompt, boundSkillNames: boundNamesOf(this.view.skills) })
      this.saved = this.view
    } catch (error) {
      this.commit({
        ...previous,
        writeError: error instanceof Error ? error.message : String(error),
      })
    }
  }

  async toggle(name: string): Promise<void> {
    const previous = this.view
    const skills = toggleBound(this.view.skills, name)
    this.commit({ ...this.view, skills, writeError: '' })
    try {
      await this.persist({ extraPrompt: this.view.extraPrompt, boundSkillNames: boundNamesOf(skills) })
      this.saved = this.view
    } catch (error) {
      this.commit({
        ...previous,
        writeError: error instanceof Error ? error.message : String(error),
      })
    }
  }
}
