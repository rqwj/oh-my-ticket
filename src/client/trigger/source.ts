/**
 * OMT '@' trigger source: ticket candidates from the `/omt` RPC channel,
 * pick → ReferenceInsert (U+FFFC chip in the draft), and a codec that
 * serializes each reference into model-readable ticket content on submit.
 * Lives in the browser half; all host data arrives through rpc.call.
 *
 * The trigger contracts below mirror the frozen cross-package contract in
 * ui-input-trigger (packages/client/ui-input-trigger/src/types.ts). They are
 * re-declared structurally so this out-of-tree package typechecks without
 * the monorepo's client-runtime type graph; the runtime registry is
 * duck-typed against the same shapes.
 */

import type { NodeSummary } from '../store.ts'
import type { Translate } from '../locales.ts'

/** Subset of ClientSessionContext: stable session identity only. */
export interface ClientSessionContext {
  readonly sessionId: string
}

/** One menu candidate (display data). */
export interface InputTriggerCandidate {
  readonly name: string
  readonly description?: string
  readonly icon?: string
  readonly hint?: string
}

/** Candidate request: query text + supersede signal. */
export interface CandidateRequest {
  readonly query: string
  readonly position: 'leading' | 'inline'
  readonly signal: AbortSignal
}

/** Pick-moment snapshot (CAS on draft revision). */
export interface TokenSpan {
  readonly start: number
  readonly end: number
  readonly draftRev: number
}

/** Everything a source receives on pick. */
export interface InputTriggerPick {
  readonly candidate: InputTriggerCandidate
  readonly session: ClientSessionContext
  readonly position: 'leading' | 'inline'
  readonly via: 'menu' | 'space' | 'enter'
  readonly span: TokenSpan
}

/** Inline reference insertion (U+FFFC placeholder + owner projections). */
export interface ReferenceInsert {
  readonly source: string
  readonly ref: string
  readonly label: string
  readonly clipboardText: string
}

/** Unified pick return (subset used by this source). */
export type PickOutcome =
  | { readonly insert: ReferenceInsert }
  | { readonly text: string }
  | 'handled'
  | undefined

/** Reference codec for sources producing insert outcomes. */
export interface ReferenceCodec {
  clipboardText(ref: string): string
  serialize(ref: string, signal: AbortSignal): Promise<string>
}

/** Trigger source contract (ui-input-trigger registerSource shape). */
export interface InputTriggerSource {
  readonly trigger: '/' | '@'
  readonly name: string
  readonly order?: number
  candidates(session: ClientSessionContext, req: CandidateRequest): Promise<readonly InputTriggerCandidate[]>
  onPick(pick: InputTriggerPick): PickOutcome
  warm?(session: ClientSessionContext): void
  lexicon?(session: ClientSessionContext): readonly string[] | undefined
  readonly codec?: ReferenceCodec
}

/** Minimal RPC caller shape (matches ClientConnectionRpc). */
export interface RpcCaller {
  call(channel: string, endpoint: string, payload: unknown, signal?: AbortSignal): Promise<RpcResultLike>
}

export type RpcResultLike = { ok: true; value: unknown } | { ok: false; error: { message: string } }

interface GetResult {
  readonly node: { id: string; type: NodeSummary['type']; title: string; status: NodeSummary['status']; archived: boolean; priority: number; path: string }
  readonly parent?: NodeSummary
  readonly children: readonly NodeSummary[]
  readonly body: string
}

const CHANNEL = '/omt'
/**
 * Reference source key; one U+FFFC placeholder per occurrence carries it.
 * MUST equal this source's `name`: the submit pipeline resolves the codec
 * via `roster.all().find(s => s.name === occurrence.source)`.
 */
export const OMT_REF_SOURCE = 'ticket'

const CHILDREN_BEGIN = '<!-- omt:children -->'
const CHILDREN_END = '<!-- /omt:children -->'

/** Drop the managed children block (kept out of the model serialization). */
function stripChildrenBlock(body: string): string {
  const begin = body.indexOf(CHILDREN_BEGIN)
  const end = body.indexOf(CHILDREN_END)
  if (begin >= 0 && end > begin) {
    return (body.slice(0, begin) + body.slice(end + CHILDREN_END.length)).replace(/\n{3,}/g, '\n\n').trim()
  }
  return body.trim()
}

function escapeXml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

/**
 * Candidate ordering: active statuses keep the search-relevance order up
 * front; done sinks, archived sinks last (TICKET-0002). Stable sort.
 */
function statusRank(status: NodeSummary['status']): number {
  return status === 'done' ? 1 : 0
}

