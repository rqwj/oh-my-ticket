---
id: TICKET-0026
type: ticket
title: 执行中锁定面板操作
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-18T03:55:30.512Z'
updated_at: '2026-08-18T04:53:16.055Z'
---

## 描述

ticket 处于"执行中"状态时，详情面板的执行/归档/追加进度应为不可操作，避免执行期间的冲突操作。

## 实现要点

- `doc.data.running` 存在即锁定：执行、归档、追加（textarea + 按钮）禁用
- 禁用提示（title）说明"执行中，完成后可操作"
- 与归档只读规则叠加（archived || running 均禁用）

## 验收标准

- 执行中三个操作禁用且有提示
- 执行结束（done/归档）后恢复可操作

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.9）：执行中（doc.data.running 存在）锁定执行/归档/追加进度三个操作（禁用 + "执行中，完成后可操作"提示），与归档只读规则叠加；恢复按钮在执行中不锁定已归档语义不变（执行中的节点不会被归档）。89/89 单测通过。
