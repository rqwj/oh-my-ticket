/**
 * U1 工作区列表纯函数测试（KTD14）：显示名/缩写/颜色/上级目录、首次迁
 * 移（daemon 已开 + known-homes open 条目）、收录去重、移除后的选中规则
 * 与 localStorage 存取。
 */
import { describe, expect, it, beforeEach } from 'vitest'
import {
  WORKSPACES_KEY,
  addCandidates,
  abbreviationOf,
  appendWorkspace,
  hueOf,
  migrateWorkspaces,
  parentDirOf,
  readStoredWorkspaces,
  removeWorkspace,
  saveStoredWorkspaces,
  workspaceColor,
  workspaceNameOf,
} from '../src/workspaceList'
import type { HomeInfo } from '../src/types'

beforeEach(() => window.localStorage.clear())

const homeOf = (homeId: string, path: string): HomeInfo => ({ homeId, kind: 'workspace', path })

describe('显示名 / 缩写 / 颜色 / 上级目录', () => {
  it('workspaceNameOf：取工作区根目录 basename；全局 home 压缩为 ~', () => {
    expect(workspaceNameOf('/tmp/a/app/.omt')).toBe('app')
    expect(workspaceNameOf('/Users/x/.omt', '/Users/x')).toBe('~')
    expect(workspaceNameOf('/Users/x/dev/omt 仓库/.omt', '/Users/x')).toBe('omt 仓库')
  })

  it('abbreviationOf：显示名前两个字符（按字符切片，不切开 emoji）', () => {
    expect(abbreviationOf('app')).toBe('ap')
    expect(abbreviationOf('桌面终端')).toBe('桌面')
    expect(abbreviationOf('a')).toBe('a')
    // P3（§64）：emoji 是代理对，按 UTF-16 码元切片会只剩半个字符
    expect(abbreviationOf('🚀🚀火箭')).toBe('🚀🚀')
    expect(abbreviationOf('🚀项目')).toBe('🚀项')
  })

  it('hueOf/workspaceColor：同路径稳定、不同路径（选取样例）不同色相', () => {
    expect(hueOf('/tmp/one/app/.omt')).toBe(hueOf('/tmp/one/app/.omt'))
    expect(hueOf('/tmp/one/app/.omt')).not.toBe(hueOf('/tmp/two/app/.omt'))
    expect(workspaceColor('/tmp/one/app/.omt')).not.toBe(workspaceColor('/tmp/two/app/.omt'))
  })

  it('parentDirOf：根路径去最后一段；homeDir 前缀压缩后再取；没有上级（全局 home 的 ~）→ null', () => {
    expect(parentDirOf('/tmp/one/app/.omt')).toBe('/tmp/one')
    expect(parentDirOf('/Users/x/dev/app/.omt', '/Users/x')).toBe('~/dev')
    // P3（§64）：全局 home 压缩成 ~ 后没有上级目录，返回 null（悬停提示
    // 不再拼出「~（~）」）
    expect(parentDirOf('/Users/x/.omt', '/Users/x')).toBeNull()
  })
})

describe('首次迁移（KTD14）', () => {
  const known = (over: Partial<{ path: string; open: boolean; missing: boolean; lastHomeId: string }> = {}) => ({
    path: '/tmp/ws3/.omt',
    name: '.omt',
    kind: 'workspace',
    open: true,
    missing: false,
    lastHomeId: 'h_c',
    ...over,
  })

  it('daemon 已开 home 在前，known-homes 的 open 条目随后，按 path 去重', () => {
    const list = migrateWorkspaces(
      [homeOf('h_a', '/tmp/ws1/.omt'), homeOf('h_b', '/tmp/ws2/.omt')],
      [known({ path: '/tmp/ws1/.omt' }), known()],
      '/tmp',
    )
    expect(list.map(w => w.path)).toEqual(['/tmp/ws1/.omt', '/tmp/ws2/.omt', '/tmp/ws3/.omt'])
    expect(list.map(w => w.homeId)).toEqual(['h_a', 'h_b', 'h_c'])
    expect(list.map(w => w.name)).toEqual(['ws1', 'ws2', 'ws3'])
  })

  it('known-homes 的未开条目不迁移；无 lastHomeId 的 open 条目无法引用，跳过', () => {
    const list = migrateWorkspaces(
      [homeOf('h_a', '/tmp/ws1/.omt')],
      [known({ open: false }), known({ path: '/tmp/ws4/.omt', lastHomeId: undefined as unknown as string })],
    )
    expect(list.map(w => w.path)).toEqual(['/tmp/ws1/.omt'])
  })
})

