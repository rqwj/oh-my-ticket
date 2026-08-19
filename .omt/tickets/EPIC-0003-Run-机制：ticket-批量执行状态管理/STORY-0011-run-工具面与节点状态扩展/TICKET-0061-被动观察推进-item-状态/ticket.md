---
id: TICKET-0061
type: ticket
title: 被动观察推进 item 状态
status: done
priority: 1
parent: STORY-0011
created_at: '2026-08-19T09:10:44.281Z'
updated_at: '2026-08-19T14:46:38.485Z'
---

## 任务

被动观察推进：复用现有信号（execute RPC、工具 status 流转）驱动 run_items：

- run 激活期间 item 对应 ticket 置 in_progress → item running（记录 executor）
- 置 done → item done；归档 → item skipped
- **omt_update 直置 blocked/skipped 时 item 同步置对应状态**（绕开 report 的
  合法路径不得使 run 僵死）
- run paused 时 running item 照常观察推进，仅停止新项派发与续跑 nudge
  （对齐决策 9 与 TICKET-0055）
- **claim 优先**：已被 omt_run_claim 认领的 item，被动观察照常推进状态，
  但**不覆盖 executor_session_id**（归属保持 claim 者）
- **跨 run 广播**：ticket 属多个活跃 run 时，状态推进广播到所有持有它的
  活跃 run 的对应 item（见 Epic 决策 1）
- 手动 status 变更不启动 running 标记的现有约定（TICKET-0028）保持

## 验收

- 各信号源 → item 状态映射单测（含 blocked/skipped 直改路径）
- paused 期间 running item 照常推进、无新派发的行为有单测
- claim 后手动操作不覆盖 executor 归属的单测
- 跨 run 广播推进的单测

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
