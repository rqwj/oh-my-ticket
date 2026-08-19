---
id: TICKET-0003
type: ticket
title: 变更即时推送刷新
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-17T15:04:01.511Z'
updated_at: '2026-08-18T02:49:45.363Z'
---

## 描述

新增任务或任务状态变化时，Ticket 树应立即更新，而不是依赖手动刷新或抽屉重开。

## 实现要点

- host：`ctx.webServer.register()` 注册 `/omt/events` SSE 端点 + ChangeHub（变更广播）
- 触达点：omt_create/update/move/reindex 工具执行成功、RPC update/reindex 后广播
- client：controller 用 EventSource 订阅，变更后防抖刷新树 + 相关列表 + 当前打开的文档

## 验收标准

- 模型用工具创建/更新 ticket 后，抽屉树在 1 秒内自动刷新
- 打开的文档详情同步刷新
- 无变更时不产生多余请求

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 实现完成（v0.1.4）：ChangeHub 变更广播 + `/omt/events` SSE 端点（ctx.webServer.register）+ 工具/RPC 变更触达（create/update/move/reindex）+ client EventSource 订阅（300ms 防抖，刷新树/相关列表/打开的文档）。SSE 端到端实测：变更后 1 秒内收到 data 帧。65/65 单测通过。
