/**
 * Pool tests: workspace-aware home resolution (local .omt wins, global
 * fallback), per-home core caching, and the startup-janitor live-session
 * wiring (review fix #4: a reloaded plugin must not demote items whose
 * executor session is still alive).
 */
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OmtCorePool } from '../src/host/pool.ts'

let root: string
let globalHome: string
let workspace: string
let pool: OmtCorePool

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'omt-pool-test-'))
  globalHome = join(root, 'global')
  workspace = join(root, 'workspace')
  await mkdir(workspace, { recursive: true })
  pool = new OmtCorePool(globalHome)
})

afterEach(async () => {
  await pool.closeAll()
  await rm(root, { recursive: true, force: true })
})

it('falls back to the global home when the workspace has no .omt', () => {
  expect(pool.homeFor(workspace)).toBe(globalHome)
  expect(pool.homeFor(undefined)).toBe(globalHome)
})

it('prefers the workspace-local home when .omt exists', async () => {
  await mkdir(join(workspace, '.omt'))
  expect(pool.homeFor(workspace)).toBe(join(workspace, '.omt'))
})

it('routes cores per home and caches them', async () => {
  await mkdir(join(workspace, '.omt'))
  const globalCore = await pool.coreFor(undefined)
  const localCore = await pool.coreFor(workspace)
  expect(localCore).not.toBe(globalCore)
  expect(localCore.home).toBe(join(workspace, '.omt'))
  expect(globalCore.home).toBe(globalHome)
  // Cached: same home resolves to the same instance.
  expect(await pool.coreFor(workspace)).toBe(localCore)

  // Writes land in the routed home.
  const epic = await localCore.create({ type: 'epic', title: '项目内' })
  expect(localCore.tree()).toHaveLength(1)
  expect(globalCore.tree()).toHaveLength(0)
  expect(epic.path).toContain('tickets')
})

describe('startup janitor live-session wiring (review fix #4)', () => {
  /** Seed a run with one running item executed by `sessionId`, then close. */
  async function seedRunningItem(sessionId: string): Promise<{ runId: string; nodeId: string }> {
    const seedPool = new OmtCorePool(globalHome)
    const core = await seedPool.coreFor(undefined)
    const epic = await core.create({ type: 'epic', title: '批量' })
    const story = await core.create({ type: 'story', title: '批次', parentId: epic.id })
    const ticket = await core.create({ type: 'ticket', title: '任务', parentId: story.id })
    const run = await core.createRun({ nodeIds: [ticket.id] })
    await core.startRun(run.id)
    await core.claimRunItem(run.id, sessionId)
    const nodeId = ticket.id
    const runId = run.id
    await seedPool.closeAll()
    return { runId, nodeId }
  }

  it('keeps the running item when the executor session is live at open time', async () => {
    const { runId, nodeId } = await seedRunningItem('s1')

    // Plugin reload with s1 still live: the janitor must not demote it.
    const livePool = new OmtCorePool(globalHome, { activeSessionIds: () => ['s1'] })
    const core = await livePool.coreFor(undefined)
    expect(core.getRunItem(runId, nodeId)?.state).toBe('running')
    expect(core.getRun(runId)?.status).toBe('running')
    await livePool.closeAll()

    // Reload with no live sessions: the same item is demoted to interrupted.
    const deadPool = new OmtCorePool(globalHome, { activeSessionIds: () => [] })
    const deadCore = await deadPool.coreFor(undefined)
    expect(deadCore.getRunItem(runId, nodeId)?.state).toBe('interrupted')
    await deadPool.closeAll()
  })

  it('a pool without a provider keeps the safe default (everything demoted)', async () => {
    const { runId, nodeId } = await seedRunningItem('s1')
    const plainPool = new OmtCorePool(globalHome)
    const core = await plainPool.coreFor(undefined)
    expect(core.getRunItem(runId, nodeId)?.state).toBe('interrupted')
    await plainPool.closeAll()
  })
})
