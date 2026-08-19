---
id: TICKET-0070
type: ticket
title: awaiting_confirmation 确认入口
status: open
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.394Z'
updated_at: '2026-08-19T13:27:14.652Z'
---

## 任务

awaiting_confirmation 的人工确认入口：

- run 详情中 awaiting_confirmation 项显示「确认完成 / 打回」操作
- 确认 → item done + ticket done
- 打回 → item interrupted，**ticket status 保持 in_progress**（对齐 TICKET-0059
  failed 语义），详情面板待确认标识随 item 离开 awaiting_confirmation 即清除；
  可经 run 详情的 retry（行级）重置重跑（见 TICKET-0068）
- ticket 详情面板同步显示该 ticket 的待确认状态

## 验收

- 确认/打回两条路径状态正确联动
- 打回后 ticket 保持 in_progress、标识清除、retry 重跑路径可走通
- 浏览器测试

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
