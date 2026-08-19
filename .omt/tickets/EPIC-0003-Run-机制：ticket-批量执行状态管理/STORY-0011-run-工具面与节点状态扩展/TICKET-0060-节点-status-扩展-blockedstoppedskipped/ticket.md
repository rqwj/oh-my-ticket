---
id: TICKET-0060
type: ticket
title: 节点 status 扩展 blocked/stopped/skipped
status: done
priority: 1
parent: STORY-0011
created_at: '2026-08-19T09:10:44.272Z'
updated_at: '2026-08-19T14:46:38.475Z'
---

## 任务

节点 status 枚举扩展：`open / in_progress / done` + `blocked / skipped`。

- store 层校验、omt_update schema、类型定义同步扩展
- skipped 语义：必须跳过的场景，或人为主动跳过 ticket 执行
- blocked 语义：因外部条件做不下去（等待上游/依赖），解除后可恢复
- **stopped 已移出本次扩展**（round-2 评审裁决：语义空洞——无产生路径、
  无 item 映射；skipped 覆盖人为跳过、blocked 覆盖做不下去）
- 与 archived 维度保持正交；done/archive 清理 running 标记的现有约定不变
- UI 黄色呈现在 STORY-0013

## 验收

- omt_update 接受新状态；非法状态仍被拒绝
- omt_list 按新状态过滤正常
- 单测更新

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
