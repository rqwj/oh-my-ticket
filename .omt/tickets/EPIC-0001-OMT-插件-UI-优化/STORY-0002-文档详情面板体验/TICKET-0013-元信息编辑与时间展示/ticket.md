---
id: TICKET-0013
type: ticket
title: 元信息编辑与时间展示
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-17T23:40:53.520Z'
updated_at: '2026-08-18T04:18:14.280Z'
---

## 描述

元信息完善：标题重命名、优先级编辑、时间戳展示。

## 实现要点

- host：RPC update 端点补 `title`/`priority` 参数（core 均已支持）
- 标题点击可编辑（inline input，回车保存/Esc 取消）
- 优先级用数字输入或 0–3 档选择器
- created_at/updated_at 以相对时间展示（如"3 分钟前"），悬停显示完整时间
- 标题变更后树/引用条/状态条同步（SSE 已覆盖）

## 验收标准

- 标题、优先级可在面板内直接修改
- 时间戳以相对时间 + 悬停完整时间呈现
- 变更即时反映到所有视图

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.8）：RPC update 补 title/priority 参数（返回摘要同步补 priority）；面板标题点击内联编辑（Enter 保存/Esc 或失焦取消，归档禁用）；优先级 P0–P3 下拉（状态下拉旁，归档禁用）；时间戳行"创建于 x 前 · 更新于 x 前"（formatRelative 工具函数，悬停 title 显示完整 ISO）。变更经 SSE 同步到树/引用条/状态条。89/89 单测通过。
