/**
 * U10 store: ticket tree + selection + runs + settings state, fed by the
 * bridge. Pull-based freshness contract (plan U10 Approach): the home
 * list re-pulls on window focus (no cross-surface push signal in v1);
 * per-home ticket data streams through events/resume subscriptions.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { omtCall, subscribeEvents, type EventEnvelope } from './bridge'
import type { HomeInfo, OmtNode, RunDetail, RunSummary, SavedFilters } from './types'

const FILTERS_KEY = 'tauri:ui' // KD3: surface-prefixed bag key
const RECENT_KEY = 'recent' // KD3's single shared cross-surface key (R4)

export interface KnownHome {
  path: string
  name: string
  kind: string
  open: boolean
  missing: boolean
}

export interface TreeState {
  homes: HomeInfo[]
  knownHomes: KnownHome[]
  homeDir?: string
  harnessByHome: Record<string, import('./HarnessSelect').HarnessConfig>
  activeHome: HomeInfo | null
  nodes: OmtNode[]
  selectedId: string | null
  filters: SavedFilters
  recentIds: string[]
  runs: RunSummary[]
  loading: boolean
  error: string | null
}

const INITIAL: TreeState = {
  homes: [],
  knownHomes: [],
  harnessByHome: {},
  activeHome: null,
  nodes: [],
  selectedId: null,
  filters: {},
  recentIds: [],
  runs: [],
  loading: true,
  error: null,
}

/** QUOTA_EXCEEDED(rule=open-homes) must name the limit and the remedy. */
export function presentError(error: unknown): string {
  const text = String(error)
  if (text.includes('QUOTA_EXCEEDED') && text.includes('open-homes')) {
    return `同时打开的 home 数量已达 daemon 配额上限。解法：调大 daemon.json 的 limits.max_open_homes，或重启 daemon 释放空闲 home。(${text})`
  }
  if (text.includes('revision') || text.includes('CONFLICT')) {
    return `该 ticket 已被其他端修改（revision 冲突）— 请刷新后重试。(${text})`
  }
  if (text.includes('HOME_LOCKED')) {
    return `该 home 被 ts-bridge 占用，需显式 takeover（omt takeover）。(${text})`
  }
  return text
}

