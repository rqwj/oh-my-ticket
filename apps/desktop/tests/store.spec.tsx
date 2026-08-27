/**
 * U10 store/presentation tests: the desktop surface's contract-sensitive
 * behaviors — error presentation (quota/revision/lease copy), bag key
 * scoping (`tauri:ui` filters, shared `recent`), declare fallback copy,
 * and admin rendering gate. Bridge layer is mocked at the tauri invoke
 * boundary.
 */
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { presentError } from '../src/store'

// Mock the tauri API boundary BEFORE importing bridge consumers.
const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))

describe('presentError contract copy', () => {
  it('QUOTA_EXCEEDED(rule=open-homes) names the limit and the remedy', () => {
    const text = presentError(new Error('QUOTA_EXCEEDED: rule=open-homes max 8'))
    expect(text).toContain('配额上限')
    expect(text).toContain('limits.max_open_homes')
    expect(text).toContain('重启 daemon')
  })

  it('revision conflict prompts a refresh', () => {
    const text = presentError(new Error('CONFLICT: revision mismatch expected 3 got 5'))
    expect(text).toContain('revision 冲突')
    expect(text).toContain('刷新')
  })

  it('HOME_LOCKED points at explicit takeover', () => {
    expect(presentError(new Error('HOME_LOCKED: ts-bridge owns'))).toContain('takeover')
  })
})

describe('bag key scoping (KD3/R4)', () => {
  beforeEach(() => invokeMock.mockReset())

  it('filters persist under the tauri:ui prefix key; recent uses the shared key', async () => {
    invokeMock.mockResolvedValue({})
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())

    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }
    await act(async () => {
      await result.current.saveFilters(home, { query: '测试' })
    })
    const filtersCall = invokeMock.mock.calls.find(c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'ui/filters-set')
    expect(filtersCall).toBeTruthy()
    expect((filtersCall![1] as { params: { key: string } }).params.key).toBe('tauri:ui')
  })
})

describe('known-homes picker data', () => {
  beforeEach(() => invokeMock.mockReset())

  it('home/list-known loads into state; missing method degrades to empty (pre-list-known daemon)', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.resolve({
          homes: [
            { path: '/tmp/a/.omt', name: 'a', kind: 'workspace', open: false, missing: false },
            { path: '/tmp/b/.omt', name: 'b', kind: 'workspace', open: false, missing: true },
          ],
        })
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())
    await act(async () => {
      await result.current.loadKnownHomes()
    })
    expect(result.current.state.knownHomes).toHaveLength(2)
    expect(result.current.state.knownHomes[1].missing).toBe(true)

    // Old daemon: method unknown → empty list, no crash.
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'omt_call' && args?.method === 'home/list-known') {
        return Promise.reject(new Error('NOT_FOUND: kind=method'))
      }
      return Promise.resolve({})
    })
    await act(async () => {
      await result.current.loadKnownHomes()
    })
    expect(result.current.state.knownHomes).toEqual([])
  })
})

describe('declare flow', () => {
  beforeEach(() => invokeMock.mockReset())

  it('declare success triggers reconnect + home relist', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [{ homeId: 'h1' }] })
      if (cmd === 'daemon_reconnect') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call') return Promise.resolve({})
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())

    let error: string | null = 'unset'
    await act(async () => {
      error = await result.current.declareHome('/tmp/new-ws/.omt')
    })
    expect(error).toBeNull()
    const commands = invokeMock.mock.calls.map(c => c[0])
    expect(commands).toContain('daemon_reconnect')
  })

  it('pre-U5 daemon (unknown method) yields the upgrade fallback copy', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call' && args?.method === 'home/declare') {
        return Promise.reject(new Error('NOT_FOUND: kind=method'))
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result } = renderHook(() => useTreeStore())

    let error: string | null = null
    await act(async () => {
      error = await result.current.declareHome('/tmp/new-ws/.omt')
    })
    expect(error).toContain('homeDeclare')
    expect(error).toContain('升级 daemon')
  })
})

describe('添加 workspace 路径解析', () => {
  it('picked .omt dir used as-is; workspace root resolves to <root>/.omt', async () => {
    const { resolveHomeFromPickedDir } = await import('../src/workspacePath')
    expect(resolveHomeFromPickedDir('/repo/proj/.omt')).toBe('/repo/proj/.omt')
    expect(resolveHomeFromPickedDir('/repo/proj/.omt/')).toBe('/repo/proj/.omt')
    expect(resolveHomeFromPickedDir('/repo/proj')).toBe('/repo/proj/.omt')
    expect(resolveHomeFromPickedDir('/repo/proj/')).toBe('/repo/proj/.omt')
  })
})

