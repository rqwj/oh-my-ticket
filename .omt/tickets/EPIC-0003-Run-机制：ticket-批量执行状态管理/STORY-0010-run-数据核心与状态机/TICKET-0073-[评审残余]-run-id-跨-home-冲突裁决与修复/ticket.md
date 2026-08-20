---
id: TICKET-0073
type: ticket
title: '[评审残余] run id 跨 home 冲突裁决与修复'
status: open
priority: 2
parent: STORY-0010
created_at: '2026-08-19T22:38:32.470Z'
updated_at: '2026-08-19T22:38:32.470Z'
---

## 背景（ce-code-review run 20260819-235553-fa534279 finding #13，经用户裁决落 tracker）

run id 按 home 分别计数（store.nextRunId 用 per-home meta 计数器），workspace `.omt`
与全局 home 可以同时存在 `RUN-0001`。`pool.coreForRun` 按调用方 cwd workspace-first
解析：从 mismatched cwd 寻址时会静默绑到另一个 home 的同名 run。ticket 节点 id 有
pool 级全局唯一保证（allocateId），run 没有。

## 任务

裁决并修复 run id 跨 home 冲突，两个候选方向：

1. **pool 级计数器**：nextRunId 像 allocateId 一样跨候选 home 同步 counter_RUN
2. **id 命名空间**：run id 携带 home 前缀，bare id 天然无歧义

## 验收

- 同一 cwd 下不会解析到别的 home 的 run
- 跨 home 同名 id 的回归测试（两个 home 各有 RUN-0001，从各自 cwd 解析）

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
