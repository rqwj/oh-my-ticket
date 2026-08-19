---
id: TICKET-0017
type: ticket
title: OMT 文档与工具详情共存策略
status: done
priority: 0
parent: STORY-0002
created_at: '2026-08-17T23:41:43.940Z'
updated_at: '2026-08-18T04:56:27.690Z'
---

## 描述

消除 OMT 文档与原工具详情面板的互斥：当前遮蔽语义下，打开 OMT 文档时无法查看工具调用详情。

## 实现要点

- 方案评估：① OMT 面板内嵌 tab（OMT 文档 / 工具详情）——但工具详情内容由 ui-conversation 的 DetailsPanel 拥有，外部无法复用其内部组件；② 打开工具详情时 OMT 自动收起（单向让位，代价小）；③ 上游 PR：details 座位改 list 型或支持分栏
- 推荐先做 ②（单向让位），把 ③ 记入上游议题清单

## 验收标准

- 用户查看工具详情后能快速回到 OMT 文档（或反之），无需手工翻树
- 互斥行为有明确设计文档/注释

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 方案②实现完成（v0.2.16）：单向让位——文档遮蔽激活时，捕获阶段监听工具行点击（data-chat-call-id），dispose 遮蔽让原面板显示工具详情，details 列保持展开；经激活状态条/树可一键回到 OMT 文档。TODO 项（方案③上游跟进）见 TICKET-0030。97/97 单测通过。
