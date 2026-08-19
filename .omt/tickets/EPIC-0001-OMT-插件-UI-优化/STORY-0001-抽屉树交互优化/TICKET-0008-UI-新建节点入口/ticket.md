---
id: TICKET-0008
type: ticket
title: UI 新建节点入口
status: open
priority: 0
parent: STORY-0001
created_at: '2026-08-17T23:31:31.438Z'
updated_at: '2026-08-17T23:31:31.438Z'
---

## 描述

抽屉树提供 UI 新建入口，不再只能由模型通过工具创建。

## 实现要点

- host 新增 `create` RPC 端点（zod 校验 type/title/parentId，复用 pool 路由与 id 分配）
- 树头部加"新建 Epic"按钮；节点行悬停加"+"按钮，按层级规则只提供合法子类型（story→substory|ticket 等）
- 新建弹出轻量表单（标题 + 正文可选）；成功后 SSE 推送自动刷新并选中新节点
- 空态引导中也放"新建 Epic"入口

## 验收标准

- 可从 UI 创建 Epic 及各层合法子节点
- 非法子类型不出现在入口中
- 创建后树自动刷新并定位到新节点

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
