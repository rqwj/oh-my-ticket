import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    // Daemon fixtures spawn + handshake in hooks; shared CI runners can be
    // slow enough that the 10 s default is tight even with the daemon
    // prebuilt (workflows prebuild it before `pnpm test`).
    hookTimeout: 30_000,
    // Same reasoning for test bodies: daemon-restart scenarios (fixture
    // spawn → op → kill → respawn → convergence poll) outlast the 5 s
    // default on loaded shared runners. 20 s still catches real hangs.
    testTimeout: 20_000,
    // .dsh-checkout is a local symlink to the deepseek-harness checkout
    // (created by `pnpm run setup`); never collect its test suites.
    // corpus/** is the U4b scenario corpus: its runner imports the retired
    // src/host/core.ts (U7a deleted the TS core in favor of omt-daemon) and
    // is executed by scripts/run-corpus.mjs, not by vitest.
    // apps/** surfaces have their OWN vitest configs (jsdom + tauri API
    // mocks); the root suite is node-only.
    exclude: ['**/node_modules/**', '.dsh-checkout/**', 'corpus/**', 'apps/**'],
  },
  resolve: {
    alias: {
      // Browser platform module, answered by the loader table at runtime;
      // the local double keeps controller tests hermetic under node.
      '@deepseek-ai/dsh-client-store': new URL('./tests/mocks/runtime-client.ts', import.meta.url).pathname,
      // UI primitives (MarkdownText) for DocPanel component tests.
      '@deepseek-ai/dsh-client-ui-primitives': new URL('./tests/mocks/ui-primitives.ts', import.meta.url).pathname,
    },
  },
})
