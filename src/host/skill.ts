/**
 * Embedded OMT skill: registered at runtime through ctx.skills.register()
 * (one of the three skill contribution paths; no SKILL.md file needed).
 * The body teaches the model the OMT hierarchy, the omt_* tool family, and
 * — critically — the boundary against process/workflow skills: OMT manages
 * WHERE progress is recorded, never HOW to develop.
 */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-skill'

export const OMT_SKILL_NAME = 'omt'

export const OMT_SKILL_DESCRIPTION =
  'OMT ticket 体系的操作规范：Epic→Story→[SubStory]→Ticket→[SubTicket] 五层结构的创建、'
  + '查询、状态流转与进度记录。实质性开发（新功能、重新对接、改造、非琐碎实现）'
  + '以及用户提到 ticket、epic、story、任务拆解、进度记录，'
  + '或消息中出现 TICKET-/EPIC-/STORY- 等 OMT 节点 id 时使用。'
  + '本 skill 只负责 ticket 管理，不规定开发流程；开发方法论由其它 skill 负责。'

export const OMT_SKILL_CONTENT = `# OMT Ticket 管理

OMT 是一个五级 ticket 体系：\`Epic → Story → [SubStory] → Ticket → [SubTicket]\`。
每个节点是一个 Markdown 文件（详细内容），元数据与父子关系存于 SQLite。
你通过 omt_* 工具操作这个体系。

## 数据归属

ticket 按工作区归属：当前工作区根目录下存在 \`.omt/\` 时使用它（ticket 随项目走）；
否则使用全局 home（~/.omt 或 OMT_HOME）。omt_* 工具自动路由，规则：

- **创建 Epic**：可用 scope 参数显式指定（workspace/global）；未指定时系统会
  弹窗请用户选择归属——不要替用户做决定，也不要跳过该询问。
- **创建子节点**：总是落在父节点所在的 home（跟随父节点，无需也无法指定）。
- **show/update/move**：按节点 id 自动解析所属 home；move 不支持跨 home。

## 职责边界（重要）

- 你负责且仅负责：ticket 节点的 CRUD、层级合法性、状态流转、进度记录。
- 你**不负责**规定开发流程（如何规划、实现、调试、提交）。当前任务涉及开发
  方法论时，检查 <available_skills> 中是否有对应的流程类 skill（计划/实现/
  调试/commit 类），有则调用 skill 工具加载并遵循它。你与其并行生效：
  **它管"怎么做"，你管"进度记在哪"**。

## 层级规则

- epic → story；story → substory 或 ticket；substory → ticket；ticket → subticket。
- 只有 epic 可以没有父节点；SubStory/SubTicket 各最多一层，不可再嵌套。
- 非法挂载会被工具拒绝——不要重试同样的挂载，改用合法层级。

## 工具

| 工具 | 用途 |
|------|------|
| omt_create | 创建节点（type/title/parentId/body?/priority?） |
| omt_list | 列出节点（type?/status?/query? 关键词搜索） |
| omt_show | 查看节点详情（元信息、正文、父节点、子节点） |
| omt_update | 更新标题/状态/优先级，替换正文（body）或追加进度（append）；archived=true 归档、false 恢复 |
| omt_move | 移动节点（连同子树）到新父节点 |
| omt_reindex | 磁盘文件被手工修改后重建索引 |

**只用 omt_* 工具操作 ticket**，不要用文件工具直接改写 .omt 目录下的文件
（会破坏 SQLite 与文件的双写一致性；如怀疑不一致，调用 omt_reindex）。

## 工作约定

- **接手任务先定位 ticket**：用户消息中出现节点 id（如 TICKET-0001）时，先
  omt_show 读取其正文与验收标准，再开始工作。
- **状态流转**：开始处理时将状态置为 in_progress；完成时把关键结论通过
  omt_update 的 append 追加到正文（进度记录），并将状态置为 done。
- **归档是独立维度**（archived=true/false），与状态（open/in_progress/done）正交；
  归档节点只读——除恢复外的修改都会被拒绝，先恢复再改。
- **实质性开发先建单**：新功能 / 重新对接 / 改造 / 非琐碎实现，先 omt_create
  骨架再动手；不要等用户说「拆任务」。
- **任务拆解**：先建 Epic/Story 骨架，再逐层细化 Ticket；拆解结果落成
  真实的 OMT 节点，而不是只写在回复里。
- **范围克制**：不为与当前任务无关的节点做变更；不确定归属哪个节点时，
  先 omt_list/omt_show 确认，仍不确定则询问用户。
`

/** Register the embedded OMT skill (model- and user-invocable). */
export function registerOmtSkill(ctx: Context): void {
  ctx.skills.register({
    name: OMT_SKILL_NAME,
    description: OMT_SKILL_DESCRIPTION,
    content: OMT_SKILL_CONTENT,
    source: 'runtime',
  })
}
