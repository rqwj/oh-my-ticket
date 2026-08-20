/**
 * Always-on OMT prompt: built-in spec + extra rules + bound-skill load clause.
 * Composition is a pure function so tests do not boot Cordis.
 */
import { OMT_SKILL_CONTENT } from './skill.ts'

export interface OmtPromptInputs {
  extraPrompt: string
  boundSkillNames: readonly string[]
  installedNames: readonly string[]
}

const MUSTACHE_GROUP = /\{\{[^}]*\}\}/g

const PHASE_CLAUSE = [
  '## OMT 阶段与绑定 skill',
  '',
  '本段始终在场，不必再 load `omt` skill。',
  '**OMT 阶段**在下列情况进入，不必等用户说 ticket / 拆任务：',
  '- 实质性开发：新功能、重新对接、改造、非琐碎实现、跨越多文件的改动',
  '- 用户明确提到 ticket、拆任务，或 `TICKET-` / `EPIC-` / `STORY-` / `SUBSTORY-` / `SUBTICKET-` 节点 id',
  '进入后、动手写代码或只在回复里拆步骤之前，先完成拆票再 omt_create Epic/Story 骨架（已有匹配节点则 omt_show 接手）。',
  '绑定的拆票 skill 只在 OMT 阶段使用。其它目录 skill 仍可按任务自行发现，但不得跳过建单。',
  '拆解结果必须落成真实 OMT 节点，不要只写在回复里。',
  '琐碎单步（改一个字、查一个状态、回答一个事实）不必建单。',
].join('\n')

/** Strip complete `{{…}}` groups so user extra text cannot fail renderPrompt. */
export function sanitizeExtraPrompt(extra: string): string {
  return extra.replace(MUSTACHE_GROUP, '')
}

/** Bound names that still exist in the installed catalog, in bind order. */
export function liveBoundNames(bound: readonly string[], installed: readonly string[]): string[] {
  const live = new Set(installed)
  return bound.filter(name => live.has(name))
}

export function composeOmtPrompt(inputs: OmtPromptInputs): string {
  const extra = sanitizeExtraPrompt(inputs.extraPrompt).trim()
  const live = liveBoundNames(inputs.boundSkillNames, inputs.installedNames)
  const parts = [OMT_SKILL_CONTENT.trimEnd(), '', PHASE_CLAUSE]
  if (extra !== '') {
    parts.push('', '## 追加约定', '', extra)
  }
  if (live.length === 0) {
    parts.push('', '当前未绑定拆票 skill。未绑定也按本规范拆成真实 OMT 节点。')
  } else {
    const names = live.map(name => '`' + name + '`').join('、')
    parts.push(
      '',
      'OMT 阶段必须先用 skill 工具 load 并**遵循**绑定名单：' + names + '。',
      '禁止用 `brainstorming` / `writing-plans` 代替绑定的拆票 skill。',
      '未跑完绑定 skill 的流程（定范围、写出计划）不得 `omt_create`。run_code 里同样要 `tools.skill({ name })`，不能只读 SKILL.md 或 console.log。',
    )
  }
  return parts.join('\n')
}

interface SystemPromptLike {
  section(section: { name: string; order: number; text: string | (() => string) }): () => void
}

/** Register the always-on `plugin:omt` section. */
export function registerOmtPrompt(ctx: { systemPrompt: SystemPromptLike }, getInputs: () => OmtPromptInputs): void {
  ctx.systemPrompt.section({
    name: 'plugin:omt',
    order: 150,
    text: () => composeOmtPrompt(getInputs()),
  })
}
