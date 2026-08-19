---
id: STORY-0012
type: story
title: 执行保障三层 hook
status: open
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.797Z'
updated_at: '2026-08-19T10:47:58.175Z'
---

# 执行保障三层 hook

done 可靠性的核心机制。挂点选择原则：执行现场由近到远。

## 范围

- Tier 1：agent/status → idle，同会话 followup（未收尾提醒 + run 续跑 nudge，
  autoContinue 默认开，每 ticket 每 session 防抖一次）
- Tier 2：agent/disposed，subagent 终止 → 父会话 followup 兜底（inject 不唤醒
  idle 会话）；父也不在 → interrupted
- Tier 3：重启恢复（running → interrupted）+ UI 核对入口
- 信任策略 awaiting_confirmation（默认 propose，autoVerify 可放开）
- 通知：item 完成逐个通知执行会话；run 终态总结
- executor 谱系：RunningInfo 扩展 parentSessionId/isSubagent

详见 Epic 正文决策 3/5/6/16。

<!-- omt:children -->
## 子节点

- [TICKET-0062 idle hook：未收尾提醒与 run 续跑 nudge](TICKET-0062-idle-hook：未收尾提醒与-run-续跑-nudge/ticket.md) — in_progress
- [TICKET-0063 disposed hook：subagent 终止父会话兜底](TICKET-0063-disposed-hook：subagent-终止父会话兜底/ticket.md) — open
- [TICKET-0064 awaiting_confirmation 信任策略](TICKET-0064-awaiting_confirmation-信任策略/ticket.md) — open
- [TICKET-0065 item 完成通知与 run 终态总结](TICKET-0065-item-完成通知与-run-终态总结/ticket.md) — open
- [TICKET-0066 RunningInfo 执行者谱系扩展](TICKET-0066-RunningInfo-执行者谱系扩展/ticket.md) — open
<!-- /omt:children -->
