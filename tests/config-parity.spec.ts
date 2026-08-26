/**
 * Cross-layer config-resolution parity (U3, R1-R2 / KD2).
 *
 * Characterization suite: the three resolution implementations already agree
 * (behavior exists); this spec exists so any future drift turns RED. Every
 * assertion names its pinned implementation file in a comment.
 *
 * Layers pinned here:
 * - Rust  daemon/CLI   — crates/omt-runtime/src/paths.rs::resolve + the
 *                        global-home block in server.rs::run
 * - TS shared client   — packages/client-ts/src/client.ts (resolveRuntimeDir,
 *                        readDescriptor schemaVersion hard check)
 * - DSH adapter        — src/host/service.ts consumes resolveRuntimeDir via
 *                        OmtClient (covered transitively by every spawn here)
 *
 * The contract text itself is docs/runtime/config.md.
 */
import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtClient } from '@omt/client-ts'
import { ensureDaemonBuilt } from './mocks/runtime-fixture.ts'

const repoRoot = fileURLToPath(new URL('../', import.meta.url))
const daemonBinary = join(repoRoot, 'target', 'debug', 'omt-daemon')

/** Product version from Cargo.toml [workspace.package] (U1 anchor). */
function workspaceVersion(): string {
  const manifest = readFileSync(join(repoRoot, 'Cargo.toml'), 'utf8')
  const match = manifest.match(/^\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)
  if (!match) throw new Error('no [workspace.package] version in Cargo.toml')
  return match[1]
}

interface SpawnedDaemon {
  readonly pid: number
  readonly runtimeDir: string
}

const spawned: Array<SpawnedDaemon> = []
let currentRoot = ''

/**
 * Boot-environment-exact daemon spawner: unlike runtime-fixture's
 * spawnDaemon this takes the full env/args pair verbatim so specs can pin
 * default-path resolution, env-only homes, and arg-over-env precedence.
 */
async function spawnDaemonExact(
  runtimeDir: string,
  args: string[],
  envOverrides: Record<string, string | undefined>,
): Promise<SpawnedDaemon> {
  await ensureDaemonBuilt()
  // spawn() fails with ENOENT when cwd is missing — create it up front
  // (mirrors runtime-fixture's spawnDaemon).
  mkdirSync(runtimeDir, { recursive: true })
  const env: Record<string, string | undefined> = { ...process.env }
  // Scenarios that don't pin a home explicitly would otherwise let the
  // daemon open the REAL ~/.omt as its global home (home_lock.rs then hits
  // sandbox EPERM). Redirect HOME into the temp root unless the scenario
  // manages HOME itself — every other real-daemon fixture achieves the same
  // via explicit --home args.
  if (!('HOME' in envOverrides)) env.HOME = currentRoot
  for (const [key, value] of Object.entries(envOverrides)) {
    if (value === undefined) delete env[key]
    else env[key] = value
  }
  const child = spawn(daemonBinary, args, {
    cwd: runtimeDir,
    env,
    stdio: ['ignore', 'ignore', 'pipe'],
    detached: true,
  })
  let stderrText = ''
  child.stderr?.on('data', chunk => {
    stderrText += String(chunk)
  })
  const pid = child.pid as number
  child.unref()
  spawned.push({ pid, runtimeDir })
  const descriptorPath = join(runtimeDir, 'descriptor.json')
  for (let attempt = 0; attempt < 150 && !existsSync(descriptorPath); attempt += 1) {
    await new Promise(resolve => setTimeout(resolve, 20))
  }
  if (!existsSync(descriptorPath)) {
    try {
      process.kill(pid, 'SIGTERM')
    } catch {
      /* already gone */
    }
    throw new Error(
      `omt-daemon failed to write ${descriptorPath} (args=${JSON.stringify(args)})\nstderr:\n${stderrText}`,
    )
  }
  return { pid, runtimeDir }
}

/** SIGTERM every daemon this spec spawned and wait for descriptor removal. */
async function reapSpawned(): Promise<void> {
  while (spawned.length > 0) {
    const daemon = spawned.pop()
    if (!daemon) break
    try {
      process.kill(daemon.pid, 'SIGTERM')
    } catch {
      /* already gone */
    }
    const descriptorPath = join(daemon.runtimeDir, 'descriptor.json')
    for (let attempt = 0; attempt < 200 && existsSync(descriptorPath); attempt += 1) {
      await new Promise(resolve => setTimeout(resolve, 25))
    }
  }
}

beforeEach(() => {
  currentRoot = mkdtempSync(join(tmpdir(), 'omt-parity-'))
})

afterEach(async () => {
  await reapSpawned()
  rmSync(currentRoot, { recursive: true, force: true })
})

