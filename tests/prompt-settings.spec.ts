import { describe, expect, it } from 'vitest'
import { DEFAULT_PROMPT_SETTINGS, describeBoundCatalog, InstalledSkillCache } from '../src/host/prompt-settings.ts'

describe('describeBoundCatalog', () => {
  it('flags bound and missing names', () => {
    const rows = describeBoundCatalog(
      [{ name: 'ce-plan', description: 'plan' }, { name: 'omt', description: 'omt' }],
      ['ce-plan', 'gone'],
    )
    expect(rows).toEqual([
      { name: 'ce-plan', description: 'plan', bound: true, missing: false },
      { name: 'omt', description: 'omt', bound: false, missing: false },
      { name: 'gone', description: '', bound: true, missing: true },
    ])
  })
})

describe('InstalledSkillCache', () => {
  it('keeps model-invocable names and drops the rest', async () => {
    const cache = new InstalledSkillCache()
    await cache.refresh(async () => [
      { name: 'ce-plan', invocation: { modelInvocable: true } },
      { name: 'hidden', invocation: { modelInvocable: false } },
      { name: 'plain' },
    ])
    expect(cache.names()).toEqual(['ce-plan', 'plain'])
  })
})

describe('defaults', () => {
  it('starts with empty extra prompt and binds', () => {
    expect(DEFAULT_PROMPT_SETTINGS).toEqual({ extraPrompt: '', boundSkillNames: [] })
  })
})
