---
id: TICKET-0072
type: ticket
title: omt / omt-runs skill 拆分与内容编写
status: done
priority: 1
parent: STORY-0014
created_at: '2026-08-19T09:12:00.415Z'
updated_at: '2026-08-19T15:19:21.666Z'
---

## 任务

按拆分设计落地（内容见 Story 正文设计要点与会话讨论）：

1. `omt` skill：现有内容基本不变，末尾加一行「批量执行 / run 续跑 /
   结果报告场景，加载 omt-runs skill」
2. 新增 `omt-runs` skill 注册：run 概念模型（快照/状态机/终态）、
   run 工具族用法、report 四词汇、claim、收到 nudge 的行为约定、
   信任策略（awaiting_confirmation 时该怎么响应）
3. description 路由词覆盖 run / 批量 / 续跑 / 跳过；两 skill 均
   ctx.skills.register() 注册

## 验收

- catalog 中两 skill 独立可见、路由描述准确
- skill.spec.ts 更新
- 内容经过一轮评审（ce-skill-work 规范）

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
