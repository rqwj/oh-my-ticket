// @vitest-environment jsdom
/**
 * Run UI component tests (STORY-0013): RunsView list/detail rendering and
 * row-level actions (TICKET-0068/0069/0070), the join-run picker modal and
 * notice bar (TICKET-0067), the TicketPanel section nav + blocked/skipped
 * filter chips + 加入 run button, and the DocPanel run links +
 * awaiting_confirmation badge. Rendered with react-dom under jsdom; CSS
 * modules are stubbed by vitest so queries go by text/role.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createElement, useSyncExternalStore, type ReactElement } from 'react'
import { act } from 'react-dom/test-utils'
import { createRoot, type Root } from 'react-dom/client'
import { createSnapshotStore, type SnapshotStore } from './mocks/runtime-client.ts'
import { runFixture, runProgress, type RunFixtureOptions } from './mocks/run-fixtures.ts'
import { RunsView, type RunBindings } from '../src/client/components/RunsView.tsx'
import { NoticeBar, RunPickerModal } from '../src/client/components/RunPicker.tsx'
import { TicketPanel } from '../src/client/components/TicketPanel.tsx'
import { OmtController } from '../src/client/controller.ts'
import type { RpcResultLike } from '../src/client/trigger/source.ts'
import { DocPanel, type DocPanelProps } from '../src/client/components/DocPanel.tsx'
import { zh, type OmtKey, type Translate } from '../src/client/locales.ts'
import type {
  DocData,
  Notice,
  OmtTreeNode,
  PanelSection,
  RunDetailState,
  RunListState,
  RunPickerState,
  RunSummary,
  TreeState,
} from '../src/client/store.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */

;(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true

/** zh dictionary translator with {placeholder} interpolation. */
const t: Translate = ((key: OmtKey, params?: Record<string, unknown>) => {
  let text: string = zh[key]
  for (const [name, value] of Object.entries(params ?? {})) {
    text = text.replaceAll(`{${name}}`, String(value))
  }
  return text
}) as Translate

/** Renderer-bound selector hook over a mock snapshot store. */
function useStore<T>(store: SnapshotStore<T>) {
  return function useBound<S>(selector: (snapshot: T) => S): S {
    return useSyncExternalStore(store.subscribe, () => selector(store.getSnapshot()))
  }
}

interface Mounted {
  container: HTMLElement
  unmount: () => void
}

function mount(element: ReactElement): Mounted {
  const container = document.createElement('div')
  document.body.appendChild(container)
  const root: Root = createRoot(container)
  act(() => root.render(element))
  return {
    container,
    unmount: () => {
      act(() => root.unmount())
      container.remove()
    },
  }
}

function click(element: Element): void {
  act(() => {
    element.dispatchEvent(new MouseEvent('click', { bubbles: true }))
  })
}

function setValue(element: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(element.constructor.prototype, 'value')?.set
  act(() => {
    setter?.call(element, value)
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }))
  })
}

const progress = runProgress

/** 批次-labelled runs with a recent created_at (relative-time rendering). */
function run(id: string, status: RunSummary['status'], overrides: RunFixtureOptions = {}): RunSummary {
  return runFixture(id, status, {
    title: `批次 ${id}`,
    createdAt: new Date(Date.now() - 5 * 60_000).toISOString(),
    ...overrides,
  })
}

interface BindingSpies {
  calls: { name: string; args: unknown[] }[]
}

function makeBindings(overrides: {
  runs?: RunListState
  runDetail?: RunDetailState
  runPicker?: RunPickerState
  notice?: Notice
  panelSection?: PanelSection
} = {}): { bindings: RunBindings; spies: BindingSpies; stores: Record<string, SnapshotStore<any>> } {
  const stores = {
    runs: createSnapshotStore<RunListState>(overrides.runs ?? { status: 'idle' }),
    runDetail: createSnapshotStore<RunDetailState>(overrides.runDetail ?? { status: 'idle' }),
    runPicker: createSnapshotStore<RunPickerState | undefined>(overrides.runPicker),
    notice: createSnapshotStore<Notice | undefined>(overrides.notice),
    panelSection: createSnapshotStore<PanelSection>(overrides.panelSection ?? 'tickets'),
  }
  const spies: BindingSpies = { calls: [] }
  const spy = (name: string) => (...args: unknown[]) => {
    spies.calls.push({ name, args })
    if (name === 'showRuns') stores.panelSection.set('runs')
    if (name === 'showTickets') stores.panelSection.set('tickets')
    if (name === 'pickRun') stores.runPicker.set(undefined)
    if (name === 'cancelRunPicker') stores.runPicker.set(undefined)
    if (name === 'closeRunDetail') stores.runDetail.set({ status: 'idle' })
  }
  const bindings: RunBindings = {
    useRuns: useStore(stores.runs),
    useRunDetail: useStore(stores.runDetail),
    useRunPicker: useStore(stores.runPicker),
    useNotice: useStore(stores.notice),
    usePanelSection: useStore(stores.panelSection),
    showRuns: spy('showRuns'),
    showTickets: spy('showTickets'),
    refreshRuns: spy('refreshRuns'),
    openRun: spy('openRun'),
    closeRunDetail: spy('closeRunDetail'),
    showRunInPanel: spy('showRunInPanel'),
    runControl: spy('runControl'),
    runConfirm: spy('runConfirm'),
    joinRun: spy('joinRun'),
    pickRun: spy('pickRun'),
    cancelRunPicker: spy('cancelRunPicker'),
  }
  return { bindings, spies, stores }
}

