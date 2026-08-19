/**
 * Skill registration tests: the embedded omt skill registers with the
 * expected name/description/body through ctx.skills.register.
 */
import { describe, expect, it } from 'vitest'
import {
  OMT_SKILL_CONTENT,
  OMT_SKILL_DESCRIPTION,
  OMT_SKILL_NAME,
  registerOmtSkill,
} from '../src/host/skill.ts'

it('registers the omt skill with routing description and full body', () => {
  const registered: { name: string; description: string; content: string }[] = []
  const stubCtx = {
    skills: {
      register(skill: { name: string; description: string; content: string }) {
        registered.push(skill)
      },
    },
  }
  registerOmtSkill(stubCtx as never)

  expect(registered).toHaveLength(1)
  const skill = registered[0]!
  expect(skill.name).toBe(OMT_SKILL_NAME)
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

describe('skill content conventions', () => {
  it('instructs the model to defer workflow methodology to other skills', () => {
    expect(OMT_SKILL_CONTENT).toContain('skill 工具')
    expect(OMT_SKILL_CONTENT).toContain('开发流程')
  })
})
