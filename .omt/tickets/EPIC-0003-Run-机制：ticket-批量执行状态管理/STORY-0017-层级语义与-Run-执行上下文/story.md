---
id: STORY-0017
type: story
title: 层级语义与 Run 执行上下文
status: done
priority: 0
parent: EPIC-0003
created_at: '2026-08-21T11:56:04.226Z'
updated_at: '2026-08-21T12:27:26.528Z'
---

## 能力结果
OMT 能够用清晰的 Epic/Story/Ticket 内容边界组织任务，并在 run 认领 Ticket 时提供完整、可追踪的祖先背景，使执行者只执行当前 Ticket，同时遵守父级目标与共享约束。

## 使用者
- 创建与拆解 OMT 节点的用户和代理
- 通过 run 批量执行 Ticket/SubTicket 的代理

## 范围
- 明确 Epic、Story/SubStory、Ticket/SubTicket 的内容边界
- Story 按可独立验收的产品/系统能力划分
- Ticket 按一次认领可完成、可独立报告的单一结果划分
- OMT Skill 提示词与分类型默认模板保持一致
- omt_run_claim 返回最新祖先正文和当前执行项的结构化上下文
- 超出上下文预算时明确截断，优先保留离 Ticket 最近的父级背景

## 非范围
- 注入兄弟 Ticket 或整个子树
- 自动摘要父级正文
- 在 run 创建/启动时冻结上下文快照
- 对正文粒度进行硬拒绝式校验

## 验收标准
- Epic/Story/SubStory 始终作为只读背景，不成为可执行 run item
- Ticket/SubTicket 是唯一可认领、执行和报告的节点
- 执行上下文按 Epic→Story→SubStory→当前 Ticket 顺序清晰分区
- 父级正文修改后，后续 claim 使用最新内容

<!-- omt:children -->
## 子节点

- [TICKET-0081 定义层级内容契约并在 claim 注入祖先上下文](TICKET-0081-定义层级内容契约并在-claim-注入祖先上下文/ticket.md) — done
<!-- /omt:children -->

能力已交付：TICKET-0081 完成层级内容契约、分类型模板和 run claim 祖先上下文注入，相关验证全部通过。

进入本地打包安装与用户验收阶段。
