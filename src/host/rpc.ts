/**
 * OMT RPC channel: exposes read endpoints to the browser half through the
 * generic Connection RPC transport (`POST /omt/<endpoint>`). External
 * plugins cannot rely on Typert's monorepo-built strict descriptors, so
 * payloads are validated here with zod on both ends.
 *
 * Endpoints (all read-only; mutations stay with the omt_* tools):
 *   tree   { rootId? }              → OmtTreeNode[] (full forest or subtree)
 *   search { query?, limit? }       → NodeSummary[] (@-trigger candidates)
 *   get    { id }                   → { node, parent?, children, body }
 */
import { z } from 'zod'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-connection'
import type { RpcResult } from '@deepseek-ai/dsh-host-apiproxy/api'
import type { OmtCorePool } from './pool.ts'
import type { ChangeHub } from './changes.ts'
import type { RecentRegistry } from './recent.ts'
import type { RunningRegistry } from './running.ts'
import { describeBoundCatalog, type SkillCatalogEntry } from './prompt-settings.ts'
import { OmtError, type OmtNode, type OmtTreeNode } from './types.ts'

export interface PromptRpcHost {
  getSettings(): { extraPrompt: string; boundSkillNames: string[] }
  listCatalog(): Promise<readonly SkillCatalogEntry[]>
}

/** Structural ctx.agents face: sessionId → agent → session header cwd. */
interface AgentsLike {
  get(id: string): { session: { header: { cwd: string } } } | undefined
}

// Every payload may carry the calling session so the handler can resolve
// the workspace home (absent sessionId → global fallback).
const sessionField = { sessionId: z.string().optional() }
const treePayloadSchema = z.object({ rootId: z.string().optional(), ...sessionField }).strict()
const searchPayloadSchema = z.object({
  query: z.string().default(''),
  limit: z.number().int().min(1).max(100).default(20),
  ...sessionField,
}).strict()
const getPayloadSchema = z.object({ id: z.string().min(1), ...sessionField }).strict()
const updatePayloadSchema = z.object({
  id: z.string().min(1),
  title: z.string().min(1).optional(),
  status: z.enum(['open', 'in_progress', 'done']).optional(),
  archived: z.boolean().optional(),
  priority: z.number().int().optional(),
  append: z.string().min(1).optional(),
  ...sessionField,
}).strict()
const reindexPayloadSchema = z.object({ ...sessionField }).strict()
const skillsPayloadSchema = z.object({}).strict()
const recentPayloadSchema = z.object({ sessionId: z.string().min(1) }).strict()
const executePayloadSchema = z.object({ id: z.string().min(1), sessionId: z.string().min(1) }).strict()

export interface NodeSummary {
  readonly id: string
  readonly type: OmtNode['type']
  readonly title: string
  readonly status: OmtNode['status']
  readonly archived: boolean
  readonly priority: number
}

function summarize(node: OmtNode): NodeSummary {
  return { id: node.id, type: node.type, title: node.title, status: node.status, archived: node.archived, priority: node.priority }
}

type RpcSuccess = { ok: true; value: unknown }

function ok(value: unknown): RpcSuccess {
  return { ok: true, value }
}

function failure(message: string): RpcResult<unknown> {
  return { ok: false, error: { code: 'internal', message, details: {} } }
}

function badRequest(message: string, issues: z.core.$ZodIssue[]): RpcResult<unknown> {
  return { ok: false, error: { code: 'bad-request', message, details: { issues } } }
}

