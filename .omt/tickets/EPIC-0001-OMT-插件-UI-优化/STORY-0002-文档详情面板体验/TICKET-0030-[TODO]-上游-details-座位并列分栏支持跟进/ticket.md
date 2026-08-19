---
id: TICKET-0030
type: ticket
title: '[TODO] 上游 details 座位并列/分栏支持跟进'
status: open
priority: 0
parent: STORY-0002
created_at: '2026-08-18T04:56:10.629Z'
updated_at: '2026-08-18T04:56:10.629Z'
---

## 描述

上游议题跟进：DSH 的 details 座位目前是 single 型（遮蔽语义），导致 OMT 文档与工具详情无法并列共存。当前以 TICKET-0017 方案②（点击工具行时 OMT 单向让位）过渡。

## 跟进内容（等待上游变化）

- 向 DSH 提 PR/议题：details 座位改 list 型（可并列添加第二个右侧面板），或支持右侧分栏
- 相关上游议题备选：turnTail 链式改 stack 语义（多贡献共存）；chip tooltip 与 label 分离；`#` 触发字符；可变宽 chip
- 上游落地后：移除单向让位逻辑，改为并列面板或分栏

## 验收标准

- 上游有对应 PR/议题链接记录
- 上游支持后完成迁移

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
