/**
 * Pool tests: workspace-aware home resolution (local .omt wins, global
 * fallback) and per-home core caching.
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
