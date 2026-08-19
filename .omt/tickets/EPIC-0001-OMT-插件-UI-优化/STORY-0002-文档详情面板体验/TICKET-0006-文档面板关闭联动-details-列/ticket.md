---
id: TICKET-0006
type: ticket
title: 文档面板关闭联动 details 列
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-17T15:52:40.331Z'
updated_at: '2026-08-18T02:50:24.536Z'
---

## 描述

关闭 OMT 文档面板时，右侧 details 面板应同时关闭，不要残留单独显示的原工具详情面板。

## 实现要点

`OmtController.closeDoc()` 目前只 dispose 遮蔽注册，缺少 `layout.closeDetails()`——补上即可。

## 验收标准

- 文档面板点 × 后，整个右侧 details 列关闭
- 再次打开 ticket 详情时面板正常展开

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 实现完成（v0.1.8）：closeDoc 补 layout.closeDetails()——关闭文档面板时右侧 details 列同步关闭，不再残留原工具详情面板。73/73 单测通过（含关闭联动断言）。
