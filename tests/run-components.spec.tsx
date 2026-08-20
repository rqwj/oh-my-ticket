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
import { DocPanel } from '../src/client/components/DocPanel.tsx'
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

function treeNode(id: string, status: OmtTreeNode['status'], children: OmtTreeNode[] = []): OmtTreeNode {
  return { id, type: 'ticket', title: `节点 ${id}`, status, archived: false, priority: 0, path: 'p', created_at: '', updated_at: '', children }
}

describe('TicketPanel (TICKET-0067/0069)', () => {
  function renderPanel(treeState?: TreeState) {
    const { bindings, spies, stores } = makeBindings()
    const tree = createSnapshotStore<TreeState>(treeState ?? {
      status: 'ready',
      forest: [treeNode('EPIC-0001', 'open', [treeNode('TICKET-0001', 'blocked'), treeNode('TICKET-0002', 'skipped')])],
    })
    const active = createSnapshotStore(undefined)
    const collapsed = createSnapshotStore<Record<string, boolean>>({})
    const container = render(createElement(TicketPanel, {
      useTree: useStore(tree),
      useActive: useStore(active),
      useCollapsed: useStore(collapsed),
      toggleCollapsed: () => {},
      refreshTree: () => {},
      reindex: () => {},
      select: () => {},
      archive: () => {},
      runView: bindings,
      sessionId: 's1',
      t,
    }))
    return { container, spies, stores }
  }

  it('offers blocked/skipped filter chips alongside the lifecycle states', () => {
    const { container } = renderPanel()
    expect(byText(container, zh['status.blocked'])).toBeDefined()
    expect(byText(container, zh['status.skipped'])).toBeDefined()
    // The blocked/skipped rows render (dots are CSS-stubbed; text survives).
    expect(container.textContent).toContain('节点 TICKET-0001')
    expect(container.textContent).toContain('节点 TICKET-0002')
  })

  it('every non-archived tree row has a 加入 run button that fires joinRun', () => {
    const { container, spies } = renderPanel()
    const joinButtons = Array.from(container.querySelectorAll('button')).filter(el => el.title === zh['run.joinTitle'])
    expect(joinButtons.length).toBe(3) // epic + 2 tickets
    click(joinButtons[1] as HTMLElement)
    expect(spies.calls.some(call => call.name === 'joinRun' && call.args[0] === 'TICKET-0001')).toBe(true)
  })

  it('the section nav switches between the tree and the Runs 区块', () => {
    const { container, spies, stores } = renderPanel()
    expect(byText(container, zh['run.sectionTickets'])).toBeDefined()
    click(byText(container, zh['run.sectionRuns']) as HTMLElement)
    expect(spies.calls.some(call => call.name === 'showRuns')).toBe(true)
    expect(stores.panelSection.getSnapshot()).toBe('runs')
    // Runs 区块 replaces the tree (loading state with no RPC behind it).
    expect(container.textContent).toContain(zh['run.loading'])
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

  function renderDoc(data?: DocData) {
    const { bindings, spies } = makeBindings()
    const doc = createSnapshotStore({ status: 'ready', data: data ?? docData() } as any)
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
      select: () => {},
      forget: () => {},
      ...bindings,
      t,
    }))
    return { container, spies }
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
})
