/**
 * Running-state tests (TICKET-0025): registry start/stop, the execute
 * endpoint (in_progress + running mark + get carrying running info), and
 * stop-on-done. U7a: endpoint cases run against a REAL omt-daemon via the
 * runtime fixture.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { dirname } from 'node:path'
import { BoundSkillGate } from '../src/host/skill-gate.ts'
import { registerOmtTools } from '../src/host/tools.ts'
import { stubToolCtx, toolOf, type RegisteredTool } from './mocks/registered-tool.ts'
import { RecentRegistry } from '../src/host/recent.ts'
import { RunningRegistry } from '../src/host/running.ts'
import { registerOmtRpc } from '../src/host/rpc.ts'
import type { OmtService } from '../src/host/service.ts'
import { createRuntimeFixture, type RuntimeFixture } from './mocks/runtime-fixture.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

describe('RunningRegistry', () => {
  it('starts, reads, and stops', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 's1', 'demo 的会话')
    expect(registry.get('TICKET-0001')).toMatchObject({ sessionId: 's1', sessionLabel: 'demo 的会话' })
    registry.stop('TICKET-0001')
    expect(registry.get('TICKET-0001')).toBeUndefined()
  })

  it('snapshots executor lineage for subagent sessions (TICKET-0066)', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 'child-1', 'demo 的会话', { parentSessionId: 'parent-1', isSubagent: true })
    expect(registry.get('TICKET-0001')).toMatchObject({
      sessionId: 'child-1',
      parentSessionId: 'parent-1',
      isSubagent: true,
    })
  })

  it('keeps colliding home ownership separate and refuses ambiguous legacy reads', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 's1', 'workspace', {}, 'workspace')
    registry.start('TICKET-0001', 's2', 'global', {}, 'global')
    expect(registry.forSession('s1')).toHaveLength(1)
    expect(registry.forSession('s2')).toHaveLength(1)
    expect(registry.get('TICKET-0001')).toBeUndefined()
    registry.stop('TICKET-0001')
    expect(registry.get('TICKET-0001', 'workspace')?.sessionId).toBe('s1')
    registry.stop('TICKET-0001', 'global')
    expect(registry.forSession('s2')).toEqual([])
    expect(registry.get('TICKET-0001', 'workspace')?.sessionId).toBe('s1')
  })

  it('plain sessions carry an empty lineage', () => {
    const registry = new RunningRegistry()
    registry.start('TICKET-0001', 's1', 'demo 的会话')
    const info = registry.get('TICKET-0001')
    expect(info?.parentSessionId).toBeUndefined()
    expect(info?.isSubagent).toBeUndefined()
  })
})

describe('/omt execute endpoint', () => {
  let fixture: RuntimeFixture
  let service: OmtService
  let epicId: string
  let handler: (endpoint: string, payload: unknown, signal: AbortSignal) => Promise<any>
  let running: RunningRegistry

  beforeEach(async () => {
    fixture = await createRuntimeFixture({ label: 'running', workspace: true })
    service = fixture.service
    running = new RunningRegistry()
    registerOmtRpc({ connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } } } as never, service, new RecentRegistry(), undefined, running)
    // Daemon allocates ids (no caller-supplied id over the protocol).
    epicId = (await service.createNode(fixture.globalHome, { type: 'epic', title: '用户体系' })).id
  })

  afterEach(async () => {
    await fixture.stop()
  })

  it('execute sets in_progress and records the session; get reports it', async () => {
    const result = await handler('execute', { id: epicId, sessionId: 's1' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    expect(result.value.status).toBe('in_progress')

    const detail = await handler('get', { id: epicId }, new AbortController().signal)
    expect(detail.value.running).toMatchObject({ sessionId: 's1' })
  })

  it.each(['execute', 'tool', 'claim'])('preserves workspace authorization when a global duplicate completes after %s start', async (startPath) => {
    const cwd = dirname(fixture.workspaceHome!.path)
    const agents = { get: (id: string) => ({ session: { header: { id, ...(id === 'workspace' ? { cwd } : {}) } } }) }
    registerOmtRpc({ agents, connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } } } as never, service, undefined, undefined, running)
    const tools = new Map<string, RegisteredTool>()
    registerOmtTools(stubToolCtx(tools) as never, service, undefined, undefined, running)
    const tickets = []
    for (const home of [fixture.globalHome, fixture.workspaceHome!]) {
      const epic = await service.createNode(home, { type: 'epic', title: 'epic' })
      const story = await service.createNode(home, { type: 'story', title: 'story', parentId: epic.id })
      tickets.push(await service.createNode(home, { type: 'ticket', title: 'ticket', parentId: story.id }))
    }
    const id = tickets[0]!.id
    expect(id).toBe('TICKET-0001')
    expect(tickets[1]!.id).toBe(id)
    const exec = { agent: { session: { header: { id: 'workspace', cwd } } } } as any
    if (startPath === 'execute') {
      expect((await handler('execute', { id, sessionId: 'workspace' }, new AbortController().signal)).ok).toBe(true)
    } else if (startPath === 'tool') {
      await toolOf(tools, 'omt_update').execute({ id, status: 'in_progress' }, exec)
    } else {
      const run = await service.createRun(fixture.workspaceHome!, { nodeIds: [id] })
      await service.controlRun(fixture.workspaceHome!, run.run.id, 'start')
      await toolOf(tools, 'omt_run_claim').execute({ id: run.run.id }, exec)
    }
    const gate = new BoundSkillGate({ getBoundSkillNames: () => ['ce-work'], hasRunningNode: sessionId => running.forSession(sessionId).length > 0 })
    gate.onStep('workspace', 1, 0)
    gate.onSkillResult('workspace', 'ce-work', true)
    gate.onStep('workspace', 1, 1)
    const authorized = () => gate.guard({ sessionId: 'workspace', rootCallId: 'write', name: 'write', arguments: { file_path: 'src/a.ts' } })
    expect(authorized()).toBeUndefined()
    expect((await handler('update', { id, status: 'done', scope: 'global', sessionId: 'workspace' }, new AbortController().signal)).ok).toBe(true)
    expect(authorized()).toBeUndefined()
    expect((await handler('update', { id, status: 'done', scope: 'workspace', sessionId: 'workspace' }, new AbortController().signal)).ok).toBe(true)
    expect(running.forSession('workspace')).toEqual([])
    expect(authorized()).toContain('in_progress')
  })

  it('manual in_progress via update does NOT mark running (TICKET-0028)', async () => {
    await handler('update', { id: epicId, status: 'in_progress', sessionId: 's1' }, new AbortController().signal)
    const detail = await handler('get', { id: epicId }, new AbortController().signal)
    expect(detail.value.running).toBeUndefined()
  })

  it('done clears the running mark', async () => {
    await handler('execute', { id: epicId, sessionId: 's1' }, new AbortController().signal)
    await handler('update', { id: epicId, status: 'done' }, new AbortController().signal)
    const detail = await handler('get', { id: epicId }, new AbortController().signal)
    expect(detail.value.running).toBeUndefined()
  })

  it('execute snapshots the session lineage into the running mark (TICKET-0066)', async () => {
    // Re-register with an agents registry whose session header carries the
    // subagent lineage (parentSession + origin). Same service: only the
    // agents registry changes.
    const withAgents = {
      connection: { rpc: { handle: (_c: string, h: any) => { handler = h } } },
      agents: {
        get: (id: string) => (id === 'child-1'
          ? { session: { header: { cwd: undefined, parentSession: 'parent-1', origin: 'subagent' } } }
          : undefined),
      },
    }
    registerOmtRpc(withAgents as never, service, new RecentRegistry(), undefined, running)

    const result = await handler('execute', { id: epicId, sessionId: 'child-1' }, new AbortController().signal)
    expect(result.ok).toBe(true)
    const detail = await handler('get', { id: epicId }, new AbortController().signal)
    expect(detail.value.running).toMatchObject({
      sessionId: 'child-1',
      parentSessionId: 'parent-1',
      isSubagent: true,
    })
  })
})
