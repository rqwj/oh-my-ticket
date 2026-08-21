/**
 * Skill registration tests: the embedded omt / omt-runs skills register
 * with the expected names, routing descriptions, and bodies through
 * ctx.skills.register. Split rationale (STORY-0014): the omt skill stays
 * lean; run/batch-execution discipline loads progressively via the catalog
 * routing words on the omt-runs description.
 */
import { describe, expect, it } from 'vitest'
import {
  OMT_RUNS_SKILL_CONTENT,
  OMT_RUNS_SKILL_DESCRIPTION,
  OMT_RUNS_SKILL_NAME,
  OMT_SKILL_CONTENT,
  OMT_SKILL_DESCRIPTION,
  OMT_SKILL_NAME,
  registerOmtRunsSkill,
  registerOmtSkill,
} from '../src/host/skill.ts'

function collect(register: (ctx: never) => void) {
  const registered: { name: string; description: string; content: string }[] = []
  const stubCtx = {
    skills: {
      register(skill: { name: string; description: string; content: string }) {
        registered.push(skill)
      },
    },
  }
  register(stubCtx as never)
  return registered
}

it('registers the omt skill with routing description and full body', () => {
  const registered = collect(registerOmtSkill)

  expect(registered).toHaveLength(1)
  const skill = registered[0]!
  expect(skill.name).toBe(OMT_SKILL_NAME)
  expect(skill.name).toBe('omt')
  expect(skill.name).toMatch(/^[a-z0-9]+(?:-[a-z0-9]+)*$/) // kebab-case contract
  expect(skill.description).toBe(OMT_SKILL_DESCRIPTION)
  // Catalog routing cues: when-to-use triggers and the boundary statement.
  expect(skill.description).toContain('ticket')
  expect(skill.description).toContain('不规定开发流程')
  // Body covers the boundary, hierarchy, all six tools, and conventions.
  expect(skill.content).toBe(OMT_SKILL_CONTENT)
  for (const tool of ['omt_create', 'omt_list', 'omt_show', 'omt_update', 'omt_move', 'omt_reindex']) {
    expect(skill.content).toContain(tool)
  }
  expect(skill.content).toContain('职责边界')
  expect(skill.content).toContain('层级规则')
})

it('registers the omt-runs skill alongside omt (catalog-visible pair)', () => {
  const registered = [...collect(registerOmtSkill), ...collect(registerOmtRunsSkill)]
  const names = registered.map(skill => skill.name)
  expect(names).toEqual(['omt', 'omt-runs'])
  const runs = registered[1]!
  expect(runs.name).toBe(OMT_RUNS_SKILL_NAME)
  expect(runs.name).toMatch(/^[a-z0-9]+(?:-[a-z0-9]+)*$/)
  expect(runs.description).toBe(OMT_RUNS_SKILL_DESCRIPTION)
  expect(runs.content).toBe(OMT_RUNS_SKILL_CONTENT)
})

describe('omt skill: node-status enum extension and runs routing', () => {
  it('teaches the extended status enum (blocked/skipped)', () => {
    for (const status of ['open', 'in_progress', 'done', 'blocked', 'skipped']) {
      expect(OMT_SKILL_CONTENT).toContain(status)
    }
  })

  it('routes batch-execution / run scenarios to the omt-runs skill', () => {
    expect(OMT_SKILL_CONTENT).toContain('omt-runs')
    expect(OMT_SKILL_CONTENT).toMatch(/批量执行/)
  })

  it('defines distinct Epic/Story/Ticket content contracts and avoids copied context', () => {
    expect(OMT_SKILL_CONTENT).toMatch(/Epic.*总体目标.*范围.*非目标.*全局约束.*成功标准/s)
    expect(OMT_SKILL_CONTENT).toMatch(/Story.*独立验收.*产品或系统能力/s)
    expect(OMT_SKILL_CONTENT).toMatch(/Ticket.*一次认领.*单一结果/s)
    expect(OMT_SKILL_CONTENT).toMatch(/不要.*复制.*父级背景/s)
  })
})

