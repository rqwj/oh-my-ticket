---
id: TICKET-0064
type: ticket
title: awaiting_confirmation 信任策略
status: open
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.289Z'
updated_at: '2026-08-19T13:41:57.785Z'
---

## 任务

信任策略：hook 触发的被动判断不直接落 done。

- 默认（autoVerify=false）：模型在 hook 提醒后声明完成 → item 置
  `awaiting_confirmation`，UI 一键确认/打回；打回 → interrupted
- autoVerify=true：声明完成直接 done
- 无响应/含糊 → 保持 interrupted，绝不自动 done
- **机械分流标准**：item 处于 running 且本次 done 由该 executor 会话在未先经
  omt_run_report 的情况下直接 omt_update 落 done → 进 awaiting_confirmation；
  经 omt_run_report done 的视为显式报告、直接落 done。接受理由：模型绕过
  （先 report 或等待时机）的成本高于如实报告，分流抓住的是「沉默收尾」主路径
- 模型主动 omt_update 的正常路径不受影响（非 run 成员的 ticket 不变）

## 验收

- 三种信任路径 + 分流标准单测
- awaiting_confirmation 可被人/UI 确认与打回

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
