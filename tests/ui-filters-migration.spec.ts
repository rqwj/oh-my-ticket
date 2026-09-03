/**
 * TICKET-0123 acceptance: legacy `<home>/ui-filters.json` preference files
 * migrate into daemon-owned storage (ui/filters-set) exactly once, the file
 * is renamed `.imported`, and the bag SURVIVES a daemon restart — filters
 * and recents are daemon state now, never adapter-side home writes.
 */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import {
    DSH_FILTERS_KEY,
    LEGACY_FILTERS_KEY,
    RECENT_SHARED_KEY,
    importLegacyUiFiltersFile,
  } from '../src/host/service.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

let fixture: RuntimeFixture

describe('legacy ui-filters.json migration', () => {
  // Scoped INSIDE this describe: an outer-level beforeEach would also run
  // for the sibling describe below, whose own beforeEach then overwrites
  // `fixture` — orphaning the first fixture's daemon forever (process leak).
  beforeEach(async () => {
    fixture = await createRuntimeFixture({ label: 'ui-migration' })
  })

  afterEach(async () => {
    await fixture.stop()
  })
  it('imports the legacy bag into daemon storage and renames the file', async () => {
    const home = fixture.globalHome
    const legacyPath = join(home.path, 'ui-filters.json')
    const legacyBag = {
      query: '登录',
      showArchived: true,
      statuses: ['in_progress'],
      sortOrder: 'priority-desc',
    }
    const { writeFile } = await import('node:fs/promises')
    await writeFile(legacyPath, JSON.stringify(legacyBag), 'utf8')

    const imported = await importLegacyUiFiltersFile(home.path, home.homeId, async (homeId, key, filters) => {
      await fixture.service.filtersSet({ homeId } as never, key, filters as never)
    })
    expect(imported).toBe(true)

    // The file is retired (renamed), so a second pass is a no-op.
    await expect(readFile(legacyPath, 'utf8')).rejects.toThrow()
    expect(await readFile(`${legacyPath}.imported`, 'utf8')).toContain('priority-desc')
    const again = await importLegacyUiFiltersFile(home.path, home.homeId, async () => {})
    expect(again).toBe(false)

    // The daemon now owns the bag — under the surface-prefixed key since U4
    // (the importer writes ONLY 'dsh:ui', never the bare legacy key).
    const saved = await fixture.service.filtersGet(home, DSH_FILTERS_KEY)
    expect(saved.query).toBe('登录')
    expect(saved.showArchived).toBe(true)
    expect(saved.statuses).toEqual(['in_progress'])
    expect(saved.sortOrder).toBe('priority-desc')
    // The bare legacy key stays untouched (empty) after import.
    expect(await fixture.service.filtersGet(home, 'ui')).toEqual({})

    // …and it survives a daemon restart over the same runtime dir.
    await fixture.restart()
    // Post-restart convergence is asynchronous: the daemon re-opens homes
    // and the client re-handshakes (credential scope) on their own clocks,
    // and the service has no transparent requiresRehandshake retry for
    // reads — on slow CI runners the first read can land in the
    // NOT_FOUND/requiresRehandshake window. Poll until the scope converges.
    let revived: Record<string, unknown> | undefined
    const deadline = Date.now() + 15_000
    for (;;) {
      try {
        revived = await fixture.service.filtersGet(home, DSH_FILTERS_KEY)
        break
      } catch (error) {
        if (Date.now() > deadline) throw error
        await new Promise(resolve => setTimeout(resolve, 200))
      }
    }
    expect(revived!.sortOrder).toBe('priority-desc')
    expect(revived!.query).toBe('登录')
  })

  it('skips missing and corrupt legacy files without touching daemon storage', async () => {
    const home = fixture.globalHome
    expect(
      await importLegacyUiFiltersFile(home.path, home.homeId, async () => {
        throw new Error('must not be called for a missing file')
      }),
    ).toBe(false)

    const { writeFile } = await import('node:fs/promises')
    const corruptPath = join(home.path, 'ui-filters.json')
    await writeFile(corruptPath, '{not json', 'utf8')
    expect(
      await importLegacyUiFiltersFile(home.path, home.homeId, async () => {
        throw new Error('must not be called for an unparsable file')
      }),
    ).toBe(false)
    // The unparsable file stays put (no rename without a successful push).
    await expect(readFile(corruptPath, 'utf8')).resolves.toBe('{not json')
  })
})

/**
 * U4 (R3-R5 / KD3): bag key scoping — surface-prefixed filters keys, bare
 * `'ui'` read-fallback + write-through, and the single shared `recent` key.
 */