function byText(container: HTMLElement, text: string): HTMLElement | undefined {
  // Innermost match wins: ancestors share the same trimmed textContent when
  // they wrap a single labelled control (dispatching at the wrapper never
  // reaches the control's own handler).
  const matches = Array.from(container.querySelectorAll<HTMLElement>('button, span, div')).filter(el => el.textContent?.trim() === text)
  return matches.find(el => !matches.some(other => other !== el && el.contains(other)))
}

let mounted: Mounted[]

beforeEach(() => {
  mounted = []
})

afterEach(() => {
  for (const entry of mounted) entry.unmount()
  mounted = []
})

function render(element: ReactElement): HTMLElement {
  const entry = mount(element)
  mounted.push(entry)
  return entry.container
}

describe('RunsView list (TICKET-0068)', () => {
  it('shows non-terminal runs in the main list and folds terminal runs into the collapsed 历史 group', () => {
    const { bindings } = makeBindings({
      runs: {
        status: 'ready',
        runs: [run('RUN-1', 'running'), run('RUN-2', 'interrupted'), run('RUN-3', 'completed'), run('RUN-4', 'canceled')],
      },
    })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    // Main list: running + interrupted (显著展示 + 恢复按钮).
    expect(container.textContent).toContain('批次 RUN-1')
    expect(container.textContent).toContain('批次 RUN-2')
    expect(container.textContent).not.toContain('批次 RUN-3')
    const resume = byText(container, zh['run.action.resume'])
    expect(resume).toBeDefined()
    // 历史 group collapsed by default, expands on click.
    const toggle = byText(container, '▸ 历史（2）')
    expect(toggle).toBeDefined()
    click(toggle as HTMLElement)
    expect(container.textContent).toContain('批次 RUN-3')
    expect(container.textContent).toContain('批次 RUN-4')
  })

  it('opens the detail on row click and resumes an interrupted run in place', () => {
    const { bindings, spies } = makeBindings({
      runs: { status: 'ready', runs: [run('RUN-1', 'running'), run('RUN-2', 'interrupted')] },
    })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    const row = byText(container, '批次 RUN-1')?.closest('div')
    click(row as HTMLElement)
    expect(spies.calls.some(call => call.name === 'openRun' && call.args[0] === 'RUN-1')).toBe(true)
    click(byText(container, zh['run.action.resume']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'runControl' && call.args[0] === 'RUN-2' && call.args[1] === 'resume')).toBe(true)
  })

  it('renders the empty hint when no runs exist', () => {
    const { bindings } = makeBindings({ runs: { status: 'ready', runs: [] } })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    expect(container.textContent).toContain('还没有 run')
  })

  it('renders the load-failed placeholder for a list error', () => {
    const { bindings } = makeBindings({ runs: { status: 'error', message: 'RPC 炸了' } })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    expect(container.textContent).toContain('加载失败：RPC 炸了')
  })

  it('shows the emptyActive hint plus the 历史 group when only terminal runs exist', () => {
    const { bindings } = makeBindings({
      runs: { status: 'ready', runs: [run('RUN-3', 'completed'), run('RUN-4', 'canceled')] },
    })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    expect(container.textContent).toContain(zh['run.emptyActive'])
    // 历史分组照常折叠呈现。
    expect(byText(container, '▸ 历史（2）')).toBeDefined()
  })
})

