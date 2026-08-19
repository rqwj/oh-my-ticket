---
id: TICKET-0056
type: ticket
title: 启动 janitor 与 reindex 保护
status: open
priority: 1
parent: STORY-0010
created_at: '2026-08-19T09:10:08.266Z'
updated_at: '2026-08-19T09:49:53.695Z'
---

## 任务

1. 启动 janitor：DSH 启动时扫描，无活跃执行者的 running run/item → interrupted
   （run 级与 item 级分别处理）
2. reindex 保护：omt_reindex 重建索引时显式排除 runs/run_items 表

## 验收

- 模拟崩溃现场（running 残留）重启后全部降级 interrupted
- reindex 后 run 数据完整保留
- 单测覆盖

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
