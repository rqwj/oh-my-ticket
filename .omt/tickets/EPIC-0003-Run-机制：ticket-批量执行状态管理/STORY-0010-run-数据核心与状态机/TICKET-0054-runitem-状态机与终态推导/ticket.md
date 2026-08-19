---
id: TICKET-0054
type: ticket
title: run/item 状态机与终态推导
status: open
priority: 1
parent: STORY-0010
created_at: '2026-08-19T09:10:08.248Z'
updated_at: '2026-08-19T10:20:29.937Z'
---

## 任务

实现 run 与 item 的状态机（store/core 层的合法流转校验）：

- run：pending → running → paused → completed / completed_with_failures /
  canceled / interrupted；**interrupted 可经 resume 回 running**（非绝对终态）
- item：pending / running / done / failed / blocked / skipped / interrupted /
  awaiting_confirmation
- **终态推导**：item 全部完结（done/skipped）→ completed；含 failed 或
  interrupted 项 → completed_with_failures（**interrupted 项计失败**）；
  canceled 仅由人主动取消产生

## 验收

- 非法流转被拒绝并报错
- 终态推导三种结果各有用例（含 interrupted 计失败）
- interrupted → resume → running 出口有单测
- 状态机单测覆盖全流转