/**
 * DocPanel: the OMT document view registered into the `details` slot with
 * priority shadowing while a ticket doc is active. Disposing the
 * registration (closeDoc) restores the stock tool-details panel.
 */
import { useState } from 'react'
import { MarkdownText } from '@deepseek-ai/dsh-client-ui-primitives'
import { formatRelative } from '../relative-time.ts'
import { priorityOptionLabel } from '../priority.ts'
import { ITEM_STATE_KEY, STATUS_KEY, type Translate } from '../locales.ts'
import type { DocRunLink, DocState, NodeSummary, OmtTreeNode } from '../store.ts'
import type { Selector } from './Drawer.tsx'
import { NoticeBar, RunPickerModal } from './RunPicker.tsx'
import type { RunBindings } from './RunsView.tsx'
import css from './DocPanel.module.css'

/** Public composer actions (session provide channel; may be absent). */
interface InputActionsLike {
  setDraft(text: string): void
  submit(): void
}

/**
 * DocPanelProps extends the run bindings flat: the inject hooks channel
 * binds stores/callbacks as top-level use<Name>/callback props (STORY-0013:
 * 加入 run button, run links, picker/notice hosting).
 */
export interface DocPanelProps extends RunBindings {
  /** Framework session-scope prop; routes RPC to the workspace home. */
  readonly sessionId?: string
  /** Composer actions from the session kit (used by the Execute button). */
  readonly inputActions?: InputActionsLike
  /** Execute RPC: in_progress + running mark + broadcast (instant refresh). */
  readonly executeTicket: (id: string, sessionId?: string) => void
  readonly useDoc: Selector<DocState>
  readonly closeDoc: () => void
  readonly setStatus: (id: string, status: OmtTreeNode['status'], sessionId?: string) => void
  readonly setArchived: (id: string, archived: boolean, sessionId?: string) => void
  readonly rename: (id: string, title: string, sessionId?: string) => void
  readonly setPriority: (id: string, priority: number, sessionId?: string) => void
  readonly appendNote: (id: string, text: string, sessionId?: string) => void
  readonly select: (id: string, sessionId?: string) => void
  /** NOT_FOUND cleanup: unpin + drop from related + close. */
  readonly forget: (id: string, sessionId?: string) => void
  /** Framework-injected translate seat (registration declares locale: NS). */
  readonly t: Translate
}

const STATUS_OPTIONS: { value: OmtTreeNode['status']; icon: string }[] = [
  { value: 'open', icon: '⚪' },
  { value: 'in_progress', icon: '🔵' },
  { value: 'done', icon: '🟢' },
  { value: 'blocked', icon: '🟡' },
  { value: 'skipped', icon: '⏭' },
]

const CHILDREN_BEGIN = '<!-- omt:children -->'
const CHILDREN_END = '<!-- /omt:children -->'

/** The managed block is rendered structurally (chips), not as markdown. */
function stripChildrenBlock(body: string): string {
  const begin = body.indexOf(CHILDREN_BEGIN)
  const end = body.indexOf(CHILDREN_END)
  if (begin >= 0 && end > begin) {
    return (body.slice(0, begin) + body.slice(end + CHILDREN_END.length)).trim()
  }
  return body.trim()
}

function RelationChip({ node, onSelect, t }: { node: NodeSummary; onSelect: (id: string) => void; t: Translate }) {
  return (
    <button
      type="button"
      className={css.chip}
      onClick={() => onSelect(node.id)}
      title={t('node.titleWithStatus', { id: node.id, title: node.title, status: t(STATUS_KEY[node.status]) })}
    >
      <span className={`omt-dot ${node.archived ? 'omt-status-archived' : `omt-status-${node.status}`}`} />
      {node.id} {node.title}
    </button>
  )
}

/** Run link chip (TICKET-0068): one per non-terminal run holding this ticket. */
function RunLinkChip({ link, onOpen, t }: { link: DocRunLink; onOpen: (id: string) => void; t: Translate }) {
  const awaiting = link.itemState === 'awaiting_confirmation'
  return (
    <button
      type="button"
      className={`${css.chip} ${awaiting ? css.chipAwaiting : ''}`}
      onClick={() => onOpen(link.id)}
      title={awaiting ? t('doc.awaitingConfirmation') : link.id}
    >
      <span className={`omt-dot omt-itemstate-${link.itemState}`} />
      {link.title ?? link.id} {link.progress.done}/{link.progress.total}
      {awaiting && <span className={css.awaitingBadge}>{t(ITEM_STATE_KEY.awaiting_confirmation)}</span>}
    </button>
  )
}

