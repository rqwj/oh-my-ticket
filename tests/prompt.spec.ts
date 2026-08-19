/**
 * Always-on OMT prompt composition. Pure function: no Cordis boot.
 */
import { describe, expect, it } from 'vitest'
import { OMT_SKILL_CONTENT } from '../src/host/skill.ts'
import { composeOmtPrompt, liveBoundNames, registerOmtPrompt, sanitizeExtraPrompt } from '../src/host/prompt.ts'

describe('sanitizeExtraPrompt', () => {
  it('leaves plain text unchanged', () => {
    expect(sanitizeExtraPrompt('拆票标题用中文')).toBe('拆票标题用中文')
  })

  it('strips complete mustache groups so renderPrompt cannot throw', () => {
    expect(sanitizeExtraPrompt('见 {{model}} 再拆')).toBe('见  再拆')
  })
})

describe('liveBoundNames', () => {
  it('keeps only names that are still installed', () => {
    expect(liveBoundNames(['ce-plan', 'gone', 'omt'], ['ce-plan', 'omt', 'ce-work'])).toEqual(['ce-plan', 'omt'])
  })
})

describe('composeOmtPrompt', () => {
  it('includes the built-in spec and an unbound-split clause when extra and binds are empty', () => {
    const text = composeOmtPrompt({ extraPrompt: '', boundSkillNames: [], installedNames: [] })
    expect(text).toContain('omt_create')
    expect(text).toContain('omt_list')
    expect(text).toContain('层级规则')
    expect(text).toContain('未绑定')
    expect(text).not.toContain('必须先 load `omt`')
  })

  it('appends extra text after the built-in spec, not inside it', () => {
    const extra = '拆票标题用中文'
    const text = composeOmtPrompt({ extraPrompt: extra, boundSkillNames: [], installedNames: [] })
    const specIndex = text.indexOf('层级规则')
    const extraIndex = text.indexOf(extra)
    expect(specIndex).toBeGreaterThan(-1)
    expect(extraIndex).toBeGreaterThan(specIndex)
  })

  it('lists still-installed bound skills with OMT-phase triggers', () => {
    const text = composeOmtPrompt({
      extraPrompt: '',
      boundSkillNames: ['ce-plan', 'missing'],
      installedNames: ['ce-plan', 'omt'],
    })
    expect(text).toContain('ce-plan')
    expect(text).not.toContain('missing')
    expect(text).toContain('拆任务')
    expect(text).toContain('TICKET-')
    expect(text).toContain('skill')
  })

  it('does not instruct loading omt unless the user bound that name', () => {
    const unbound = composeOmtPrompt({ extraPrompt: '', boundSkillNames: [], installedNames: ['omt'] })
    expect(unbound).not.toMatch(/先 load [`']omt[`']/) 
    const bound = composeOmtPrompt({ extraPrompt: '', boundSkillNames: ['omt'], installedNames: ['omt'] })
    expect(bound).toContain('`omt`')
  })

  it('qualifies methodology loads so bound names wait for the OMT phase', () => {
    const text = composeOmtPrompt({ extraPrompt: '', boundSkillNames: ['ce-plan'], installedNames: ['ce-plan'] })
    expect(text).toContain('OMT 阶段')
    expect(text).not.toBe(OMT_SKILL_CONTENT)
  })
})

it('registers plugin:omt at order 150 from current inputs', () => {
  const sections: { name: string; order: number; text: string }[] = []
  registerOmtPrompt({
    systemPrompt: {
      section(section) {
        const text = typeof section.text === 'function' ? section.text() : section.text
        sections.push({ name: section.name, order: section.order, text })
        return () => {}
      },
    },
  }, () => ({ extraPrompt: '拆票标题用中文', boundSkillNames: ['ce-plan'], installedNames: ['ce-plan'] }))
  expect(sections).toHaveLength(1)
  expect(sections[0]!.name).toBe('plugin:omt')
  expect(sections[0]!.order).toBe(150)
  expect(sections[0]!.text).toContain('拆票标题用中文')
  expect(sections[0]!.text).toContain('ce-plan')
})
