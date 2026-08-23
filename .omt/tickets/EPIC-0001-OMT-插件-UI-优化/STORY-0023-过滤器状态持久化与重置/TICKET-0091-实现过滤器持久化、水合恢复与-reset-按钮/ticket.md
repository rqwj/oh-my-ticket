---
id: TICKET-0091
type: ticket
title: 实现过滤器持久化、水合恢复与 reset 按钮
status: done
priority: 0
parent: STORY-0023
created_at: '2026-08-22T00:47:45.211Z'
updated_at: '2026-08-22T01:00:25.169Z'
---

## 交付结果
ticket tree 过滤器状态持久化到 `<home>/ui-filters.json`，面板加载时恢复；过滤器面板末尾新增靠右 reset 按钮，重置立即落盘。

## 工作范围
- host：ui-state 模块（zod 校验 + 默认值合并 + 原子写），core 方法，`filters-get` / `filters-set` RPC 端点
- client：controller load/save 流；TicketPanel 挂载水合 + 变更防抖自动保存 + reset 按钮（右对齐，立即保存）
- 三种壳（Drawer/FloatWindow/TicketTab）props 转发；中英文案；CSS
- .gitignore 排除 `.omt/ui-filters.json`（个人偏好不进 git）
- host/client 回归测试

## 依赖
- 既有 tree-filter 状态形状与 RPC 通道（/omt）

## 验收标准
- 改过滤器 → 刷新 → 恢复一致；reset → 立即保存默认值
- 保存文件缺失/损坏回退默认，不阻塞加载
- 全量测试、typecheck、build 通过

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

实现完成：
- host：新增 ui-state.ts（zod 校验、默认值合并、temp+rename 原子写）；core.savedFilters/saveSavedFilters；RPC `filters-get`/`filters-set`（部分补丁合并语义）。存储位置 `<home>/ui-filters.json`（workspace .omt 优先，无则全局 home）。
- client：controller.loadFilters/saveFilters；TicketPanel 挂载水合 + 变更防抖(300ms)自动保存；过滤器行末尾右对齐 reset 按钮（立即落盘默认值）；三壳 props 转发；中英文案。
- 卫生：.gitignore 排除 .omt/ui-filters.json（个人偏好不进 git）；README 特性清单更新。
验证：typecheck / 27 文件 344 测试 / build / git diff --check 全部通过；新增 rpc 持久化+损坏回退用例与组件水合/reset 用例。