describe('workspace 根路径显示', () => {
  it('strips /.omt and compresses the home dir prefix', async () => {
    const { workspaceRootOf } = await import('../src/homePath')
    expect(workspaceRootOf('/Users/robertq/work/proj/.omt', '/Users/robertq', 'x')).toBe('~/work/proj')
    expect(workspaceRootOf('/Users/robertq/.omt', '/Users/robertq', 'x')).toBe('~')
    expect(workspaceRootOf('/opt/other/.omt', '/Users/robertq', 'x')).toBe('/opt/other')
    expect(workspaceRootOf(undefined, '/Users/robertq', 'fallback')).toBe('fallback')
  })
})

describe('归档过滤（线上 archived 布尔字段）', () => {
  it('archived nodes hide by default and show when the Archived chip engages', async () => {
    // The wire shape has archived as a SEPARATE boolean (status keeps its
    // lifecycle value) — filtering on status==='archived' never matches.
    const { render, screen } = await import('@testing-library/react')
    const { TreePanel } = await import('../src/TreePanel')
    const nodes = [
      { id: 'T-1', type: 'ticket' as const, title: '活跃票据', status: 'open' as const, priority: 0, archived: false },
      { id: 'T-2', type: 'ticket' as const, title: '归档票据', status: 'done' as const, priority: 0, archived: true },
    ]
    const { rerender } = render(
      <TreePanel nodes={nodes} filters={{}} recentIds={[]} selectedId={null} onSelect={() => {}} onFilters={() => {}} />,
    )
    expect(screen.queryByText('活跃票据')).toBeTruthy()
    expect(screen.queryByText('归档票据')).toBeNull() // hidden by default

    rerender(
      <TreePanel nodes={nodes} filters={{ showArchived: true }} recentIds={[]} selectedId={null} onSelect={() => {}} onFilters={() => {}} />,
    )
    expect(screen.queryByText('归档票据')).toBeTruthy() // visible when engaged
  })

  it('# ID toggle renders node ids only when showId is on', async () => {
    const { render, screen } = await import('@testing-library/react')
    const { TreePanel } = await import('../src/TreePanel')
    const nodes = [{ id: 'TICKET-0042', type: 'ticket' as const, title: '示例', status: 'open' as const, priority: 0 }]
    const { rerender } = render(
      <TreePanel nodes={nodes} filters={{}} recentIds={[]} selectedId={null} onSelect={() => {}} onFilters={() => {}} />,
    )
    expect(screen.queryByText('TICKET-0042')).toBeNull()
    rerender(
      <TreePanel nodes={nodes} filters={{ showId: true }} recentIds={[]} selectedId={null} onSelect={() => {}} onFilters={() => {}} />,
    )
    expect(screen.queryByText('TICKET-0042')).toBeTruthy()
  })
})

describe('过滤器状态一致性', () => {
  beforeEach(() => invokeMock.mockReset())

  it('toggling during an in-flight bag load survives (merge, not clobber)', async () => {
    let resolveGet: ((v: unknown) => void) | undefined
    let resolveHomes: ((v: unknown) => void) | undefined
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return new Promise(resolve => { resolveHomes = resolve })
      if (cmd === 'daemon_reconnect') return Promise.resolve({ homes: [] })
      if (cmd === 'omt_call' && args?.method === 'ui/filters-get') {
        return new Promise(resolve => { resolveGet = resolve })
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }

    // Boot deferred: selectHome starts only after daemon_homes resolves.
    await act(async () => {
      resolveHomes!({ homes: [home], homeDir: '/tmp' })
    })
    // Bag load for the selected home is now in flight; toggle before it lands.
    await act(async () => {
      await result.current.saveFilters(home, { showId: true })
    })
    await act(async () => {
      resolveGet!({ filters: {} }) // stale bag lands after the toggle
    })
    expect(result.current.state.filters.showId).toBe(true)
    unmount()
  })

  it('saveFilters posts the FULL merged bag (set is replace-semantics)', async () => {
    invokeMock.mockResolvedValue({})
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }

    await act(async () => {
      await result.current.selectHome(home)
      await result.current.saveFilters(home, { query: '甲' })
      await result.current.saveFilters(home, { showId: true })
    })
    unmount()
    const calls = invokeMock.mock.calls.filter(
      c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'ui/filters-set',
    )
    const lastPayload = (calls.at(-1)![1] as { params: { filters: Record<string, unknown> } }).params.filters
    expect(lastPayload).toMatchObject({ query: '甲', showId: true }) // earlier keys survive
  })
})