describe('收录与移除', () => {
  const ws1 = { path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' }
  const ws2 = { path: '/tmp/ws2/.omt', homeId: 'h_b', name: 'ws2' }
  const ws3 = { path: '/tmp/ws3/.omt', homeId: 'h_c', name: 'ws3' }

  it('appendWorkspace：按 path 与 homeId 去重，新条目排末尾', () => {
    const list = appendWorkspace([ws1, ws2], ws3)
    expect(list).toEqual([ws1, ws2, ws3])
    expect(appendWorkspace(list, { ...ws3, name: 'other' })).toBe(list) // path 命中 → 原样
    expect(appendWorkspace(list, { path: '/tmp/other/.omt', homeId: 'h_c', name: 'o' })).toBe(list) // homeId 命中
  })

  it('removeWorkspace：移除当前 → 选上一个；没有上一个选下一个；都没有 → null（空态）', () => {
    const list = [ws1, ws2, ws3]
    expect(removeWorkspace(list, 'h_b', 'h_b')).toEqual({ list: [ws1, ws3], selectHomeId: 'h_a' })
    expect(removeWorkspace(list, 'h_a', 'h_a')).toEqual({ list: [ws2, ws3], selectHomeId: 'h_b' })
    expect(removeWorkspace([ws1], 'h_a', 'h_a')).toEqual({ list: [], selectHomeId: null })
  })

  it('removeWorkspace：移除的不是当前 → 选中不变；未知 homeId → 原样', () => {
    const list = [ws1, ws2]
    expect(removeWorkspace(list, 'h_a', 'h_b')).toEqual({ list: [ws2], selectHomeId: 'h_b' })
    expect(removeWorkspace(list, 'h_x', 'h_a')).toEqual({ list, selectHomeId: 'h_a' })
  })
})

describe('添加弹窗候选（F-TERM-5）', () => {
  const known = (over: Partial<{ path: string; open: boolean; missing: boolean; lastHomeId: string }> = {}) => ({
    path: '/tmp/ws2/.omt',
    open: false,
    missing: false,
    kind: 'workspace',
    lastHomeId: 'h_b',
    ...over,
  })

  it('候选 = known-homes 条目 + daemon 已开的 home；已在图标栏里的（path 或 homeId）不列', () => {
    const candidates = addCandidates(
      // 图标栏已有 ws1（h_a）与 ws3（h_c）
      [{ path: '/tmp/ws1/.omt', homeId: 'h_a' }, { path: '/tmp/ws3/.omt', homeId: 'h_c' }],
      [
        known(), // ws2 未开、不在列表 → 候选
        known({ path: '/tmp/ws3/.omt' }), // 已在列表（path 命中）→ 不列
        known({ path: '/tmp/ws5/.omt', lastHomeId: 'h_a' }), // homeId 命中列表项 → 不列
      ],
      [
        { homeId: 'h_a', path: '/tmp/ws1/.omt' }, // 在列表 → 不列
        { homeId: 'h_d', path: '/tmp/ws4/.omt' }, // daemon 已开但不在列表 → 候选
      ],
    )
    expect(candidates.map(c => c.path)).toEqual(['/tmp/ws4/.omt', '/tmp/ws2/.omt'])
    expect(candidates[0]).toMatchObject({ homeId: 'h_d', missing: false })
    expect(candidates[1]).toMatchObject({ homeId: 'h_b', missing: false })
  })

  it('同一 path 在 daemon 与 known-homes 都出现 → 只列一次；missing 透传；无 lastHomeId 的 known 条目仍可添加', () => {
    const candidates = addCandidates(
      [],
      [known({ missing: true }), known({ path: '/tmp/ws6/.omt', lastHomeId: undefined as unknown as string })],
      [{ homeId: 'h_x', path: '/tmp/ws2/.omt' }],
    )
    expect(candidates.map(c => c.path)).toEqual(['/tmp/ws2/.omt', '/tmp/ws6/.omt'])
    expect(candidates[0]).toMatchObject({ homeId: 'h_x', missing: false })
    expect(candidates[1]).toMatchObject({ homeId: null })
  })

  it('F-TERM-9：同一 home 按去重——daemon 报原始路径（/tmp）、known-homes 报规范路径（/private/tmp）时只列一行', () => {
    const candidates = addCandidates(
      [],
      [known({ path: '/private/tmp/ws2/.omt', lastHomeId: 'h_b' })],
      [{ homeId: 'h_b', path: '/tmp/ws2/.omt' }],
    )
    // 同一 home（h_b）在 daemon 与 known-homes 各报一种路径写法——只按
    // path 去重会出现两行；收录 open home 的 homeId 后 known 条目被拦下
    expect(candidates.map(c => c.path)).toEqual(['/tmp/ws2/.omt'])
    expect(candidates[0]).toMatchObject({ homeId: 'h_b' })
  })

  it('F-TERM-9：known 条目之间也按 lastHomeId 去重（不同路径指向同一历史 home）', () => {
    const candidates = addCandidates(
      [],
      [
        known({ path: '/tmp/ws2/.omt', lastHomeId: 'h_b' }),
        known({ path: '/private/tmp/ws2/.omt', lastHomeId: 'h_b' }),
      ],
      [],
    )
    expect(candidates.map(c => c.path)).toEqual(['/tmp/ws2/.omt'])
  })
})

describe('localStorage 存取', () => {
  it('save → read 往返；无存储 → null', () => {
    expect(readStoredWorkspaces()).toBeNull()
    const list = [{ path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' }]
    saveStoredWorkspaces(list)
    expect(readStoredWorkspaces()).toEqual(list)
  })

  it('非 JSON / 非数组 → null（调用方重新迁移）', () => {
    window.localStorage.setItem(WORKSPACES_KEY, '{oops')
    expect(readStoredWorkspaces()).toBeNull()
    window.localStorage.setItem(WORKSPACES_KEY, '42')
    expect(readStoredWorkspaces()).toBeNull()
  })

  // P3（§64）：原标题「形状不对 → null」与行为不符——形状不对的条目被剔除、
  // 其余保留，只有非 JSON / 非数组才返回 null
  it('数组里形状不对的条目被剔除，其余条目保留', () => {
    window.localStorage.setItem(WORKSPACES_KEY, JSON.stringify([{ path: '/x/.omt' }, { path: '/y/.omt', homeId: 'h_y', name: 'y' }]))
    expect(readStoredWorkspaces()).toEqual([{ path: '/y/.omt', homeId: 'h_y', name: 'y' }])
  })

  // §64 遗留：原始数组非空但全部条目无效 → 按首次运行处理（null，重新
  // 迁移）——这种形态不是「用户清空了列表」（用户清空存的是空数组 []）
  it('全部条目无效 → null（按首次运行处理）；空数组仍是有效的「用户清空」', () => {
    window.localStorage.setItem(WORKSPACES_KEY, JSON.stringify([{ path: 42 }, 'oops', null]))
    expect(readStoredWorkspaces(), '全无效 ≠ 用户清空——不得据为存量跳过迁移').toBeNull()
    window.localStorage.setItem(WORKSPACES_KEY, '[]')
    expect(readStoredWorkspaces(), '空数组是用户清空过的有效状态').toEqual([])
  })
})