describe('U4 bag key scoping', () => {
  beforeEach(async () => {
    fixture = await createRuntimeFixture({ label: 'bag-scoping' })
  })

  afterEach(async () => {
    await fixture.stop()
  })

  /** Raw daemon RPC for server-level assertions (bypasses adapter helpers). */
  function rawRpc<T>(method: string, params: Record<string, unknown>): Promise<T> {
    return (fixture.service as unknown as { rpc: <T>(m: string, p: Record<string, unknown>) => Promise<T> }).rpc<T>(
      method,
      params,
    )
  }

  it('migrates a seeded bare-key bag on first prefixed read and writes through', async () => {
    const home = fixture.globalHome
    // Upgrade simulation: a pre-U4 build left the bag under the bare key.
    await rawRpc('ui/filters-set', { homeId: home.homeId, key: 'ui', filters: { query: '旧键数据' } })
    expect(await rawRpc('ui/filters-get', { homeId: home.homeId, key: 'dsh:ui' })).toMatchObject({
      filters: {},
    })

    // First prefixed read falls back AND materializes the prefixed key.
    const migrated = await fixture.service.filtersGetDsh(home)
    expect(migrated.query).toBe('旧键数据')
    const prefixed = await rawRpc<{ filters: Record<string, unknown> }>('ui/filters-get', {
      homeId: home.homeId,
      key: DSH_FILTERS_KEY,
    })
    expect(prefixed.filters).toMatchObject({ query: '旧键数据' })
  })

  it('subsequent sets merge into the prefixed key while the bare key stays frozen', async () => {
    const home = fixture.globalHome
    await rawRpc('ui/filters-set', { homeId: home.homeId, key: 'ui', filters: { query: '原始' } })
    await fixture.service.filtersGetDsh(home) // migrate once

    const merged = await fixture.service.filtersSet(home, DSH_FILTERS_KEY, {
      showArchived: true,
    })
    expect(merged).toMatchObject({ query: '原始', showArchived: true })

    // The prefixed bag carries the merged view…
    const prefixed = await fixture.service.filtersGet(home, DSH_FILTERS_KEY)
    expect(prefixed).toMatchObject({ query: '原始', showArchived: true })
    // …while the bare legacy bag keeps its original value untouched
    // (no delete RPC — orphans are allowed to persist).
    const legacy = await rawRpc<{ filters: Record<string, unknown> }>('ui/filters-get', {
      homeId: home.homeId,
      key: LEGACY_FILTERS_KEY,
    })
    expect(legacy.filters).toEqual({ query: '原始' })
  })

  it('imports into the prefixed key only — the importer never writes the bare key', async () => {
    const home = fixture.globalHome
    const seen: Array<{ key: string }> = []
    const imported = await importLegacyUiFiltersFile(home.path, home.homeId, async (_homeId, key) => {
      seen.push({ key })
    })
    expect(imported).toBe(false) // no legacy file present → importer no-ops

    const { writeFile } = await import('node:fs/promises')
    await writeFile(join(home.path, 'ui-filters.json'), JSON.stringify({ sortOrder: 'priority-desc' }), 'utf8')
    const second = await importLegacyUiFiltersFile(home.path, home.homeId, async (_homeId, key) => {
      seen.push({ key })
    })
    expect(second).toBe(true)
    expect(seen.map(entry => entry.key)).toEqual(['dsh:ui'])
  })

  it('keeps different surface prefixes independent at the server level', async () => {
    const home = fixture.globalHome
    const dshBag = await fixture.service.filtersSet(home, 'dsh:tree', { query: 'DSH 视图' })
    expect(dshBag.query).toBe('DSH 视图')
    const tauriBag = await fixture.service.filtersSet(home, 'tauri:tree', { query: '桌面视图' })
    expect(tauriBag.query).toBe('桌面视图')

    // Cross-reads stay isolated in both directions.
    expect((await fixture.service.filtersGet(home, 'dsh:tree')).query).toBe('DSH 视图')
    expect((await fixture.service.filtersGet(home, 'tauri:tree')).query).toBe('桌面视图')
    expect(await fixture.service.filtersGet(home, 'tauri:nope')).toEqual({})
  })

  it('converges recents from different sessions onto the one shared key', async () => {
    // Two "surfaces" writing through the shared key see one list.
    await fixture.service.recentSet(RECENT_SHARED_KEY, ['EPIC-0001'])
    // A second surface (different session id, same storage contract):
    const fromOtherSurface = await fixture.service.recentGet(RECENT_SHARED_KEY)
    expect(fromOtherSurface).toEqual(['EPIC-0001'])

    // Legacy per-session keys are orphans by design — never read back by the
    // shared-key path and never cleaned (no delete RPC).
    await fixture.service.recentSet('session:legacy', ['EPIC-0002'])
    expect(await fixture.service.recentGet(RECENT_SHARED_KEY)).toEqual(['EPIC-0001'])
    expect(await fixture.service.recentGet('session:legacy')).toEqual(['EPIC-0002'])
  })
})
