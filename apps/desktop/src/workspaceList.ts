/**
 * U1 工作区列表（KTD14）：桌面端自管的工作区清单，持久化在 localStorage
 * `omt-desktop-workspaces`——不依赖 daemon 的已打开 home（最多开 8 个、空闲
 * 30 分钟即关闭，撑不起「左侧常驻列表」）。每项 `{path, homeId, name}`：
 * - path：home 目录（<root>/.omt）；
 * - homeId：daemon 打开后分配（随机 id，声明前不可知）——列表只收录开过
 *   的条目，重开靠点击时的懒声明；
 * - name：显示名（R7），取 workspaceRootOf(path) 的 basename——daemon 给的
 *   `name` 一律是 `.omt`，不可用。
 */
import type { HomeInfo } from './types'
import { workspaceRootOf } from './homePath'

export const WORKSPACES_KEY = 'omt-desktop-workspaces'

export interface WorkspaceEntry {
  path: string
  homeId: string
  name: string
}

/** 显示名（R7）：工作区根目录的 basename；全局 home 压缩后是 `~`，原样保留。 */
export function workspaceNameOf(path: string, homeDir?: string): string {
  const root = workspaceRootOf(path, homeDir, path)
  const base = root.split('/').pop() ?? root
  return base === '' ? root : base
}

/** 图标缩写（KTD14）：显示名前两个字符（按 Unicode 字符而非 UTF-16 码元切片，不切开 emoji）。 */
export function abbreviationOf(name: string): string {
  return Array.from(name).slice(0, 2).join('')
}

/** 上级目录（悬停提示用）：根路径去掉最后一段；没有上级（根被 homeDir
 *  压缩成 `~`、或已是顶层路径）→ null，调用方不再拼「name（parent）」。 */
export function parentDirOf(path: string, homeDir?: string): string | null {
  const root = workspaceRootOf(path, homeDir, path)
  const idx = root.lastIndexOf('/')
  return idx > 0 ? root.slice(0, idx) : null
}

/**
 * 路径哈希色相（0–359）：同缩写的工作区靠颜色区分（KTD14）。字符串 31 进
 * 哈希，稳定且与展示顺序无关。
 */
export function hueOf(path: string): number {
  let hash = 0
  for (let i = 0; i < path.length; i++) {
    hash = (hash * 31 + path.charCodeAt(i)) >>> 0
  }
  return hash % 360
}

/** 图标底色：哈希色相 → 固定饱和度/亮度的 hsl（逗号参数形式，jsdom 可解析）。 */
export function workspaceColor(path: string): string {
  return `hsl(${hueOf(path)}, 45%, 42%)`
}

/**
 * 首次迁移（KTD14）：daemon 已打开的 home 在前，known-homes 里 open 且尚未
 * 收录的条目随后（homeId 用 lastHomeId——它是历史值，声明后可能轮换，由
 * selectWorkspace 回写）。按 path 去重；known 条目的 missing 不拦 open
 * （open 即 daemon 已持有），路径失效由读侧的 missingPaths 标记。
 */
export function migrateWorkspaces(
  homes: HomeInfo[],
  knownHomes: ReadonlyArray<{ path: string; open: boolean; lastHomeId?: string }>,
  homeDir?: string,
): WorkspaceEntry[] {
  const out: WorkspaceEntry[] = []
  const seen = new Set<string>()
  const add = (path: string, homeId: string) => {
    if (path === '' || homeId === '' || seen.has(path)) return
    seen.add(path)
    out.push({ path, homeId, name: workspaceNameOf(path, homeDir) })
  }
  for (const home of homes) add(home.path ?? '', home.homeId)
  for (const known of knownHomes) {
    if (!known.open) continue
    add(known.path, known.lastHomeId ?? '')
  }
  return out
}

/** 收录一个工作区：按 path 与 homeId 去重，新条目排在末尾（加入顺序）。 */
export function appendWorkspace(list: WorkspaceEntry[], entry: WorkspaceEntry): WorkspaceEntry[] {
  if (list.some(w => w.homeId === entry.homeId || w.path === entry.path)) return list
  return [...list, entry]
}

/**
 * 从列表移除（KTD14 选中规则）：移除项不是当前 → 选中不变；是当前 →
 * 上一个，没有上一个就下一个，都没有 → null（调用方显示带「添加工作区」
 * 的空状态）。
 */
