/**
 * RunsView: the peer Runs 区块 (TICKET-0068) — run list and run detail,
 * shared by all three presentations (drawer / floating window / OMT tab)
 * through TicketPanel. Pure props component like every other shell:
 * reactive facts arrive as renderer-bound use<Name> selector hooks,
 * actions as plain callbacks.
 *
 * Layout: non-terminal runs (interrupted included — it needs human review)
 * form the main list; terminal runs fold into the collapsible 历史 group.
 * The detail view shows the item roster with 谱系/attempts/last_error,
 * stalled and Tier-3 markers, run-level controls, row-level retry/remove,
 * and the awaiting_confirmation 确认/打回 entry (TICKET-0070).
 */
import { useState } from 'react'
import { formatRelative } from '../relative-time.ts'
import { ITEM_STATE_KEY, RUN_STATUS_KEY, type Translate } from '../locales.ts'
import { canConfirmItem, canRemoveItem, canRetryItem, groupRuns, runControlActions, type RunControlAction, type RunControlCommand } from '../run-view.ts'
import type {
  Notice,
  PanelSection,
  RunDetailState,
  RunItemView,
  RunListState,
  RunPickerState,
  RunSummary,
} from '../store.ts'
import type { Selector } from './TicketPanel.tsx'
import css from './RunsView.module.css'

/**
 * Everything a shell forwards so the Runs 区块, the join-run entry
 * (TICKET-0067), the picker modal, and the result notice can render.
 * Grouped into one prop object so Drawer/FloatWindow/TicketTab forward it
 * untouched.
 */
export interface RunBindings {
  readonly useRuns: Selector<RunListState>
  readonly useRunDetail: Selector<RunDetailState>
  readonly useRunPicker: Selector<RunPickerState | undefined>
  readonly useNotice: Selector<Notice | undefined>
  readonly usePanelSection: Selector<PanelSection>
  readonly showRuns: (sessionId?: string) => void
  readonly showTickets: () => void
  readonly refreshRuns: (sessionId?: string) => void
  readonly openRun: (id: string, sessionId?: string) => void
  readonly closeRunDetail: () => void
  /** Deep-link from the doc panel: open the panel on this run's detail. */
  readonly showRunInPanel: (id: string, sessionId?: string) => void
  readonly runControl: (id: string, action: RunControlCommand, nodeId?: string, sessionId?: string) => void
  readonly runConfirm: (id: string, nodeId: string, decision: 'confirm' | 'reject', sessionId?: string) => void
  /** 加入 run entry (tree row + doc panel): collect node + subtree. */
  readonly joinRun: (nodeId: string, sessionId?: string) => void
  readonly pickRun: (runId: string, sessionId?: string) => void
  readonly cancelRunPicker: () => void
}

export interface RunsViewProps {
  readonly bindings: RunBindings
  /** Open the ticket doc for an item row. */
  readonly select: (id: string, sessionId?: string) => void
  readonly sessionId: string | undefined
  readonly t: Translate
}

function progressText(run: RunSummary): string {
  return `${run.progress.done}/${run.progress.total}`
}

function RunRow({ run, onOpen, onResume, t }: { run: RunSummary; onOpen: () => void; onResume?: () => void; t: Translate }) {
  return (
    <div
      className={`${css.runRow} ${run.status === 'interrupted' ? css.runRowInterrupted : ''}`}
      onClick={onOpen}
      title={run.id}
    >
      <span className={`omt-runbadge omt-runstatus-${run.status}`}>{t(RUN_STATUS_KEY[run.status])}</span>
      <span className={css.runTitle}>{run.title ?? run.id}</span>
      <span className={css.runProgress}>{progressText(run)}</span>
      {run.progress.failed > 0 && <span className={css.runFailed}>{t('run.failedCount', { count: run.progress.failed })}</span>}
      {run.stalled > 0 && <span className={css.runStalled}>{t('run.stalledCount', { count: run.stalled })}</span>}
      {run.status === 'interrupted' && onResume !== undefined && (
        <button
          type="button"
          className={css.inlineAction}
          title={t('run.resumeTitle')}
          onClick={(event) => {
            event.stopPropagation()
            onResume()
          }}
        >
          {t('run.action.resume')}
        </button>
      )}
      <span className={css.runTime}>{formatRelative(t, run.created_at)}</span>
    </div>
  )
}

