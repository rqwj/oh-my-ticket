/**
 * OmtCorePool: workspace-aware home resolution with per-home core caching.
 * Resolution rule (option A): a workspace root carrying its own `.omt/`
 * directory wins; everything else falls back to the global home (plugin
 * config > OMT_HOME > ~/.omt). Reads and writes resolve identically, so a
 * project opts into a local home simply by having `.omt/` present (created
 * manually or by moving an existing home in).
 */
import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { OmtCore } from './core.ts'
import { TYPE_PREFIX, type NodeType } from './types.ts'

export interface OmtCorePoolOptions {
  /**
   * Live DSH session ids, consulted by the startup janitor every time a
   * core (re)opens (review fix #4): without it the janitor assumes NO
   * session is alive and demotes every running item to interrupted on
   * plugin load/reload. Cores open lazily — well after the agents registry
   * is injected — so evaluating the provider at open time is safe.
   */
  readonly activeSessionIds?: () => readonly string[]
  /**
   * Observer attached to each core right after it opens (run-event
   * listeners, TICKET-0065). Attached post-open, so the startup janitor's
   * demotions never reach it.
   */
  readonly onCoreOpened?: (core: OmtCore) => void
}

export class OmtCorePool {
  private readonly cores = new Map<string, Promise<OmtCore>>()

  /** @param globalHome - fallback home (already resolved from config/env). */
  constructor(
    readonly globalHome: string,
    private readonly options: OmtCorePoolOptions = {},
  ) {}

  /** Resolve the home directory for one workspace cwd (or the global fallback). */
  homeFor(cwd: string | undefined): string {
    if (cwd !== undefined) {
      const local = join(cwd, '.omt')
      if (existsSync(local)) return local
    }
    return this.globalHome
  }

  /** Open (or reuse) the core for one workspace cwd. */
  coreFor(cwd: string | undefined): Promise<OmtCore> {
    return this.coreForHome(this.homeFor(cwd))
  }

  /** Homes in play for one cwd: workspace-local (when present) + global. */
  private candidateHomes(cwd: string | undefined): string[] {
    const homes: string[] = []
    if (cwd !== undefined) {
      const local = join(cwd, '.omt')
      if (existsSync(local)) homes.push(local)
    }
    if (!homes.includes(this.globalHome)) homes.push(this.globalHome)
    return homes
  }

  /**
   * Allocate a pool-wide unique id: counters across every candidate home
   * are synced to the same value so ids never collide between the global
   * and workspace homes (bare-id ownership resolution depends on it).
   */
  async allocateId(type: NodeType, cwd: string | undefined, includeWorkspace = false): Promise<string> {
    const homes = this.candidateHomes(cwd)
    if (includeWorkspace && cwd !== undefined) {
      const local = join(cwd, '.omt')
      if (!homes.includes(local)) homes.unshift(local)
    }
    let max = 0
    const cores: OmtCore[] = []
    for (const home of homes) {
      const core = await this.coreForHome(home)
      cores.push(core)
      max = Math.max(max, core.counterValue(type))
    }
    const next = max + 1
    for (const core of cores) core.setCounter(type, next)
    return `${TYPE_PREFIX[type]}-${String(next).padStart(4, '0')}`
  }

  /** Open (or reuse) the core for an explicit home directory. */
  coreForHome(home: string): Promise<OmtCore> {
    let core = this.cores.get(home)
    if (core === undefined) {
      core = OmtCore.open(home, { activeSessionIds: this.options.activeSessionIds?.() })
      const onCoreOpened = this.options.onCoreOpened
      if (onCoreOpened !== undefined) {
        // Rejection is owned by the awaited promise above (callers see it);
        // this branch only skips the observer on a failed open.
        void core.then(opened => onCoreOpened(opened), () => {})
      }
      this.cores.set(home, core)
    }
    return core
  }

  /**
   * Explicit create target: 'workspace' uses the workspace's `.omt/` even
   * when it does not exist yet (creation implies opting in); 'global' (or a
   * missing cwd) always lands in the global home.
   */
  coreForScope(cwd: string | undefined, scope: 'workspace' | 'global'): Promise<OmtCore> {
    if (scope === 'global' || cwd === undefined) return this.coreForHome(this.globalHome)
    return this.coreForHome(join(cwd, '.omt'))
  }

  /**
   * Shared ownership resolution: workspace home (when present) first, then
   * global; the first core where `contains` holds wins. When no home
   * contains the id, the fallback core is returned so the caller's
   * NOT_FOUND error path stays consistent (homes is never empty — the
   * global home is always included).
   */
  private async coreForId(id: string, cwd: string | undefined, contains: (core: OmtCore, id: string) => boolean): Promise<OmtCore> {
    const homes = this.candidateHomes(cwd)
    let fallback: OmtCore | undefined
    for (const home of homes) {
      const core = await this.coreForHome(home)
      fallback ??= core
      if (contains(core, id)) return core
    }
    return fallback as OmtCore
  }

  /**
   * Resolve the core CONTAINING a node id: workspace home (when present)
   * first, then global. Child nodes must live in their parent's home, so
   * id-addressed operations route by ownership rather than by cwd. When no
   * home contains the id, the default-resolution core is returned so the
   * caller's NOT_FOUND error path stays consistent.
   */
  coreForNode(id: string, cwd: string | undefined): Promise<OmtCore> {
    return this.coreForId(id, cwd, (core, nodeId) => core.getNode(nodeId) !== undefined)
  }

  /**
   * Resolve the core CONTAINING a run id. Run ids count per home, so the
   * same id can exist in several homes — resolution follows the node rule
   * (workspace home first, then global; the caller's workspace context
   * disambiguates). When no home contains the id, the fallback core is
   * returned so the caller's NOT_FOUND path stays consistent.
   */
  coreForRun(id: string, cwd: string | undefined): Promise<OmtCore> {
    return this.coreForId(id, cwd, (core, runId) => core.getRun(runId) !== undefined)
  }

  /** Close every open core (tests and plugin teardown). */
  async closeAll(): Promise<void> {
    for (const core of this.cores.values()) {
      await core.then(instance => instance.close(), () => {})
    }
    this.cores.clear()
  }
}
