#!/usr/bin/env node
/**
 * Build the Rust daemon binary for tests/development (plan U7a).
 *
 * Runs `cargo build -p omt-runtime --bin omt-daemon` in the repository root
 * (cached: a second invocation is a no-op unless sources changed). The
 * artifact lands at `<repo>/target/debug/omt-daemon`, which is the default
 * `daemonPath` the vitest runtime fixture (tests/mocks/runtime-fixture.ts)
 * and the OmtClient spawn resolution resolve to.
 */
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..')
const binary = join(repoRoot, 'target', 'debug', 'omt-daemon')

const skip = process.env.OMT_DAEMON_SKIP_BUILD === '1'
if (skip && existsSync(binary)) {
  console.log(`[build-daemon] skipped (OMT_DAEMON_SKIP_BUILD=1), using ${binary}`)
  process.exit(0)
}

// Fast path: cargo is a no-op when the fingerprint matches, so an explicit
// build per test session stays cheap while guaranteeing freshness.
console.log('[build-daemon] cargo build -p omt-runtime --bin omt-daemon …')
// Repo-local CARGO_HOME (.cargo-home/, pre-warmed registry) keeps the build
// self-contained and sandbox-friendly; fall back to the ambient CARGO_HOME
// when the local one has not been prepared.
const repoCargoHome = join(repoRoot, '.cargo-home')
const env = { ...process.env }
if (existsSync(join(repoCargoHome, 'registry'))) env.CARGO_HOME = repoCargoHome
const result = spawnSync('cargo', ['build', '-p', 'omt-runtime', '--bin', 'omt-daemon'], {
  cwd: repoRoot,
  stdio: 'inherit',
  env,
})
if (result.error !== undefined || result.status !== 0) {
  console.error(`[build-daemon] cargo build failed (status ${String(result.status)})`)
  process.exit(result.status ?? 1)
}
if (!existsSync(binary)) {
  console.error(`[build-daemon] expected artifact missing: ${binary}`)
  process.exit(1)
}
console.log(`[build-daemon] ready: ${binary}`)
