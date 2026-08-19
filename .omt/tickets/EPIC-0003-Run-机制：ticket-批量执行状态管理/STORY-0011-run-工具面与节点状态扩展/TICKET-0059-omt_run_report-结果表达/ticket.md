---
id: TICKET-0059
type: ticket
title: omt_run_report 结果表达
status: done
priority: 1
parent: STORY-0011
created_at: '2026-08-19T09:10:44.264Z'
updated_at: '2026-08-19T14:46:38.464Z'
---

## 任务

`omt_run_report(run_id, node_id, outcome, note)`：模型显式报告执行结果，
outcome ∈ done / failed / blocked / skipped。

- done：ticket 置 done + item 置 done
- **failed：节点 status 不变（保持 in_progress），仅 item 置 failed**——
  节点枚举无 failed（决策 4），note 记入 last_error 并 append 到 ticket 正文
- blocked/skipped：ticket status 置对应新状态 + item 同步 + note 同上
- 触发 stop-on-failure 判定（**仅 failed 触发**，见 TICKET-0055）

这是模型表达"做不下去/必须跳过"的合法词汇，替代含糊的 status 留白。

## 验收

- 双写一致：blocked/skipped 校验节点侧 + item 侧，failed 校验 item 侧
- note 追加到 ticket 进度记录
- 工具层单测

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
