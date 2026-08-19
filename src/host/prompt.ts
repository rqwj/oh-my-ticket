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
  '绑定的拆票 skill 只在 **OMT 阶段** 加载：用户提到 ticket、拆任务，或 `TICKET-` / `EPIC-` / `STORY-` / `SUBSTORY-` / `SUBTICKET-` 节点 id。',
  '未到 OMT 阶段不要为了拆票去 load 绑定名单。其它目录 skill 仍可按任务自行发现。',
  'OMT 阶段若用户在拆任务或新建工单，把结果落成真实 OMT 节点，不要只写在回复里。',
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
    parts.push('', 'OMT 阶段先用 skill 工具 load：' + names + '。')
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
