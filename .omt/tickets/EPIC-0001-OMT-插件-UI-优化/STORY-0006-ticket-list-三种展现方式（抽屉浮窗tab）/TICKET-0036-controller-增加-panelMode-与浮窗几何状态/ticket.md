---
id: TICKET-0036
type: ticket
title: controller 增加 panelMode 与浮窗几何状态
status: done
priority: 0
parent: STORY-0006
created_at: '2026-08-18T06:33:42.618Z'
updated_at: '2026-08-18T06:52:48.373Z'
---

## 任务

controller 增加三种展现方式所需的状态与动作：

- `panelMode: SnapshotStore<'drawer'|'float'>`（persist: omt-panel-mode）
- 浮窗位置 `floatPos {x,y}` 与尺寸 `floatSize {w,h}`（persist）
- `setPanelMode` / `setFloatPos` / `setFloatSize` 动作（带最小尺寸/边界钳制）

## 验收

- 模式与几何偏好刷新后保留
- 钳制逻辑有单元测试

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

已完成：controller 增加 `panelMode`（persist omt-panel-mode）、`floatPos`/`floatSize`（persist）及 `setPanelMode`/`setFloatPos`/`setFloatSize`/`openPanel`；纯函数钳制在 `float-geometry.ts`（最小 280×320、视口可达性 80/40px），9 个单测覆盖。