/** Status dot shown as the candidate icon (MenuView renders icon as text). */
const STATUS_ICON: Record<NodeSummary['status'], string> = {
  open: '⚪',
  in_progress: '🔵',
  done: '🟢',
}

/** Create the ticket trigger source bound to one RPC caller. */
export function createTicketSource(
  rpc: RpcCaller,
  onSerialized: ((sessionId: string | undefined, summary: NodeSummary) => void) | undefined,
  /** Bound translate (locale service) for the serialized block's labels. */
  t: Translate,
): InputTriggerSource {
  // Per-session id roll backing the synchronous lexicon hook (chip
  // decoration); refreshed on warm and after every candidates fetch.
  const lexiconCache = new Map<string, readonly string[]>()
  // id → title, refreshed on every candidates fetch; onPick uses it so the
  // chip's hover tooltip (title={label}) shows the full ticket name.
  const titles = new Map<string, string>()
  // The codec contract (serialize(ref, signal)) carries no session, so the
  // source tracks the most recent session its menu-facing callbacks saw —
  // a pick followed by a submit is always same-session in practice.
  let lastSessionId: string | undefined

  const fetchCandidates = async (sessionId: string, query: string, signal: AbortSignal): Promise<readonly NodeSummary[]> => {
    const result = await rpc.call(CHANNEL, 'search', { sessionId, query, limit: 20 }, signal)
    if (!result.ok) return []
    return result.value as readonly NodeSummary[]
  }



  return {
    trigger: '@',
    name: 'ticket',
    // After the built-in subagent group.
    order: 10,

    async candidates(session: ClientSessionContext, req: CandidateRequest): Promise<readonly InputTriggerCandidate[]> {
      lastSessionId = session.sessionId
      const nodes = await fetchCandidates(session.sessionId, req.query, req.signal)
      lexiconCache.set(session.sessionId, nodes.map(node => node.id))
      for (const node of nodes) titles.set(node.id, node.title)
      // Archived nodes are never offered (TICKET-0004); of the rest,
      // active first (relevance order preserved), done last.
      const visible = nodes.filter(node => !node.archived)
      const sorted = [...visible].sort((a, b) => statusRank(a.status) - statusRank(b.status))
      return sorted.map(node => ({
        name: node.id,
        description: `[${node.type} · ${node.status}] ${node.title}`,
        icon: STATUS_ICON[node.status],
      }))
    },

    onPick(pick: InputTriggerPick): PickOutcome {
      const id = pick.candidate.name
      const title = titles.get(id)
      return {
        insert: {
          source: OMT_REF_SOURCE,
          ref: id,
          // label doubles as the chip's hover tooltip — include the title so
          // hovering reveals the full ticket name (the cell itself clips).
          label: title !== undefined ? `${id} ${title}` : id,
          clipboardText: `@${id}`,
        },
      }
    },

    warm(session: ClientSessionContext): void {
      void fetchCandidates(session.sessionId, '', new AbortController().signal).then(
        nodes => {
          lexiconCache.set(session.sessionId, nodes.map(node => node.id))
        },
        () => {
          // Cold start without a reachable host: lexicon stays undefined,
          // decoration simply waits for the first successful candidates call.
        },
      )
    },

    lexicon(session: ClientSessionContext): readonly string[] | undefined {
      return lexiconCache.get(session.sessionId)
    },

    codec: {
      clipboardText: (ref: string): string => `@${ref}`,
      async serialize(ref: string, signal: AbortSignal): Promise<string> {
        const result = await rpc.call(CHANNEL, 'get', { sessionId: lastSessionId, id: ref }, signal)
        if (!result.ok) {
          // Serialization failure blocks the send (contract) — surface why.
          throw new Error(t('serialize.failed', { ref, message: result.error.message }))
        }
        const { node, parent, children, body } = result.value as GetResult
        // The reference survived into a submitted message — it is related to
        // this session's upcoming turn.
        onSerialized?.(lastSessionId, { id: node.id, type: node.type, title: node.title, status: node.status, archived: node.archived, priority: node.priority })
        const lines = [
          `<omt-ticket id="${escapeXml(node.id)}" type="${escapeXml(node.type)}" status="${escapeXml(node.status)}" title="${escapeXml(node.title)}">`,
          parent !== undefined
            ? t('serialize.parentLine', { id: parent.id, title: parent.title })
            : t('serialize.parentRoot'),
          children.length > 0
            ? t('serialize.childrenLine', { list: children.map(child => `${child.id} ${child.title}`).join(t('serialize.childSep')) })
            : t('serialize.childrenNone'),
          '',
          stripChildrenBlock(body),
          `</omt-ticket>`,
        ]
        return lines.join('\n')
      },
    },
  }
}