describe('RunsView detail (TICKET-0068/0069/0070)', () => {
  const detailReady: RunDetailState = {
    status: 'ready',
    id: 'RUN-1',
    data: {
      run: {
        ...run('RUN-1', 'running'),
        config: { stopOnFailure: true, autoContinue: true, autoVerify: false, concurrency: 1 },
      },
      items: [
        { node_id: 'TICKET-0001', position: 0, state: 'done', attempts: 1, node: { id: 'TICKET-0001', title: '甲', status: 'done', archived: false } },
        {
          node_id: 'TICKET-0002', position: 1, state: 'running', attempts: 0,
          executor: { sessionId: 'sess-9', label: '子任务会话', parentSessionId: 'sess-1', isSubagent: true },
          node: { id: 'TICKET-0002', title: '乙', status: 'in_progress', archived: false },
        },
        { node_id: 'TICKET-0003', position: 2, state: 'awaiting_confirmation', attempts: 0, node: { id: 'TICKET-0003', title: '丙', status: 'done', archived: false } },
        { node_id: 'TICKET-0004', position: 3, state: 'failed', attempts: 2, last_error: '构建失败', node: { id: 'TICKET-0004', title: '丁', status: 'in_progress', archived: false } },
        { node_id: 'TICKET-0005', position: 4, state: 'pending', attempts: 0, stalled: true, node: { id: 'TICKET-0005', title: '戊', status: 'open', archived: false } },
        { node_id: 'TICKET-0006', position: 5, state: 'interrupted', attempts: 0, node: { id: 'TICKET-0006', title: '己', status: 'in_progress', archived: false } },
        { node_id: 'TICKET-0007', position: 6, state: 'blocked', attempts: 0, node: { id: 'TICKET-0007', title: '庚', status: 'blocked', archived: false } },
        { node_id: 'TICKET-0008', position: 7, state: 'skipped', attempts: 0, node: { id: 'TICKET-0008', title: '辛', status: 'skipped', archived: false } },
      ],
    },
  }

  it('renders items with states, lineage, attempts, error, and markers', () => {
    const { bindings } = makeBindings({ runDetail: detailReady })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    // 谱系「父会话 ↳ subagent」
    expect(container.textContent).toContain('↳ 子任务会话（子代理）')
    // attempts (failed item: attempts 2 → 尝试 3 次)
    expect(container.textContent).toContain('尝试 3 次')
    // last_error
    expect(container.textContent).toContain('构建失败')
    // 停滞 + Tier 3 核对 markers
    expect(container.textContent).toContain(zh['run.item.stalled'])
    expect(container.textContent).toContain(zh['run.item.interruptedBadge'])
    // config 只读
    expect(container.textContent).toContain(zh['run.configTitle'])
    expect(container.textContent).toContain(zh['run.config.stopOnFailure'])
    // blocked/skipped 状态呈现（0069: 详情一致呈现）
    expect(container.textContent).toContain(zh['run.itemState.blocked'])
    expect(container.textContent).toContain(zh['run.itemState.skipped'])
  })

  it('awaiting_confirmation items expose 确认完成/打回 (run-confirm)', () => {
    const { bindings, spies } = makeBindings({ runDetail: detailReady })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    click(byText(container, zh['run.action.confirmDone']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'runConfirm' && call.args[0] === 'RUN-1' && call.args[1] === 'TICKET-0003' && call.args[2] === 'confirm')).toBe(true)
    click(byText(container, zh['run.action.reject']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'runConfirm' && call.args[1] === 'TICKET-0003' && call.args[2] === 'reject')).toBe(true)
  })

  it('retry covers failed/interrupted/stalled-pending; remove skips in-flight items', () => {
    const { bindings, spies } = makeBindings({ runDetail: detailReady })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    const retryButtons = Array.from(container.querySelectorAll('button')).filter(el => el.textContent === zh['run.action.retry'])
    // failed + stalled pending + interrupted = 3 retry buttons.
    expect(retryButtons).toHaveLength(3)
    for (const button of retryButtons) click(button)
    const retries = spies.calls.filter(call => call.name === 'runControl' && call.args[1] === 'retry').map(call => call.args[2])
    expect(retries.sort()).toEqual(['TICKET-0004', 'TICKET-0005', 'TICKET-0006'])
    // Remove: done/failed/pending/interrupted/blocked/skipped can go;
    // running and awaiting_confirmation cannot.
    const removeButtons = Array.from(container.querySelectorAll('button')).filter(el => el.textContent === zh['run.action.remove'])
    expect(removeButtons).toHaveLength(6)
  })

  it('pending runs expose 开始执行; running runs expose 暂停/取消', () => {
    const pending: RunDetailState = {
      status: 'ready',
      id: 'RUN-P',
      data: {
        run: { ...run('RUN-P', 'pending'), config: { stopOnFailure: false, autoContinue: true, autoVerify: false, concurrency: 1 } },
        items: [],
      },
    }
    const { bindings, spies } = makeBindings({ runDetail: pending })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    const start = byText(container, zh['run.action.start'])
    expect(start).toBeDefined()
    click(start as HTMLElement)
    expect(spies.calls.some(call => call.name === 'runControl' && call.args[0] === 'RUN-P' && call.args[1] === 'start')).toBe(true)

    const { bindings: running, spies: spies2 } = makeBindings({ runDetail: detailReady })
    const container2 = render(createElement(RunsView, { bindings: running, select: () => {}, sessionId: 's1', t }))
    expect(byText(container2, zh['run.action.pause'])).toBeDefined()
    click(byText(container2, zh['run.action.cancel']) as HTMLElement)
    expect(spies2.calls.some(call => call.name === 'runControl' && call.args[1] === 'cancel')).toBe(true)
  })

  it('the back button returns to the list', () => {
    const { bindings, spies } = makeBindings({ runDetail: detailReady })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    click(byText(container, zh['run.back']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'closeRunDetail')).toBe(true)
  })

  it('renders the detail error with a working back button', () => {
    const { bindings, spies } = makeBindings({ runDetail: { status: 'error', id: 'RUN-9', message: 'NOT_FOUND' } })
    const container = render(createElement(RunsView, { bindings, select: () => {}, sessionId: 's1', t }))
    expect(container.textContent).toContain('加载失败：NOT_FOUND')
    click(byText(container, zh['run.back']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'closeRunDetail')).toBe(true)
  })
})

