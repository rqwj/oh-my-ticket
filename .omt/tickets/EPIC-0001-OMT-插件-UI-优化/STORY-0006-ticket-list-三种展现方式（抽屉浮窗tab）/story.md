---
id: STORY-0006
type: story
title: ticket list 三种展现方式（抽屉/浮窗/tab）
status: done
priority: 0
parent: EPIC-0001
created_at: '2026-08-18T06:33:12.196Z'
updated_at: '2026-08-18T07:03:21.931Z'
---

## 目标

OMT ticket list 支持三种展现方式并可互相切换：

1. **左侧抽屉**（现状）：`shell.overlay` 浮层，顶部 OMT 按钮开合
2. **浮窗**：同样注册 `shell.overlay`，`position: fixed` 自由拖拽 + 缩放
3. **OMT tab**：注册 `conversation.view`（order 20，label 'OMT'），出现在 Chat ｜ Trajectory 旁

## 方案要点

- 抽取共享 `TicketPanel`（搜索/过滤/排序/树行），三个壳复用
- controller 增加 `panelMode: 'drawer'|'float'`（persist）+ 浮窗位置/尺寸 persist stores
- 复用 DrawerDragHandle 的 pointer capture + rAF 拖拽模式
- tab 模式直接用标准 props 的 `sessionId`，不走 useSessions 侧信道
- Host 侧零改动（复用 /omt RPC + SSE）

## 验收标准

- 三种模式都能完整展示/操作树（选择、归档、过滤、排序、刷新、reindex）
- 抽屉 ↔ 浮窗可在面板 header 一键切换；tab 内可"弹出为浮窗"
- 模式与浮窗几何偏好持久化，刷新后保留
- 窄视口（<640px）下浮窗有最小尺寸约束
- 现有测试通过，新增模式切换逻辑测试

<!-- omt:children -->
## 子节点

- [TICKET-0035 抽取共享 TicketPanel 组件（重构 Drawer）](TICKET-0035-抽取共享-TicketPanel-组件（重构-Drawer）/ticket.md) — done
- [TICKET-0036 controller 增加 panelMode 与浮窗几何状态](TICKET-0036-controller-增加-panelMode-与浮窗几何状态/ticket.md) — done
- [TICKET-0037 FloatWindow 浮窗壳（拖拽 + 缩放）](TICKET-0037-FloatWindow-浮窗壳（拖拽-+-缩放）/ticket.md) — done
- [TICKET-0038 OMT tab 注册 conversation.view](TICKET-0038-OMT-tab-注册-conversation.view/ticket.md) — done
- [TICKET-0039 模式切换接线、文案与测试](TICKET-0039-模式切换接线、文案与测试/ticket.md) — done
- [TICKET-0040 浮窗激活时隐藏 OMT tab 并回退 Chat](TICKET-0040-浮窗激活时隐藏-OMT-tab-并回退-Chat/ticket.md) — done
<!-- /omt:children -->

五个 ticket 全部完成。三种展现方式（抽屉/浮窗/tab）共享 TicketPanel，mode 与浮窗几何持久化；0.2.22 已安装到 web profile，DSH 重启 + 页面刷新后生效。

TICKET-0040 完成：浮窗激活时 OMT tab 隐藏、视图自动回退 Chat。
