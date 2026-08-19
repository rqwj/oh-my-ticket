---
id: TICKET-0012
type: ticket
title: 正文内联编辑
status: open
priority: 0
parent: STORY-0002
created_at: '2026-08-17T23:40:53.504Z'
updated_at: '2026-08-17T23:40:53.504Z'
---

## 描述

正文从"只读 + 追加"升级为可完整编辑——本 Story 最大的验收缺口。

## 实现要点

- 阅读/编辑双态切换（编辑按钮 ↔ textarea + 保存/取消）
- host：`/omt` RPC 的 update 端点补 `body` 参数（core.update 已支持，RPC 层未暴露）
- 编辑中防止 SSE 自动刷新覆盖草稿（编辑态暂停当前文档的自动重载，或基于 updated_at 做冲突提示）
- 保存后走 ChangeHub 广播，树与其它视图同步

## 验收标准

- 正文可直接编辑并保存，Markdown 预览正确
- 编辑过程中外部变更不打断/覆盖草稿
- 取消编辑不改动任何数据

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
