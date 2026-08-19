import type { PromptSettingsView } from '../prompt-settings-model.ts'
import type { Translate } from '../locales.ts'

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
  return (
    <section>
      <p>{t('settings.helper')}</p>
      <label>
        {t('settings.extraLabel')}
        <textarea
          value={view.extraPrompt}
          placeholder={t('settings.extraPlaceholder')}
          onChange={event => props.setDraftExtra(event.target.value)}
          onBlur={event => props.setExtraPrompt(event.target.value)}
        />
      </label>
      <p>{t('settings.extraHelper')}</p>
      <fieldset>
        <legend>{t('settings.bindLabel')}</legend>
        {view.catalogStatus === 'loading' && <p>{t('settings.loading')}</p>}
        {view.catalogStatus === 'empty' && <p>{t('settings.empty')}</p>}
        {view.catalogStatus === 'error' && (
          <p>
            {t('settings.loadFailed', { message: view.catalogError })}
            <button type="button" onClick={props.retry}>{t('settings.retry')}</button>
          </p>
        )}
        {(view.catalogStatus === 'ready' || view.skills.some(row => row.missing)) && view.skills.map(row => {
          const label = row.missing
            ? `${row.name} ${t('settings.missing')}`
            : row.name === 'omt'
              ? `${row.name} (${t('settings.omtHint')})`
              : row.name
          return (
            <label key={row.name}>
              <input type="checkbox" checked={row.bound} onChange={() => props.toggle(row.name)} />
              {label}
            </label>
          )
        })}
      </fieldset>
      {view.writeError !== '' && (
        <p>
          {t('settings.writeFailed', { message: view.writeError })}
          <button type="button" onClick={props.retry}>{t('settings.retry')}</button>
        </p>
      )}
    </section>
  )
}
