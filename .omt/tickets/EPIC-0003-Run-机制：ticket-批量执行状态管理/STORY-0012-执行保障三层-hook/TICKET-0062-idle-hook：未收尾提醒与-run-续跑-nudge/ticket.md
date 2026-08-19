---
id: TICKET-0062
type: ticket
title: idle hook：未收尾提醒与 run 续跑 nudge
status: open
priority: 1
parent: STORY-0012
created_at: '2026-08-19T09:11:19.261Z'
updated_at: '2026-08-19T13:26:14.851Z'
---

## 任务

Tier 1 hook：监听 cordis `agent/status` 事件，agent 转 idle 时：

1. **未收尾提醒**：该 session 名下有 running ticket（未 done）→ followup 注入
   「TICKET-xxxx 仍标记执行中，请 omt_update 收尾或说明进度」
2. **run 续跑 nudge**：该 session 是 **running 状态** run 的执行者且 run 有
   pending 项 → followup 注入「继续下一项 TICKET-yyyy」（autoContinue 默认 true；
   不用「活跃」一词——paused 的 run 不续跑，对齐决策 9）

**nudge 预算（停滞升级路径）**：同一 item 的续跑 nudge 最多 3 次（指数退避
间隔；item 记录 nudged_at 与 nudge_count，见 TICKET-0053 schema）。
预算耗尽后不再 nudge，item 在 run 详情标记为**停滞**（UI 显著提示人工介入，
停滞项可经 retry 重置重跑，见 TICKET-0068），不自动降级 interrupted。
常规防循环：每 ticket 每 session 的未收尾提醒仍只一次。

## 验收

- 两类注入的时机与内容正确；paused 的 run 不触发续跑 nudge
- 预算内退避重试、耗尽转 UI 停滞标记的行为有单测
- autoContinue=false 时只提醒不续跑
- 单测（mock agent 事件与 followup）