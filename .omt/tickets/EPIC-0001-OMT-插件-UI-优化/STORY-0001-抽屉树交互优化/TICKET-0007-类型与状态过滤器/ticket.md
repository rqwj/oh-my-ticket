---
id: TICKET-0007
type: ticket
title: 类型与状态过滤器
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-17T23:31:31.423Z'
updated_at: '2026-08-18T03:43:11.549Z'
---

## 描述

抽屉树在关键词搜索之外，增加类型与状态两个维度的过滤器。

## 实现要点

- 搜索框下方/侧边加两组过滤 chip：类型（E/S/SS/T/ST 多选）+ 状态（open/in_progress/done；archived 由现有 checkbox 控制）
- 与关键词搜索、归档开关叠加生效（filterForest 扩展 filter 参数）
- 过滤条件为组件本地查看状态即可

## 验收标准

- 类型/状态可多选过滤，与关键词搜索叠加
- 过滤后保留匹配节点的祖先链
- 默认不过滤（全部类型、全部未归档状态）

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.6）：搜索框下方新增两组过滤 chip——类型（E/S/SS/T/ST 多选，彩色字母）+ 状态（未开始/进行中/已完成，带状态点）；与关键词搜索、归档开关叠加（filterForest 扩展 types/statuses 白名单参数）；未激活 chip 半透明显示；过滤保留祖先链。86/86 单测通过。