/** Register the `/omt` RPC channel (loopback authority: local GUI only). */
export function registerOmtRpc(ctx: Context, pool: OmtCorePool, recent?: RecentRegistry, changes?: ChangeHub, running?: RunningRegistry, prompt?: PromptRpcHost): void {
  const agents = (ctx as unknown as { agents?: AgentsLike }).agents
  const sessionLabelOf = (sessionId: string): string => {
    const cwd = agents?.get(sessionId)?.session.header.cwd
    const base = cwd === undefined ? undefined : cwd.split('/').filter(Boolean).pop()
    return base !== undefined ? `${base} 的会话` : sessionId.slice(0, 13)
  }
  const coreFor = (sessionId: string | undefined) =>
    pool.coreFor(sessionId === undefined ? undefined : agents?.get(sessionId)?.session.header.cwd)
  ctx.connection.rpc.handle('/omt', async (endpoint, payload) => {
    try {
      switch (endpoint) {
        case 'tree': {
          const parsed = treePayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid tree payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          const forest: OmtTreeNode[] = core.tree(parsed.data.rootId)
          return ok(forest)
        }
        case 'search': {
          const parsed = searchPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid search payload', parsed.error.issues)
          const { query, limit } = parsed.data
          const core = await coreFor(parsed.data.sessionId)
          // Empty query backs the just-typed-'@' candidate list: latest nodes first.
          const nodes = query.trim() === '' ? core.list({}).slice(-limit).reverse() : core.list({ query }).slice(0, limit)
          return ok(nodes.map(summarize))
        }
        case 'get': {
          const parsed = getPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid get payload', parsed.error.issues)
          const cwd = parsed.data.sessionId === undefined ? undefined : agents?.get(parsed.data.sessionId)?.session.header.cwd
          const core = await pool.coreForNode(parsed.data.id, cwd)
          const result = await core.show(parsed.data.id)
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          const runningInfo = running?.get(parsed.data.id)
          return ok({
            home: core.home,
            ...(runningInfo !== undefined ? { running: runningInfo } : {}),
            node: result.node,
            ...(result.parent !== undefined ? { parent: summarize(result.parent) } : {}),
            children: result.children.map(summarize),
            body: result.body,
          })
        }
        case 'update': {
          const parsed = updatePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid update payload', parsed.error.issues)
          const { title, status, archived, priority, append } = parsed.data
          if ([title, status, archived, priority, append].every(v => v === undefined)) {
            return badRequest('update requires at least one change (title/status/archived/priority/append)', [])
          }
          const cwd = parsed.data.sessionId === undefined ? undefined : agents?.get(parsed.data.sessionId)?.session.header.cwd
          const core = await pool.coreForNode(parsed.data.id, cwd)
          const node = await core.update({ id: parsed.data.id, title, status, archived, priority, append })
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          // Manual status changes never START a running mark — execution is
          // claimed only by the execute endpoint and model tool calls
          // (TICKET-0028). Done/archive always clear it.
          if (status === 'done' || archived === true) running?.stop(parsed.data.id)
          changes?.bump(core.home)
          return ok(summarize(node))
        }
        case 'execute': {
          const parsed = executePayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid execute payload', parsed.error.issues)
          const cwd = agents?.get(parsed.data.sessionId)?.session.header.cwd
          const core = await pool.coreForNode(parsed.data.id, cwd)
          // Executing un-archives nothing and starts work: in_progress + running mark.
          const node = await core.update({ id: parsed.data.id, status: 'in_progress' })
          running?.start(parsed.data.id, parsed.data.sessionId, sessionLabelOf(parsed.data.sessionId))
          recent?.touch(parsed.data.sessionId, parsed.data.id)
          changes?.bump(core.home)
          return ok(summarize(node))
        }
        case 'recent': {
          const parsed = recentPayloadSchema.safeParse(payload)
          if (!parsed.success) return badRequest('invalid recent payload', parsed.error.issues)
          const sessionId = parsed.data.sessionId
          const cwd = agents?.get(sessionId)?.session.header.cwd
          const summaries: NodeSummary[] = []
          for (const id of (recent !== undefined ? await recent.resolve(sessionId) : [])) {
            const owner = await pool.coreForNode(id, cwd)
            const node = owner.getNode(id)
            if (node !== undefined) summaries.push(summarize(node))
          }
          return ok(summaries)
        }
        case 'skills': {
          const parsed = skillsPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid skills payload', parsed.error.issues)
          const settings = prompt?.getSettings() ?? { extraPrompt: '', boundSkillNames: [] }
          const catalog = prompt === undefined ? [] : await prompt.listCatalog()
          return ok({
            extraPrompt: settings.extraPrompt,
            skills: describeBoundCatalog(catalog, settings.boundSkillNames),
          })
        }
        case 'reindex': {
          const parsed = reindexPayloadSchema.safeParse(payload ?? {})
          if (!parsed.success) return badRequest('invalid reindex payload', parsed.error.issues)
          const core = await coreFor(parsed.data.sessionId)
          const result = await core.reindex()
          changes?.bump(core.home)
          return ok(result)
        }
        default:
          return badRequest(`unknown endpoint: ${endpoint}`, [])
      }
    } catch (error) {
      if (error instanceof OmtError) return failure(`${error.code}: ${error.message}`)
      throw error
    }
  }, { authority: 'loopback' })
}
