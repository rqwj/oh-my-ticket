---
id: TICKET-0029
type: ticket
title: 显示开关行与优先级筛选
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-18T04:37:18.286Z'
updated_at: '2026-08-18T04:53:56.621Z'
---

## 描述

抽屉树显示控制增强。

## 实现要点

- 筛选栏新增优先级筛选 chips（P0–P3 四档多选，带信号条），归档 checkbox 改为同风格 icon 开关（📦 已归档）并置于行末
- 筛选栏下方新增显示开关行：显示编号（默认关）、排序三选（不排序/优先级降序/优先级升序，互斥）
- filterForest 扩展 priorities 白名单；新增 sortForest（按 priority 排序，递归子级，同级次关键字 id）

## 验收标准

- 优先级筛选与原类型/状态/关键词/归档叠加生效
- 排序三选互斥，即刻生效于所有层级
- 显示编号开关控制树行 id 显隐
- 归档开关与筛选 chips 视觉一致

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.14）：① 筛选栏新增 P0–P3 优先级筛选 chips（带信号条，多选）；"已归档"checkbox 改为 📦 图标开关（与筛选 chips 同风格）置于行末；② 新增显示开关行——# 编号（默认关，树行显示节点 id）+ 排序三选互斥（不排序/优先级降序/优先级升序，sortForest 递归生效、同级次关键字 id）。95/95 单测通过。
