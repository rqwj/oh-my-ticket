---
id: TICKET-0019
type: ticket
title: RecentRegistry 持久化
status: done
archived: true
priority: 0
parent: STORY-0003
created_at: '2026-08-18T00:25:13.081Z'
updated_at: '2026-08-18T02:51:31.259Z'
---

## 描述

host 进程重启后 RecentRegistry（会话相关 ticket 列表）丢失——它是纯内存态。需持久化。

## 实现要点

- 存储：全局 home 的 omt.db `meta` 表，键 `recent:<sessionId>`，值为 JSON 数组（最近优先，上限 10）——复用现有表结构，无需新表
- OmtCore 增加 `getSessionRecent`/`setSessionRecent`（meta 读写）
- RecentRegistry 挂接持久化委托：touch 时异步落盘；list 内存未命中时从磁盘加载并回填内存
- `/omt recent` 端点改为 async resolve（内存→磁盘）

## 验收标准

- host 重启后相关列表从磁盘恢复（刷新页面即重现）
- 常规 touch/list 行为不变（去重、上限、会话隔离）

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.1.11）：RecentRegistry 挂接持久化委托——touch 异步写入全局 home 的 meta 表（recent:<sessionId>，JSON 数组，上限 10）；`/omt recent` 端点改用 async resolve（内存未命中→磁盘加载→回填内存）。host 重启后相关列表经页面刷新即恢复。74/74 单测通过（含模拟重启恢复用例）。
