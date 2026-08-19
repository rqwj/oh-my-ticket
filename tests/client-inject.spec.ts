import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

describe('client service injection', () => {
  it('declares settingsScope before reading the settings service', () => {
    const source = readFileSync(new URL('../src/client/index.ts', import.meta.url), 'utf8')
    const declaration = source.match(/export const inject = \[([^\]]+)\]/)?.[1]

    expect(declaration).toContain("'settingsScope'")
  })

  it('gives the settings section a locale-backed nav label', () => {
    const source = readFileSync(new URL('../src/client/index.ts', import.meta.url), 'utf8')
    expect(source).toContain("label: () => ctx.locale.bind(NS)('settings.nav')")
  })
})