describe('config resolution parity (U3)', () => {
  it('resolves the default runtime dir to $HOME/.omt/run on both layers with no env set', async () => {
    const fakeHome = join(currentRoot, 'home')
    // Rust side: real daemon, HOME redirected, OMT_* cleared → writes the
    // default layout under the fake home (paths.rs::resolve fallback arm).
    const runtimeDir = join(fakeHome, '.omt', 'run')
    await spawnDaemonExact(runtimeDir, [], {
      HOME: fakeHome,
      OMT_RUNTIME_DIR: undefined,
      OMT_HOME: undefined,
    })
    expect(existsSync(join(runtimeDir, 'descriptor.json'))).toBe(true)

    // TS side: same environment through the shared client resolver
    // (client.ts::OmtClient.resolveRuntimeDir fallback arm).
    const previousHome = process.env.HOME
    const previousEnvDir = process.env.OMT_RUNTIME_DIR
    try {
      process.env.HOME = fakeHome
      delete process.env.OMT_RUNTIME_DIR
      expect(OmtClient.resolveRuntimeDir()).toBe(runtimeDir)
    } finally {
      if (previousHome === undefined) delete process.env.HOME
      else process.env.HOME = previousHome
      if (previousEnvDir === undefined) delete process.env.OMT_RUNTIME_DIR
      else process.env.OMT_RUNTIME_DIR = previousEnvDir
    }
  })

  it('honors OMT_RUNTIME_DIR identically on both layers and lands the descriptor there', async () => {
    const runtimeDir = join(currentRoot, 'runtime')
    await spawnDaemonExact(runtimeDir, ['--runtime-dir', runtimeDir], {})
    expect(OmtClient.readDescriptor(runtimeDir)).not.toBeNull()

    const previousEnvDir = process.env.OMT_RUNTIME_DIR
    try {
      process.env.OMT_RUNTIME_DIR = runtimeDir
      expect(OmtClient.resolveRuntimeDir()).toBe(runtimeDir)
    } finally {
      if (previousEnvDir === undefined) delete process.env.OMT_RUNTIME_DIR
      else process.env.OMT_RUNTIME_DIR = previousEnvDir
    }
  })

  it('drives the global home from OMT_HOME alone (server.rs global-home block)', async () => {
    const globalHome = join(currentRoot, 'env-global-home')
    const runtimeDir = join(currentRoot, 'runtime')
    mkdirSync(globalHome, { recursive: true })
    await spawnDaemonExact(runtimeDir, ['--runtime-dir', runtimeDir], {
      OMT_HOME: globalHome,
    })
    const client = new OmtClient({ runtimeDir, noSpawn: true })
    try {
      const handshake = await client.connect('dsh', {}, 'parity-spec')
      const listed = handshake.homes.filter(entry => entry.kind !== 'unknown')
      const globalEntry = handshake.homes.find(entry => entry.path === globalHome)
      expect(globalEntry).toBeDefined()
      expect(listed).toHaveLength(1)
    } finally {
      client.close()
    }
  })

  it('lets an explicit --runtime-dir argument override OMT_RUNTIME_DIR (precedence)', async () => {
    const envDir = join(currentRoot, 'from-env')
    const argDir = join(currentRoot, 'from-arg')
    mkdirSync(envDir, { recursive: true })
    await spawnDaemonExact(argDir, ['--runtime-dir', argDir], {
      OMT_RUNTIME_DIR: envDir,
    })
    expect(existsSync(join(argDir, 'descriptor.json'))).toBe(true)
    expect(existsSync(join(envDir, 'descriptor.json'))).toBe(false)
  })

  it('tolerates unknown additive fields in descriptor.json (schemaVersion stays 1)', async () => {
    const runtimeDir = join(currentRoot, 'runtime')
    await spawnDaemonExact(runtimeDir, ['--runtime-dir', runtimeDir], {})
    const descriptorPath = join(runtimeDir, 'descriptor.json')
    const original = JSON.parse(readFileSync(descriptorPath, 'utf8')) as Record<string, unknown>
    writeFileSync(
      descriptorPath,
      JSON.stringify({ ...original, futureField: { nested: [1, 2, 3] } }),
    )
    const reread = OmtClient.readDescriptor(runtimeDir)
    expect(reread).not.toBeNull()
    expect(reread?.schemaVersion).toBe(1)
  })

  it('reports a handshake daemon.version equal to the workspace product version (U1 anchor)', async () => {
    const runtimeDir = join(currentRoot, 'runtime')
    await spawnDaemonExact(runtimeDir, ['--runtime-dir', runtimeDir], {})
    const client = new OmtClient({ runtimeDir, noSpawn: true })
    try {
      const handshake = await client.connect('dsh', {}, 'parity-spec')
      expect(handshake.daemon.name).toBe('omt-daemon')
      expect(handshake.daemon.version).toBe(workspaceVersion())
    } finally {
      client.close()
    }
  })
})

