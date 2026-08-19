/**
 * OmtShowRow: keyed tool.call.toolview renderer for the omt_show tool —
 * the tool result (a ticket document) renders as markdown inside the chat
 * tool card and the details panel's tool body.
 */
import { MarkdownText } from '@deepseek-ai/dsh-client-ui-primitives'
import type { Translate } from '../locales.ts'

/** Defensive view over the wire ToolCallBlock (running vs settled). */
interface ToolCallBlockLike {
  readonly kind?: string
  readonly content?: readonly { type: string; text?: string }[]
  readonly args?: { id?: string }
  readonly arguments?: { id?: string }
}

export interface OmtShowRowProps {
  readonly block: ToolCallBlockLike
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

export function OmtShowRow({ block, t }: OmtShowRowProps) {
  const id = block.args?.id ?? block.arguments?.id
  if (block.kind === undefined) {
    return <div style={{ padding: '4px 0', fontSize: 'var(--dsw-font-xxs-12-font-size)', opacity: 0.7 }}>{t('show.loading', { id: id ?? '' })}</div>
  }
  const text = (block.content ?? [])
    .filter(item => item.type === 'text' && typeof item.text === 'string')
    .map(item => item.text as string)
    .join('\n\n')
  if (text === '') {
    return <div style={{ padding: '4px 0', fontSize: 'var(--dsw-font-xxs-12-font-size)', opacity: 0.7 }}>{t('show.empty')}</div>
  }
  return <MarkdownText text={text} />
}
