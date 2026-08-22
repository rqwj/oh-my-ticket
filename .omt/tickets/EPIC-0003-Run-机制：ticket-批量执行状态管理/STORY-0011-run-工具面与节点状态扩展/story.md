---
id: STORY-0011
type: story
title: run 工具面与节点状态扩展
status: done
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.782Z'
updated_at: '2026-08-19T14:46:38.428Z'
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

- [TICKET-0057 omt_run_create / list / show / control](TICKET-0057-omt_run_create-list-show-control/ticket.md) — done
- [TICKET-0058 omt_run_claim 原子认领](TICKET-0058-omt_run_claim-原子认领/ticket.md) — done
- [TICKET-0059 omt_run_report 结果表达](TICKET-0059-omt_run_report-结果表达/ticket.md) — done
- [TICKET-0060 节点 status 扩展 blocked/stopped/skipped](TICKET-0060-节点-status-扩展-blockedstoppedskipped/ticket.md) — done
- [TICKET-0061 被动观察推进 item 状态](TICKET-0061-被动观察推进-item-状态/ticket.md) — done
<!-- /omt:children -->


## 进度记录

- 2026-08-19 完成（commit 55f5046）：六个 run 工具 + 节点状态扩展（blocked/skipped）
  + 被动观察推进全部落地；31 个新测试，全量 173 通过，typecheck 干净。
- 实现期裁定：run id 跨 home 解析走 pool.coreForRun（workspace 优先，同
  coreForNode）；claim 只绑成员不触节点状态；blocked/skipped 与 done/archive
  一样清除 running 标记（执行已结束）；ITEM_TRANSITIONS pending 出口扩
  done/blocked（直改不僵死语义需要）。
- 遗留：awaiting_confirmation 分流属 TICKET-0064（P2）；skill.ts 的状态文案
  待 TICKET-0072 更新。