describe('omt-runs skill: routing description', () => {
  it('carries the routing words (run / 批量 / 续跑 / 跳过 / blocked)', () => {
    expect(OMT_RUNS_SKILL_DESCRIPTION).toContain('run')
    expect(OMT_RUNS_SKILL_DESCRIPTION).toContain('批量')
    expect(OMT_RUNS_SKILL_DESCRIPTION).toContain('续跑')
    expect(OMT_RUNS_SKILL_DESCRIPTION).toContain('跳过')
    expect(OMT_RUNS_SKILL_DESCRIPTION).toContain('blocked')
  })

  it('keeps the same boundary style as omt (batch discipline only)', () => {
    expect(OMT_RUNS_SKILL_DESCRIPTION).toContain('不规定开发流程')
  })
})

describe('omt-runs skill: content conventions', () => {
  it('covers the concept model: snapshot, item state machine, run terminal states', () => {
    expect(OMT_RUNS_SKILL_CONTENT).toContain('快照')
    for (const state of ['pending', 'running', 'done', 'failed', 'blocked', 'skipped', 'interrupted', 'awaiting_confirmation']) {
      expect(OMT_RUNS_SKILL_CONTENT).toContain(state)
    }
    for (const status of ['completed', 'completed_with_failures', 'canceled', 'interrupted']) {
      expect(OMT_RUNS_SKILL_CONTENT).toContain(status)
    }
  })

  it('documents all six run tools', () => {
    for (const tool of ['omt_run_create', 'omt_run_list', 'omt_run_show', 'omt_run_control', 'omt_run_claim', 'omt_run_report']) {
      expect(OMT_RUNS_SKILL_CONTENT).toContain(tool)
    }
  })

  it('teaches the report vocabulary (done/failed/blocked/skipped) and claim loop', () => {
    expect(OMT_RUNS_SKILL_CONTENT).toMatch(/done.*failed.*blocked.*skipped/s)
    expect(OMT_RUNS_SKILL_CONTENT).toContain('omt_run_claim')
    expect(OMT_RUNS_SKILL_CONTENT).toContain('omt_run_report')
  })

  it('limits executable members to Ticket/SubTicket and keeps hierarchy containers as context', () => {
    expect(OMT_RUNS_SKILL_CONTENT).toContain('Ticket/SubTicket')
    expect(OMT_RUNS_SKILL_CONTENT).toContain('Epic/Story/SubStory')
    expect(OMT_RUNS_SKILL_CONTENT).toContain('绝不作为可认领 item')
  })

  it('teaches claim-time live ancestor context and read-only execution boundaries', () => {
    expect(OMT_RUNS_SKILL_CONTENT).toMatch(/claim.*最新.*Epic.*Story.*SubStory/s)
    expect(OMT_RUNS_SKILL_CONTENT).toContain('只读背景')
    expect(OMT_RUNS_SKILL_CONTENT).toContain('当前 Ticket/SubTicket')
    expect(OMT_RUNS_SKILL_CONTENT).toContain('完整用户正文')
    expect(OMT_RUNS_SKILL_CONTENT).toMatch(/祖先读取失败.*保留/s)
    expect(OMT_RUNS_SKILL_CONTENT).toMatch(/截断.*最近/s)
  })

  it('teaches the trust policy (awaiting_confirmation) and nudge behavior', () => {
    expect(OMT_RUNS_SKILL_CONTENT).toContain('awaiting_confirmation')
    expect(OMT_RUNS_SKILL_CONTENT).toMatch(/nudge|续跑提醒|idle/)
  })
})

describe('skill content conventions', () => {
  it('instructs the model to defer workflow methodology to other skills', () => {
    expect(OMT_SKILL_CONTENT).toContain('skill 工具')
    expect(OMT_SKILL_CONTENT).toContain('开发流程')
    expect(OMT_RUNS_SKILL_CONTENT).toContain('开发流程')
  })
})
