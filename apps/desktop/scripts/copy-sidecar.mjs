#!/usr/bin/env node
/**
 * U8/KD5 sidecar staging: copy the SAME-SOURCE omt-daemon build into the
 * tauri sidecar layout. Tauri's bundler requires the target-triple suffix
 * (`binaries/omt-daemon-aarch64-apple-darwin`) and strips it into
 * Contents/MacOS/omt-daemon at bundle time; dev mode resolves sidecars
 * relative to current_exe().parent(), so the debug copy must ALSO sit
 * next to the dev binary (target/debug/omt-daemon) — tauri dev does not
 * copy externalBin into the target dir (bundler-only behavior, verified
 * in .tmp-tauri-research/TAURI2_DESKTOP_RESEARCH.md).
 *
 * U3: `omt` CLI (the MCP bridge for dispatched harnesses) is staged the
 * same way — both binaries come from the same omt-runtime crate build.
 *
 * Sources (KTD1 same-version rule): workspace target/ build of the
 * omt-runtime crate — release for bundling, debug for --dev.
 *
 * Usage: node scripts/copy-sidecar.mjs [--dev]
 */
import { copyFileSync, chmodSync, existsSync, mkdirSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const SRC_TAURI = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'src-tauri')
const REPO_ROOT = resolve(SRC_TAURI, '..', '..', '..')
const dev = process.argv.includes('--dev')

const triple = execFileSync('rustc', ['--print', 'host-tuple'], { encoding: 'utf8' }).trim()
const profile = dev ? 'debug' : 'release'
const binDir = join(REPO_ROOT, 'target', profile)
const sidecars = ['omt-daemon', 'omt']

const missing = sidecars
  .map((bin) => join(binDir, bin))
  .filter((source) => !existsSync(source))

if (missing.length > 0) {
  console.error(
    `sidecar source missing: ${missing.join(', ')}\n` +
      `build it first: cargo build ${dev ? '' : '--release '}-p omt-runtime --bin omt-daemon --bin omt`,
  )
  process.exit(1)
}

// 1. Bundler staging (always): binaries/<name>-<triple>
const stagingDir = join(SRC_TAURI, 'binaries')
mkdirSync(stagingDir, { recursive: true })
for (const bin of sidecars) {
  const source = join(binDir, bin)
  const staged = join(stagingDir, `${bin}-${triple}`)
  copyFileSync(source, staged)
  chmodSync(staged, 0o755)
  console.log(`sidecar staged: ${staged} (from ${profile})`)
}

// 2. Dev-mode placement: next to the dev executable (current_exe().parent()).
if (dev) {
  const devDir = join(REPO_ROOT, 'target', 'debug')
  for (const bin of sidecars) {
    const source = join(binDir, bin)
    const devCopy = join(devDir, bin)
    // The workspace debug build IS the target/debug path already — the
    // desktop dev binary lives in the same target/debug under the shared
    // workspace target dir, so no copy is needed when they coincide. If a
    // future layout splits target dirs, copy here.
    if (devCopy !== source) {
      copyFileSync(source, devCopy)
      chmodSync(devCopy, 0o755)
    }
  }
  console.log(`dev sidecars in place: ${sidecars.join(', ')} under ${devDir}`)
}
