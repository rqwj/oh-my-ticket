---
id: STORY-0012
type: story
title: 执行保障三层 hook
status: done
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.797Z'
updated_at: '2026-08-19T23:17:23.385Z'
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

- [TICKET-0062 idle hook：未收尾提醒与 run 续跑 nudge](TICKET-0062-idle-hook：未收尾提醒与-run-续跑-nudge/ticket.md) — done
- [TICKET-0063 disposed hook：subagent 终止父会话兜底](TICKET-0063-disposed-hook：subagent-终止父会话兜底/ticket.md) — done
- [TICKET-0064 awaiting_confirmation 信任策略](TICKET-0064-awaiting_confirmation-信任策略/ticket.md) — done
- [TICKET-0065 item 完成通知与 run 终态总结](TICKET-0065-item-完成通知与-run-终态总结/ticket.md) — done
- [TICKET-0066 RunningInfo 执行者谱系扩展](TICKET-0066-RunningInfo-执行者谱系扩展/ticket.md) — done
<!-- /omt:children -->


## 进度记录

- 2026-08-19 P2 host 侧完成（commit 4020ed7）：0063 disposed hook、0064 信任门、
  0065 通知闭环、0066 谱系全部落地；另补齐 P1 评审遗漏的 #4（janitor 活跃会话
  接线，pool provider 注入）与 #5（skill interrupted 文案）。33 个新测试，
  全量 239 通过。
- 设计要点：0064 用显式 reported 标志做分流；0065 同 tick 同会话通知合批、
  wake 优先整批 followup；janitor 取 provider 注入而非延后钩子。
