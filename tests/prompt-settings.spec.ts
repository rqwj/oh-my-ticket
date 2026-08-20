import { describe, expect, it } from 'vitest'
import { catalogLookupsFromAgents, collectBindableCatalog, DEFAULT_PROMPT_SETTINGS, describeBoundCatalog, InstalledSkillCache, resolveLiveAgent, selectBindableSkills } from '../src/host/prompt-settings.ts'

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

describe('selectBindableSkills', () => {
  it('keeps installed skills unless they opt out of model invocation', () => {
    const rows = selectBindableSkills([
      { name: 'ce-plan', description: 'plan', invocation: { modelInvocable: true } },
      { name: 'hidden', description: 'x', invocation: { modelInvocable: false } },
      { name: 'plain', description: 'p' },
      { name: 'omt', description: 'omt', invocation: { modelInvocable: true } },
    ])
    expect(rows.map(row => row.name)).toEqual(['ce-plan', 'plain', 'omt'])
  })
})

describe('catalogLookupsFromAgents', () => {
  it('collects unique session cwds so settings can see filesystem skills', () => {
    expect(catalogLookupsFromAgents([
      { session: { header: { cwd: '/ws/a' } } },
      { session: { header: { cwd: '/ws/a' } } },
      { session: { header: { cwd: '/ws/b' } } },
      { session: { header: { cwd: '' } } },
    ])).toEqual([{ cwd: '/ws/a' }, { cwd: '/ws/b' }, {}])
  })
})

describe('resolveLiveAgent', () => {
  it('prefers the named session then falls back to the first live agent', () => {
    const a = { id: 'a' }
    const b = { id: 'b' }
    expect(resolveLiveAgent('b', id => id === 'b' ? b : undefined, () => [a, b])).toBe(b)
    expect(resolveLiveAgent('missing', () => undefined, () => [a, b])).toBe(a)
    expect(resolveLiveAgent(undefined, () => undefined, () => [a])).toBe(a)
  })
})

describe('collectBindableCatalog', () => {
  it('merges session and global lookups without dropping filesystem skills', async () => {
    const rows = await collectBindableCatalog(
      async lookup => lookup.cwd === '/ws/a'
        ? [{ name: 'ce-plan', description: 'plan', invocation: { modelInvocable: true } }]
        : [{ name: 'omt', description: 'omt', invocation: { modelInvocable: true } }],
      [{ cwd: '/ws/a' }, {}],
    )
    expect(rows.map(row => row.name)).toEqual(['ce-plan', 'omt'])
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

  it('does not shrink a full catalog down to runtime-only omt', async () => {
    const cache = new InstalledSkillCache()
    await cache.refresh(async () => [
      { name: 'ce-plan', invocation: { modelInvocable: true } },
      { name: 'ce-work', invocation: { modelInvocable: true } },
    ])
    await cache.refresh(async () => [{ name: 'omt', invocation: { modelInvocable: true } }])
    expect(cache.names()).toEqual(['ce-plan', 'ce-work'])
  })
})

describe('defaults', () => {
  it('starts with empty extra prompt and binds', () => {
    expect(DEFAULT_PROMPT_SETTINGS).toEqual({ extraPrompt: '', boundSkillNames: [] })
  })
})