describe('RunPickerModal + NoticeBar (TICKET-0067)', () => {
  it('lists only the picker options with title/progress/created and picks one', () => {
    const picker: RunPickerState = {
      nodeId: 'STORY-0001',
      options: [run('RUN-1', 'running'), run('RUN-2', 'paused')],
    }
    const { bindings, spies } = makeBindings({ runPicker: picker })
    const container = render(createElement(RunPickerModal, {
      useRunPicker: bindings.useRunPicker,
      pickRun: bindings.pickRun,
      cancelRunPicker: bindings.cancelRunPicker,
      sessionId: 's1',
      t,
    }))
    expect(container.textContent).toContain(zh['run.picker.title'])
    expect(container.textContent).toContain('STORY-0001')
    expect(container.textContent).toContain('批次 RUN-1')
    expect(container.textContent).toContain('批次 RUN-2')
    expect(container.textContent).toContain('1/4')
    click(byText(container, '批次 RUN-2')?.closest('button') as HTMLElement)
    expect(spies.calls.some(call => call.name === 'pickRun' && call.args[0] === 'RUN-2')).toBe(true)
  })

  it('cancel dismisses the modal', () => {
    const picker: RunPickerState = { nodeId: 'T-1', options: [run('RUN-1', 'running'), run('RUN-2', 'paused')] }
    const { bindings, spies } = makeBindings({ runPicker: picker })
    const container = render(createElement(RunPickerModal, {
      useRunPicker: bindings.useRunPicker,
      pickRun: bindings.pickRun,
      cancelRunPicker: bindings.cancelRunPicker,
      sessionId: 's1',
      t,
    }))
    click(byText(container, zh['run.picker.cancel']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'cancelRunPicker')).toBe(true)
    expect(container.textContent).not.toContain(zh['run.picker.title'])
  })

  it('NoticeBar renders key-based and raw-text notices', () => {
    const ok: Notice = { kind: 'ok', key: 'run.noticeAdded', params: { run: '批次 RUN-1', added: 3, running: 1, skippedDone: 2, skippedArchived: 1, duplicates: 0 } }
    const { bindings } = makeBindings({ notice: ok })
    const container = render(createElement(NoticeBar, { useNotice: bindings.useNotice, t }))
    expect(container.textContent).toContain('已加入 批次 RUN-1')
    expect(container.textContent).toContain('新增 3 项')

    const err: Notice = { kind: 'error', text: 'INVALID_INPUT: 跨 home 加入被拒绝' }
    const { bindings: b2 } = makeBindings({ notice: err })
    const container2 = render(createElement(NoticeBar, { useNotice: b2.useNotice, t }))
    expect(container2.textContent).toContain('跨 home 加入被拒绝')
  })
})

function treeNode(id: string, status: OmtTreeNode['status'], children: OmtTreeNode[] = [], type: OmtTreeNode['type'] = 'ticket'): OmtTreeNode {
  return { id, type, title: `节点 ${id}`, status, archived: false, priority: 0, path: 'p', created_at: '', updated_at: '', children }
}

