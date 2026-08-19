---
id: TICKET-0002
type: ticket
title: '@ 候选排序与状态着色'
status: done
archived: true
priority: 0
parent: STORY-0003
created_at: '2026-08-17T14:49:04.342Z'
updated_at: '2026-08-18T02:51:07.376Z'
---

## 描述

`@` 引用 ticket 时优化候选菜单：已完成/已归档的节点排到候选最后；候选项用颜色区分状态。

## 实现要点

- 排序：在 client 触发源 candidates() 内做稳定排序——open/in_progress 保持搜索相关性顺序在前，done 靠后，archived 最后
- 颜色：候选的 icon 字段用状态对应的彩色圆点 emoji（⚪ 未开始 / 🔵 进行中 / 🟢 已完成 / ⚫ 已归档）——MenuView 按字符串渲染 icon，无需改宿主 UI

## 验收标准

- `@` 候选中 done/archived 节点排在最后
- 每个候选项图标按状态着色
- 搜索相关性在同状态组内保持

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 实现完成（v0.1.3）：candidates() 稳定排序（open/in_progress 保持相关性在前，done 靠后，archived 最后）+ 状态彩色圆点 icon（⚪🔵🟢⚫，MenuView 按文本渲染无需改宿主）。62/62 单测通过。顺带实测确认 turn-tail 数据链路（3080 recent 端点正确返回本会话的 STORY-0003/TICKET-0001）。