export function DocPanel(props: DocPanelProps) {
  const t = props.t
  const doc = props.useDoc(snapshot => snapshot)
  const [draft, setDraft] = useState('')
  const [copied, setCopied] = useState<string | undefined>(undefined)
  const [editingTitle, setEditingTitle] = useState(false)

  const copy = (key: string, text: string): void => {
    void navigator.clipboard?.writeText(text)
    setCopied(key)
    setTimeout(() => setCopied(current => (current === key ? undefined : current)), 1200)
  }

  if (doc.status === 'idle') return null
  if (doc.status === 'loading') {
    return (
      <div className={css.panel}>
        <div className={css.skeletonTitle} />
        <div className={css.skeletonLine} style={{ width: '40%' }} />
        <div className={css.skeletonLine} />
        <div className={css.skeletonLine} />
        <div className={css.skeletonLine} style={{ width: '70%' }} />
      </div>
    )
  }
  if (doc.status === 'error') {
    const notFound = doc.message.includes('NOT_FOUND')
    return (
      <div className={css.panel}>
        <div className={css.headerTop}>
          <div className={css.header}>
            <span className={css.id}>{doc.id}</span>
            <button type="button" className={css.closeButton} onClick={props.closeDoc} title={t('doc.close')}>×</button>
          </div>
        </div>
        <div className={css.errorBox}>
          <div className={css.errorIcon}>⚠</div>
          <div className={css.errorTitle}>{notFound ? t('doc.notFound') : t('doc.loadFailed')}</div>
          <div className={css.errorDetail}>{doc.message}</div>
          <div className={css.errorActions}>
            <button type="button" className={css.action} onClick={() => props.select(doc.id, props.sessionId)}>
              {t('doc.retry')}
            </button>
            {notFound && (
              <button type="button" className={css.action} onClick={() => props.forget(doc.id, props.sessionId)}>
                {t('doc.forget')}
              </button>
            )}
          </div>
        </div>
      </div>
    )
  }

  const { node, parent, children, body } = doc.data
  // While a session executes this ticket, mutating actions lock (TICKET-0026);
  // archived nodes stay sealed as before.
  const locked = node.archived || doc.data.running !== undefined
  const lockReason = node.archived ? undefined : t('doc.locked')
  return (
    <div className={css.panel}>
      <div className={css.headerTop}>
      <div className={css.header}>
        <span className={css.id} title={node.id}>{node.id}</span>
        <select
          className={css.statusSelect}
          value={node.status}
          disabled={node.archived}
          title={node.archived ? t('doc.statusReadonly') : undefined}
          onChange={event => props.setStatus(node.id, event.target.value as OmtTreeNode['status'], props.sessionId)}
        >
          {STATUS_OPTIONS.map(option => (
            <option key={option.value} value={option.value}>{option.icon} {t(STATUS_KEY[option.value])}</option>
          ))}
        </select>
        <select
          className={css.statusSelect}
          value={node.priority}
          disabled={node.archived}
          title={t('priority.selectTitle')}
          onChange={event => props.setPriority(node.id, Number(event.target.value), props.sessionId)}
        >
          {[0, 1, 2, 3].map(p => (
            <option key={p} value={p}>{priorityOptionLabel(t, p)}</option>
          ))}
        </select>
        <button type="button" className={css.closeButton} onClick={props.closeDoc} title={t('doc.close')}>
          ×
        </button>
      </div>
      {editingTitle && !node.archived ? (
        <input
          className={css.titleInput}
          defaultValue={node.title}
          autoFocus
          onKeyDown={event => {
            if (event.key === 'Enter') {
              props.rename(node.id, event.currentTarget.value, props.sessionId)
              setEditingTitle(false)
            }
            if (event.key === 'Escape') setEditingTitle(false)
          }}
          onBlur={() => setEditingTitle(false)}
        />
      ) : (
        <div
          className={css.title}
          title={node.archived ? undefined : t('doc.editTitle')}
          onClick={() => {
            if (!node.archived) setEditingTitle(true)
          }}
        >
          <span className={css.titleText}>{node.title}</span>
          {node.archived && <span className={css.archivedBadge}>{t('status.archived')}</span>}
        </div>
      )}
      <div className={css.times} title={t('doc.timesTitle', { created: node.created_at, updated: node.updated_at })}>
        {t('doc.createdAt', { time: formatRelative(t, node.created_at) })} · {t('doc.updatedAt', { time: formatRelative(t, node.updated_at) })}
      </div>

      {doc.data.running !== undefined && (
        <div className={css.running}>
          {t('doc.running', {
            session: doc.data.running.sessionLabel,
            since: new Date(doc.data.running.since).toLocaleTimeString(t('time.localeTag'), { hour: '2-digit', minute: '2-digit' }),
          })}
        </div>
      )}

      {parent !== undefined && (
        <div className={css.relation}>
          <span className={css.relationLabel}>{t('doc.parent')}</span>
          <RelationChip node={parent} onSelect={id => props.select(id, props.sessionId)} t={t} />
        </div>
      )}
      {children.length > 0 && (
        <div className={css.relation}>
          <span className={css.relationLabel}>{t('doc.children')}</span>
          <div className={css.chipRow}>
            {children.map(child => <RelationChip key={child.id} node={child} onSelect={id => props.select(id, props.sessionId)} t={t} />)}
          </div>
        </div>
      )}
      {doc.data.runs !== undefined && doc.data.runs.length > 0 && (
        <div className={css.relation}>
          <span className={css.relationLabel}>{t('doc.runs')}</span>
          <div className={css.chipRow}>
            {doc.data.runs.map(link => (
              <RunLinkChip key={link.id} link={link} onOpen={id => props.showRunInPanel(id, props.sessionId)} t={t} />
            ))}
          </div>
        </div>
      )}

      <div className={css.actions}>
        <button
          type="button"
          className={css.actionPrimary}
          disabled={props.inputActions === undefined || locked}
          title={node.archived ? t('doc.executeArchived') : locked ? lockReason : t('doc.executeTitle')}
          onClick={() => {
            // Execute first: status/running mark + SSE broadcast refresh the
            // panel immediately; the conversation submit follows.
            props.executeTicket(node.id, props.sessionId)
            props.inputActions?.setDraft(`@${node.id} ${t('doc.executeDraft')}`)
            props.inputActions?.submit()
          }}
        >
          {t('doc.execute')}
        </button>
        <button
          type="button"
          className={css.action}
          disabled={node.archived}
          title={node.archived ? t('doc.executeArchived') : t('run.joinTitle')}
          onClick={() => props.joinRun(node.id, props.sessionId)}
        >
          {t('run.join')}
        </button>
        <button
          type="button"
          className={css.action}
          disabled={!node.archived && doc.data.running !== undefined}
          title={!node.archived && doc.data.running !== undefined ? lockReason : undefined}
          onClick={() => props.setArchived(node.id, !node.archived, props.sessionId)}
        >
          {node.archived ? t('doc.restore') : t('doc.archive')}
        </button>
        <button type="button" className={css.action} onClick={() => copy('id', node.id)}>
          {copied === 'id' ? t('doc.copied') : t('doc.copyId')}
        </button>
        <button
          type="button"
          className={css.action}
          onClick={() => copy('path', doc.data.home !== undefined ? `${doc.data.home}/${node.path}` : node.path)}
        >
          {copied === 'path' ? t('doc.copied') : t('doc.copyPath')}
        </button>
      </div>

      </div>

      <div className={css.bodyScroll}>
        <MarkdownText text={stripChildrenBlock(body)} />
      </div>

      <div className={css.appendArea}>
      <div className={css.append}>
        <textarea
          className={css.appendInput}
          placeholder={node.archived ? t('doc.appendPlaceholderArchived') : locked ? t('doc.appendPlaceholderLocked') : t('doc.appendPlaceholder')}
          value={draft}
          disabled={locked}
          onChange={event => setDraft(event.target.value)}
          rows={2}
        />
        <button
          type="button"
          className={css.appendButton}
          disabled={locked}
          onClick={() => {
            props.appendNote(node.id, draft, props.sessionId)
            setDraft('')
          }}
        >
          {t('doc.append')}
        </button>
      </div>
      </div>

      <NoticeBar useNotice={props.useNotice} t={t} />
      <RunPickerModal
        useRunPicker={props.useRunPicker}
        pickRun={props.pickRun}
        cancelRunPicker={props.cancelRunPicker}
        sessionId={props.sessionId}
        t={t}
      />
    </div>
  )
}
