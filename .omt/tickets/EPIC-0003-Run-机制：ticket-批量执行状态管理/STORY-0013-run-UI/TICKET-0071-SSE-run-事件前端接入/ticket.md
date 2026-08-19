---
id: TICKET-0071
type: ticket
title: SSE run 事件前端接入
status: open
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.404Z'
updated_at: '2026-08-19T09:50:09.805Z'
---

## 任务

SSE 通道扩展 run 维度：

- 现有 ChangeHub bump 之外，run 变化（item 推进、run 状态变化、通知）触发
  前端 store 刷新 run 视图
- payload 设计向后兼容（旧客户端忽略 run 字段）
- controller.ts store 增加 run 相关 snapshot

## 验收

- run 操作后 UI 即时刷新，无需手动
- events/controller 单测更新

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
