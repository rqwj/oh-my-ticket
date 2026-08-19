---
id: TICKET-0035
type: ticket
title: 抽取共享 TicketPanel 组件（重构 Drawer）
status: done
priority: 0
parent: STORY-0006
created_at: '2026-08-18T06:33:42.606Z'
updated_at: '2026-08-18T06:52:48.363Z'
---

## 任务

把 Drawer.tsx 中的树内容（header 操作、toolbar、filters、tree 行）抽成共享的 `TicketPanel` 纯 props 组件，Drawer 变为壳（定位 + 拖拽 handle）包 TicketPanel。

## 要点

- 过滤/排序等 viewing 状态随 TicketPanel 走（每个壳实例各自持有）
- Drawer 对外行为不变（回归保证）
- TreeRow / ReindexButton / DrawerDragHandle 等子组件一并整理

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

已完成：抽出 `TicketPanel.tsx` + `TicketPanel.module.css`（header/toolbar/filters/tree 全部共享内容），Drawer 变为纯壳（open 门、宽度、拖拽 handle）。Selector 类型从 TicketPanel re-export，旧引用不破。
