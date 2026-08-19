---
id: TICKET-0011
type: ticket
title: 抽屉宽度拖拽与折叠状态记忆
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-17T23:32:01.862Z'
updated_at: '2026-08-18T03:43:10.164Z'
---

## 描述

抽屉布局灵活性：宽度可拖拽调整，折叠状态可记忆。

## 实现要点

- 抽屉右缘拖拽手柄，宽度持久化（localStorage 或 snapshot store persist）
- 节点折叠状态按会话/工作区记忆（snapshot store persist 或 localStorage）
- 宽度限制合理范围（240–480px）

## 验收标准

- 拖拽调宽即刻生效并持久化
- 刷新/重开后折叠状态保留

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.2）：① 抽屉右缘拖拽手柄（pointer 事件，240–480px 钳制，即刻生效）——宽度存于 snapshot store（persist: omt-drawer-width）刷新保留；② 折叠状态从组件本地 useState 迁移到共享 persist store（omt-collapsed，按节点 id 记忆），刷新/重开后保留。81/81 单测通过（新增宽度钳制与折叠切换用例）。

- 2026-08-18 拖拽体验修正（v0.2.4）：改用 DSH AppFrame 的 DragHandle 模式——`setPointerCapture`（指针离开把手事件不中断，原实现 pointermove 挂在 aside 上、快速移动即丢跟踪）+ rAF 节流 + 基于拖拽起点记录基准宽度；拖拽中把手高亮（data-dragging）。84/84 单测通过。

- 2026-08-18 拖拽把手视觉修正（v0.2.5）：悬停/拖拽反馈从整条 6px 亮带改为居中 2px 细线（可点区域保持 6px 不变），避免过粗的高亮边框。
