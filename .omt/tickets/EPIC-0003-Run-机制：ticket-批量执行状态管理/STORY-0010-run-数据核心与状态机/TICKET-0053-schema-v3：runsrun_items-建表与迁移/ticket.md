---
id: TICKET-0053
type: ticket
title: schema v3：runs/run_items 建表与迁移
status: done
priority: 1
parent: STORY-0010
created_at: '2026-08-19T09:10:08.239Z'
updated_at: '2026-08-19T14:19:31.204Z'
---

## 任务

store.ts meta 版本 v2 → v3 迁移，新建两张表：

- `runs`：id / title(可选) / status / config(JSON) / created_at / finished_at
- `run_items`：run_id / node_id / position / state / executor_session_id /
  attempts / last_error / nudged_at / nudge_count(默认 0) / started_at / finished_at

config 含 stopOnFailure / autoContinue(默认 true) / autoVerify(默认 false) /
concurrency(预留，默认 1)——**无 order**（排序由 run_items.position 承载）。
run id 用 RUN-0001 序号（复用 meta 计数器）。title 供多 run 选择弹窗区分
目标（缺省回退 id）。nudged_at + nudge_count 供 idle hook 预算防抖使用
（TICKET-0062）。

## 验收

- 旧库打开自动迁移，数据无损；新库直接建 v3
- 单测覆盖迁移与建表

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
