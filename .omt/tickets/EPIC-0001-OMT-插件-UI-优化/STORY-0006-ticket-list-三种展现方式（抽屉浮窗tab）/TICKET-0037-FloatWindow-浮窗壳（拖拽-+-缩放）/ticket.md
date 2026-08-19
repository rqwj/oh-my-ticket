---
id: TICKET-0037
type: ticket
title: FloatWindow 浮窗壳（拖拽 + 缩放）
status: done
priority: 0
parent: STORY-0006
created_at: '2026-08-18T06:33:42.630Z'
updated_at: '2026-08-18T06:52:48.382Z'
---

## 任务

新建 `FloatWindow.tsx`：注册 `shell.overlay`（id: omt-float），包 TicketPanel 的可拖拽浮窗壳。

## 要点

- header 拖拽移动（pointer capture + rAF，复用 DrawerDragHandle 模式）
- 右下角缩放 handle（最小 280×320，钳制在视口内）
- z-index 与抽屉一致（60）；窄视口约束
- header 提供"切换为抽屉"按钮
- panelMode === 'float' 且 panel 打开时渲染

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

已完成：`FloatWindow.tsx` + css，注册 shell.overlay（id omt-float, order 51）。header 拖拽移动（按钮/输入框除外）、右下角缩放，均 pointer capture + rAF 节流 + 视口钳制；几何偏好按渲染时实时视口重钳制，窗口缩放不会遗留屏外。
