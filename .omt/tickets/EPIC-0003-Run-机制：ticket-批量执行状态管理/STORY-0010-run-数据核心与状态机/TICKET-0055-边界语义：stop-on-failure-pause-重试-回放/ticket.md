---
id: TICKET-0055
type: ticket
title: 边界语义：stop-on-failure / pause / 重试 / 回放
status: done
priority: 1
parent: STORY-0010
created_at: '2026-08-19T09:10:08.257Z'
updated_at: '2026-08-19T14:19:31.227Z'
---

## 任务

落实已确认的边界语义：

1. stop-on-failure：**仅 item 置 failed 触发**（blocked/skipped 不触发）→
   run 转 paused，pending 项保留，由人决定 resume/cancel
2. pause 只停止派发与续跑 nudge，已 running 的 item 继续观察推进
3. 重试：item 就地重置回 pending，attempts+1，last_error 保留（不建历史表）；
   适用 failed 与 interrupted 两种状态（及停滞 pending 项，见 TICKET-0068）；
   **retry 时清零 nudge_count 与 nudged_at**（新一轮执行获得新预算）
4. 回放：run 进行中 ticket **done/blocked/skipped → open** 均使对应
   item 回退 pending（保留 position）
5. resume 语义：paused → running 继续派发；**interrupted → running 恢复，仅
   pending 项重新可派发，interrupted item 不自动重置**（须经行级 retry）
6. cancel：pending/running item **冻结原位**并停止观察推进，不改动对应 ticket
   节点状态；canceled run 的 retry 不可用

## 验收

- 六条语义各有单测
- resume 跳过失败项继续、interrupted 项不自动重置的逻辑正确
- stop-on-failure 触发集合（仅 failed）有单测

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
