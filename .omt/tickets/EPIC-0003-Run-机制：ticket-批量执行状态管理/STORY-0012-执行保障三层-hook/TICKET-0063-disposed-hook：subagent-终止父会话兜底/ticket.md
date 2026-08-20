---
id: TICKET-0063
type: ticket
title: disposed hook：subagent 终止父会话兜底
status: done
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.279Z'
updated_at: '2026-08-19T23:08:21.799Z'
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

## 完成记录（P2）

- 新增 src/host/disposed-hook.ts：监听 cordis `agent/disposed`。
- subagent 路径：header.origin==='subagent' 且 parentSession 可读 → 父会话存活时
  followup（唤醒 idle）兜底通知，含未完成 run/item 清单 + 最终报告摘要
  （best-effort 取会话事件流最后一条 assistant 文本，截断 500 字）；item 保持
  running 交父会话接管。父会话不在 → janitor 降级 item→interrupted。
- 主会话路径：名下 running run 有未完项 → core.janitorSweep（存活会话谓词，
  显式剔除已 dispose 的会话）；已终态 run 不受扰动。死会话 RunningRegistry
  标记同步清理。
- core 新增 executorItems(sessionId)（active runs 的执行者介入探测）。
- 测试：tests/disposed-hook.spec.ts 7 例（subagent 通知/降级/无介入/followup
  抛错包容；主会话 interrupted/旁观会话无影响/终态不动）。
