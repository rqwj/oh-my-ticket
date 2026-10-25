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
 * 图标调色板：12 个人眼可分的底色（白字对比度足够）。不在 360° 色相上连续
 * 取值——相差 20° 以内的两个色相肉眼几乎分不出，同缩写的工作区就失去了
 * 用颜色区分的意义（KTD14）。
 */
export const WORKSPACE_PALETTE: readonly string[] = [
  'hsl(0, 55%, 46%)', // 红
  'hsl(24, 70%, 42%)', // 橙
  'hsl(45, 65%, 36%)', // 芥黄
  'hsl(90, 40%, 36%)', // 橄榄绿
  'hsl(150, 50%, 33%)', // 绿
  'hsl(182, 55%, 32%)', // 青
  'hsl(205, 60%, 44%)', // 蓝
  'hsl(232, 45%, 54%)', // 靛
  'hsl(268, 42%, 52%)', // 紫
  'hsl(300, 38%, 44%)', // 品红
  'hsl(335, 52%, 47%)', // 粉
  'hsl(210, 14%, 44%)', // 石板灰
]

/**
 * 路径 → 调色板下标：FNV-1a 再做一次 fmix32 末端混合，只差末尾一两个字符
 * 的路径（ws/ws2/ws3）也能散开。稳定、与展示顺序无关；工作区多于调色板时
 * 仍可能撞色。
 */
export function colorIndexOf(path: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < path.length; i++) {
    h ^= path.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  h ^= h >>> 16
  h = Math.imul(h, 0x85ebca6b)
  h ^= h >>> 13
  h = Math.imul(h, 0xc2b2ae35)
  h ^= h >>> 16
  return (h >>> 0) % WORKSPACE_PALETTE.length
}

/** 图标底色（hsl 逗号参数形式，jsdom 可解析）。 */
export function workspaceColor(path: string): string {
  return WORKSPACE_PALETTE[colorIndexOf(path)]
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
 * 从列表移除（KTD14 选中规则）：条目按 path **或** homeId 命中（homeId 为
 * 空串的未开条目靠 path 找到）。移除项不是当前 → 选中不变；是当前 →
 * 上一个，没有上一个就下一个，都没有 → null（调用方显示带「添加工作区」
 * 的空状态）。返回**条目本身**（不是 homeId）——空串 homeId 的条目无法
 * 按 id 唯一找回。
 */
export function removeWorkspace(
  list: WorkspaceEntry[],
  ws: { homeId: string; path: string },
  current: { homeId: string | null; path?: string | null } | null,
): { list: WorkspaceEntry[]; removedCurrent: boolean; next: WorkspaceEntry | null } {
  const idx = list.findIndex(
    w => (ws.homeId !== '' && w.homeId === ws.homeId) || w.path === ws.path,
  )
  if (idx === -1) return { list, removedCurrent: false, next: null }
  const next = list.filter((_, i) => i !== idx)
  // 当前选中按 homeId **或** path 命中（homeId 轮换回写未必及时）
  const currentHomeId = current?.homeId ?? null
  const currentPath = current?.path ?? null
  const removedCurrent =
    list[idx].homeId === currentHomeId
    || (currentPath != null && currentPath !== '' && list[idx].path === currentPath)
  return {
    list: next,
    removedCurrent,
    next: removedCurrent ? next[idx - 1] ?? next[idx] ?? null : null,
  }
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

/**
 * F-TERM-12：图标栏条目是否已失效（KTD14 置灰带警告、点击不声明）。
 * 路径精确匹配优先：known-homes 有同路径条目时只看这些条目。没有同路径
 * 条目（条目记原始写法、known-homes 报规范路径：/tmp vs /private/tmp）才按
 * homeId（lastHomeId）回退，且该 homeId 的条目**全部**失效才算失效。
 * homeId 存在 home 自己的库里，项目目录被移动 / 改名后不变，而 known-homes
 * 的旧路径条目永久 missing——只要一条 missing 就判失效，会把移动后的项目
 * 误判为失效、从不声明有效路径。
 */
export function isWorkspaceMissing(
  ws: { path: string; homeId: string },
  knownHomes: ReadonlyArray<{ path: string; missing: boolean; lastHomeId?: string }>,
): boolean {
  const byPath = knownHomes.filter(k => k.path === ws.path)
  if (byPath.length > 0) return byPath.every(k => k.missing)
  if (ws.homeId === '') return false
  const byHomeId = knownHomes.filter(k => k.lastHomeId === ws.homeId)
  return byHomeId.length > 0 && byHomeId.every(k => k.missing)
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
