import { describe, expect, it } from 'vitest'
import { boundNamesOf, catalogStatusOf, PromptSettingsModel, toggleBound } from '../src/client/prompt-settings-model.ts'

const rows = [
  { name: 'ce-plan', description: 'plan', bound: false, missing: false },
  { name: 'gone', description: '', bound: true, missing: true },
]

describe('toggleBound', () => {
  it('adds and removes a name without dropping missing rows', () => {
    const checked = toggleBound(rows, 'ce-plan')
    expect(boundNamesOf(checked)).toEqual(['ce-plan', 'gone'])
    const unchecked = toggleBound(checked, 'gone')
    expect(boundNamesOf(unchecked)).toEqual(['ce-plan'])
    expect(unchecked.find(row => row.name === 'gone')?.missing).toBe(true)
  })
})

describe('catalogStatusOf', () => {
  it('is empty when the catalog has no rows', () => {
    expect(catalogStatusOf([])).toBe('empty')
    expect(catalogStatusOf(rows)).toBe('ready')
  })
})

describe('PromptSettingsModel', () => {
  it('loads extra prompt and skills', async () => {
    const model = new PromptSettingsModel(async () => ({ extraPrompt: '中文', skills: rows }), async () => {})
    await model.load()
    expect(model.view.extraPrompt).toBe('中文')
    expect(model.view.catalogStatus).toBe('ready')
    expect(model.view.skills[1]?.missing).toBe(true)
  })

  it('keeps extra prompt editable after a catalog error', async () => {
    const model = new PromptSettingsModel(async () => { throw new Error('rpc down') }, async () => {})
    await model.load()
    expect(model.view.catalogStatus).toBe('error')
    expect(model.view.catalogError).toBe('rpc down')
    await model.setExtraPrompt('仍可改')
    expect(model.view.extraPrompt).toBe('仍可改')
  })

  it('reverts a failed write and does not treat the draft as saved', async () => {
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async () => { throw new Error('persist failed') },
    )
    await model.load()
    await model.toggle('ce-plan')
    expect(model.view.skills.find(row => row.name === 'ce-plan')?.bound).toBe(false)
    expect(model.view.writeError).toBe('persist failed')
  })
})
