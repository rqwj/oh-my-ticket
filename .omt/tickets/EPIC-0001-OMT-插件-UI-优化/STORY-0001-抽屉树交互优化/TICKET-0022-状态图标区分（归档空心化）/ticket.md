---
id: TICKET-0022
type: ticket
title: 状态图标区分（归档空心化）
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-18T02:33:19.633Z'
updated_at: '2026-08-18T02:59:18.556Z'
---

## 描述

"未开始"（实心灰点）与"已归档"（实心暗灰点）状态图标颜色过于接近，难以区分。

## 实现要点

- 形状语言区分（色盲友好）：open 保持实心点（微调浅灰蓝 #9aa3b2），archived 改空心圆点（透明填充 + 灰描边）
- 涉及四处样式：Drawer / DocPanel / TurnTickets / ReferencedBar 的 status_* 类
- `@` 候选 emoji：archived ⚫ → 📦

## 验收标准

- 未开始与已归档在所有视图（树/详情/轮次尾部/引用条/候选菜单）一眼可辨

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.1.14）：采用形状语言区分——open 实心浅灰蓝 #9aa3b2，archived 改空心圆点（透明填充+灰描边，色盲友好且小尺寸可辨）；`@` 候选 archived 图标 ⚫→📦。四处样式（Drawer/DocPanel/TurnTickets/ReferencedBar）同步更新。75/75 单测通过。
