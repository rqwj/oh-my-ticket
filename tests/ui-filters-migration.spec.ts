/**
 * TICKET-0123 acceptance: legacy `<home>/ui-filters.json` preference files
 * migrate into daemon-owned storage (ui/filters-set) exactly once, the file
 * is renamed `.imported`, and the bag SURVIVES a daemon restart — filters
 * and recents are daemon state now, never adapter-side home writes.
 */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { importLegacyUiFiltersFile } from '../src/host/service.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

let fixture: RuntimeFixture

beforeEach(async () => {
  fixture = await createRuntimeFixture({ label: 'ui-migration' })
})

afterEach(async () => {
  await fixture.stop()
})

describe('legacy ui-filters.json migration', () => {
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

    // The daemon now owns the bag.
    const saved = await fixture.service.filtersGet(home, 'ui')
    expect(saved.query).toBe('登录')
    expect(saved.showArchived).toBe(true)
    expect(saved.statuses).toEqual(['in_progress'])
    expect(saved.sortOrder).toBe('priority-desc')

    // …and it survives a daemon restart over the same runtime dir.
    await fixture.restart()
    const revived = await fixture.service.filtersGet(home, 'ui')
    expect(revived.sortOrder).toBe('priority-desc')
    expect(revived.query).toBe('登录')
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
