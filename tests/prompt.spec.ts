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
    expect(sanitizeExtraPrompt('见 {{model} }} 再拆')).toBe('见  再拆')
  })

  it('keeps an unmatched opener as literal text', () => {
    expect(sanitizeExtraPrompt('见 {{model 再拆')).toBe('见 {{model 再拆')
  })
})

describe('liveBoundNames', () => {
  it('keeps only names that are still installed', () => {
    expect(liveBoundNames(['ce-plan', 'gone', 'omt'], ['ce-plan', 'omt', 'ce-work'])).toEqual(['ce-plan', 'omt'])
  })

  it('keeps configured prerequisites while the catalog is empty or only partially warm', () => {
    expect(liveBoundNames(['ce-plan', 'ce-work'], ['omt'])).toEqual(['ce-plan', 'ce-work'])
    expect(liveBoundNames(['ce-work', 'omt'], ['omt'])).toEqual(['ce-work', 'omt'])
    expect(liveBoundNames(['ce-plan', 'ce-work', 'omt'], ['ce-plan', 'omt'])).toEqual(['ce-plan', 'ce-work', 'omt'])
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

  it('lists still-installed bound skills and enters OMT on substantial work', () => {
    const text = composeOmtPrompt({
      extraPrompt: '',
      boundSkillNames: ['ce-plan', 'missing'],
      installedNames: ['ce-plan', 'omt'],
    })
    expect(text).toContain('ce-plan')
    expect(text).not.toContain('missing')
    expect(text).toContain('实质性开发')
    expect(text).toContain('重新对接')
    expect(text).toContain('拆任务')
    expect(text).toContain('TICKET-')
    expect(text).toContain('skill')
    expect(text).toContain('遵循')
    expect(text).toContain('brainstorming')
    expect(text).toContain('不得 `omt_create`')
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

  it('requires ce-work when implementing a bound ticket', () => {
    const text = composeOmtPrompt({
      extraPrompt: '',
      boundSkillNames: ['ce-brainstorm', 'ce-plan', 'ce-work', 'ce-worktree', 'ce-test-browser'],
      installedNames: ['ce-brainstorm', 'ce-plan', 'ce-work', 'ce-worktree', 'ce-test-browser'],
    })
    expect(text).toContain('实施 ticket')
    expect(text).toContain('`ce-work`')
    expect(text).toContain('`ce-worktree`')
    expect(text).toContain('`ce-test-browser`')
    expect(text).toContain('禁止不 load `ce-work` 就直接改代码')
    expect(text).toContain('omt_bypass')
    expect(text).toContain('next model step')
    expect(text).toContain('可以继续只读操作')
    expect(text).toContain('拆票阶段 Skill 只阻止 `omt_create` 与 `plans` 计划文档写入')
    expect(text).toContain('实施阶段 Skill 只阻止其它 `edit/write`')
    expect(text).not.toContain('调用任何绑定 skill 后必须结束当前 run_code/工具批次')
    expect(text).toContain('DSH turn')
    expect(text).toContain('重新 load')
  })

  it('does not claim mutation hard gates for verification-only bindings', () => {
    const text = composeOmtPrompt({
      extraPrompt: '',
      boundSkillNames: ['ce-test-browser', 'omt'],
      installedNames: ['ce-test-browser', 'omt'],
    })
    expect(text).not.toContain('next model step')
    expect(text).not.toContain('omt_bypass')
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
