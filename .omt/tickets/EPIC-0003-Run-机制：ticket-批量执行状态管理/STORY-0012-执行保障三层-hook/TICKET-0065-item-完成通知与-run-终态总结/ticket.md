---
id: TICKET-0065
type: ticket
title: item 完成通知与 run 终态总结
status: open
priority: 2
parent: STORY-0012
created_at: '2026-08-19T09:11:19.297Z'
updated_at: '2026-08-19T13:27:14.642Z'
---

## 任务

通知闭环：

1. **item 完成逐个通知**：run 中每个 ticket done/failed/blocked/skipped 时向执行会话
   inject 一条进度（「RUN-0003 进度 7/12：TICKET-0041 done」）——inject 仅会话
   running 时即时可见，item 进度属低优信息，不唤醒
2. **paused 待决通知**：run 因 stop-on-failure 转 paused 时向执行会话以
   **followup** 唤醒注入待决通知（失败项 + last_error 摘要 + resume/cancel
   选项）——执行会话此时往往 idle，inject 会滞留
3. **awaiting_confirmation 待确认提示**：item 进入 awaiting_confirmation 时向
   执行会话注入提示（item 标识 + 到 run 详情确认/打回的指引）——默认
   autoVerify=false 下没有它 run 会静默卡壳
4. **run 终态总结**：completed/completed_with_failures/canceled 时用 **followup**
   唤醒注入总结（各项结果 + 失败项 last_error 摘要）；**interrupted 终态不注入
   总结**（执行会话往往已销毁），走 UI 核对入口（TICKET-0068）

注意与 idle nudge 的时序协调（含 paused 与待确认通知），避免同一会话被连续
注入多条。

## 验收

- 四类通知的内容与时机
- interrupted 不注入的例外路径
- 与 nudge 合并/去重策略
- 单测

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
