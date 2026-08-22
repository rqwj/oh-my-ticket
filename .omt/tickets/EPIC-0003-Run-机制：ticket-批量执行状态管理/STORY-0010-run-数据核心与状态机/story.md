---
id: STORY-0010
type: story
title: run 数据核心与状态机
status: done
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.765Z'
updated_at: '2026-08-19T14:19:31.188Z'
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

- [TICKET-0053 schema v3：runs/run_items 建表与迁移](TICKET-0053-schema-v3：runsrun_items-建表与迁移/ticket.md) — done
- [TICKET-0054 run/item 状态机与终态推导](TICKET-0054-runitem-状态机与终态推导/ticket.md) — done
- [TICKET-0055 边界语义：stop-on-failure / pause / 重试 / 回放](TICKET-0055-边界语义：stop-on-failure-pause-重试-回放/ticket.md) — done
- [TICKET-0056 启动 janitor 与 reindex 保护](TICKET-0056-启动-janitor-与-reindex-保护/ticket.md) — done
- [TICKET-0073 [评审残余] run id 跨 home 冲突裁决与修复](TICKET-0073-[评审残余]-run-id-跨-home-冲突裁决与修复/ticket.md) — open
<!-- /omt:children -->


## 进度记录

- 2026-08-19 完成（commit 77c1e47，分支 feat/run-mechanism-p1）：schema v3 迁移、
  run/item 状态机、六条边界语义、janitor + reindex 保护全部落地；27 个新测试，
  全量 142 通过，typecheck 干净。
- 实现期裁定（worker 报告，orchestrator 确认）：blocked 在终态推导中计失败
  （决策 7 未指明，取保守读法）；stopOnFailure 默认 false；retry 可将
  completed_with_failures 重新打开为 running；空 run start 即 completed。
- 遗留：janitor 启动时序（插件加载时无活跃会话注册，全部降级为保守 interrupted，
  靠 resume/retry 恢复）→ 交 STORY-0012 接入会话注册表后改进；RUN id 按 home
  分别计数，跨 home 解析规则交 STORY-0011。