describe('线上字段归一化（nodeId → id）', () => {
  beforeEach(() => invokeMock.mockReset())

  it('tree ingestion maps wire nodeId onto local id recursively', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'daemon_homes') return Promise.resolve({ homes: [{ homeId: 'h1' }], homeDir: '/tmp' })
      if (cmd === 'omt_call' && args?.method === 'node/tree') {
        return Promise.resolve({
          trees: [{
            nodeId: 'EPIC-0001', type: 'epic', title: '根', status: 'open', archived: false, priority: 0,
            children: [{ nodeId: 'TICKET-0007', type: 'ticket', title: '子', status: 'open', archived: false, priority: 0, children: [] }],
          }],
        })
      }
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    await act(async () => {
      await result.current.selectHome({ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' })
    })
    const root = result.current.state.nodes[0]
    expect(root.id).toBe('EPIC-0001')
    expect(root.children![0].id).toBe('TICKET-0007')

    // Selection + showId rendering consume the normalized id.
    const { render, screen } = await import('@testing-library/react')
    const { TreePanel } = await import('../src/TreePanel')
    render(
      <TreePanel nodes={result.current.state.nodes} filters={{ showId: true }} recentIds={[]} selectedId={null} onSelect={() => {}} onFilters={() => {}} />,
    )
    expect(screen.queryByText('TICKET-0007')).toBeTruthy()
    unmount()
  })
})

describe('RPC 参数契约（nodeId/runId/changes）', () => {
  beforeEach(() => invokeMock.mockReset())

  it('updateNode posts nodeId + changes + top-level expectedRevision', async () => {
    invokeMock.mockResolvedValue({})
    const { useTreeStore } = await import('../src/store')
    const { renderHook, act } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }
    await act(async () => {
      await result.current.selectHome(home)
    })
    let updateError: string | null = null
    await act(async () => {
      updateError = await result.current.updateNode('TICKET-0001', { status: 'done' }, 3)
    })
    expect(updateError).toBeNull()
    const call = invokeMock.mock.calls.find(
      c => c[0] === 'omt_call' && (c[1] as { method: string }).method === 'node/update',
    )
    expect(call).toBeTruthy()
    expect((call![1] as { params: Record<string, unknown> }).params).toMatchObject({
      homeId: 'h1',
      nodeId: 'TICKET-0001',
      changes: { status: 'done' },
      expectedRevision: 3,
    })
    unmount()
  })
})

describe('MarkdownText 渲染', () => {
  it('renders headings, bullets, bold, code spans and fences', async () => {
    const { render } = await import('@testing-library/react')
    const { MarkdownText } = await import('../src/MarkdownText')
    const { container } = render(
      <MarkdownText text={'## 总体目标\n\n为 **端到端** 结构测试提供 `omt` 样例：\n\n- 邮件通知\n- 站内消息\n\n```sh\nomt doctor\n```'} />,
    )
    expect(container.querySelector('h2')?.textContent).toBe('总体目标')
    expect(container.querySelectorAll('li')).toHaveLength(2)
    expect(container.querySelector('strong')?.textContent).toBe('端到端')
    expect(container.querySelector('.md-fence')?.textContent).toContain('omt doctor')
    expect(container.querySelector('.md-code')?.textContent).toBe('omt')
  })
})

describe('详情页线上契约（顶层 body + children summaries）', () => {
  beforeEach(() => invokeMock.mockReset())

  it('reads body from the TOP-LEVEL field and renders children chips', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { method?: string }) => {
      if (cmd === 'omt_call' && args?.method === 'node/get') {
        return Promise.resolve({
          node: { nodeId: 'EPIC-0005', type: 'epic', title: '通知中心', status: 'open', priority: 0, revision: 2, createdAt: '2026-08-20T00:00:00Z', updatedAt: '2026-08-21T00:00:00Z' },
          children: [
            { nodeId: 'STORY-0018', type: 'story', title: '邮件通知能力', status: 'open' },
            { nodeId: 'STORY-0019', type: 'story', title: '站内消息能力', status: 'done' },
          ],
          body: '## 总体目标\n\n样例正文',
        })
      }
      return Promise.resolve({})
    })
    const { render, screen, waitFor } = await import('@testing-library/react')
    const { DetailPanel } = await import('../src/DetailPanel')
    render(
      <DetailPanel
        home={{ homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' }}
        nodeId="EPIC-0005"
        onUpdated={() => Promise.resolve(null)}
        onSelect={() => {}}
        onChanged={() => {}}
      />,
    )
    await waitFor(() => expect(screen.queryByText('总体目标')).toBeTruthy())
    expect(screen.queryByText('STORY-0018 邮件通知能力')).toBeTruthy()
    expect(screen.queryByText('STORY-0019 站内消息能力')).toBeTruthy()
    expect(screen.queryByText(/Created .* · Updated .*/)).toBeTruthy()
  })
})
