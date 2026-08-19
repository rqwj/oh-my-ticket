---
id: TICKET-0028
type: ticket
title: 手动置进行中误标执行中
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-18T04:11:15.742Z'
updated_at: '2026-08-18T04:53:47.624Z'
---

## 描述

手动在面板把状态切换为"进行中"时，不应显示"执行中"徽标，也不应锁定面板操作。

## 根因

RPC `update` 端点在 status=in_progress 时无条件 `running.start`（v0.2.3 引入）——手动状态切换与会话执行共用同一端点，导致手动切换被误标记为执行中。

## 实现要点

- RPC update 只在 done/归档时清除 running 标记，不再 start
- running.start 仅保留两处：execute 端点（执行按钮）+ 工具流 trackRunning（模型调用）

## 验收标准

- 手动切换为进行中：无执行中徽标、操作不锁定
- 执行按钮/模型执行：徽标正常显示

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 修复完成（v0.2.11）：RPC update 不再在 status=in_progress 时打 running 标记（仅保留 done/归档时清除）；running.start 只剩 execute 端点（执行按钮）与工具流 trackRunning（模型调用）两处。93/93 单测通过（含手动切换不标记的回归用例）。
