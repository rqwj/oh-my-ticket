---
id: STORY-0010
type: story
title: run 数据核心与状态机
status: open
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.765Z'
updated_at: '2026-08-19T09:09:31.765Z'
---

# Run 数据核心与状态机

runs / run_items 两张表的落地（meta 版本 v2 → v3 迁移），状态机与全部边界语义。

## 范围

- 建表 + 迁移；run 归属单一 home（创建校验 item 同 home）
- run 状态机：pending → running → paused → completed / completed_with_failures /
  canceled / interrupted（无 failed）
- item 状态机：pending / running / done / failed / blocked / skipped / interrupted /
  awaiting_confirmation
- 边界：stop-on-failure→paused 保留 pending；pause 只停派发；重试就地重置
  （attempts+1、last_error 保留）；done→open 回放 pending
- 启动 janitor + reindex 保护

详见 Epic 正文决策 2/7/8/9/10/11/12/13。

<!-- omt:children -->
## 子节点

- [TICKET-0053 schema v3：runs/run_items 建表与迁移](TICKET-0053-schema-v3：runsrun_items-建表与迁移/ticket.md) — open
- [TICKET-0054 run/item 状态机与终态推导](TICKET-0054-runitem-状态机与终态推导/ticket.md) — open
- [TICKET-0055 边界语义：stop-on-failure / pause / 重试 / 回放](TICKET-0055-边界语义：stop-on-failure-pause-重试-回放/ticket.md) — open
- [TICKET-0056 启动 janitor 与 reindex 保护](TICKET-0056-启动-janitor-与-reindex-保护/ticket.md) — open
<!-- /omt:children -->
