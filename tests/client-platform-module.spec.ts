import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const root = fileURLToPath(new URL('../', import.meta.url))

function source(path: string): string {
  return readFileSync(new URL(path, `file://${root}/`), 'utf8')
}

describe('DSH client platform module compatibility', () => {
  it('uses the 0.1.2 client-store seed instead of the removed client-runtime module', () => {
    const files = [
      'src/client/controller.ts',
      'src/client/index.ts',
      'src/client/externals.d.ts',
      'tsdown.config.ts',
      'vitest.config.ts',
    ]

    for (const file of files) {
      expect(source(file), file).not.toContain('@deepseek-ai/dsh-client-runtime/client')
    }

    expect(source('src/client/controller.ts')).toContain("from '@deepseek-ai/dsh-client-store'")
    expect(source('src/client/index.ts')).toContain("from '@deepseek-ai/dsh-client-store'")
    expect(source('tsdown.config.ts')).toContain("'@deepseek-ai/dsh-client-store'")
  })
})
