---
id: TICKET-0004
type: ticket
title: 归档操作与树过滤
status: done
archived: true
priority: 0
parent: STORY-0001
created_at: '2026-08-17T15:18:58.199Z'
updated_at: '2026-08-18T02:49:58.900Z'
---

## 描述

抽屉树交互增强四件套。

## 验收标准

1. 树节点行有归档按钮（点击置为 archived，SSE 推送后树自动刷新）
2. 抽屉顶部有搜索框，按 id/标题关键词过滤（保留匹配节点的祖先链）
3. 搜索框旁有"显示已归档"checkbox，默认不勾选（不显示归档节点）
4. 聊天框 `@` 候选不显示已归档的 ticket

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-17 实现完成（v0.1.5）：① 树行悬停显示归档按钮（⛁，点击置 archived，SSE 推送即时刷新）② 抽屉顶部搜索框（id/标题关键词过滤，保留祖先链，filterForest 纯函数）③ 搜索框旁"已归档"checkbox（默认不勾选即隐藏归档节点，含其整个子树）④ `@` 候选排除 archived。70/70 单测通过。
