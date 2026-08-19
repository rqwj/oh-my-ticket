---
id: TICKET-0063
type: ticket
title: disposed hook：subagent 终止父会话兜底
status: open
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.279Z'
updated_at: '2026-08-19T09:50:09.726Z'
---

## 任务

Tier 2 hook：监听 `agent/disposed`：

- executor 是 subagent：终止时名下 running item 未完成 → 向父会话
  **followup**（唤醒 idle 驱动；inject 不唤醒 idle 会话，框架 API 契约证实）
  「你委派的 subagent 已结束，TICKET-xxxx 未标 done，最终报告：…」；
  父会话也不在 → item interrupted
- executor 是主会话：会话结束且 run 有未完项 → run interrupted（等 resume）

## 验收

- subagent / 主会话两条路径分别覆盖
- 父会话 inject 内容含 subagent 最终报告摘要
- **父会话 idle 时 followup 通知必达**（唤醒驱动）
- 单测

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
