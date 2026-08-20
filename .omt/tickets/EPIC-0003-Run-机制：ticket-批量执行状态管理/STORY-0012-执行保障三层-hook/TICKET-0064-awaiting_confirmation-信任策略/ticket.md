---
id: TICKET-0064
type: ticket
title: awaiting_confirmation 信任策略
status: done
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.289Z'
updated_at: '2026-08-19T23:05:20.649Z'
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

## 完成记录（P2）

- 机械分流落于 observeNodeStatus 的 done 分支：item running + 观察会话==执行者会话
  + 非 report 通道 + run.autoVerify=false → awaiting_confirmation；其余（report、
  非执行者会话、无会话、pending 项）直接 done。
- report 信号通道：UpdateInput 新增内部 `reported` 标志，reportRunItem 的双写
  update 携带 reported=true（结构上 item 已先 transition，双保险）。
- 确认：awaiting_confirmation 是 in-flight，omt_run_report done 直接确认完成；
  打回：ticket 重开 open → item interrupted（ITEM_TRANSITIONS 增加该出口）。
- 行为变更：默认 run 下执行者裸 done 不再直接落 done；受影响旧测试改用
  autoVerify=true（broadcast/paused/replay）或改写为信任门断言。
- 测试：tests/run-tools.spec.ts「TICKET-0064 trust policy」8 例 + 0061 更新 4 例。
