/**
 * Real-runtime test fixture (U7a): every converted spec runs against a REAL
 * omt-daemon. Each fixture call:
 *
 *   1. ensures the debug daemon binary exists (cached cargo build),
 *   2. mkdtemps an isolated root with a global home (+ optional workspace
 *      `.omt` dir pre-created so the daemon can open it as a workspace home),
 *   3. spawns a fresh daemon (`--runtime-dir <root>/runtime --home ...`,
 *      OMT_HOME env marks the global home; ~140ms cold start measured),
 *   4. hands back a CONNECTED OmtService plus typed home handles,
 *   5. `stop()` closes the client, SIGTERMs the daemon, waits for exit and
 *      removes the temp root (best-effort; leaks warn loudly).
 *
 * Per-test daemons keep specs hermetic: no shared SQLite state, no cross-test
 * home pollution, no ordering hazards. The suite cost is dominated by spawn
 * time (~0.2s per test) — acceptable vs the characterization value.
 */
import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { OmtClient } from '@omt/client-ts'
import { OmtService } from '../../src/host/service.ts'

/** Repo root (this file lives at tests/mocks/runtime-fixture.ts). */
const repoRoot = fileURLToPath(new URL('../../', import.meta.url))

/** Debug daemon binary path produced by scripts/build-daemon.mjs. */
export const daemonBinary = join(repoRoot, 'target', 'debug', 'omt-daemon')

let buildPromise: Promise<void> | undefined

