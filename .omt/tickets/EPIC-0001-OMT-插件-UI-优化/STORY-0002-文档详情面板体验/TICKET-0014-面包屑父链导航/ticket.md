---
id: TICKET-0014
type: ticket
title: 面包屑父链导航
status: open
priority: 0
parent: STORY-0002
created_at: '2026-08-17T23:41:18.586Z'
updated_at: '2026-08-17T23:41:18.586Z'
---

## 描述

父链导航升级为完整面包屑，快速跳转到任意祖先。

## 实现要点

- host：RPC get 返回父链（ancestors 数组，自根到直接父节点；core 已有 parentOf 逐级可查）
- 面板顶部面包屑：EPIC › STORY › …，逐级可点击跳转（复用 select）
- 与现有父/子 chip 区整合，避免信息重复

## 验收标准

- 任意层级节点都能看到完整祖先链并逐级跳转
- 根节点不显示面包屑

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
