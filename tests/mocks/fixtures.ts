/**
 * Shared run-test fixtures: the epic → story → N tickets tree (core- and
 * tool-surface variants) plus a run-item lookup that fails loudly.
 */
import { expect } from 'vitest'
import type { OmtCore } from '../../src/host/core.ts'
import type { OmtNode, OmtRunItem } from '../../src/host/types.ts'
import { toolOf, type RegisteredTool } from './registered-tool.ts'

/** Standard fixture: epic → story → `count` tickets, created through core. */
export async function ticketFixture(core: OmtCore, count = 3): Promise<OmtNode[]> {
  const epic = await core.create({ type: 'epic', title: '批量' })
  const story = await core.create({ type: 'story', title: '批次', parentId: epic.id })
  const tickets: OmtNode[] = []
  for (let index = 0; index < count; index += 1) {
    tickets.push(await core.create({ type: 'ticket', title: `任务${index + 1}`, parentId: story.id }))
  }
  return tickets
}

/** epic → story → n tickets, created through the tool surface (global home). */
export async function ticketFixtureViaTools(tools: Map<string, RegisteredTool>, count: number): Promise<string[]> {
  const noExec = {}
  const epic = await toolOf(tools, 'omt_create').execute({ type: 'epic', title: '批量' }, noExec)
  const story = await toolOf(tools, 'omt_create').execute({ type: 'story', title: '批次', parentId: epic.id }, noExec)
  const ids: string[] = []
  for (let index = 0; index < count; index += 1) {
    const ticket = await toolOf(tools, 'omt_create').execute({ type: 'ticket', title: `任务${index + 1}`, parentId: story.id }, noExec)
    ids.push(ticket.id)
  }
  return ids
}

/** Look up a run item (fails the test when missing). */
export function requireItem(core: OmtCore, runId: string, nodeId: string): OmtRunItem {
  const item = core.getRunItem(runId, nodeId)
  expect(item).toBeDefined()
  return item as OmtRunItem
}
