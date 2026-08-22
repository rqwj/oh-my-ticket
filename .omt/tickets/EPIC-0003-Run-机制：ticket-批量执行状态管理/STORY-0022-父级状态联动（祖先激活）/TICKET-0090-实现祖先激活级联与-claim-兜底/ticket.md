---
id: TICKET-0090
type: ticket
title: 实现祖先激活级联与 claim 兜底
status: done
priority: 0
parent: STORY-0022
created_at: '2026-08-22T00:09:56.051Z'
updated_at: '2026-08-22T00:20:51.186Z'
---

## 交付结果
实现祖先激活级联：ticket/subticket 进入 in_progress（任意路径）或 run claim 成功时，把整条祖先链中仍为 open 的节点升级为 in_progress。

## 工作范围
- core.update：状态置 in_progress 后沿 parentOf 链激活 open 祖先
- claimRunItem：认领成功后兜底执行同一激活（失败不阻断认领）
- 仅 open → in_progress；done/blocked/skipped 不动；归档祖先静默跳过
- 复用既有 dual-write、父子块再生与 SSE 事件路径
- 更新 omt / omt-runs skill 与 omt_run_claim 描述
- 新增 core 级与工具级回归测试

## 依赖
- STORY-0017 已交付的层级语义（祖先链只读背景）与本 ticket 的状态写入互不冲突

## 验收标准
- 单个执行与 run 认领都能点亮 Epic/Story/SubStory/父 Ticket
- 非 open 与归档祖先保持原状且不报错
- 回退/打回不降级祖先
- 全量测试、typecheck、build 通过

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

开始实现：core.update 级联 + claim 兜底激活祖先。

实现完成：
- core.update：ticket/subticket 置 in_progress 后沿 parentOf 链激活 open 祖先（含父 Ticket→SubStory→Story→Epic）。
- claimRunItem：认领成功后兜底激活，失败不阻断认领（best-effort try/catch）。
- 保护规则：仅 open → in_progress；done/blocked/skipped 永不重开；归档祖先静默跳过。
- 顺带修复既有小问题：父节点 children 块在子节点状态变化后同步刷新（此前仅 title 变化才刷新，状态会滞留）。
- 更新 omt / omt-runs skill 与 omt_run_claim 描述。
验证：pnpm test（27 文件、340 测试通过）、pnpm typecheck、pnpm build、git diff --check 全部通过。