function RunList({ runs, bindings, sessionId, t }: { runs: readonly RunSummary[]; bindings: RunBindings; sessionId: string | undefined; t: Translate }) {
  const [historyOpen, setHistoryOpen] = useState(false)
  const { main, history } = groupRuns(runs)
  return (
    <div className={css.list}>
      {main.length === 0 && history.length === 0 ? (
        <div className={css.placeholder}>{t('run.empty')}</div>
      ) : (
        <>
          {main.length === 0 && <div className={css.placeholder}>{t('run.emptyActive')}</div>}
          {main.map(entry => (
            <RunRow
              key={entry.id}
              run={entry}
              onOpen={() => bindings.openRun(entry.id, sessionId)}
              onResume={entry.status === 'interrupted' ? () => bindings.runControl(entry.id, 'resume', undefined, sessionId) : undefined}
              t={t}
            />
          ))}
          {history.length > 0 && (
            <div className={css.historyGroup}>
              <button type="button" className={css.historyToggle} onClick={() => setHistoryOpen(!historyOpen)}>
                {historyOpen ? '▾' : '▸'} {t('run.historyGroup', { count: history.length })}
              </button>
              {historyOpen && history.map(entry => (
                <RunRow key={entry.id} run={entry} onOpen={() => bindings.openRun(entry.id, sessionId)} t={t} />
              ))}
            </div>
          )}
        </>
      )}
    </div>
  )
}

const CONTROL_ACTION_KEY: Record<RunControlAction, 'run.action.start' | 'run.action.pause' | 'run.action.resume' | 'run.action.cancel'> = {
  start: 'run.action.start',
  pause: 'run.action.pause',
  resume: 'run.action.resume',
  cancel: 'run.action.cancel',
}

function ItemRow({ item, runId, bindings, select, sessionId, t }: {
  item: RunItemView
  runId: string
  bindings: RunBindings
  select: (id: string, sessionId?: string) => void
  sessionId: string | undefined
  t: Translate
}) {
  const stalled = item.stalled === true
  return (
    <div className={`${css.itemRow} ${item.state === 'interrupted' ? css.itemRowInterrupted : ''}`}>
      <div className={css.itemMain}>
        <span className={`omt-dot omt-itemstate-${item.state}`} />
        <span className={css.itemState}>{t(ITEM_STATE_KEY[item.state])}</span>
        <button
          type="button"
          className={css.itemNode}
          title={item.node !== undefined ? `${item.node_id} ${item.node.title}` : item.node_id}
          onClick={() => select(item.node_id, sessionId)}
        >
          {item.node_id}{item.node !== undefined ? ` ${item.node.title}` : ''}
        </button>
        {item.attempts > 0 && <span className={css.itemMeta}>{t('run.item.attempts', { count: item.attempts + 1 })}</span>}
      </div>
      {item.executor !== undefined && (
        <div className={css.itemLineage} title={item.executor.parentSessionId ?? item.executor.sessionId}>
          {item.executor.isSubagent === true
            ? t('run.executorSubagent', { label: item.executor.label })
            : item.executor.label}
        </div>
      )}
      {stalled && (
        <span className={css.stalledBadge} title={t('run.item.stalledTitle')}>{t('run.item.stalled')}</span>
      )}
      {item.state === 'interrupted' && (
        <span className={css.reviewBadge} title={t('run.item.interruptedTitle')}>{t('run.item.interruptedBadge')}</span>
      )}
      {item.last_error !== undefined && <div className={css.itemError} title={item.last_error}>{item.last_error}</div>}
      <div className={css.itemActions}>
        {canConfirmItem(item) && (
          <>
            <button
              type="button"
              className={css.confirmAction}
              onClick={() => bindings.runConfirm(runId, item.node_id, 'confirm', sessionId)}
            >
              {t('run.action.confirmDone')}
            </button>
            <button
              type="button"
              className={css.rejectAction}
              onClick={() => bindings.runConfirm(runId, item.node_id, 'reject', sessionId)}
            >
              {t('run.action.reject')}
            </button>
          </>
        )}
        {canRetryItem(item) && (
          <button
            type="button"
            className={css.inlineAction}
            title={stalled ? t('run.item.stalledTitle') : undefined}
            onClick={() => bindings.runControl(runId, 'retry', item.node_id, sessionId)}
          >
            {t('run.action.retry')}
          </button>
        )}
        {canRemoveItem(item) && (
          <button
            type="button"
            className={css.removeAction}
            title={t('run.item.removeTitle')}
            onClick={() => bindings.runControl(runId, 'remove', item.node_id, sessionId)}
          >
            {t('run.action.remove')}
          </button>
        )}
      </div>
    </div>
  )
}