describe('TicketPanel (TICKET-0067/0069)', () => {
  interface PanelSpies {
    readonly saved: { filters: any; sessionId: string | undefined }[]
    readonly created: { input: any; sessionId: string | undefined }[]
    readonly selected: { id: string; sessionId: string | undefined; scope: string | undefined }[]
    loadFilters: (sessionId?: string) => Promise<any>
  }
  function renderPanel(
    treeState?: TreeState,
    saved: any = undefined,
    createNode: (input: any, sessionId?: string) => Promise<string | undefined> = async () => undefined,
    sessionId?: string,
  ) {
    const resolvedSessionId = arguments.length >= 4 ? sessionId : 's1'
    const { bindings, spies, stores } = makeBindings()
    const tree = createSnapshotStore<TreeState>(treeState ?? {
      status: 'ready',
      forest: [treeNode('EPIC-0001', 'open', [treeNode('TICKET-0001', 'blocked'), treeNode('TICKET-0002', 'skipped')])],
    })
    const active = createSnapshotStore(undefined)
    const collapsed = createSnapshotStore<Record<string, boolean>>({})
    const session = createSnapshotStore<string | undefined>(resolvedSessionId)
    const useSession = useStore(session)
    const panelSpies: PanelSpies = {
      saved: [],
      created: [],
      selected: [],
      loadFilters: () => Promise.resolve(saved ?? {
        query: '', showArchived: false, types: [], statuses: [], priorities: [], showId: false, sortOrder: 'none',
      }),
    }
    function PanelHarness() {
      const currentSessionId = useSession(value => value)
      return createElement(TicketPanel, {
        useTree: useStore(tree),
        useActive: useStore(active),
        useCollapsed: useStore(collapsed),
        toggleCollapsed: () => {},
        refreshTree: () => {},
        reindex: () => {},
        loadFilters: panelSpies.loadFilters,
        saveFilters: (targetSessionId, filters) => {
          panelSpies.saved.push({ sessionId: targetSessionId, filters })
          return Promise.resolve()
        },
        select: (id, targetSessionId, scope) => { panelSpies.selected.push({ id, sessionId: targetSessionId, scope }) },
        archive: () => {},
        createNode: async (input, targetSessionId) => {
          panelSpies.created.push({ input, sessionId: targetSessionId })
          return await createNode(input, targetSessionId)
        },
        expandIds: () => {},
        runView: bindings,
        sessionId: currentSessionId,
        t,
      })
    }
    const mounted = render(createElement(PanelHarness))
    const panelStores: Record<string, SnapshotStore<any>> = { ...stores, session }
    return { container: mounted, spies, stores: panelStores, panelSpies }
  }

  const flush = async (): Promise<void> => { await act(async () => { await new Promise(resolve => setTimeout(resolve, 400)) }) }
  /** Resolve the hydration promise (no debounce wait) — keeps legacy sync tests act-clean. */
  const settle = async (): Promise<void> => { await act(async () => { await Promise.resolve() }) }
  const searchInput = (container: HTMLElement): HTMLInputElement =>
    container.querySelector('input[type="search"]') as HTMLInputElement

  it('offers blocked/skipped filter chips alongside the lifecycle states', async () => {
    const { container } = renderPanel()
    await settle()
    expect(byText(container, zh['status.blocked'])).toBeDefined()
    expect(byText(container, zh['status.skipped'])).toBeDefined()
    // The blocked/skipped rows render (dots are CSS-stubbed; text survives).
    expect(container.textContent).toContain('节点 TICKET-0001')
    expect(container.textContent).toContain('节点 TICKET-0002')
  })

  it('every non-archived tree row has a 加入 run button that fires joinRun', async () => {
    const { container, spies } = renderPanel()
    await settle()
    const joinButtons = Array.from(container.querySelectorAll('button')).filter(el => el.title === zh['run.joinTitle'])
    expect(joinButtons.length).toBe(3) // epic + 2 tickets
    click(joinButtons[1] as HTMLElement)
    expect(spies.calls.some(call => call.name === 'joinRun' && call.args[0] === 'TICKET-0001')).toBe(true)
  })

  it('the section nav switches between the tree and the Runs 区块', async () => {
    const { container, spies, stores } = renderPanel()
    await settle()
    expect(byText(container, zh['run.sectionTickets'])).toBeDefined()
    click(byText(container, zh['run.sectionRuns']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'showRuns')).toBe(true)
    expect(stores.panelSection.getSnapshot()).toBe('runs')
    // Runs 区块 replaces the tree (loading state with no RPC behind it).
    expect(container.textContent).toContain(zh['run.loading'])
  })

  it('hydrates saved filters on mount so a reload restores the view (STORY-0023)', async () => {
    const { container } = renderPanel(undefined, {
      query: 'ticket-0001', showArchived: false, types: [], statuses: ['blocked'], priorities: [], showId: true, sortOrder: 'priority-desc',
    })
    await flush()
    expect(searchInput(container).value).toBe('ticket-0001')
    // The query filters the forest; the skipped sibling stays hidden.
    expect(container.textContent).toContain('节点 TICKET-0001')
    expect(container.textContent).not.toContain('节点 TICKET-0002')
  })

  it('autosaves filter changes (debounced) and reset persists immediately (STORY-0023)', async () => {
    const { container, panelSpies } = renderPanel()
    await flush() // hydration completes → autosave armed

    click(byText(container, zh['status.blocked']) as HTMLElement)
    await flush()
    const afterChip = panelSpies.saved.at(-1)
    expect(afterChip?.filters.statuses).toEqual(['blocked'])
    expect(afterChip?.sessionId).toBe('s1')

    // Reset: right-aligned button at the end of the filter rows; defaults
    // persist immediately (no debounce) and the panel returns to defaults.
    const savedBefore = panelSpies.saved.length
    click(byText(container, zh['drawer.resetFilters']) as HTMLElement)
    const resetWrite = panelSpies.saved.at(-1)
    expect(resetWrite?.filters).toEqual({
      query: '', showArchived: false, types: [], statuses: [], priorities: [], showId: false, sortOrder: 'none',
    })
    expect(searchInput(container).value).toBe('')
    expect(panelSpies.saved.length).toBeGreaterThanOrEqual(savedBefore + 1)
  })

  it('offers the direct legal child types for a story parent', async () => {
    const story = treeNode('STORY-0001', 'open', [], 'story')
    const { container } = renderPanel({ status: 'ready', forest: [story] })
    await settle()
    click(container.querySelector(`button[title="${zh['drawer.addChild']}"]`) as HTMLElement)
    const typeSelect = container.querySelector('form select[name="type"]') as HTMLSelectElement
    expect(Array.from(typeSelect.options).map(option => option.value)).toEqual(['substory', 'ticket'])
  })

  it('requires an explicit home scope when creating a root Epic', async () => {
    const { container, panelSpies } = renderPanel(undefined, undefined, async () => 'EPIC-0099')
    await settle()
    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    const form = container.querySelector('form') as HTMLFormElement
    setValue(form.querySelector('input') as HTMLInputElement, '根 Epic')
    act(() => { form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) })
    expect(panelSpies.created).toEqual([])

    const scope = form.querySelector('select[name="scope"]') as HTMLSelectElement
    expect(Array.from(scope.options).map(option => option.value)).toEqual(['', 'workspace', 'global'])
    setValue(scope, 'global')
    await act(async () => {
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      await Promise.resolve()
    })
    expect(panelSpies.created).toEqual([{ input: { type: 'epic', title: '根 Epic', scope: 'global', parentId: undefined, body: undefined }, sessionId: 's1' }])
    expect(panelSpies.selected).toEqual([{ id: 'EPIC-0099', sessionId: 's1', scope: 'global' }])
  })

  it('does not offer workspace scope without a live session', async () => {
    const { container } = renderPanel(undefined, undefined, async () => undefined, undefined)
    await settle()
    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    const scope = container.querySelector('select[name="scope"]') as HTMLSelectElement
    expect(Array.from(scope.options).map(option => option.value)).toEqual(['', 'global'])
  })

  it('submits a create request only once while it is pending', async () => {
    let resolveCreate!: (id: string | undefined) => void
    const pending = new Promise<string | undefined>(resolve => { resolveCreate = resolve })
    const { container, panelSpies } = renderPanel(undefined, undefined, async () => await pending)
    await settle()
    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    const form = container.querySelector('form') as HTMLFormElement
    setValue(form.querySelector('input') as HTMLInputElement, '唯一 Epic')
    setValue(form.querySelector('select[name="scope"]') as HTMLSelectElement, 'workspace')
    act(() => {
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    })
    expect(panelSpies.created).toHaveLength(1)
    expect((form.querySelector('button[type="submit"]') as HTMLButtonElement).disabled).toBe(true)
    await act(async () => { resolveCreate(undefined); await pending })
  })

  it.each(['envelope', 'transport'] as const)('shows %s create failure inline, retains drafts, and clears the error on retry', async kind => {
    let retry = false
    let resolveRetry!: (result: RpcResultLike) => void
    const controller = new OmtController({
      async call(): Promise<RpcResultLike> {
        if (retry) return await new Promise(resolve => { resolveRetry = resolve })
        if (kind === 'transport') throw new Error('connection lost')
        return { ok: false, error: { message: 'creation rejected' } }
      },
    }, { openDetails: () => {}, closeDetails: () => {} })
    const { container, panelSpies } = renderPanel(undefined, undefined, controller.createNode)
    await settle()
    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    const form = container.querySelector('form') as HTMLFormElement
    setValue(form.querySelector('input') as HTMLInputElement, 'draft title')
    setValue(form.querySelector('textarea') as HTMLTextAreaElement, 'draft body')
    setValue(form.querySelector('select[name="scope"]') as HTMLSelectElement, 'global')
    await act(async () => { form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) })
    expect(form.querySelector('[role="alert"]')?.textContent).toBe(kind === 'transport' ? 'connection lost' : 'creation rejected')
    expect((form.querySelector('input') as HTMLInputElement).value).toBe('draft title')
    expect((form.querySelector('textarea') as HTMLTextAreaElement).value).toBe('draft body')
    expect((form.querySelector('button[type="submit"]') as HTMLButtonElement).disabled).toBe(false)
    expect(panelSpies.selected).toEqual([])
    retry = true
    click(form.querySelector('button[type="submit"]')!)
    expect(form.querySelector('[role="alert"]')).toBeNull()
    expect((form.querySelector('button[type="submit"]') as HTMLButtonElement).disabled).toBe(true)
    await act(async () => { resolveRetry({ ok: true, value: { id: 'EPIC-0099' } }) })
    expect(container.querySelector('form')).toBeNull()
    expect(panelSpies.selected).toEqual([{ id: 'EPIC-0099', sessionId: 's1', scope: 'global' }])
    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    expect((container.querySelector('form input') as HTMLInputElement).value).toBe('')
    expect((container.querySelector('form textarea') as HTMLTextAreaElement).value).toBe('')
    expect(container.querySelector('form [role="alert"]')).toBeNull()
  })

  it('resets creation state and ignores an old completion after the session changes', async () => {
    const resolvers: Array<(id: string | undefined) => void> = []
    const { container, stores } = renderPanel(undefined, undefined, async () => await new Promise(resolve => { resolvers.push(resolve) }))
    await settle()
    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    let form = container.querySelector('form') as HTMLFormElement
    setValue(form.querySelector('input') as HTMLInputElement, '旧会话 Epic')
    setValue(form.querySelector('select[name="scope"]') as HTMLSelectElement, 'workspace')
    act(() => { form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) })

    act(() => stores.session.set('s2'))
    expect(container.querySelector('form')).toBeNull()

    click(container.querySelector(`button[title="${zh['drawer.newEpic']}"]`) as HTMLElement)
    form = container.querySelector('form') as HTMLFormElement
    setValue(form.querySelector('input') as HTMLInputElement, '新会话 Epic')
    setValue(form.querySelector('select[name="scope"]') as HTMLSelectElement, 'workspace')
    act(() => { form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) })
    expect(resolvers).toHaveLength(2)

    await act(async () => { resolvers[0]!(undefined); await Promise.resolve() })
    expect((form.querySelector('button[type="submit"]') as HTMLButtonElement).disabled).toBe(true)
    await act(async () => { resolvers[1]!(undefined); await Promise.resolve() })
  })
})

