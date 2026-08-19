---
id: TICKET-0018
type: ticket
title: 刷新后相关 ticket 列表恢复
status: done
archived: true
priority: 0
parent: STORY-0003
created_at: '2026-08-18T00:19:59.401Z'
updated_at: '2026-08-18T02:51:21.444Z'
---

## 描述

刷新页面后，对话轮次尾部的"相关 ticket"列表消失。

## 根因

client 端 related store 是内存态，刷新即空；链式选择器原逻辑"有相关数据才认领"，导致组件从不挂载、永远不去拉取 host 端仍然存活的 RecentRegistry 数据（鸡生蛋）。

## 实现要点

- turnTail 链式 select 改为：有当前会话即认领（priority 20 仍让位 deliverables）
- 组件挂载即 refreshRelated 拉取 host recent → store 填充 → 列表重现
- 真无相关时组件渲染 null（无视觉噪音）

## 验收标准

- 刷新页面后，有相关 ticket 的会话其轮次尾部列表恢复显示
- 无相关 ticket 的会话无任何渲染
- deliverables 产出文件条目优先权不变

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 修复完成（v0.1.10）：turnTail 链式 select 改为"有当前会话即认领"（priority 20 仍让位 deliverables），组件挂载即 refreshRelated 拉取 host 端存活的 recent 数据，打破"无数据不认领→不挂载→永不拉取"的循环；真无相关时渲染 null。残留限制：host 进程重启后 RecentRegistry 丢失（内存态），持久化留待后续。73/73 单测通过。
