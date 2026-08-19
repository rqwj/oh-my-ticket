---
id: TICKET-0038
type: ticket
title: OMT tab 注册 conversation.view
status: done
priority: 0
parent: STORY-0006
created_at: '2026-08-18T06:33:42.641Z'
updated_at: '2026-08-18T06:52:48.390Z'
---

## 任务

新建 `TicketTab.tsx`：注册 `conversation.view`（id: 'omt'，order: 20，label: 'OMT'），复用 TicketPanel 嵌入会话主体。

## 要点

- sessionId 直接取框架标准 props，不走 useSessions 侧信道
- 面板 header 提供"弹出为浮窗"按钮
- 注意 conversation.view 是 session scope，多会话各自持有 viewing 状态

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

已完成：`TicketTab.tsx` 注册 conversation.view（id 'omt', order 20, label 'OMT'），sessionId 直接取框架标准 props；header 提供"弹出为浮窗"（setPanelMode('float') + openPanel）。
