/**
 * Embedded OMT skills: registered at runtime through ctx.skills.register()
 * (one of the three skill contribution paths; no SKILL.md file needed).
 * Split into two skills (STORY-0014) so the catalog gives progressive
 * loading: `omt` stays lean (node CRUD / hierarchy / status), `omt-runs`
 * carries the batch-execution discipline and only loads when its routing
 * words match. The bodies teach — critically — the boundary against
 * process/workflow skills: OMT manages WHERE progress is recorded, never
 * HOW to develop.
 */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-skill'

export const OMT_SKILL_NAME = 'omt'

export const OMT_SKILL_DESCRIPTION =
  'OMT ticket 体系的操作规范：Epic→Story→[SubStory]→Ticket→[SubTicket] 五层结构的创建、'
  + '查询、状态流转与进度记录。当用户提到 ticket、epic、story、任务拆解、进度记录，'
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

## 内容边界

- **Epic**：描述总体目标、范围、非目标、全局约束与成功标准；不写具体执行步骤。
- **Story/SubStory**：描述一个可独立验收的产品或系统能力、使用者、共享规则与
  边界。默认按能力拆分，不按前端/Host/数据库等技术模块机械拆分。
- **Ticket/SubTicket**：描述一次认领可完成、可独立报告的单一结果、工作范围、
  依赖与验收标准。一个 Ticket 可以跨多个技术层，但不能包含多个可独立交付结果。
- **上下文继承**：不要在 Ticket 中复制 Epic/Story 的父级背景；父级已有的目标、
  范围和共享约束由 run claim 在执行时注入。Ticket 只写相对父级新增的具体任务信息。
- 创建节点时按上述边界组织正文；省略 body 时，默认模板会为不同节点类型提供对应章节。

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
  omt_update 的 append 追加到正文（进度记录），并将状态置为 done；
  做不下去置 blocked（外部条件阻塞）、必须跳过置 skipped，均附 append 说明。
  ticket 置 in_progress（或 run 认领成功）时，系统会自动把祖先链中仍为 open
  的父 Ticket/SubStory/Story/Epic 一并置为 in_progress——不要手动改父级状态，
  也不要重开已 done/blocked/skipped 的父级。
- **归档是独立维度**（archived=true/false），与状态（open/in_progress/done/
  blocked/skipped）正交；归档节点只读——除恢复外的修改都会被拒绝，先恢复再改。
- **任务拆解**：先建 Epic/Story 骨架，再逐层细化 Ticket；拆解结果落成
  真实的 OMT 节点，而不是只写在回复里。
- **范围克制**：不为与当前任务无关的节点做变更；不确定归属哪个节点时，
  先 omt_list/omt_show 确认，仍不确定则询问用户。
- **批量执行多个 ticket**（run 的创建/续跑/结果汇报）时，加载 \`omt-runs\`
  skill 并遵循其中的执行纪律。
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

export const OMT_RUNS_SKILL_NAME = 'omt-runs'

export const OMT_RUNS_SKILL_DESCRIPTION =
  'OMT run（ticket 批量执行批次）的执行纪律：run 的创建/控制/认领/结果汇报、'
  + '续跑 nudge 的响应、failed/blocked/skipped 的如实报告。'
  + '当需要批量执行多个 ticket、创建或续跑 run、跳过某项、或处理 blocked/失败项时使用。'
  + '本 skill 只负责批次纪律，不规定开发流程；开发方法论由其它 skill 负责。'

export const OMT_RUNS_SKILL_CONTENT = `# OMT Run 批量执行

run 是 ticket 的批量执行机制：一次做完一批 ticket，有队列、有进度、
有单项成败记录、可中断续跑。你通过 omt_run_* 工具驱动它。

## 概念模型

- **run = Ticket/SubTicket 的有序快照**：创建时把可执行成员（可跨
  Story/Epic 挑选）按执行顺序快照进 run；Epic/Story/SubStory 只提供背景与
  选择范围，绝不作为可认领 item。此后 run 与 ticket 树不维持结构链接。
  同一 ticket 可同时属于多个活跃 run；所有成员必须同属一个 OMT home。
