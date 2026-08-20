---
id: TICKET-0071
type: ticket
title: SSE run 事件前端接入
status: done
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.404Z'
updated_at: '2026-08-20T00:14:00.310Z'
---

## 任务

SSE 通道扩展 run 维度：

- 现有 ChangeHub bump 之外，run 变化（item 推进、run 状态变化、通知）触发
  前端 store 刷新 run 视图
- payload 设计向后兼容（旧客户端忽略 run 字段）
- controller.ts store 增加 run 相关 snapshot

## 验收

- run 操作后 UI 即时刷新，无需手动
- events/controller 单测更新

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->


## 进度（P2 host 侧，2026-08-19）

Host 侧数据/操作通道已备齐（UI worker 可直接对接）：

- SSE：core `onRunEvent` → `bridgeRunEvents`（changes.ts）→ ChangeHub bump，
  事件新增可选 `run: { id, kind: 'run'|'item', nodeId? }` 维度（additive，
  旧客户端忽略）；pool 的 `onCoreOpened` 同时挂 notifier 与该 bridge。
- 新增 RPC 端点（`/omt` channel，zod strict）：`run-list` / `run-show` /
  `run-control`（start 附带向当前会话 followup 注入认领指引）/ `run-create`
  （子树收集 + 默认配置直建）/ `run-add`（去重、done/archived 跳过计数、
  in_progress → item running + RunningRegistry 执行者快照、同 home 校验）/
  `run-confirm`（确认 → item+ticket done；打回 → item interrupted、ticket
  保持 in_progress）。`get` 响应新增 `runs`（该 ticket 所属非终态 run +
  itemState + progress）。
- core 新增 `addRunMembers`（仅活跃 run 可加入；interrupted 需先 resume）
  与 `runsOfNode`；types 新增 `RUN_ACTIVE_STATUSES` / `RUN_HISTORY_STATUSES`。
- 测试：tests/run-rpc.spec.ts（19）、run-core.spec.ts +5、events.spec.ts +2；
  全套 265 passed + tsc 绿。

**剩余（UI worker）**：前端 store 的 run snapshot 与 run 视图刷新接线
（本 ticket 验收的 controller/UI 部分）。


## 进度（P2 UI worker，2026-08-19）

- controller store 新增 run 相关 snapshot：runs（RunListState）、runDetail（RunDetailState）、panelSection、runPicker、notice。
- SSE 接入：connectEvents 解析 data JSON 的可选 run hint（{id, kind, nodeId?}，additive——解析失败按无 hint 处理，向后兼容）；去抖刷新中加入 run 列表（每次 bump）与打开的 run 详情（仅 hint.id 匹配时），树/doc/related 沿用原有路径。
- run 操作后 UI 即时刷新：RPC 操作显式刷新 + SSE 双通道。
- 测试：tests/run-ui.spec.ts SSE describe（stub EventSource：匹配 hint 触发 run-list+run-show，其他 run 的 hint 不触发详情重载）。

