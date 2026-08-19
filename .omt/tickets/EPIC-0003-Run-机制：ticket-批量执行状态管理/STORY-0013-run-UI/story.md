---
id: STORY-0013
type: story
title: run UI
status: open
priority: 2
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.810Z'
updated_at: '2026-08-19T09:38:06.768Z'
---

# Run UI

独立 run 视图 + 加入 run 入口 + 新状态呈现。**树节点上不加批次进度条**。

## 范围

- 树节点与详情面板「加入 run」按钮：收集当前节点+全部下级；目标 run 唯一时
  直接加入，多个活跃 run 时弹窗由用户选择；不做多选勾选
- run 视图：run 列表 + 详情（item 状态、执行者谱系「父会话 ↳ subagent」、
  进度、pause/resume/cancel/retry 操作）
- blocked/stopped/skipped 黄色呈现（徽章 + 状态点）
- awaiting_confirmation 确认入口
- SSE run 事件接入前端 store

详见 Epic 正文决策 4/6/15/16。