/** Ensure the daemon binary exists (cargo build once per worker). */
export async function ensureDaemonBuilt(): Promise<void> {
  if (existsSync(daemonBinary)) return
  buildPromise ??= new Promise<void>((resolveBuild, rejectBuild) => {
    console.warn('[omt-fixture] building omt-daemon (first use in this worker)...')
    try {
      const env = { ...process.env }
      // Sandbox-safe CARGO_HOME when a repo-local one is warmed.
      if (!env.CARGO_HOME && existsSync(join(repoRoot, '.cargo-home'))) {
        env.CARGO_HOME = join(repoRoot, '.cargo-home')
      }
      execFileSync('cargo', ['build', '-p', 'omt-runtime', '--bin', 'omt-daemon'], {
        cwd: repoRoot,
        env,
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      resolveBuild()
    } catch (error) {
      rejectBuild(error)
    }
  })
  await buildPromise
}

export interface FixtureHome {
  /** Daemon-issued id for this home. */
  readonly homeId: string
  /** Absolute path on disk. */
  readonly path: string
}

export interface RuntimeFixture {
  /** Connected service (ready() awaited before handoff). */
  readonly service: OmtService
  readonly runtimeDir: string
  readonly root: string
  /** The global home (OMT_HOME-marked; first opened unless workspace-first). */
  readonly globalHome: FixtureHome
  /** Present when `workspace: true`. */
  readonly workspaceHome?: FixtureHome
  /**
   * Stop the daemon and start a NEW one over the same runtime dir and homes.
   * The handed-out service survives (its client reconnects with backoff);
   * used to exercise the startup janitor's lease-demotion path.
   */
  restart(): Promise<void>
  /** Kill the daemon only (no respawn, no cleanup); pair with restart(). */
  halt(): Promise<void>
  stop(): Promise<void>
}

export interface RuntimeFixtureOptions {
  /** Pre-create `<root>/workspace/.omt` and open it as a second home. */
  readonly workspace?: boolean
  /** Extra cwd used to resolve the workspace home in the adapter. */
  readonly label?: string
}

interface SpawnedDaemon {
  readonly pid: number
  readonly runtimeDir: string
}

function waitMs(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}

/**
 * Spawn a dedicated daemon over the given homes. Mirrors OmtClient's spawn
 * protocol but keeps the pid for teardown (discoverOrSpawn detaches without
 * exposing it).
 */
/**
 * Spawn one dedicated daemon over the given homes. Exported for specs that
 * must control the boot environment directly (e.g. home-ownership cases
 * planting foreign markers before the daemon starts).
 */
export async function spawnDaemon(runtimeDir: string, homes: Array<{ path: string; global: boolean }>): Promise<SpawnedDaemon> {
  await ensureDaemonBuilt()
  mkdirSync(runtimeDir, { recursive: true })
  const env: Record<string, string | undefined> = { ...process.env, OMT_RUNTIME_DIR: runtimeDir }
  const args: string[] = ['--runtime-dir', runtimeDir]
  for (const home of homes) {
    if (home.global) env.OMT_HOME = home.path
    args.push('--home', home.path)
  }
  const child = spawn(daemonBinary, args, {
    cwd: runtimeDir,
    env,
    stdio: ['ignore', 'ignore', 'inherit'],
    detached: true,
  })
  const pid = child.pid
  child.unref()
  // Wait for descriptor.json (the client polls too; this just bounds errors).
  const descriptorPath = join(runtimeDir, 'descriptor.json')
  for (let attempt = 0; attempt < 100 && !existsSync(descriptorPath); attempt += 1) {
    await waitMs(20)
  }
  if (!existsSync(descriptorPath)) throw new Error(`omt-daemon failed to write ${descriptorPath}`)
  return { pid: pid as number, runtimeDir }
}

/** Create one isolated runtime + connected service. */
export async function createRuntimeFixture(options: RuntimeFixtureOptions = {}): Promise<RuntimeFixture> {
  const root = mkdtempSync(join(tmpdir(), `omt-test-${options.label ?? 'rt'}-`))
  const globalPath = join(root, 'global-home')
  mkdirSync(globalPath, { recursive: true })

  const workspacePath = join(root, 'workspace')
  if (options.workspace) {
    mkdirSync(join(workspacePath, '.omt'), { recursive: true })
  }

  const homes = [{ path: globalPath, global: true }]
  if (options.workspace) homes.push({ path: join(workspacePath, '.omt'), global: false })
  const daemon = await spawnDaemon(join(root, 'runtime'), homes)

  const service = new OmtService({
    runtimeDir: daemon.runtimeDir,
    name: `oh-my-ticket-test-${process.pid}`,
    // The fixture owns the daemon lifecycle (spawn/restart/stop); the
    // service must never win a respawn race with a home-less daemon.
    noSpawn: true,
  })
  await service.ready()

  const registry = service.homes()
  const byPath = new Map(registry.map(home => [home.path ?? '', home.homeId]))
  const globalHomeId = byPath.get(globalPath)
  if (globalHomeId === undefined) throw new Error(`daemon did not report global home ${globalPath}: ${JSON.stringify(registry.map(h => h.path))}`)

  let stopped = false
  let currentPid = daemon.pid
  const spawnOver = async (): Promise<void> => {
    const next = await spawnDaemon(daemon.runtimeDir, homes)
    currentPid = next.pid
  }
  const killDaemon = async (): Promise<void> => {
    try {
      process.kill(currentPid, 'SIGKILL')
    } catch {
      /* already gone */
    }
    // Wait for the old process to release the socket.
    for (let attempt = 0; attempt < 100; attempt += 1) {
      try {
        process.kill(currentPid, 0)
        await waitMs(20)
      } catch {
        break
      }
    }
  }
  return {
    service,
    runtimeDir: daemon.runtimeDir,
    root,
    globalHome: { homeId: globalHomeId, path: globalPath },
    ...(options.workspace
      ? {
          workspaceHome: (() => {
            const wsId = byPath.get(join(workspacePath, '.omt'))
            if (wsId === undefined) throw new Error('daemon did not report workspace home')
            return { homeId: wsId, path: join(workspacePath, '.omt') }
          })(),
        }
      : {}),
    async restart(): Promise<void> {
      await killDaemon()
      await spawnOver()
    },
    /**
     * Kill the daemon WITHOUT respawning or deleting anything: the window
     * where the home directories are unowned (home-lock scenarios plant a
     * foreign lock here, then `restart`).
     */
    async halt(): Promise<void> {
      await killDaemon()
    },
    async stop(): Promise<void> {
      if (stopped) return
      stopped = true
      try {
        await service.close()
      } catch {
        /* already down */
      }
      try {
        process.kill(currentPid, 'SIGTERM')
      } catch {
        /* already gone */
      }
      for (let attempt = 0; attempt < 50; attempt += 1) {
        try {
          process.kill(currentPid, 0)
          await waitMs(20)
        } catch {
          break
        }
      }
      rmSync(root, { recursive: true, force: true })
    },
  }
}

/** Vitest lifecycle glue: beforeEach/afterEach pair owning one fixture. */
export interface RuntimeSuite extends RuntimeFixture {}
