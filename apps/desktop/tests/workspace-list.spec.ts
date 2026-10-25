/**
 * U1 工作区列表纯函数测试（KTD14）：显示名/缩写/颜色/上级目录、首次迁
 * 移（daemon 已开 + known-homes open 条目）、收录去重、移除后的选中规则
 * 与 localStorage 存取。
 */
import { describe, expect, it, beforeEach } from 'vitest'
import {
  WORKSPACES_KEY,
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

  it('abbreviationOf：显示名前两个字符', () => {
    expect(abbreviationOf('app')).toBe('ap')
    expect(abbreviationOf('桌面终端')).toBe('桌面')
    expect(abbreviationOf('a')).toBe('a')
  })

  it('hueOf/workspaceColor：同路径稳定、不同路径（选取样例）不同色相', () => {
    expect(hueOf('/tmp/one/app/.omt')).toBe(hueOf('/tmp/one/app/.omt'))
    expect(hueOf('/tmp/one/app/.omt')).not.toBe(hueOf('/tmp/two/app/.omt'))
    expect(workspaceColor('/tmp/one/app/.omt')).not.toBe(workspaceColor('/tmp/two/app/.omt'))
  })

  it('parentDirOf：根路径去最后一段；homeDir 前缀压缩后再取', () => {
    expect(parentDirOf('/tmp/one/app/.omt')).toBe('/tmp/one')
    expect(parentDirOf('/Users/x/dev/app/.omt', '/Users/x')).toBe('~/dev')
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

describe('localStorage 存取', () => {
  it('save → read 往返；无存储 → null', () => {
    expect(readStoredWorkspaces()).toBeNull()
    const list = [{ path: '/tmp/ws1/.omt', homeId: 'h_a', name: 'ws1' }]
    saveStoredWorkspaces(list)
    expect(readStoredWorkspaces()).toEqual(list)
  })

  it('损坏内容（非 JSON / 非数组 / 条目形状不对）→ null（调用方重新迁移）', () => {
    window.localStorage.setItem(WORKSPACES_KEY, '{oops')
    expect(readStoredWorkspaces()).toBeNull()
    window.localStorage.setItem(WORKSPACES_KEY, '42')
    expect(readStoredWorkspaces()).toBeNull()
    window.localStorage.setItem(WORKSPACES_KEY, JSON.stringify([{ path: '/x/.omt' }, { path: '/y/.omt', homeId: 'h_y', name: 'y' }]))
    expect(readStoredWorkspaces()).toEqual([{ path: '/y/.omt', homeId: 'h_y', name: 'y' }])
  })
})
