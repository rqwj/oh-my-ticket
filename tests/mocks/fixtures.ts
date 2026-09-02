/**
 * Shared run-test fixtures (U7a): the epic → story → N tickets tree built
 * through the runtime SERVICE (real omt-daemon) or through the tool
 * surface, plus a run-item lookup that fails loudly.
 */
import { expect } from 'vitest'
import type { HomeRef, OmtService } from '../../src/host/service.ts'
import { OmtError, type OmtNode, type OmtRunItem } from '../../src/host/types.ts'
import { toolOf, type RegisteredTool } from './registered-tool.ts'

/** Standard fixture: epic → story → `count` tickets, created via service. */
export async function ticketFixture(service: OmtService, home: HomeRef, count = 3): Promise<OmtNode[]> {
  const epic = await service.createNode(home, { type: 'epic', title: '批量' })
  const story = await service.createNode(home, { type: 'story', title: '批次', parentId: epic.id })
  const tickets: OmtNode[] = []
  for (let index = 0; index < count; index += 1) {
    tickets.push(await service.createNode(home, { type: 'ticket', title: `任务${index + 1}`, parentId: story.id }))
  }
  return tickets
}

/** epic → story → n tickets, created through the tool surface. */
export async function ticketFixtureViaTools(tools: Map<string, RegisteredTool>, count: number): Promise<string[]> {
  const noExec = {}
  const epic = await toolOf(tools, 'omt_create').execute({ type: 'epic', title: '批量' }, noExec)
  const story = await toolOf(tools, 'omt_create').execute({ type: 'story', title: '批次', parentId: epic.id }, noExec)
  const ids: string[] = []
  for (let index = 0; index < count; index += 1) {
    const ticket = await toolOf(tools, 'omt_create').execute({ type: 'ticket', title: `任务${index + 1}`, parentId: story.id }, noExec)
    ids.push(ticket.id as string)
  }
  return ids
}

/** Look up a run item via a fresh detail fetch (fails loudly when missing). */
export async function requireItem(service: OmtService, home: HomeRef, runId: string, nodeId: string): Promise<OmtRunItem> {
  const snapshot = await service.fetchRun(home, runId)
  const item = snapshot.items.find(entry => entry.node_id === nodeId)
  expect(item).toBeDefined()
  return item as OmtRunItem
}

/**
 * Assert a promise rejects with an OmtError carrying exactly `code` and
 * (optionally) a `details` subset — the post-U2 assertion style: problem
 * codes plus structured details, never message text (R5).
 */
export async function expectProblem(
  exec: Promise<unknown>,
  code: OmtError['code'],
  details?: Record<string, unknown>,
): Promise<OmtError> {
  const thrown: unknown = await exec.then(
    () => undefined,
    (error: unknown) => error,
  )
  expect(thrown, `expected a rejection with ${code}`).toBeInstanceOf(OmtError)
  const omt = thrown as OmtError
  expect(omt.code).toBe(code)
  if (details !== undefined) expect(omt.details).toMatchObject(details)
  return omt
}