- **item 状态机**：\`pending → running → done / failed / blocked / skipped /
  interrupted\`；另有 \`awaiting_confirmation\`（见「信任策略」）。
  - \`failed\`：执行失败，仅 item 落 failed，ticket 保持 in_progress（可重试）。
  - \`blocked\`：外部条件做不下去；\`skipped\`：必须跳过。两者 ticket 与
    item 同步落对应状态。
  - \`interrupted\`：执行中断（会话销毁/宿主重启）。恢复分两步：先
    \`omt_run_control resume\` 把 run 拉回 running（resume **不会**重置
    interrupted 项），再逐项 \`omt_run_control retry\` 把 interrupted 项重置回
    pending，之后才能被重新 claim。
- **run 终态**：\`completed\`（全 done/skipped）/ \`completed_with_failures\`
  （含 failed 或 interrupted 项）/ \`canceled\` / \`interrupted\`。
  run 本身没有 failed；\`interrupted\` 不是绝对终态——可 resume 回 running 续跑。

## 执行上下文

- \`omt_run_claim\` 在 claim 成功后立即读取各节点最新正文（这是即时读取，
  不是跨文件原子快照），按层级顺序返回所有祖先（\`Epic → Story → [SubStory]
  → [父 Ticket]\`），并单独返回当前 Ticket/SubTicket 的完整用户正文
  （不含插件管理的子节点清单，避免注入兄弟节点或子树）。
- 祖先内容是**只读背景**：用于理解目标、范围与共享约束；不要执行、更新或
  report 任何祖先节点。只执行和报告“当前 Ticket/SubTicket”。
- claim 不注入兄弟节点或整个子树，也不使用 run 创建时的旧快照；父级正文修改后，
  后续 claim 会看到最新内容。单个祖先读取失败时，结果会标记该节点并保留其余可读
  背景和当前执行项；按提示用 \`omt_show\` 补读失败节点。
- 祖先正文超出预算时会显示截断标记，并优先保留离当前执行项最近的父级背景；
  不要把截断内容误当成完整约束，必要时用 \`omt_show\` 读取对应父节点。
- **祖先激活**：claim 成功会兜底把祖先链中仍为 open 的节点置为 in_progress
  （执行者随后置 in_progress 也会触发同一级联）。这是系统行为：不要手动升级
  或降级父级状态，也不要把祖先的 in_progress 当成需要你执行的任务。

## 工具

| 工具 | 用途 |
|------|------|
| omt_run_create | 创建 run（nodeIds 按执行顺序；config 可覆盖 stopOnFailure/autoContinue/autoVerify） |
| omt_run_list | 列出 run（可按状态过滤，附成员进度统计） |
| omt_run_show | 查看 run 详情：配置与成员状态/执行者/attempts/last_error |
| omt_run_control | start / pause / resume / cancel / retry(nodeId) / remove(nodeId) |
| omt_run_claim | 原子认领下一个 pending 项：置 running 并绑定当前会话为执行者 |
| omt_run_report | 报告执行结果：outcome ∈ done/failed/blocked/skipped，note 记入 ticket 进度 |

## 执行纪律（核心）

逐项循环：**做完一项 → omt_run_report → omt_run_claim 领下一项**。

1. 完成一项后立刻 \`omt_run_report\` 报告结果，再用 \`omt_run_claim\` 认领下一项；
   收到「run 有待执行项」的续跑提醒（nudge）时同样走 claim → 执行 → report。
   claim 成功后先用 omt_update 把该 ticket 置 in_progress 再动手（claim 只翻转
   item 并绑定执行者，不改 ticket 状态；置 in_progress 才有未收尾提醒兜底）。
2. **如实报告，用对词汇**：
   - 做完了 → \`done\`（ticket 与 item 同落 done）。
   - 做了但没成 → \`failed\`，note 写清失败原因（记入 last_error，ticket 保持
     in_progress 等重试）。**不许留下 in_progress 悬空就走人**。
   - 外部条件不满足做不下去 → \`blocked\`，note 写明缺什么。
   - 必须跳过 → \`skipped\`，note 写明跳过原因。
3. 收到 idle 提醒（未收尾 ticket 或 run 续跑）时：收尾或继续，**不要无视**。
4. **委派 subagent 执行时**：把 ticket id 和 run id 写进委派任务，由 subagent
   自己 omt_run_report 报告结果——不要替它报告，也不要漏报。

## 信任策略

- 经 \`omt_run_report\` 的完成是**显式报告**：item 直接落 done，note 一并记入
  ticket 进度——这是唯一不受信任门影响的完成路径。
- 未经 report、由执行者会话本人直接用 \`omt_update\` 把 ticket 落 done（item 处于
  running）：run 配置 \`autoVerify=false\`（默认）时 item 进入
  \`awaiting_confirmation\`，等待人到 run 详情确认或打回（打回把 ticket 重开为
  open，item 落 interrupted，需 retry 重置后重新认领）；\`autoVerify=true\`
  时直接落 done。
- \`awaiting_confirmation\` 绝不自动完成：无响应或含糊的更新不会改变它。
- 非执行者会话的状态修改、pending 项的直接状态设置不受信任门影响；非 run
  成员的 ticket 行为完全不变。

## 边界

- \`paused\` 的 run 不可 claim（pause 只停派发与续跑 nudge，进行中的项继续）；
  恢复用 omt_run_control resume。
- 跨 home 的成员组 run 会被拒绝；item failed 且 run 开了 stopOnFailure 时
  run 自动 pause，由人决定 resume 还是 cancel。
- 直接用 omt_update 把 ticket 置 blocked/skipped/done 会同步推进持有它的
  活跃 run 的 item；把 done/blocked/skipped 的 ticket 改回 open 会让 item
  回退 pending（回放）。这些被动推进不替代你的显式 report。
- 你负责且仅负责批次纪律；如何实现每个 ticket（开发流程）由对应的流程类
  skill 决定。
`

/** Register the embedded omt-runs skill (batch-execution discipline). */
export function registerOmtRunsSkill(ctx: Context): void {
  ctx.skills.register({
    name: OMT_RUNS_SKILL_NAME,
    description: OMT_RUNS_SKILL_DESCRIPTION,
    content: OMT_RUNS_SKILL_CONTENT,
    source: 'runtime',
  })
}
