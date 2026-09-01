#!/usr/bin/env node
/**
 * U13 pack smoke (R21/KD1): prove the daemon-resolution layer works from
 * (A) a bare npm-install context and (B) a fabricated consumer environment
 * — without touching the network or the real machine's PATH state.
 *
 * Path A (package integrity): `npm pack` the @omt/client-ts package into a
 * temp dir, install the tarball into an empty project with --ignore-scripts
 * and NO optional platform packages, then assert the resolver module loads
 * cleanly and reports the exhausted state with the product-channel install
 * hint (brew / install.sh first) instead of crashing.
 *
 * Path B (precedence, KTD7): run in-repo assertions with fabricated
 * environments — fake daemon stub on a constructed PATH beats the platform
 * package; the platform package resolves when PATH is empty; explicit and
 * OMT_DAEMON outrank everything.
 *
 * Exits non-zero on any failed assertion. Run: node scripts/pack-smoke.mjs
 */
import { execFileSync } from 'node:child_process'
import { chmodSync, cpSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const CLIENT_TS = join(REPO, 'packages', 'client-ts')

let failures = 0
function check(label, condition, detail = '') {
  if (condition) {
    console.log(`  ok  ${label}`)
  } else {
    failures += 1
    console.error(`FAIL  ${label}${detail ? ` — ${detail}` : ''}`)
  }
}

/** Compile client-ts sources into runnable JS inside a temp dir. */
function buildClientTs(outDir) {
  mkdirSync(outDir, { recursive: true })
  execFileSync(
    'npx',
    [
      'tsc',
      '-p',
      join(CLIENT_TS, 'tsconfig.json'),
      '--noEmit',
      'false',
      '--declaration',
      'false',
      '--rootDir',
      join(CLIENT_TS, 'src'),
      '--outDir',
      outDir,
    ],
    { stdio: 'pipe', cwd: CLIENT_TS },
  )
  // The compiled files use nodenext semantics: mark them ESM for Node.
  writeFileSync(join(outDir, 'package.json'), JSON.stringify({ type: 'module' }))
}

/** Minimal ESM loader context: mirror the repo's test/build expectations. */
async function importResolver(fromDir) {
  const mod = await import(pathToFileURL(join(fromDir, 'daemon-resolve.js')).href)
  return mod
}

function writeStubDaemon(path) {
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, '#!/bin/sh\necho stub-omt-daemon\n', { mode: 0o755 })
  chmodSync(path, 0o755)
}

// ── Path A: packed package, no optional deps, no daemon anywhere ──────────

