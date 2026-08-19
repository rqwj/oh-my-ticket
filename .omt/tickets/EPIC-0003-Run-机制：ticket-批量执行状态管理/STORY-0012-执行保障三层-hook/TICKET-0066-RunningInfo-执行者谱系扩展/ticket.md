---
id: TICKET-0066
type: ticket
title: RunningInfo 执行者谱系扩展
status: open
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.307Z'
updated_at: '2026-08-19T09:50:09.756Z'
---

## 任务

RunningRegistry.RunningInfo 扩展执行者谱系：

- 新增 parentSessionId / isSubagent，start() 时从 agent.session.header
  （parentSession、origin === 'subagent'）读一次快照
- RPC show/list 返回谱系字段，供 UI 展示「父会话 ↳ subagent」

## 验收

- subagent 执行时谱系字段正确填充
- 普通会话执行为空谱系
- 单测更新（running.spec.ts）

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
