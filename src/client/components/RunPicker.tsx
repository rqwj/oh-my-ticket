/**
 * RunPicker modal + NoticeBar: the multi-active-run chooser (TICKET-0067)
 * and the transient join/error notice. Both render inside whichever shell
 * triggered the flow (TicketPanel or DocPanel). Colors read host tokens
 * through var(--omt-*) / var(--dsw-*) only.
 */
import { formatRelative } from '../relative-time.ts'
import { RUN_STATUS_KEY, type Translate } from '../locales.ts'
import type { Notice, RunPickerState } from '../store.ts'
import type { Selector } from './TicketPanel.tsx'
import css from './RunPicker.module.css'

export interface RunPickerModalProps {
  readonly useRunPicker: Selector<RunPickerState | undefined>
  readonly pickRun: (runId: string, sessionId?: string) => void
  readonly cancelRunPicker: () => void
  readonly sessionId: string | undefined
  readonly t: Translate
}

export function RunPickerModal({ useRunPicker, pickRun, cancelRunPicker, sessionId, t }: RunPickerModalProps) {
  const picker = useRunPicker(snapshot => snapshot)
  if (picker === undefined) return null
  return (
    <div className={css.overlay} role="dialog" aria-label={t('run.picker.title')}>
      <div className={css.modal}>
        <div className={css.title}>{t('run.picker.title')}</div>
        <div className={css.subtitle}>{picker.nodeId}</div>
        <div className={css.options}>
          {picker.options.map(run => (
            <button
              key={run.id}
              type="button"
              className={css.option}
              onClick={() => pickRun(run.id, sessionId)}
              title={run.id}
            >
              <span className={`omt-runbadge omt-runstatus-${run.status}`}>{t(RUN_STATUS_KEY[run.status])}</span>
              <span className={css.optionTitle}>{run.title ?? run.id}</span>
              <span className={css.optionProgress}>{run.progress.done}/{run.progress.total}</span>
              <span className={css.optionTime}>{formatRelative(t, run.created_at)}</span>
            </button>
          ))}
        </div>
        <div className={css.footer}>
          <button type="button" className={css.cancel} onClick={cancelRunPicker}>
            {t('run.picker.cancel')}
          </button>
        </div>
      </div>
    </div>
  )
}

export interface NoticeBarProps {
  readonly useNotice: Selector<Notice | undefined>
  readonly t: Translate
}

export function NoticeBar({ useNotice, t }: NoticeBarProps) {
  const notice = useNotice(snapshot => snapshot)
  if (notice === undefined) return null
  const text = notice.text ?? (notice.key !== undefined ? t(notice.key, notice.params) : '')
  return (
    <div className={`${css.notice} ${notice.kind === 'error' ? css.noticeError : css.noticeOk}`} role="status">
      {text}
    </div>
  )
}
