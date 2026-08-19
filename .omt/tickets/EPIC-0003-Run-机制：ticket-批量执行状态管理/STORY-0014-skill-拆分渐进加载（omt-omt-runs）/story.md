---
id: STORY-0014
type: story
title: skill 拆分渐进加载（omt / omt-runs）
status: open
priority: 1
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.823Z'
updated_at: '2026-08-19T10:47:58.198Z'
---

# skill 拆分渐进加载

run 机制使工具面从 6 个涨到 ~12 个，全部塞进单个 omt skill 会让每次 ticket
操作都背负 run 的上下文成本。拆分为 omt（核心）+ omt-runs（批量执行）两个
内嵌 skill，利用 DSH skill catalog 天然实现渐进加载。

## 设计要点

- omt：现有内容基本不变，仅加一行指引（批量执行/续跑场景加载 omt-runs）
- omt-runs：run 概念模型、run 工具族、report 词汇、claim、nudge 行为约定、
  信任策略；description 路由词覆盖 run/批量/续跑/跳过
- 两个 skill 都经 ctx.skills.register() 注册，source: 'runtime'

详见 Epic 正文及会话中 skill 设计讨论。

<!-- omt:children -->
## 子节点

- [TICKET-0072 omt / omt-runs skill 拆分与内容编写](TICKET-0072-omt-omt-runs-skill-拆分与内容编写/ticket.md) — open
<!-- /omt:children -->
