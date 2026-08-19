---
id: TICKET-0021
type: ticket
title: 相关列表会话归属修正
status: done
archived: true
priority: 0
parent: STORY-0005
created_at: '2026-08-18T02:26:12.389Z'
updated_at: '2026-08-18T02:51:46.788Z'
---

## 描述

会话切换时相关 ticket 列表串台/丢失：有相关 ticket 的会话 1 切到无相关的会话 2 再切回，列表消失；而会话 2 反而显示会话 1 的列表。

## 根因

turnTail 链式 select 读取的是全局 `controller.currentSessionId`（由 ToggleButton 的 effect 上报，渲染后才更新），而不是**当前渲染的会话**：
- 切换会话的瞬间，选择器读到的是上一个会话 id → matched 携带错误 session
- matched 在选举时固定，后续 store 更新不会重新选举 → 串台/消失

## 实现要点

- select 无条件认领（返回 {}，priority 20 仍让位 deliverables）
- TurnTickets 组件改用**框架下发的 sessionId prop**（session 作用域标准件），与渲染的会话天然一致
- controller.refreshRelated 加 2 秒/会话节流（每轮尾部挂载都会触发拉取）

## 验收标准

- 会话来回切换，相关列表始终属于当前会话
- 无相关 ticket 的会话不渲染

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 修复完成（v0.1.13）：turnTail 链式 select 改为无条件认领（matched 在选举时冻结，读全局 currentSessionId 必然串台）；TurnTickets 改用框架下发的 sessionId prop（与渲染会话天然一致）；refreshRelated 增加 2 秒/会话节流。75/75 单测通过。