describe('DocPanel (TICKET-0067/0068/0070)', () => {
  function docData(overrides: Partial<DocData> = {}): DocData {
    return {
      node: treeNode('TICKET-0001', 'in_progress'),
      children: [],
      body: '正文',
      runs: [
        { id: 'RUN-1', title: '批次 RUN-1', status: 'running', itemState: 'running', progress: progress({ total: 3, done: 1 }) },
        { id: 'RUN-2', title: '批次 RUN-2', status: 'running', itemState: 'awaiting_confirmation', progress: progress({ total: 5, done: 4 }) },
      ],
      ...overrides,
    }
  }

  function renderDoc(data?: DocData, saveBodyOverride?: DocPanelProps['saveBody']) {
    const { bindings, spies } = makeBindings()
    const doc = createSnapshotStore({ status: 'ready', data: data ?? docData() } as any)
    const savedBodies: { id: string; body: string; sessionId: string | undefined }[] = []
    const bodyEditing: boolean[] = []
    const container = render(createElement(DocPanel, {
      sessionId: 's1',
      executeTicket: () => {},
      useDoc: useStore(doc),
      closeDoc: () => {},
      setStatus: () => {},
      setArchived: () => {},
      rename: () => {},
      setPriority: () => {},
      appendNote: () => {},
      saveBody: async (id, body, expectedRevision, sessionId, scope) => {
        savedBodies.push({ id, body, sessionId })
        await saveBodyOverride?.(id, body, expectedRevision, sessionId, scope)
      },
      setBodyEditing: editing => { bodyEditing.push(editing) },
      select: () => {},
      forget: () => {},
      ...bindings,
      t,
    }))
    return { container, spies, doc, savedBodies, bodyEditing }
  }

  it('lists every non-terminal run link with progress; awaiting items carry the 待确认 badge', () => {
    const { container } = renderDoc()
    expect(container.textContent).toContain(zh['doc.runs'])
    expect(container.textContent).toContain('批次 RUN-1 1/3')
    expect(container.textContent).toContain('批次 RUN-2 4/5')
    expect(container.textContent).toContain(zh['run.itemState.awaiting'])
  })

  it('run links deep-open the run detail in the panel', () => {
    const { container, spies } = renderDoc()
    const chip = byText(container, '批次 RUN-2 4/5')?.closest('button')
      ?? Array.from(container.querySelectorAll('button')).find(el => el.textContent?.includes('批次 RUN-2'))
    click(chip as HTMLElement)
    expect(spies.calls.some(call => call.name === 'showRunInPanel' && call.args[0] === 'RUN-2')).toBe(true)
  })

  it('the 加入 run button fires the join flow; the status select offers blocked/skipped', () => {
    const { container, spies } = renderDoc()
    click(byText(container, zh['run.join']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'joinRun' && call.args[0] === 'TICKET-0001')).toBe(true)
    const options = Array.from(container.querySelectorAll('option')).map(el => el.value)
    expect(options).toContain('blocked')
    expect(options).toContain('skipped')
  })

  it('cancels a body draft when selecting another ticket', () => {
    const { container, doc, savedBodies, bodyEditing } = renderDoc()
    click(byText(container, zh['doc.editBody']) as HTMLElement)
    setValue(container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement, 'TICKET-0001 草稿')
    act(() => doc.set({ status: 'ready', data: docData({ node: treeNode('TICKET-0002', 'open'), body: '第二份正文' }) }))
    const save = byText(container, zh['doc.saveBody'])
    if (save !== undefined) click(save)
    expect(savedBodies).toEqual([])
    expect(container.querySelector('textarea[rows="12"]')).toBeNull()
    expect(bodyEditing.at(-1)).toBe(false)
  })

  it('disables unqualified run actions for a global node in a workspace session', () => {
    const { container } = renderDoc(docData({ scope: 'global' }))
    expect((byText(container, zh['doc.execute']) as HTMLButtonElement).disabled).toBe(true)
    expect((byText(container, zh['run.join']) as HTMLButtonElement).disabled).toBe(true)
  })

  it('cancels a body draft when the same id switches to another home', () => {
    const { container, doc } = renderDoc(docData({ scope: 'global' }))
    click(byText(container, zh['doc.editBody']) as HTMLElement)
    setValue(container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement, '全局草稿')
    act(() => doc.set({ status: 'ready', data: docData({ scope: 'workspace' }) }))
    expect(container.querySelector('textarea[rows="12"]')).toBeNull()
    expect(container.textContent).toContain('正文')
  })

  it.each(['priority', 'status'] as const)('keeps an unsaved body through %s refresh and loads the saved body after saving', async field => {
    let data = docData({ scope: 'workspace', node: { ...treeNode('TICKET-0001', 'in_progress'), revision: 1 } })
    let deferGet = false
    let finishGet: (() => void) | undefined
    const controller = new OmtController({
      async call(_channel: string, endpoint: string, payload: any): Promise<RpcResultLike> {
        if (endpoint === 'get') {
          if (deferGet) await new Promise<void>(resolve => { finishGet = resolve })
          return { ok: true, value: data }
        }
        if (endpoint === 'update') {
          data = {
            ...data,
            node: {
              ...data.node,
              ...(payload.priority !== undefined ? { priority: payload.priority } : {}),
              ...(payload.status !== undefined ? { status: payload.status } : {}),
              ...(payload.body !== undefined ? { revision: 2 } : {}),
            },
            ...(payload.body !== undefined ? { body: payload.body } : {}),
          }
          return { ok: true, value: {} }
        }
        return { ok: true, value: [] }
      },
    }, { openDetails: () => {}, closeDetails: () => {} })
    await controller.select('TICKET-0001', 's1', 'workspace')
    const { bindings } = makeBindings()
    const container = render(createElement(DocPanel, {
      ...bindings, t, sessionId: 's1', useDoc: useStore(controller.doc),
      executeTicket: controller.executeTicket, closeDoc: controller.closeDoc,
      setStatus: controller.setStatus, setArchived: controller.setArchived, rename: controller.rename,
      setPriority: controller.setPriority, appendNote: controller.appendNote,
      saveBody: controller.saveBody, setBodyEditing: controller.setBodyEditing,
      select: controller.select, forget: controller.forget,
    }))
    click(byText(container, zh['doc.editBody'])!)
    setValue(container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement, 'unsaved draft')
    deferGet = true
    const select = container.querySelectorAll('select')[field === 'status' ? 0 : 1]!
    setValue(select, field === 'status' ? 'done' : '2')
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect((container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement | null)?.value).toBe('unsaved draft')
    expect(finishGet).toBeUndefined()
    click(byText(container, zh['doc.saveBody'])!)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(finishGet).toBeTypeOf('function')
    await act(async () => { finishGet!() })
    expect(controller.doc.getSnapshot()).toMatchObject({ status: 'ready', data: { body: 'unsaved draft', node: { revision: 2, [field]: field === 'status' ? 'done' : 2 } } })
    expect(container.querySelector('textarea[rows="12"]')).toBeNull()
    expect(container.textContent).toContain('unsaved draft')
  })

  it('preserves the body draft when saving fails', async () => {
    const { container } = renderDoc(undefined, async () => { throw new Error('RPC failed') })
    click(byText(container, zh['doc.editBody']) as HTMLElement)
    const editor = container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement
    setValue(editor, '保留这份草稿')
    await act(async () => {
      byText(container, zh['doc.saveBody'])!.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await Promise.resolve()
    })
    expect((container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement).value).toBe('保留这份草稿')
    expect(container.textContent).toContain('RPC failed')
  })

  it('ignores an old body-save settlement after selecting another ticket', async () => {
    let resolveSave!: () => void
    const pendingSave = new Promise<void>(resolve => { resolveSave = resolve })
    const { container, doc } = renderDoc(undefined, async () => await pendingSave)
    click(byText(container, zh['doc.editBody']) as HTMLElement)
    setValue(container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement, '旧草稿')
    click(byText(container, zh['doc.saveBody']) as HTMLElement)

    act(() => doc.set({ status: 'ready', data: docData({ node: treeNode('TICKET-0002', 'open'), body: '第二份正文' }) }))
    click(byText(container, zh['doc.editBody']) as HTMLElement)
    setValue(container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement, '新草稿')
    await act(async () => { resolveSave(); await pendingSave })

    expect((container.querySelector('textarea[rows="12"]') as HTMLTextAreaElement).value).toBe('新草稿')
  })
})
