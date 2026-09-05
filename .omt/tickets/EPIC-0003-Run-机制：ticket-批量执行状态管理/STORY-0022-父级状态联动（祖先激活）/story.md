---
id: STORY-0022
type: story
title: 父级状态联动（祖先激活）
status: done
priority: 0
parent: EPIC-0003
created_at: '2026-08-22T00:09:42.435Z'
updated_at: '2026-08-22T00:21:01.426Z'
---

## 能力结果
ticket 无论单个执行还是 run 批量执行，其所在祖先链自动进入执行中状态，用户与代理无需手动维护父级进度。

## 使用者
- 手动推进 ticket 的用户和代理
- 通过 run 批量执行 ticket 的代理

## 范围
- ticket/subticket 置为 in_progress 时级联激活全部祖先
- omt_run_claim 成功时兜底激活祖先，不依赖执行者纪律
- 仅 open → in_progress；done/blocked/skipped 永不重开
- 归档祖先静默跳过，不影响 ticket 更新成功

## 非范围
- 祖先自动汇总为 done/blocked/skipped
- ticket 回退 open 时降级祖先
- 修改 run item 状态机或信任门

## 验收标准
- 单个执行与 run 认领两条路径都能点亮 Epic/Story/SubStory/父 Ticket
- 非 open 与归档祖先保持原状且不报错
- 相关测试、类型检查、构建通过

<!-- omt:children -->
## 子节点

- [TICKET-0090 实现祖先激活级联与 claim 兜底](TICKET-0090-实现祖先激活级联与-claim-兜底/ticket.md) — done
<!-- /omt:children -->

能力已交付：TICKET-0090 完成祖先激活级联与 claim 兜底，单个执行与批量执行两条路径均验证通过。