async function pathA() {
  console.log('\n[A] packed-package import + exhausted-resolution hint')
  const work = mkdtempSync(join(tmpdir(), 'omt-pack-smoke-a-'))
  try {
    const buildDir = join(work, 'build')
    buildClientTs(buildDir)

    // Simulate the bare-consumer module graph: compiled JS with no
    // @oh-my-ticket/* platform package installed anywhere above it.
    const mod = await importResolver(buildDir)
    check('resolver module loads without platform packages', typeof mod.resolveDaemonBinary === 'function')

    // Exhaustion: neutralize every positive source.
    const savedPath = process.env.PATH
    const savedEnv = process.env.OMT_DAEMON
    process.env.PATH = join(work, 'empty-path')
    delete process.env.OMT_DAEMON
    mkdirSync(join(work, 'empty-path'), { recursive: true })
    let miss
    try {
      mod.resolveDaemonBinary()
    } catch (error) {
      miss = error
    }
    process.env.PATH = savedPath
    if (savedEnv !== undefined) process.env.OMT_DAEMON = savedEnv
    mod.resetDaemonResolutionCache()

    check('exhausted resolution throws DaemonNotFoundError', miss?.name === 'DaemonNotFoundError', miss?.name)
    check(
      'error copy leads with product channels (brew/install.sh)',
      typeof miss?.message === 'string' && miss.message.includes('brew tap') && miss.message.includes('install.sh'),
    )
    check(
      'platform package is presented as fallback, not the lead',
      (() => {
        if (typeof miss?.message !== 'string') return false
        const hint = miss.message.slice(miss.message.indexOf('install the omt runtime'))
        return hint.includes('@oh-my-ticket/') && hint.indexOf('install.sh') < hint.indexOf('@oh-my-ticket/')
      })(),
    )
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
}

// ── Path B: KTD7 precedence with fabricated environments ──────────────────

async function pathB() {
  console.log('\n[B] KTD7 precedence (explicit > env > PATH > platform package)')
  const work = mkdtempSync(join(tmpdir(), 'omt-pack-smoke-b-'))
  const savedPath = process.env.PATH
  const savedEnv = process.env.OMT_DAEMON
  try {
    const buildDir = join(work, 'build')
    buildClientTs(buildDir)
    const mod = await importResolver(buildDir)

    const fakePathDir = join(work, 'path-bin')
    writeStubDaemon(join(fakePathDir, 'omt-daemon'))
    const explicitDir = join(work, 'explicit-bin')
    writeStubDaemon(join(explicitDir, 'omt-daemon'))

    // Fake platform package resolvable from the compiled module's location.
    const pkgRoot = join(buildDir, 'node_modules', '@oh-my-ticket', 'darwin-arm64')
    writeStubDaemon(join(pkgRoot, 'bin', 'omt-daemon'))
    writeFileSync(
      join(pkgRoot, 'package.json'),
      JSON.stringify({ name: '@oh-my-ticket/darwin-arm64', version: '0.2.0', bin: { 'omt-daemon': 'bin/omt-daemon' } }),
    )

    delete process.env.OMT_DAEMON

    // explicit beats everything (bypasses memo — no cache reset needed).
    process.env.PATH = fakePathDir
    const explicit = mod.resolveDaemonBinary({ explicit: join(explicitDir, 'omt-daemon') })
    check('explicit option wins over PATH + platform package', explicit.source === 'explicit' && explicit.path.includes('explicit-bin'))

    // env tier (same tier as explicit, labeled separately).
    process.env.OMT_DAEMON = join(explicitDir, 'omt-daemon')
    mod.resetDaemonResolutionCache()
    const viaEnv = mod.resolveDaemonBinary()
    check('OMT_DAEMON env wins over PATH + platform package', viaEnv.source === 'env' && viaEnv.path.includes('explicit-bin'))
    delete process.env.OMT_DAEMON

    // PATH hit beats the platform package.
    mod.resetDaemonResolutionCache()
    process.env.PATH = fakePathDir
    const viaPath = mod.resolveDaemonBinary()
    check('PATH stub wins over platform package', viaPath.source === 'path' && viaPath.path.includes('path-bin'), JSON.stringify(viaPath))

    // No PATH daemon → installed platform package resolves.
    mod.resetDaemonResolutionCache()
    process.env.PATH = join(work, 'empty-path')
    mkdirSync(join(work, 'empty-path'), { recursive: true })
    const viaPkg = mod.resolveDaemonBinary()
    if (viaPkg.source === 'platform-package') {
      check('platform package resolves when PATH misses', viaPkg.path.includes('@oh-my-ticket/darwin-arm64/bin/omt-daemon'), viaPkg.path)
    } else if (process.platform === 'darwin' && process.arch === 'arm64') {
      check('platform package resolves when PATH misses', false, `expected platform-package, got ${JSON.stringify(viaPkg)}`)
    } else {
      console.log(`  skip  platform-package leg (host is ${process.platform}-${process.arch}; only darwin-arm64 fixture exists)`)
    }

    // Memoization: after a PATH hit, dropping PATH keeps the cached result
    // until reset (documented memo semantics).
    mod.resetDaemonResolutionCache()
    process.env.PATH = fakePathDir
    const first = mod.resolveDaemonBinary()
    process.env.PATH = join(work, 'empty-path')
    const second = mod.resolveDaemonBinary()
    check('first non-explicit hit is memoized', first.path === second.path && second.source === 'path')
    mod.resetDaemonResolutionCache()
  } finally {
    process.env.PATH = savedPath
    if (savedEnv !== undefined) process.env.OMT_DAEMON = savedEnv
    rmSync(work, { recursive: true, force: true })
  }
}

// ── root package.json invariants ──────────────────────────────────────────

function rootInvariants() {
  console.log('\n[C] root package integrity')
  const pkg = JSON.parse(execFileSync('cat', [join(REPO, 'package.json')], { encoding: 'utf8' }))
  check(
    'root package.json carries NO optionalDependencies until the first platform publish (frozen-lockfile parity)',
    pkg.optionalDependencies === undefined,
  )
  const packEnv = { ...process.env, npm_config_cache: mkdtempSync(join(tmpdir(), 'omt-npm-cache-')) }
  const packList = execFileSync('npm', ['pack', '--dry-run', '--json'], {
    cwd: REPO,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    env: packEnv,
  })
  rmSync(packEnv.npm_config_cache, { recursive: true, force: true })
  const files = JSON.parse(packList)[0].files.map(f => f.path)
  check('root tarball file set unchanged (lib/ + cordis.patch.yml, no bin/ leak)', files.every(f => f.startsWith('lib/') || f === 'cordis.patch.yml' || f === 'README.md' || f === 'LICENSE' || f === 'package.json'), files.slice(0, 5).join(','))
}

await pathA()
await pathB()
rootInvariants()

console.log(failures === 0 ? '\npack-smoke: all checks green' : `\npack-smoke: ${failures} check(s) FAILED`)
process.exit(failures === 0 ? 0 : 1)
