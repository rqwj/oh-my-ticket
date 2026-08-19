/**
 * Tree filter tests (TICKET-0004): keyword matching with ancestor
 * preservation and archived visibility.
 */
import { describe, expect, it } from 'vitest'
import { filterForest, sortForest } from '../src/client/tree-filter.ts'
import type { OmtTreeNode } from '../src/client/store.ts'

function node(id: string, title: string, status: OmtTreeNode['status'], children: OmtTreeNode[] = [], archived = false): OmtTreeNode {
  const type = id.split('-')[0]?.toLowerCase() ?? 'ticket'
  return { id, type: type as OmtTreeNode['type'], title, status, archived, priority: 0, path: 'p', created_at: '', updated_at: '', children }
}

const FOREST: OmtTreeNode[] = [
  node('EPIC-0001', '用户体系', 'open', [
    node('STORY-0001', '登录', 'in_progress', [
      node('TICKET-0001', '登录接口', 'done'),
      node('TICKET-0002', '旧的登录页', 'open', [], true),
    ]),
    node('STORY-0002', '注册', 'open'),
  ]),
  node('EPIC-0002', '通知系统', 'open'),
]

it('passes everything through with an empty query', () => {
  const result = filterForest(FOREST, { query: '', showArchived: true })
  expect(result).toHaveLength(2)
  expect(result[0]?.children[0]?.children).toHaveLength(2)
})

it('hides archived nodes by default, reveals them when checked', () => {
  const hidden = filterForest(FOREST, { query: '', showArchived: false })
  expect(hidden[0]?.children[0]?.children.map(n => n.id)).toEqual(['TICKET-0001'])

  const shown = filterForest(FOREST, { query: '', showArchived: true })
  expect(shown[0]?.children[0]?.children.map(n => n.id)).toEqual(['TICKET-0001', 'TICKET-0002'])
})

it('keyword match keeps the ancestor chain', () => {
  const result = filterForest(FOREST, { query: '登录接口', showArchived: false })
  expect(result.map(n => n.id)).toEqual(['EPIC-0001'])
  expect(result[0]?.children.map(n => n.id)).toEqual(['STORY-0001'])
  expect(result[0]?.children[0]?.children.map(n => n.id)).toEqual(['TICKET-0001'])
})

it('matches by id case-insensitively and reports no-match', () => {
  const byId = filterForest(FOREST, { query: 'story-0002', showArchived: false })
  expect(byId[0]?.children.map(n => n.id)).toEqual(['STORY-0002'])

  expect(filterForest(FOREST, { query: '不存在', showArchived: false })).toHaveLength(0)
})

it('filters by type with ancestor preservation', () => {
  const result = filterForest(FOREST, { query: '', showArchived: false, types: ['ticket'] })
  // Only tickets match; epic/story survive as the ancestor chain.
  expect(result.map(n => n.id)).toEqual(['EPIC-0001'])
  expect(result[0]?.children.map(n => n.id)).toEqual(['STORY-0001'])
  expect(result[0]?.children[0]?.children.map(n => n.id)).toEqual(['TICKET-0001'])
})

it('filters by status, stacking with keyword', () => {
  const byStatus = filterForest(FOREST, { query: '', showArchived: false, statuses: ['in_progress'] })
  expect(byStatus[0]?.children.map(n => n.id)).toEqual(['STORY-0001'])

  const stacked = filterForest(FOREST, { query: '登录', showArchived: false, statuses: ['done'] })
  expect(stacked[0]?.children[0]?.children.map(n => n.id)).toEqual(['TICKET-0001'])
})

it('filters by priority whitelist', () => {
  const forest = [
    node('EPIC-0001', '普通', 'open', [], false),
    { ...node('EPIC-0002', '重要', 'open', [], false), priority: 2 },
  ]
  const result = filterForest(forest, { query: '', showArchived: false, priorities: [2] })
  expect(result.map(n => n.id)).toEqual(['EPIC-0002'])
})

it('sortForest orders siblings by priority, desc/asc/none, recursively', () => {
  const forest = [
    { ...node('EPIC-0001', 'A', 'open', [
      { ...node('TICKET-0001', 'a1', 'open'), priority: 3 },
      { ...node('TICKET-0002', 'a2', 'open'), priority: 1 },
    ]), priority: 0 },
    { ...node('EPIC-0002', 'B', 'open'), priority: 2 },
  ]
  expect(sortForest(forest, 'priority-desc').map(n => n.id)).toEqual(['EPIC-0002', 'EPIC-0001'])
  expect(sortForest(forest, 'priority-asc')[0]?.children.map(n => n.id)).toEqual(['TICKET-0002', 'TICKET-0001'])
  expect(sortForest(forest, 'none').map(n => n.id)).toEqual(['EPIC-0001', 'EPIC-0002'])
})

it('an archived ancestor hides its subtree by default', () => {
  const forest = [node('EPIC-0009', '旧项目', 'done', [node('TICKET-0009', '遗留任务', 'open')], true)]
  expect(filterForest(forest, { query: '', showArchived: false })).toHaveLength(0)
  expect(filterForest(forest, { query: '', showArchived: true })).toHaveLength(1)
})
