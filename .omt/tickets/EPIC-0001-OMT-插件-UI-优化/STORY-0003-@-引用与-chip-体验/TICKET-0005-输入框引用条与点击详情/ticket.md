---
id: TICKET-0005
type: ticket
title: 输入框引用条与点击详情
status: done
archived: true
priority: 0
parent: STORY-0003
created_at: '2026-08-17T15:33:48.825Z'
updated_at: '2026-08-18T02:51:13.933Z'
---

## 描述

优化输入框中被 @ ticket 的呈现与交互。

## 背景约束（宿主）

- chip 是 textarea 背景层的固定 4em 单元格（一个 U+FFFC 占位符），文字必然裁切且不可点击；改变它需要上游 PR（每引用多个占位符/可变宽单元格）
- `conversation.input.dock` 的 owner（InputZone）暴露 `input.occurrences` 引用占位表——删除 chip/提交后自动同步

## 实现要点

- 在输入框上方 dock 增加"引用"条：从 `input.occurrences`（source === 'ticket'）实时取引用列表，显示完整标题 + 类型徽章 + 状态点
- 标题/状态经 controller 的摘要缓存（ensureSummaries 按需 RPC 补齐，改名后准确）
- 点击引用条中的 ticket → controller.select() 在右侧详情面板打开

## 验收标准

- 输入框被 @ 的 ticket 在引用条中显示完整标题
- 删除 chip 后引用条即时移除对应项
- 点击引用条 ticket → 右侧详情面板展示明细

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 实现完成（v0.1.6）：输入框上方 dock 新增"引用"条（ReferencedBar）——从 InputZone.input.occurrences 实时派生（source==='ticket'，去重），显示完整标题+类型徽章+状态点；摘要经 controller.summaries 缓存（ensureSummaries 按需 RPC 补齐）；点击 → controller.select() 打开右侧详情面板。chip 本体受宿主固定 4em 单元格与背景层不可点击约束，完整标题与点击交互由引用条承载。73/73 单测通过。
