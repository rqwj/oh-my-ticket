---
id: TICKET-0039
type: ticket
title: 模式切换接线、文案与测试
status: done
priority: 0
parent: STORY-0006
created_at: '2026-08-18T06:33:42.652Z'
updated_at: '2026-08-18T06:53:04.709Z'
---

## 任务

- index.ts 接线：FloatWindow（shell.overlay）+ TicketTab（conversation.view）注册，inject hooks 补全
- 抽屉/浮窗 header 模式切换按钮，tab "弹出为浮窗"按钮
- locales（zh/en）补齐新增文案
- 测试：模式切换、几何钳制；跑通全部既有测试
- build + typecheck 通过

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

已完成：index.ts 接线（drawer/float 共享 overlayInject，mode 门互斥渲染；tab 独立注入 session 作用域）；zh/en 新增 panel.*/float.*/tab.aria 文案；float-geometry 9 测 + controller 3 测；114/114 测试通过，typecheck、build 通过。已打包 0.2.22 并安装进 ~/.dsh/profiles/web（待 DSH 重启生效）。
