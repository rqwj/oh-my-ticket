import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    // .dsh-checkout is a local symlink to the deepseek-harness checkout
    // (created by `pnpm run setup`); never collect its test suites.
    exclude: ['**/node_modules/**', '.dsh-checkout/**'],
  },
  resolve: {
    alias: {
      // Browser platform module, answered by the loader table at runtime;
      // the local double keeps controller tests hermetic under node.
      '@deepseek-ai/dsh-client-runtime/client': new URL('./tests/mocks/runtime-client.ts', import.meta.url).pathname,
      // UI primitives (MarkdownText) for DocPanel component tests.
      '@deepseek-ai/dsh-client-ui-primitives': new URL('./tests/mocks/ui-primitives.ts', import.meta.url).pathname,
    },
  },
})
