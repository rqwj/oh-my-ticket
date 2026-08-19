---
id: TICKET-0025
type: ticket
title: 执行中状态与执行会话展示
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-18T03:12:02.217Z'
updated_at: '2026-08-18T04:52:49.769Z'
---

## 描述

ticket 被某会话执行时，详情面板应立即显示运行状态及执行它的会话。

## 实现要点

- host RunningRegistry：start（update 置 in_progress / execute 端点）/stop（置 done 或归档）；get 响应携带 running {sessionId, sessionLabel, since}
- 新增 `/omt execute` RPC 端点：置 in_progress + running.start + 广播（SSE 即时刷新面板）——详情页"执行"按钮先调它再提交对话（注意事项 1：立即刷新）
- 模型经工具调用置 in_progress 时记录会话（注意事项 2：允许延迟，随工具调用到达）
- 面板显示"执行中 · 会话标识 · 开始时间"

## 验收标准

- 详情面板点执行后，运行状态与执行会话立即显示
- 模型对话中开始执行时（工具调用到达后）面板同步显示
- 完成/归档后运行标记消失

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.3）：RunningRegistry（start/stop/get）+ `/omt execute` 端点（置 in_progress + running 标记 + SSE 广播）+ 工具流 status 转换驱动 start/stop（模型路径，允许延迟）+ get 响应携带 running {sessionId, sessionLabel, since} + 面板"执行中 · 会话 · 开始时间"徽标。执行按钮先调 execute 再提交对话，面板即时刷新。84/84 单测通过。

- 2026-08-18 动效增强（v0.2.7）：执行中徽标应用与宿主 turn-status 同款的文字扫光动效（linear-gradient 中段浅色带 + background-clip:text + background-position 动画 1.8s 无限循环），含 prefers-reduced-motion 静态回退。
