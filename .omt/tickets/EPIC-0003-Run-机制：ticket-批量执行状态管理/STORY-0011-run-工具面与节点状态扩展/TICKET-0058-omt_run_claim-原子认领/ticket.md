---
id: TICKET-0058
type: ticket
title: omt_run_claim 原子认领
status: done
priority: 1
parent: STORY-0011
created_at: '2026-08-19T09:10:44.253Z'
updated_at: '2026-08-19T14:46:38.453Z'
---

## 任务

`omt_run_claim(run_id)`：原子认领下一个 pending item——单事务内置 running
并记录 executor_session_id，返回该 item 的 ticket 摘要。

为 P3 多 subagent 并行执行铺路：两个执行者并发 claim 不会拿到同一项。
run paused 时拒绝 claim。**executor 归属遵循 claim 优先**：claim 写入后
不被被动观察覆盖（见 TICKET-0061）。

## 验收

- 并发 claim 单测（同一 run 两个调用拿到不同 item）
- 空队列返回明确信号

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
