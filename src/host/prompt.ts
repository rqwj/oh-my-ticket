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

const MUSTACHE_GROUP = /\{\{[\s\S]*?\}\}/g

const PHASE_CLAUSE = [
  '## OMT 阶段与绑定 skill',
  '',
  '本段始终在场，不必再 load `omt` skill。',
  '**OMT 阶段**在下列情况进入，不必等用户说 ticket / 拆任务：',
  '- 实质性开发：新功能、重新对接、改造、非琐碎实现、跨越多文件的改动',
  '- 用户明确提到 ticket、拆任务，或 `TICKET-` / `EPIC-` / `STORY-` / `SUBSTORY-` / `SUBTICKET-` 节点 id',
  '进入后、动手写代码或只在回复里拆步骤之前，先完成拆票再 omt_create Epic/Story 骨架（已有匹配节点则 omt_show 接手）。',
  '设置里勾选的 skill 必须按各自阶段使用，不得跳过、不得用未勾选的同类 skill 顶替。',
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
  const hit = bound.filter(name => live.has(name))
  // Host apply often sees only runtime plugin skills before workspace-aware
  // catalogs warm. Never silently drop a configured split/implementation gate.
  const missingPrerequisite = bound.some(name => (SPLIT_SKILLS.has(name) || IMPLEMENT_SKILLS.has(name)) && !live.has(name))
  if (missingPrerequisite || (hit.length === 0 && bound.length > 0)) return [...bound]
  return hit
}

const SPLIT_SKILLS = new Set(['ce-brainstorm', 'ce-plan', 'ce-ideate'])
const IMPLEMENT_SKILLS = new Set(['ce-work', 'ce-worktree'])
const VERIFY_SKILLS = new Set(['ce-test-browser', 'ce-code-review'])

export interface BoundSkillStages {
  split: string[]
  implementation: string[]
  verification: string[]
  other: string[]
}

/** One shared classification for prompt guidance and the runtime gate. */
export function boundSkillStages(live: readonly string[]): BoundSkillStages {
  return {
    split: live.filter(name => SPLIT_SKILLS.has(name)),
    implementation: live.filter(name => IMPLEMENT_SKILLS.has(name)),
    verification: live.filter(name => VERIFY_SKILLS.has(name)),
    other: live.filter(name => !SPLIT_SKILLS.has(name) && !IMPLEMENT_SKILLS.has(name) && !VERIFY_SKILLS.has(name)),
  }
}

function joinNames(names: readonly string[]): string {
  return names.map(name => '`' + name + '`').join('、')
}

/** Phase-specific must-follow lines for the bound skill checklist. */
export function boundSkillGuidance(live: readonly string[]): string[] {
  const { split, implementation: implement, verification: verify, other } = boundSkillStages(live)
  const lines = [
    '绑定名单（必须按阶段使用）：' + joinNames(live) + '。',
    'run_code 里同样要 `tools.skill({ name })`，不能只读 SKILL.md 或 console.log。',
  ]
  if (split.length > 0 || implement.length > 0) {
    lines.push(
      '运行时会按阶段硬性校验前置条件：run_code 调用绑定 skill 后可以继续只读操作；拆票阶段 Skill 只阻止 `omt_create` 与 `plans` 计划文档写入，实施阶段 Skill 只阻止其它 `edit/write`。执行对应变更前必须结束当前工具批次，让完整指令进入 next model step；验证与其它阶段 Skill 不触发该变更锁。',
      'Skill 加载 credit 与 `omt_bypass` 会在新的 DSH turn（通常是下一条用户消息）失效；继续工作时必须重新 load 当前阶段绑定 skill。',
      '非计划文档的 edit/write 还要求当前会话或父会话存在 model-owned `in_progress` OMT 节点；真正琐碎的单步修改先调用 `omt_bypass` 并说明原因，该放行只消费一次。',
    )
  }
  if (split.length > 0) {
    lines.push('拆票：必须先 load 并**遵循** ' + joinNames(split) + '。禁止用 `brainstorming` / `writing-plans` 代替。未跑完（定范围、写出计划）不得 `omt_create`。')
  }
  if (implement.length > 0) {
    lines.push('实施 ticket：必须先 load 并遵循 ' + joinNames(implement) + '。禁止不 load `ce-work` 就直接改代码。')
  }
  if (verify.length > 0) {
    lines.push('涉及页面/验收时 load 并遵循 ' + joinNames(verify) + '。')
  }
  if (other.length > 0) {
    lines.push('其它绑定 skill 按其 when-to-use 在对应步骤 load：' + joinNames(other) + '。')
  }
  return lines
}

export function composeOmtPrompt(inputs: OmtPromptInputs): string {
  const extra = sanitizeExtraPrompt(inputs.extraPrompt).trim()
  const live = liveBoundNames(inputs.boundSkillNames, inputs.installedNames)
  const parts = [OMT_SKILL_CONTENT.trimEnd(), '', PHASE_CLAUSE]
  if (extra !== '') {
    parts.push('', '## 追加约定', '', extra)
  }
  if (live.length === 0) {
    parts.push('', '当前未绑定拆票/实施 skill。未绑定也按本规范拆成真实 OMT 节点。')
  } else {
    parts.push('', ...boundSkillGuidance(live))
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
