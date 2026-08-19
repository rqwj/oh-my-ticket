---
id: STORY-0011
type: story
title: run 工具面与节点状态扩展
status: open
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.782Z'
updated_at: '2026-08-19T09:09:31.782Z'
---

# Run 工具面与节点状态扩展

模型侧全部新工具，加上节点 status 扩展与被动观察推进。

## 范围

- omt_run_create / list / show / control（pause/resume/cancel/retry）
- omt_run_claim：原子认领下一 pending 项（SQLite 事务），P3 并行基础
- omt_run_report：done/failed/blocked/skipped + note 的显式表达
- 节点 status 扩展 blocked/stopped/skipped（store 校验 + omt_update schema）
- 被动观察：execute RPC 与工具 status 流转 → item 推进；尊重 run paused

详见 Epic 正文决策 4/14。

<!-- omt:children -->
## 子节点

- [TICKET-0057 omt_run_create / list / show / control](TICKET-0057-omt_run_create-list-show-control/ticket.md) — open
- [TICKET-0058 omt_run_claim 原子认领](TICKET-0058-omt_run_claim-原子认领/ticket.md) — open
- [TICKET-0059 omt_run_report 结果表达](TICKET-0059-omt_run_report-结果表达/ticket.md) — open
- [TICKET-0060 节点 status 扩展 blocked/stopped/skipped](TICKET-0060-节点-status-扩展-blockedstoppedskipped/ticket.md) — open
- [TICKET-0061 被动观察推进 item 状态](TICKET-0061-被动观察推进-item-状态/ticket.md) — open
<!-- /omt:children -->
