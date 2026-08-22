---
id: TICKET-0070
type: ticket
title: awaiting_confirmation 确认入口
status: done
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.394Z'
updated_at: '2026-08-20T00:14:00.297Z'
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


## 进度（P2 UI worker，2026-08-19）

- run 详情 awaiting_confirmation 行显示「确认完成 / 打回」按钮 → run-confirm RPC；确认 → item+ticket done（host），打回 → item interrupted、ticket 保持 in_progress（host 语义）。
- ticket 详情面板 run 链接 chip 上 awaiting_confirmation 项显示「待确认」警示徽章（get.runs 的 itemState 驱动，离开该状态即随 doc 刷新清除）。
- controller.runConfirm 成功后刷新 run 列表/详情 + 当前打开的该 ticket doc；冲突（非 awaiting 状态）以 error notice 呈现。
- 打回后的 retry 重跑路经 run 详情行级 retry（interrupted 可 retry）——组件测试覆盖确认/打回/重试按钮。浏览器测试以 jsdom 组件测试替代。

