---
id: TICKET-0040
type: ticket
title: 浮窗激活时隐藏 OMT tab 并回退 Chat
status: done
priority: 0
parent: STORY-0006
created_at: '2026-08-18T06:59:41.310Z'
updated_at: '2026-08-18T07:03:21.908Z'
---

## 任务

浮窗激活（drawerOpen && panelMode === 'float'）时隐藏 conversation.view 的 OMT tab；浮窗关闭或切回抽屉时恢复。

## 方案

复用 details shadow 的动态 register/dispose 模式：controller 增加 `attachViewTab(factory)`，订阅 drawerOpen + panelMode，浮窗激活时 dispose tab 注册。entry 消失后 shell 的 `resolveActiveView` 自动回退到 Chat tab（ui-conversation ConversationSession.tsx 已确认），无需改 shell。

## 验收

- 浮窗打开 → OMT tab 消失，原停留在 OMT tab 的会话回退到 Chat
- 浮窗关闭 / 切回抽屉 → tab 恢复
- 重复切换不重复注册；controller 单测覆盖

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

已完成：controller 增加 `attachViewTab(factory)`（订阅 drawerOpen+panelMode，浮窗激活时 dispose tab 注册，复用 details shadow 动态注册模式）；index.ts 的 conversation.view 注册改为动态。entry 消失后 shell `resolveActiveView` 自动回退 Chat（ui-conversation ConversationSession.tsx:26 确认），无需改 shell。单测覆盖隐藏/恢复/幂等。0.2.23 已安装到 web profile，重启 DSH 生效。