export function removeWorkspace(
  list: WorkspaceEntry[],
  homeId: string,
  currentHomeId: string | null,
): { list: WorkspaceEntry[]; selectHomeId: string | null } {
  const idx = list.findIndex(w => w.homeId === homeId)
  if (idx === -1) return { list, selectHomeId: currentHomeId }
  const next = list.filter((_, i) => i !== idx)
  const selectHomeId =
    homeId === currentHomeId
      ? next[idx - 1]?.homeId ?? next[idx]?.homeId ?? null
      : currentHomeId
  return { list: next, selectHomeId }
}

/**
 * 读存量列表：无存储（未迁移过）→ null；内容损坏（非 JSON / 非数组）→
 *  也返回 null 让调用方重新迁移；数组里形状不对的条目被剔除、其余保留。
 * §64 遗留：原始数组非空但全部条目无效 → 同样返回 null（按首次运行处
 * 理，重新迁移）——这种形态不是「用户清空了列表」（用户清空存的是空数组
 * `[]`），而是历史写入损坏，据为存量会把其他已开 home 的迁移永久跳过。
 */
export function readStoredWorkspaces(): WorkspaceEntry[] | null {
  const raw = window.localStorage.getItem(WORKSPACES_KEY)
  if (raw === null) return null
  try {
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return null
    const list: WorkspaceEntry[] = []
    for (const item of parsed) {
      if (
        typeof item?.path === 'string' && item.path !== ''
        && typeof item?.homeId === 'string'
        && typeof item?.name === 'string' && item.name !== ''
      ) {
        list.push({ path: item.path, homeId: item.homeId, name: item.name })
      }
    }
    if (parsed.length > 0 && list.length === 0) return null
    return list
  } catch {
    return null
  }
}

/** 落盘（原子整体写）。localStorage 不可用时抛错，由调用方降级。 */
export function saveStoredWorkspaces(list: WorkspaceEntry[]): void {
  window.localStorage.setItem(WORKSPACES_KEY, JSON.stringify(list))
}

/** 添加弹窗的候选项（F-TERM-5）。来源：daemon 已打开的 home + known-homes
 *  条目；已在图标栏里的（path 或 homeId 命中）不列——已在列表中的不应再
 * 显示为「已知未开」，由 DSH/CLI 打开的 home 也不应凭空消失。 */
export interface AddCandidate {
  /** home 目录路径（<root>/.omt）——声明用它 */
  path: string
  /** daemon 已开 → 当前 homeId；未开 → known-homes 的 lastHomeId（可空） */
  homeId: string | null
  /** 显示名（R7） */
  name: string
  kind: string
  /** known-homes 探测到路径已失效（daemon 已开条目恒 false） */
  missing: boolean
}

export function addCandidates(
  workspaces: ReadonlyArray<{ path: string; homeId: string }>,
  knownHomes: ReadonlyArray<{ path: string; open: boolean; missing: boolean; kind: string; lastHomeId?: string }>,
  openHomes: ReadonlyArray<{ homeId: string; path?: string; kind?: string }>,
  homeDir?: string,
): AddCandidate[] {
  const out: AddCandidate[] = []
  const seen = new Set<string>()
  // F-TERM-9：同一 home 只列一次——known-homes 报规范路径、daemon 报首个
  // 声明者的原始路径（/tmp 与 /private/tmp 指向同一目录），只按 path 去重
  // 会同 home 出现两行；收录候选时同步登记 homeId，known 条目按
  // lastHomeId 再拦一道
  const seenHomeIds = new Set<string>()
  const inRail = (path: string, homeId: string | null | undefined) =>
    workspaces.some(
      w => w.path === path || (homeId != null && homeId !== '' && w.homeId === homeId),
    )
  for (const home of openHomes) {
    const path = home.path ?? ''
    if (path === '' || seen.has(path) || inRail(path, home.homeId)) continue
    seen.add(path)
    seenHomeIds.add(home.homeId)
    out.push({
      path,
      homeId: home.homeId,
      name: workspaceNameOf(path, homeDir),
      kind: home.kind ?? 'workspace',
      missing: false,
    })
  }
  for (const known of knownHomes) {
    if (seen.has(known.path) || inRail(known.path, known.lastHomeId)) continue
    if (known.lastHomeId && seenHomeIds.has(known.lastHomeId)) continue
    seen.add(known.path)
    if (known.lastHomeId) seenHomeIds.add(known.lastHomeId)
    out.push({
      path: known.path,
      homeId: known.lastHomeId ?? null,
      name: workspaceNameOf(known.path, homeDir),
      kind: known.kind,
      missing: known.missing,
    })
  }
  return out
}