export function useTreeStore() {
  const [state, setState] = useState<TreeState>(INITIAL)
  const disposers = useRef<Array<() => void>>([])
  // Synchronous mirror of state.filters: saveFilters must compute the
  // merged bag WITHOUT relying on React's async setState updater.
  const filtersRef = useRef<SavedFilters>({})
  // Same stale-closure guard for activeHome: imperative actions
  // (updateNode/fetchRun) must read the LATEST home, not the render
  // frame their closure captured.
  const activeHomeRef = useRef<HomeInfo | null>(null)
  useEffect(() => {
    filtersRef.current = state.filters
    activeHomeRef.current = state.activeHome
  }, [state.filters, state.activeHome])

  const refreshHomes = useCallback(async () => {
    try {
      // Handshake projection of open homes (pull-based freshness, v1).
      const result = await invoke<{ homes?: HomeInfo[]; homeDir?: string }>('daemon_homes')
      const homes = result.homes ?? []
      void loadKnownHomes()
      for (const home of homes) void loadHarness(home)
      if (result.homeDir !== undefined) setState(s => ({ ...s, homeDir: result.homeDir }))
      setState(s => {
        // A homes refresh must NEVER unset an existing selection — a
        // transiently incomplete list (in-flight boot, declare still
        // landing) would otherwise wipe the user's active home. Only
        // auto-pick when nothing is selected yet.
        const next = s.activeHome ?? homes[0] ?? null
        const autoPicking = s.activeHome === null && next !== null
        if (next !== s.activeHome) activeHomeRef.current = next
        return {
          ...s,
          homes,
          activeHome: next,
          // Fresh auto-pick starts with an empty bag; the load below
          // adopts the home's persisted filters.
          filters: autoPicking ? {} : s.filters,
          loading: false,
          error: null,
        }
      })
    } catch (error) {
      setState(s => ({ ...s, loading: false, error: presentError(error) }))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const refreshNodes = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ trees?: OmtNode[] }>('node/tree', { homeId: home.homeId })
      // Wire TreeNode uses `nodeId` (generated common.ts); the desktop's
      // local model calls it `id`. Normalize AT THE BRIDGE BOUNDARY so
      // every downstream consumer (row render, selection, recent, query
      // filter) sees a real id.
      const normalize = (wire: OmtNode & { nodeId?: string }): OmtNode => ({
        ...wire,
        id: wire.nodeId ?? wire.id,
        children: ((wire as { children?: OmtNode[] }).children ?? []).map(child =>
          normalize(child as OmtNode & { nodeId?: string }),
        ),
      })
      setState(s => ({ ...s, nodes: (result.trees ?? []).map(normalize), error: null }))
    } catch (error) {
      setState(s => ({ ...s, error: presentError(error) }))
    }
  }, [])

  const refreshRuns = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ runs?: RunSummary[] }>('run/list', { homeId: home.homeId })
      setState(s => ({ ...s, runs: result.runs ?? [] }))
    } catch {
      /* runs are supplementary — tree stays usable */
    }
  }, [])

  const loadFilters = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ filters?: SavedFilters }>('ui/filters-get', {
        homeId: home.homeId,
        key: FILTERS_KEY,
      })
      // Merge OVER current state: a toggle the user made while this load
      // was in flight (or whose set RPC is still landing) must survive.
      setState(s => ({ ...s, filters: { ...(result.filters ?? {}), ...s.filters } }))
    } catch {
      /* keep current filters on load failure */
    }
  }, [])

  const saveFilters = useCallback(async (home: HomeInfo, patch: SavedFilters) => {
    // Merged bag drives BOTH the optimistic render and the wire payload
    // (ui/filters-set replaces the whole bag); the ref avoids depending
    // on React's async updater timing.
    const merged = { ...filtersRef.current, ...patch }
    filtersRef.current = merged
    setState(s => ({ ...s, filters: merged }))
    await omtCall('ui/filters-set', { homeId: home.homeId, key: FILTERS_KEY, filters: merged }).catch(() => {})
  }, [])

  const loadKnownHomes = useCallback(async () => {
    try {
      const result = await omtCall<{ homes?: KnownHome[] }>('home/list-known', {})
      setState(s => ({ ...s, knownHomes: result.homes ?? [] }))
    } catch {
      // Pre-list-known daemon (method NOT_FOUND) degrades to an empty
      // known list — the picker keeps working with open homes only.
      setState(s => ({ ...s, knownHomes: [] }))
    }
  }, [])

  const loadHarness = useCallback(async (home: HomeInfo) => {
    try {
      const result = await omtCall<{ filters?: Record<string, unknown> }>('ui/filters-get', {
        homeId: home.homeId,
        key: 'tauri:harness',
      })
      const bag = (result.filters ?? {}) as { harness?: import('./HarnessSelect').HarnessConfig }
      setState(s => ({
        ...s,
        harnessByHome: bag.harness
          ? { ...s.harnessByHome, [home.homeId]: bag.harness! }
          : Object.fromEntries(Object.entries(s.harnessByHome).filter(([id]) => id !== home.homeId)),
      }))
    } catch {
      /* bag absent → no harness configured */
    }
  }, [])

  const saveHarness = useCallback(
    async (home: HomeInfo, config: import('./HarnessSelect').HarnessConfig | null) => {
      setState(s => ({
        ...s,
        harnessByHome: config
          ? { ...s.harnessByHome, [home.homeId]: config }
          : Object.fromEntries(Object.entries(s.harnessByHome).filter(([id]) => id !== home.homeId)),
      }))
      await omtCall('ui/filters-set', {
        homeId: home.homeId,
        key: 'tauri:harness',
        filters: config ? { harness: config } : {},
      }).catch(() => {})
    },
    [],
  )

  const loadRecent = useCallback(async () => {
    try {
      const result = await omtCall<{ refs?: Array<{ homeId: string; nodeId: string }> }>('ui/recent-get', { key: RECENT_KEY })
      setState(s => ({ ...s, recentIds: (result.refs ?? []).map(r => r.nodeId) }))
    } catch {
      /* recent list is cosmetic */
    }
  }, [])

  const touchRecent = useCallback(async (nodeId: string) => {
    setState(s => {
      const next = [nodeId, ...s.recentIds.filter(id => id !== nodeId)].slice(0, 10)
      void omtCall('ui/recent-set', {
        key: RECENT_KEY,
        refs: next.map(id => ({ homeId: '', nodeId: id })),
      }).catch(() => {})
      return { ...s, recentIds: next }
    })
  }, [])

  const selectHome = useCallback(
    async (home: HomeInfo) => {
      setState(s => ({ ...s, activeHome: home, selectedId: null, nodes: [], runs: [], filters: {} }))
      filtersRef.current = {}
      activeHomeRef.current = home
      await Promise.all([refreshNodes(home), refreshRuns(home), loadFilters(home)])
    },
    [refreshNodes, refreshRuns, loadFilters],
  )

  // Declare flow (U5 consumer): register an unregistered home directory,
  // then re-pull the home list. Pre-U5 daemons surface the feature gap
  // as a readable fallback message.
  const declareHome = useCallback(
    async (path: string) => {
      try {
        await omtCall('home/declare', { path })
        // requiresRehandshake: a fresh session folds the new home into the
        // credential's scoped grant before we re-list.
        await invoke('daemon_reconnect')
        await refreshHomes()
        await loadKnownHomes()
        return null
      } catch (error) {
        const text = String(error)
        if (text.includes('NOT_FOUND') || text.includes('method')) {
          return '当前 daemon 版本不支持动态收录 home（缺少 homeDeclare 能力位）— 请升级 daemon 后重试。'
        }
        return presentError(error)
      }
    },
    [refreshHomes],
  )

  const selectNode = useCallback(
    (id: string | null) => {
      setState(s => ({ ...s, selectedId: id }))
      if (id) touchRecent(id)
    },
    [touchRecent],
  )

  // Event-driven refresh (external CLI edits → second-level consistency).
  useEffect(() => {
    const home = state.activeHome
    if (!home) return
    let disposed = false
    let timer: ReturnType<typeof setTimeout> | undefined
    subscribeEvents(home.homeId, (_envelope: EventEnvelope) => {
      if (disposed) return
      // Debounce bursts (a run claim storms dozens of envelopes).
      clearTimeout(timer)
      timer = setTimeout(() => {
        void refreshNodes(home)
        void refreshRuns(home)
      }, 300)
    })
      .then(unlisten => {
        if (!disposed) disposers.current.push(unlisten)
      })
      .catch(() => {})
    return () => {
      disposed = true
      clearTimeout(timer)
    }
  }, [state.activeHome, refreshNodes, refreshRuns])

  // Boot + pull-based home freshness on window focus (v1 contract).
  useEffect(() => {
    void refreshHomes()
    void loadKnownHomes()
    void loadRecent()
    const onFocus = () => void refreshHomes()
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [refreshHomes, loadRecent])

  // Load per-home data whenever the active home changes.
  useEffect(() => {
    if (state.activeHome) void selectHome(state.activeHome)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.activeHome?.homeId])

  return {
    state,
    loadFilters,
    loadKnownHomes,
    saveHarness,
    selectHome,
    selectNode,
    saveFilters,
    declareHome,
    refreshHomes,
    refreshNodes,
    fetchRun: (runId: string) =>
      activeHomeRef.current
        ? omtCall<RunDetail>('run/get', { homeId: activeHomeRef.current.homeId, runId })
        : Promise.reject(new Error('no active home')),
    updateNode: async (id: string, patch: Record<string, unknown>, revision?: number) => {
      const home = activeHomeRef.current
      if (!home) return 'no active home'
      try {
        // Wire contract: nodeId + changes object; expectedRevision is a
        // top-level optimistic-concurrency gate (dispatch.rs).
        await omtCall('node/update', {
          homeId: home.homeId,
          nodeId: id,
          changes: patch,
          ...(revision !== undefined ? { expectedRevision: revision } : {}),
        })
        await refreshNodes(home)
        return null
      } catch (error) {
        return presentError(error)
      }
    },
  }
}
