---
id: TICKET-0001
type: ticket
title: 对话末端相关 ticket 列表
status: done
archived: true
priority: 0
parent: STORY-0005
created_at: '2026-08-17T14:08:23.614Z'
updated_at: '2026-08-18T02:51:40.363Z'
---

## 描述

每次对话轮次完成后，在对话最末端显示该轮相关的 ticket 列表；点击 ticket 后在右侧详情面板展示该 ticket。

## 实现要点

- 挂载点：`conversation.chat.turnTail`（chain 型、session 作用域 slot，Turn 尾部扩展链）——按轮次渲染，正是"对话最末端"的设计座位
- 相关性来源（MVP 取并集即可）：
  1. 本轮消息中 `@` 引用的 ticket（ReferenceInsert 占位符/序列化块中的 id）
  2. 本轮 omt_* 工具调用涉及的节点 id
  3. 当前激活 ticket（ActiveDock 状态）
- 点击行为：复用 `OmtController.select(id, sessionId)`——加载文档 + 动态遮蔽 details 面板 + `layout.openDetails()`
- 轮次数据：owner props 携带 turn 信息，从 turn 的消息/工具块中提取 `EPIC-/STORY-/TICKET-` 等 id 模式

## 验收标准

- 一轮对话结束后，末端出现相关 ticket 列表（类型徽章+标题+状态点，与抽屉视觉一致）
- 点击列表项 → 右侧详情面板打开该 ticket
- 本轮无相关 ticket 时不渲染（无视觉噪音）

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 开始实现：先调研 conversation.chat.turnTail 链式 slot 契约与轮次数据形状

- 2026-08-17 实现完成（v0.1.2）：turnTail 链式条目（priority 20，让位 deliverables）+ RecentRegistry（host 端按会话记录触达：omt_* 工具执行、RPC get/update、codec 序列化）+ client related store（codec 直录 + refreshRelated 拉取合并）+ TurnTickets 组件（类型徽章/标题/状态点，点击走 controller.select 打开详情面板）。链式 select 读热快照，TurnTailNodeView 每轮重渲染保证新鲜度。顺带修正：RPC get/update 改为按节点归属路由（coreForNode），与工具行为一致。61/61 单测通过。
