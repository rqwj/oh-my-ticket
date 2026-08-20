---
id: TICKET-0057
type: ticket
title: omt_run_create / list / show / control
status: done
priority: 1
parent: STORY-0011
created_at: '2026-08-19T09:10:44.241Z'
updated_at: '2026-08-19T14:46:38.441Z'
---

## 任务

新增 run 管理工具（路由到 item 所在 home 的 core）：

- `omt_run_create`：接受任意 ticket id 列表（可跨 Story/Epic），创建时快照
  排序写入 run_items；校验全部 item 同 home；config 可覆盖默认值；接受可选 title
- `omt_run_list`：按状态过滤列出 run（含进度统计）
- `omt_run_show`：run 详情 + item 清单（状态/执行者/attempts/last_error）
- `omt_run_control`：**start（pending → running）** / pause / resume / cancel /
  retry（node_id 指定重试项）/ **remove（node_id 指定移除项，item 级移除，
  不改动对应 ticket 节点状态）**

## 验收

- 同 home 校验、重复成员校验
- control 各动作状态流转合法（含 start：pending → running；remove 不碰 ticket）
- 工具层单测

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
