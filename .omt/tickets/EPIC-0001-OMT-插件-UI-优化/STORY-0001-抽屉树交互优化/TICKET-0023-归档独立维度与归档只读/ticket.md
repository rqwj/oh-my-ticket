---
id: TICKET-0023
type: ticket
title: 归档独立维度与归档只读
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-18T02:38:59.541Z'
updated_at: '2026-08-18T02:59:33.090Z'
---

## 描述

归档与完成混在同一个 status 字段里，归档前的状态（未开始/进行中/已完成）信息丢失，显示上无法区分。且已归档节点在详情面板仍可改状态/编辑内容，不符合"归档即封存"语义。

## 实现要点

- 数据模型：`status` 收敛为 open/in_progress/done；新增独立 `archived` 布尔维度
- 迁移：旧库 status='archived' 的行 → archived=1 + status='open'（无损信息不可恢复，取安全默认）；schema_version 升 2
- 核心规则：已归档节点只读——除"恢复"（archived=false）外的任何变更被 core 拒绝
- UI：归档节点显示空心点+"已归档"标记；详情面板归档后状态下拉/追加/执行禁用，仅保留恢复
- `@` 候选继续排除归档（改按 archived 标志）；工具/RPC update 增加 archived 参数
- frontmatter 增加 archived 字段；reindex 同步

## 验收标准

- 归档节点能看到归档前的状态（未开始/进行中/已完成）+ 归档标记
- 已归档节点在详情面板无法进行状态调整和内容编辑
- 旧库数据迁移后正常

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.0，schema v2）：archived 独立维度落地——status 收敛为 open/in_progress/done，archived 布尔单列；旧库自动迁移（status='archived' → archived=1 + status='open' 安全默认）；归档节点 core 层只读（仅恢复可过）；详情面板归档后状态下拉/追加/执行禁用并显示"已归档"徽标，仅保留恢复；树/尾部/引用条的归档空心点改由 archived 标志驱动；`@` 候选按标志排除；frontmatter 增加 archived 字段，reindex 同步；skill 文案更新。79/79 单测通过（含 v1→v2 迁移用例）。
