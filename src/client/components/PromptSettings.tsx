import { useEffect } from 'react'
import type { PromptSettingsView } from '../prompt-settings-model.ts'
import type { Translate } from '../locales.ts'
import css from './PromptSettings.module.css'

export type Selector<T> = <S>(selector: (snapshot: T) => S) => S

export interface PromptSettingsProps {
  readonly useView: Selector<PromptSettingsView>
  readonly t: Translate
  readonly setDraftExtra: (value: string) => void
  readonly setExtraPrompt: (value: string) => void
  readonly toggle: (name: string) => void
  readonly retry: () => void
}

export function PromptSettings(props: PromptSettingsProps) {
  const view = props.useView(snapshot => snapshot)
  const { t } = props
  useEffect(() => { props.retry() }, [])
  return (
    <section className={css.root}>
      <h2 className={css.title}>{t('settings.title')}</h2>
      <p className={css.intro}>{t('settings.helper')}</p>
      <label className={css.field}>
        <span className={css.label}>{t('settings.extraLabel')}</span>
        <textarea
          className={css.textarea}
          value={view.extraPrompt}
          placeholder={t('settings.extraPlaceholder')}
          rows={6}
          onChange={event => props.setDraftExtra(event.target.value)}
          onBlur={event => props.setExtraPrompt(event.target.value)}
        />
      </label>
      <p className={css.intro}>{t('settings.extraHelper')}</p>
      <div className={css.group}>
        <h3 className={css.groupTitle}>{t('settings.bindLabel')}</h3>
        {view.catalogStatus === 'loading' && <p className={css.status}>{t('settings.loading')}</p>}
        {view.catalogStatus === 'empty' && <p className={css.status}>{t('settings.empty')}</p>}
        {view.catalogStatus === 'error' && (
          <p className={css.error}>
            {t('settings.loadFailed', { message: view.catalogError })}
            <button type="button" className={css.retry} onClick={props.retry}>{t('settings.retry')}</button>
          </p>
        )}
        {(view.catalogStatus === 'ready' || view.skills.some(row => row.missing)) && (
          <div className={css.list}>
            {view.skills.map(row => {
              const hint = row.missing
                ? t('settings.missing')
                : row.name === 'omt'
                  ? t('settings.omtHint')
                  : undefined
              return (
                <label
                  key={row.name}
                  className={css.skill}
                  title={hint === undefined ? row.name : `${row.name} ${hint}`}
                >
                  <input type="checkbox" checked={row.bound} onChange={() => props.toggle(row.name)} />
                  <span className={css.skillBody}>
                    <span className={css.skillName}>{row.name}</span>
                    {hint !== undefined && (
                      <span className={`${css.skillHint} ${row.missing ? css.missing : ''}`}>{hint}</span>
                    )}
                  </span>
                </label>
              )
            })}
          </div>
        )}
      </div>
      {view.writeError !== '' && (
        <p className={css.error}>
          {t('settings.writeFailed', { message: view.writeError })}
          <button type="button" className={css.retry} onClick={props.retry}>{t('settings.retry')}</button>
        </p>
      )}
    </section>
  )
}