function RunDetail({ detail, bindings, select, sessionId, t }: {
  detail: Extract<RunDetailState, { status: 'ready' }>
  bindings: RunBindings
  select: (id: string, sessionId?: string) => void
  sessionId: string | undefined
  t: Translate
}) {
  const { run, items } = detail.data
  return (
    <div className={css.detail}>
      <div className={css.detailHeader}>
        <button type="button" className={css.backButton} onClick={bindings.closeRunDetail}>
          {t('run.back')}
        </button>
        <button type="button" className={css.refreshButton} title={t('run.refresh')} onClick={() => bindings.openRun(run.id, sessionId)}>
          ↻
        </button>
      </div>
      <div className={css.detailTitle}>
        <span className={`omt-runbadge omt-runstatus-${run.status}`}>{t(RUN_STATUS_KEY[run.status])}</span>
        <span className={css.detailName} title={run.id}>{run.title ?? run.id}</span>
        <span className={css.runProgress}>{progressText(run)}</span>
      </div>
      <div className={css.detailTimes}>
        {t('run.createdAt', { time: formatRelative(t, run.created_at) })}
        {run.finished_at !== undefined && ` · ${t('run.finishedAt', { time: formatRelative(t, run.finished_at) })}`}
      </div>
      <div className={css.controls}>
        {runControlActions(run).map(action => (
          <button
            key={action}
            type="button"
            className={action === 'cancel' ? css.removeAction : css.inlineAction}
            title={action === 'start' ? t('run.action.startTitle') : action === 'resume' ? t('run.resumeTitle') : undefined}
            onClick={() => bindings.runControl(run.id, action, undefined, sessionId)}
          >
            {t(CONTROL_ACTION_KEY[action])}
          </button>
        ))}
      </div>
      <div className={css.config} title={t('run.configTitle')}>
        <span className={css.configLabel}>{t('run.configTitle')}</span>
        <span className={css.configItem}>{t('run.config.stopOnFailure')}: {run.config.stopOnFailure ? '✓' : '—'}</span>
        <span className={css.configItem}>{t('run.config.autoContinue')}: {run.config.autoContinue ? '✓' : '—'}</span>
        <span className={css.configItem}>{t('run.config.autoVerify')}: {run.config.autoVerify ? '✓' : '—'}</span>
        <span className={css.configItem}>{t('run.config.concurrency', { count: run.config.concurrency })}</span>
      </div>
      <div className={css.itemsLabel}>{t('run.items', { count: items.length })}</div>
      <div className={css.items}>
        {items.map(item => (
          <ItemRow key={item.node_id} item={item} runId={run.id} bindings={bindings} select={select} sessionId={sessionId} t={t} />
        ))}
      </div>
    </div>
  )
}

export function RunsView({ bindings, select, sessionId, t }: RunsViewProps) {
  const runs = bindings.useRuns(snapshot => snapshot)
  const detail = bindings.useRunDetail(snapshot => snapshot)
  if (detail.status !== 'idle') {
    if (detail.status === 'ready') {
      return <RunDetail detail={detail} bindings={bindings} select={select} sessionId={sessionId} t={t} />
    }
    if (detail.status === 'loading') {
      return (
        <div className={css.detail}>
          <div className={css.detailHeader}>
            <button type="button" className={css.backButton} onClick={bindings.closeRunDetail}>{t('run.back')}</button>
          </div>
          <div className={css.placeholder}>{t('run.loading')}</div>
        </div>
      )
    }
    return (
      <div className={css.detail}>
        <div className={css.detailHeader}>
          <button type="button" className={css.backButton} onClick={bindings.closeRunDetail}>{t('run.back')}</button>
        </div>
        <div className={css.placeholder}>{t('run.loadFailed', { message: detail.message })}</div>
      </div>
    )
  }
  if (runs.status === 'idle' || runs.status === 'loading') {
    return <div className={css.placeholder}>{t('run.loading')}</div>
  }
  if (runs.status === 'error') {
    return <div className={css.placeholder}>{t('run.loadFailed', { message: runs.message })}</div>
  }
  return <RunList runs={runs.runs} bindings={bindings} sessionId={sessionId} t={t} />
}
