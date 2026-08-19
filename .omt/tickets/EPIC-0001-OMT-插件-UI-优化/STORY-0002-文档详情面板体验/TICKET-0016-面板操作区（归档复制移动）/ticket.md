---
id: TICKET-0016
type: ticket
title: 面板操作区（归档/复制/移动）
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-17T23:41:43.926Z'
updated_at: '2026-08-18T02:50:33.542Z'
---

## 描述

面板操作区：常用操作集中成一行，布局规整。

## 实现要点

- 操作行：归档/恢复按钮（按当前状态切换）、复制 id、复制文件路径
- 评估"移动到新父节点"入口（父节点选择器，omt_move 已有工具/RPC 基础）
- 与 TICKET-0009 的归档撤销策略对齐

## 验收标准

- 归档/恢复、复制 id/路径在面板内一键完成
- 移动父节点有结论（实现或明确后置）

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 需求追加：操作行增加"执行"按钮——点击后把 @ticket 引用放进对话框并提交。实现结论：公开 InputActions 只有 setDraft/submit（真实 chip 的 occurrence 创建是触发流水线内部能力，上游议题），故采用纯文本 `@ID 开始执行这个 ticket` + submit；模型侧由 omt skill 约定接管（识别 id → omt_show → in_progress → 开工）。另：移动父节点入口本期后置（模型可经 omt_move 完成；UI 父节点选择器后续单独立项）。

- 2026-08-18 实现完成（v0.1.9）：面板操作行落地——[执行]（inputActions.setDraft+submit，纯文本 @ID 引用提交，chip 真实 occurrence 为上游议题）、[归档/恢复]（按状态切换）、[复制 id]、[复制路径]（RPC get 响应补 home 字段，复制绝对路径）。移动父节点入口后置（记录在票）。73/73 单测通过。
