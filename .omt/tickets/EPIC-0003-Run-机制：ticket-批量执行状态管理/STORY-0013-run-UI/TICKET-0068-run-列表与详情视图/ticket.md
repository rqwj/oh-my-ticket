---
id: TICKET-0068
type: ticket
title: run 列表与详情视图
status: done
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.371Z'
updated_at: '2026-08-20T00:14:00.270Z'
---

## 任务

独立 run 视图：**抽屉内与 ticket 树平级的独立 Runs 区块**，三壳（抽屉/浮窗/Tab）
复用同一组件，用户从面板导航进入（树节点上**不加**批次进度条）：

- run 列表：状态、进度统计（done/total、failed 数）、创建时间；
  默认展示非终态 run；**interrupted run 不折叠**，留在主列表显著展示 +
  恢复按钮（最需要人工核对，决策 16 不注入通知）；completed/
  completed_with_failures/canceled 折叠进「历史」分组可展开
- run 详情：item 清单（状态、执行者谱系「父会话 ↳ subagent」、attempts、
  last_error）、run config 只读展示、pause/resume/cancel 操作
- **「开始执行」入口**：pending 状态的 run 在详情提供开始按钮——向**当前
  操作会话**以 **followup** 注入启动指令（inject 不唤醒 idle 会话），引导该
  会话调用 omt_run_control start；pending → running 由此触发
- **resume 入口**：paused 与 interrupted（可 resume，Epic 决策 7）的 run 提供
  恢复按钮
- **retry**：item 行级操作，适用 failed、interrupted（含打回产生的）与
  **停滞 pending 项**（重置 nudge 计数并重新可派发）；blocked/skipped 项由人
  先改 ticket 状态再回到 pending（回放语义，TICKET-0055）
- **remove**：item 行级移除操作（一键加入的补救路径，经 omt_run_control
  remove，不改动 ticket 节点状态）
- **Tier 3 核对入口**：interrupted item 显著标识 + 重试/核对操作
- **停滞标记**：nudge 预算耗尽的 pending 项显著标记为停滞，标记旁提供 retry
- ticket 详情面板显示所属 run 链接——**列出该 ticket 所属的全部非终态 run**
  （通常一个；多个时并列展示链接与各自进度）

## 验收

- 三种展现方式共享同一 run 视图组件
- 操作走 RPC 并即时刷新
- 重启后 interrupted 项可在 run 详情核对/重试
- 开始执行 / 历史分组 / 停滞标记+retry / remove / 多 run 链接各有用例
- 浏览器测试

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->


## 进度（P2 UI worker，2026-08-19）

- TicketPanel 头部下新增 Tickets | Runs 区块导航（panelSection store），三壳（Drawer/FloatWindow/TicketTab）经同一 RunsView 组件复用；树节点无批次进度条。
- run 列表：非终态主列表（interrupted 不折叠、左侧红条显著 + 行内恢复按钮）、终态折叠「历史（N）」分组可展开；行含状态徽章/进度/失败数/停滞数/创建时间。
- run 详情：item 清单（状态点+标签、执行者谱系 ↳ label（子代理）、attempts、last_error、停滞徽章、Tier 3 需核对徽章）、config 只读、start（pending，host followup 注入）/pause/resume/cancel；行级 retry（failed/interrupted/停滞 pending）、remove（非 in-flight）；awaiting_confirmation 行确认/打回（见 0070）。
- ticket 详情面板「所属 run」列出全部非终态 run 链接（title+进度+itemState 点），点击 showRunInPanel 深链到 run 详情。
- 纯函数 run-view.ts（groupRuns/runControlActions/canRetryItem/canRemoveItem/canConfirmItem）+ controller/组件测试覆盖。浏览器测试以 jsdom 组件测试替代。

