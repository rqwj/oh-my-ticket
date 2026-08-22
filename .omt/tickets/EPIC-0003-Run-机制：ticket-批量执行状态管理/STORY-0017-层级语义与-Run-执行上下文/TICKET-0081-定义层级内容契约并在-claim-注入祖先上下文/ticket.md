---
id: TICKET-0081
type: ticket
title: 定义层级内容契约并在 claim 注入祖先上下文
status: done
priority: 0
parent: STORY-0017
created_at: '2026-08-21T11:56:23.624Z'
updated_at: '2026-08-21T12:27:23.394Z'
---

## 交付结果
建立统一的 OMT 层级内容契约，并让每次 run 认领 Ticket/SubTicket 时获得最新、完整、可追踪的祖先背景；执行者仍只执行和报告当前项。

## 内容契约
- Epic：总体目标、范围、非目标、全局约束、成功标准
- Story/SubStory：一个可独立验收的产品或系统能力、共享规则与边界
- Ticket/SubTicket：一次认领可完成并可独立报告的单一结果、验收标准与明确依赖
- 父级已有背景不复制到 Ticket，由 claim 时注入

## 工作范围
- 更新 omt Skill，明确各层内容边界、拆分原则与避免重复规则
- 更新 omt-runs Skill，明确祖先只读背景和当前执行项的区别
- 更新 Epic、Story/SubStory、Ticket/SubTicket 的默认正文模板
- 扩展 omt_run_claim 的结构化结果：按 Epic→Story→SubStory 返回最新祖先正文，并返回当前 Ticket/SubTicket 完整正文
- 设置祖先上下文预算；超限时明确标记截断，优先保留离当前 Ticket 最近的背景
- 更新工具说明和回归测试

## 非范围
- 注入兄弟 Ticket 或整个子树
- 自动摘要父级正文
- 为 run 保存上下文快照
- 对正文质量做硬拒绝式校验
- 将 Epic/Story/SubStory 恢复为可执行 run item

## 验收标准
- Skill 明确 Story 按可独立验收能力划分，Ticket 按一次认领的单一结果划分
- 分类型默认模板体现上述边界
- omt_run_claim 的结果清晰分为“背景（不可执行）”与“当前执行项”
- 祖先正文在 claim 时实时读取，父级修改对后续 claim 生效
- 祖先上下文超预算时有可见截断标记，且优先保留最近父级
- 只有当前 Ticket/SubTicket 可被置为 in_progress 并通过 omt_run_report 收尾
- 相关测试、类型检查和构建通过

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

开始实现：更新层级内容契约、分类型模板与 omt_run_claim 祖先上下文返回。

完成实现：
- 为 Epic、Story/SubStory、Ticket/SubTicket 增加明确的分类型默认正文模板与 Skill 内容边界。
- omt_run_claim 现在返回按根到叶排序的最新祖先用户正文和当前执行项用户正文；祖先上下文采用 16 KiB 预算、优先保留最近父级并显式标记 UTF-8 安全截断。
- 单个祖先读取失败时保留其他可读背景和当前执行项，并通过 read_errors 与模型可见提示报告；当前执行项读取失败仍保留已认领结果并返回 context_error。
- 澄清上下文是认领成功后的即时读取，不是跨文件原子快照；插件管理的子节点块不进入执行上下文。
- 代码审查发现并修复“单个祖先失败导致当前任务正文丢失”的 P1 问题。
验证：pnpm test（27 文件、334 测试通过）、pnpm typecheck、pnpm build、git diff --check 均通过。

进入本地打包安装与用户验收阶段。
