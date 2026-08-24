/**
 * Corpus runner (TypeScript leg, plan U2): loads every
 * corpus/scenarios/*.json document and executes it against a real OmtCore
 * via the plain harness in ./harness.ts. Each scenario file is one test;
 * failures print the exact invariant diagnostics.
 *
 * Run: pnpm exec vitest run corpus/runner/ts/run.spec.ts
 */
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { runScenario, type ScenarioDoc } from './harness.ts'

const here = dirname(fileURLToPath(import.meta.url))
const scenarioDir = join(here, '..', '..', 'scenarios')

const files = readdirSync(scenarioDir).filter(file => file.endsWith('.json')).sort()

if (files.length === 0) {
  throw new Error(`no scenario documents found in ${scenarioDir}`)
}

describe('behavioral corpus (TS leg)', () => {
  it('scenario inventory is non-trivial', () => {
    expect(files.length).toBeGreaterThanOrEqual(40)
  })

  for (const file of files) {
    const doc = JSON.parse(readFileSync(join(scenarioDir, file), 'utf8')) as ScenarioDoc
    const label = doc.meta?.name ?? file.replace(/\.json$/, '')
    it(`${file} — ${label}`, async () => {
      const summary = await runScenario(doc)
      if (!summary.ok) {
        throw new Error(`${summary.failures.length} invariant failure(s):\n${summary.failures.join('\n')}`)
      }
      expect(summary.checks).toBeGreaterThan(0)
    })
  }
})
