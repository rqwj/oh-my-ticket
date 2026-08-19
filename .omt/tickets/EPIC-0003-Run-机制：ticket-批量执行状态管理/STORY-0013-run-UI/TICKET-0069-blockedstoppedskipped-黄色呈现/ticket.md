---
id: TICKET-0069
type: ticket
title: blocked/stopped/skipped 黄色呈现
status: open
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.383Z'
updated_at: '2026-08-19T13:41:57.750Z'
---

## 任务

blocked / skipped 两状态的黄色呈现：

- 状态点、徽章、过滤 chip 增加两状态，黄色系（对齐宿主语义 token，
  参考 STORY-0004 的 token 纪律；警示色系如 --dsw-alias-state-warning-*）
- 过滤器增加两状态选项
- 与 done（绿）/ in_progress（蓝）/ archived（空心）视觉层级协调
- （stopped 已移出枚举扩展，见 TICKET-0060）

## 验收

- 无字面色，全部走宿主 token
- 两状态在树/详情/过滤器一致呈现
- 窄视口与暗色主题检查

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
