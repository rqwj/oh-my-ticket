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

export interface PromptSettingsPatch {
  readonly extraPrompt?: string
  readonly boundSkillNames?: string[]
}

export class PromptSettingsModel {
  private saved: Pick<PromptSettingsView, 'extraPrompt' | 'skills'> = {
    extraPrompt: INITIAL_PROMPT_SETTINGS_VIEW.extraPrompt,
    skills: INITIAL_PROMPT_SETTINGS_VIEW.skills,
  }
  private writeQueue: Promise<void> = Promise.resolve()
  private extraWriteRevision = 0
  private skillsWriteRevision = 0
  private loadRevision = 0
  view: PromptSettingsView = { ...INITIAL_PROMPT_SETTINGS_VIEW }

  constructor(
    private readonly loadCatalog: () => Promise<{ extraPrompt: string; skills: BoundSkillRow[] }>,
    private readonly persist: (next: PromptSettingsPatch) => Promise<void>,
    private readonly emit: (view: PromptSettingsView) => void = () => {},
  ) {}

  private commit(view: PromptSettingsView): void {
    this.view = view
    this.emit(view)
  }

  private enqueuePersist(next: PromptSettingsPatch): Promise<void> {
    const write = this.writeQueue.then(async () => await this.persist(next))
    this.writeQueue = write.catch(() => {})
    return write
  }

  async load(): Promise<void> {
    const revision = ++this.loadRevision
    this.commit({ ...this.view, catalogStatus: 'loading', catalogError: '', writeError: '' })
    try {
      const data = await this.loadCatalog()
      if (revision !== this.loadRevision) return
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
      if (revision !== this.loadRevision) return
      this.commit({
        ...this.view,
        catalogStatus: 'error',
        catalogError: error instanceof Error ? error.message : String(error),
      })
    }
  }

  setDraftExtra(extraPrompt: string): void {
    if (this.view.catalogStatus === 'loading') return
    this.commit({ ...this.view, extraPrompt, writeError: '' })
  }

  async setExtraPrompt(extraPrompt: string): Promise<void> {
    if (this.view.catalogStatus === 'loading') return
    const next = { ...this.view, extraPrompt, writeError: '' }
    this.commit(next)
    const revision = ++this.extraWriteRevision
    try {
      await this.enqueuePersist({ extraPrompt })
      this.saved = { ...this.saved, extraPrompt }
    } catch (error) {
      if (revision !== this.extraWriteRevision) return
      this.commit({
        ...this.view,
        extraPrompt: this.saved.extraPrompt,
        writeError: error instanceof Error ? error.message : String(error),
      })
    }
  }

  async toggle(name: string): Promise<void> {
    if (this.view.catalogStatus === 'loading') return
    const skills = toggleBound(this.view.skills, name)
    const next = { ...this.view, skills, writeError: '' }
    this.commit(next)
    const revision = ++this.skillsWriteRevision
    try {
      await this.enqueuePersist({ boundSkillNames: boundNamesOf(skills) })
      this.saved = { ...this.saved, skills }
    } catch (error) {
      if (revision !== this.skillsWriteRevision) return
      this.commit({
        ...this.view,
        skills: this.saved.skills,
        writeError: error instanceof Error ? error.message : String(error),
      })
    }
  }
}
