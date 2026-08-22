---
id: TICKET-0066
type: ticket
title: RunningInfo 执行者谱系扩展
status: done
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.307Z'
updated_at: '2026-08-19T23:00:03.523Z'
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

## 完成记录（P2）

- RunningInfo 新增 parentSessionId/isSubagent；`start()` 接受 lineage 快照，
  `lineageOfHeader()` 从 session header（parentSession + origin==='subagent'）读取。
- 调用点接线：rpc.ts execute（AgentsLike header 扩展 parentSession/origin）、
  tools.ts trackRunning（exec.agent.session.header）。RPC get 的 running 字段自动携带谱系。
- 测试：tests/running.spec.ts — registry 谱系快照/空谱系 + execute 端点谱系填充。
