import { describe, expect, it } from 'vitest'
import { boundNamesOf, catalogStatusOf, PromptSettingsModel, toggleBound } from '../src/client/prompt-settings-model.ts'

const rows = [
  { name: 'ce-plan', description: 'plan', bound: false, missing: false },
  { name: 'gone', description: '', bound: true, missing: true },
]

describe('toggleBound', () => {
  it('adds and removes a name without dropping missing rows', () => {
    const checked = toggleBound(rows, 'ce-plan')
    expect(boundNamesOf(checked)).toEqual(['ce-plan', 'gone'])
    const unchecked = toggleBound(checked, 'gone')
    expect(boundNamesOf(unchecked)).toEqual(['ce-plan'])
    expect(unchecked.find(row => row.name === 'gone')?.missing).toBe(true)
  })
})

describe('catalogStatusOf', () => {
  it('is empty when the catalog has no rows', () => {
    expect(catalogStatusOf([])).toBe('empty')
    expect(catalogStatusOf(rows)).toBe('ready')
  })
})

describe('PromptSettingsModel', () => {
  it('loads extra prompt and skills', async () => {
    const model = new PromptSettingsModel(async () => ({ extraPrompt: '中文', skills: rows }), async () => {})
    await model.load()
    expect(model.view.extraPrompt).toBe('中文')
    expect(model.view.catalogStatus).toBe('ready')
    expect(model.view.skills[1]?.missing).toBe(true)
  })

  it('ignores edits until the initial catalog snapshot is ready', async () => {
    const writes: unknown[] = []
    const model = new PromptSettingsModel(async () => ({ extraPrompt: '', skills: rows }), async value => { writes.push(value) })
    model.setDraftExtra('premature')
    await model.setExtraPrompt('premature')
    await model.toggle('ce-plan')
    expect(model.view.extraPrompt).toBe('')
    expect(writes).toEqual([])
  })

  it('ignores an older catalog load that resolves after a newer retry', async () => {
    const loads: Array<(value: { extraPrompt: string; skills: typeof rows }) => void> = []
    const model = new PromptSettingsModel(
      async () => await new Promise(resolve => { loads.push(resolve) }),
      async () => {},
    )
    const first = model.load()
    const second = model.load()
    loads[1]!({ extraPrompt: 'new', skills: rows })
    await second
    loads[0]!({ extraPrompt: 'old', skills: rows })
    await first
    expect(model.view.extraPrompt).toBe('new')
  })

  it('reloads on remount only after all queued settings writes settle', async () => {
    let stored = { extraPrompt: '', skills: rows }
    const writes: Array<() => void> = []
    const snapshots: typeof stored[] = []
    const model = new PromptSettingsModel(
      async () => { snapshots.push(stored); return stored },
      async patch => await new Promise<void>(resolve => {
        writes.push(() => {
          stored = {
            extraPrompt: patch.extraPrompt ?? stored.extraPrompt,
            skills: patch.boundSkillNames === undefined ? stored.skills : stored.skills.map(row => ({
              ...row, bound: patch.boundSkillNames!.includes(row.name),
            })),
          }
          resolve()
        })
      }),
    )
    await model.load()
    const toggle = model.toggle('ce-plan')
    const prompt = model.setExtraPrompt('queued prompt')
    await Promise.resolve()
    const remount = model.load()
    await Promise.resolve()
    expect(snapshots).toHaveLength(1)
    expect(boundNamesOf(model.view.skills)).toEqual(['ce-plan', 'gone'])
    writes[0]!()
    await toggle
    await Promise.resolve()
    expect(snapshots).toHaveLength(1)
    writes[1]!()
    await Promise.all([prompt, remount])
    expect(model.view.extraPrompt).toBe(stored.extraPrompt)
    expect(model.view.skills).toEqual(stored.skills)
    expect(model.view.catalogStatus).toBe('ready')
  })

  it('retries a stale read when an optimistic update queues its save during reload', async () => {
    let stored = { extraPrompt: '', skills: rows }
    let finishRead!: () => void
    let finishWrite!: () => void
    let reads = 0
    let reload: Promise<void> | undefined
    let reloadOnUpdate = false
    const model = new PromptSettingsModel(
      async () => {
        const snapshot = stored
        if (++reads === 2) await new Promise<void>(resolve => { finishRead = resolve })
        return snapshot
      },
      async patch => {
        await new Promise<void>(resolve => { finishWrite = resolve })
        stored = { ...stored, extraPrompt: patch.extraPrompt! }
      },
      view => {
        // A remount triggered by publishing optimistic state can precede enqueuePersist.
        if (reloadOnUpdate && view.catalogStatus === 'ready') {
          reloadOnUpdate = false
          reload = model.load()
        }
      },
    )
    await model.load()
    reloadOnUpdate = true
    const save = model.setExtraPrompt('latest')
    await Promise.resolve()
    finishRead()
    await Promise.resolve()
    await Promise.resolve()
    expect(model.view.extraPrompt).toBe('latest')
    finishWrite()
    await Promise.all([save, reload])
    expect(model.view.extraPrompt).toBe(stored.extraPrompt)
    expect(reads).toBe(3)
  })

  it('allows retry after a pending write fails without restoring its optimistic bindings', async () => {
    let rejectWrite!: (error: Error) => void
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async () => await new Promise<void>((_resolve, reject) => { rejectWrite = reject }),
    )
    await model.load()
    const save = model.toggle('ce-plan')
    await Promise.resolve()
    const retry = model.load()
    rejectWrite(new Error('persist failed'))
    await Promise.all([save, retry])
    expect(model.view.catalogStatus).toBe('ready')
    expect(boundNamesOf(model.view.skills)).toEqual(['gone'])
    expect(model.view.writeError).toBe('')
  })

  it('keeps extra prompt editable after a catalog error without clearing unknown skill bindings', async () => {
    const writes: unknown[] = []
    const model = new PromptSettingsModel(async () => { throw new Error('rpc down') }, async value => { writes.push(value) })
    await model.load()
    expect(model.view.catalogStatus).toBe('error')
    expect(model.view.catalogError).toBe('rpc down')
    await model.setExtraPrompt('仍可改')
    expect(model.view.extraPrompt).toBe('仍可改')
    expect(writes).toEqual([{ extraPrompt: '仍可改' }])
  })

  it('preserves a concurrently typed prompt draft when a toggle write fails', async () => {
    let rejectWrite!: (error: Error) => void
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async () => await new Promise<void>((_resolve, reject) => { rejectWrite = reject }),
    )
    await model.load()
    const pending = model.toggle('ce-plan')
    await Promise.resolve()
    model.setDraftExtra('未保存草稿')
    rejectWrite(new Error('toggle failed'))
    await pending
    expect(model.view.extraPrompt).toBe('未保存草稿')
    expect(boundNamesOf(model.view.skills)).toEqual(['gone'])
  })

  it('rolls back a failed toggle even when a newer prompt write is queued', async () => {
    const settlements: Array<{ resolve: () => void; reject: (error: Error) => void }> = []
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async () => await new Promise<void>((resolve, reject) => { settlements.push({ resolve, reject }) }),
    )
    await model.load()
    const toggle = model.toggle('ce-plan')
    const prompt = model.setExtraPrompt('新提示')
    await Promise.resolve()
    settlements[0]!.reject(new Error('toggle failed'))
    await toggle
    await Promise.resolve()
    settlements[1]!.resolve()
    await prompt
    expect(boundNamesOf(model.view.skills)).toEqual(['gone'])
    expect(model.view.extraPrompt).toBe('新提示')
  })

  it('reverts a failed write and does not treat the draft as saved', async () => {
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async () => { throw new Error('persist failed') },
    )
    await model.load()
    await model.toggle('ce-plan')
    expect(model.view.skills.find(row => row.name === 'ce-plan')?.bound).toBe(false)
    expect(model.view.writeError).toBe('persist failed')
  })

  it('serializes rapid toggles so an older write cannot overwrite the latest selection', async () => {
    const writes: { boundSkillNames: string[]; resolve: () => void }[] = []
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async payload => await new Promise<void>(resolve => { writes.push({ boundSkillNames: payload.boundSkillNames ?? [], resolve }) }),
    )
    await model.load()

    const first = model.toggle('ce-plan')
    const second = model.toggle('gone')
    await Promise.resolve()
    expect(writes).toHaveLength(1)
    expect(writes[0]?.boundSkillNames).toEqual(['ce-plan', 'gone'])

    writes[0]!.resolve()
    await first
    await Promise.resolve()
    expect(writes).toHaveLength(2)
    expect(writes[1]?.boundSkillNames).toEqual(['ce-plan'])
    writes[1]!.resolve()
    await Promise.all([first, second])
  })

  it('rolls back to the last saved snapshot when rapid queued writes both fail', async () => {
    const rejects: Array<(error: Error) => void> = []
    const model = new PromptSettingsModel(
      async () => ({ extraPrompt: '', skills: rows }),
      async () => await new Promise<void>((_resolve, reject) => { rejects.push(reject) }),
    )
    await model.load()

    const first = model.toggle('ce-plan')
    const second = model.toggle('gone')
    await Promise.resolve()
    rejects[0]!(new Error('first failed'))
    await first
    await Promise.resolve()
    rejects[1]!(new Error('second failed'))
    await second

    expect(boundNamesOf(model.view.skills)).toEqual(['gone'])
    expect(model.view.writeError).toBe('second failed')
  })
})
