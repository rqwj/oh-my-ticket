---
id: EPIC-0003
type: epic
title: Run 机制：ticket 批量执行状态管理
status: open
priority: 0
created_at: '2026-08-19T09:08:48.423Z'
updated_at: '2026-08-20T01:19:26.653Z'
---

# Run 机制：ticket 批量执行状态管理

## 背景

单 ticket 执行已有状态管理（SQLite 生命周期状态 + 内存 RunningRegistry），但批量执行
（如一次做完一个 Epic 的 ticket，或跨 Story 挑选 ticket 组成批次）没有批次概念：
无队列、无整体进度、无单项成败记录、中断无法续跑、失败无法重试。

## 核心设计决策（评审已确认）

1. **run 是任意 ticket 的有序快照**：`run_items` 是成员唯一事实来源，可跨 Story/Epic
   挑选；与树节点不维持活链接；无 scope_id 字段。**同一 ticket 可属多个活跃 run**
   （快照语义）：item 推进按 ticket 状态广播到所有持有它的活跃 run；续跑 nudge
   只发给首个将 ticket 置 in_progress 的执行者所在 run。
2. **run 归属单一 home**：创建时校验所有 item 同 home（workspace `.omt` 或全局）；
   加入既有 run 走同样校验。
3. **执行者身份 = session 绑定 + 谱系**：DSH subagent 是一等 session（`Agent.id`
   即 SessionId），`exec.agent` 即实际执行者；session header 持久携带
   `parentSession` 与 `origin:'subagent'`，展示为「父会话 ↳ subagent」。
4. **节点 status 扩展**：新增 `blocked / skipped`（黄色呈现）。skipped 用于必须
   跳过或人为主动跳过执行的场景；blocked 用于因外部条件做不下去。
   **stopped 已在 round-2 评审后移出**（语义空洞：无产生路径、无 item 映射）。
5. **三层执行保障 hook**（DSH 原语：cordis 事件 agent/status、agent/disposed；
   Agent API followup/steer/inject）：
   - Tier 1：`agent/status → idle`，同会话 followup 提醒未收尾 ticket；
     **兼作 run 续跑驱动**（执行者 idle 且 running 状态 run 有 pending 项 →
     nudge 继续下一项），`autoContinue` 配置默认开。**nudge 带预算**（同一 item
     最多 3 次指数退避），耗尽后 item 在 run 详情标记停滞（可经 retry 重置），
     提示人工介入（见 TICKET-0062/0068）。
   - Tier 2：`agent/disposed`，subagent 终止时名下 running item 由父会话
     **followup**（唤醒 idle 驱动）通知兜底；父会话也不在 → item interrupted。
   - Tier 3：DSH 重启，running → interrupted，UI 提供核对入口。
6. **信任策略**：hook 触发的被动判断默认进 `awaiting_confirmation`（人一键确认），
   不自动 done；run config `autoVerify` 可放开。**机械分流**：run item 处于
   running 且 executor 会话未经 omt_run_report 直接 omt_update 落 done →
   待确认；经 report 的显式报告直接落 done（见 TICKET-0064）。
7. **run 终态**：`completed`（全 done/skipped）/ `completed_with_failures`
   （含 failed 或 interrupted 项——**interrupted 项计失败**）/ `canceled` /
   `interrupted`；run 本身无 failed。**interrupted 非绝对终态：可经
   omt_run_control resume 回 running 续跑。**
8. **stop-on-failure**：**仅 item 置 failed 触发**（blocked/skipped 不触发）；
   触发后 run 转 `paused`，pending 项保留，人决定 resume（跳过失败项继续）或 cancel。
9. **pause 语义**：只停止派发与续跑 nudge，已 running 的 item 继续观察。
10. **重试**：就地重置 item 回 pending（适用 failed、interrupted 与停滞 pending 项；
    **retry 清零 nudge 预算**），`attempts` 计数 + `last_error` 保留；
    不建 attempt 历史表（结论在 ticket markdown 追加记录里）。
11. **回放**：run 进行中 ticket **done/blocked/skipped → open** 均使对应 item
    回退 pending（保留 position）。
12. **janitor**：启动时无活跃执行者的 running run/item → interrupted。
13. **reindex 保护**：omt_reindex 重建索引时不得触碰 runs/run_items 表。
14. **工具面**：新增 omt_run_create / list / show / control（**start**/pause/resume/
    cancel/retry/**remove**）/ claim / report；omt_run_claim 原子认领（SQLite
    事务）；**claim 优先**：已 claim 项的 executor 归属不被被动观察覆盖。
15. **UI**：树节点与详情面板加「加入 run」按钮（收集当前节点+下级，跳过已
    done/archived 并报告数量；in_progress 加入即 running；多 run 时用户主动
    选择目标 run，活跃=非终态）；独立 Runs 区块（抽屉内与树平级，三壳复用）；
    不做多选勾选。
16. **通知**：run 中每个 item 完成（done/failed/blocked/skipped）时向执行会话
    inject 进度通知；awaiting_confirmation 待确认提示、paused 待决与 run 终态
    总结用 **followup** 唤醒注入；**interrupted 终态不注入总结**（执行会话往往
    已销毁），走 UI 核对入口。

## Schema（v2 → v3）

```
runs: id, title(可选), home 隐含, status, config(JSON: stopOnFailure/autoContinue/
      autoVerify/concurrency), created_at, finished_at
run_items: run_id, node_id, position, state(pending/running/done/failed/blocked/
      skipped/interrupted/awaiting_confirmation), executor_session_id,
      attempts, last_error, nudged_at, nudge_count, started_at, finished_at
```

## 阶段

- P1：数据核心 + 工具面（含 claim）+ 观察推进 + 续跑 nudge + report 词汇 +
  janitor **+ omt-runs skill**
- P2：三层 hook 完整化 + awaiting_confirmation + 全部 UI
- P3：基于 claim 的并发执行（**无 ticket，待 P1/P2 落地后按需拆解**）

混合阶段的 story 内以 ticket 级 priority 为准（priority 1/2/3 对应 P1/P2/P3）。

## Deferred / Open Questions

### From 2026-08-19 review

- **未记录「为何不复用 goal-round-driver」的评估** — Epic 决策 5（三层 hook）(P2, adversarial, confidence 75)

  DSH 仓库内已有 goal-round-driver：持久化目标、自动续跑轮次、监听 agent/status
  与 agent/disposed 驱动 nudge、resume/disarm 语义（packages/goal/goal-round-driver），
  与本设计的 nudge 驱动、janitor、终态推导高度同构，其轮次上限/blocked 语义恰好覆盖
  一次性 nudge 的停滞问题。全部 26 个文件没有「为何不用/不可复用」的记录，团队可能
  并行建造并长期维护第二套续跑引擎。需要先实际评估其能否作为 run 续跑载体，
  再把结论（复用或不复用的理由）补录进 Epic 决策记录。

- **run config 创建后全程只读，中途调整无入口** — TICKET-0067（默认直建）/ TICKET-0057 (P2, design-lens, product-lens, confidence 75)

  config（stopOnFailure/autoContinue/autoVerify）创建时一次性写入，UI 只读、
  omt_run_control 无修改动作——人想中途收紧信任策略或停掉续跑 nudge 又不想
  整个 pause 时，只能 cancel 重建（丢失 run 内进度记录）。真实取舍：中途可改
  config（control 加 set-config / UI 编辑）vs 明确声明为一次性决策，
  建议与 P3 并发配置一起考虑。

<!-- omt:children -->
## 子节点

- [STORY-0010 run 数据核心与状态机](STORY-0010-run-数据核心与状态机/story.md) — done
- [STORY-0011 run 工具面与节点状态扩展](STORY-0011-run-工具面与节点状态扩展/story.md) — done
- [STORY-0012 执行保障三层 hook](STORY-0012-执行保障三层-hook/story.md) — open
- [STORY-0013 run UI](STORY-0013-run-UI/story.md) — open
- [STORY-0014 skill 拆分渐进加载（omt / omt-runs）](STORY-0014-skill-拆分渐进加载（omt-omt-runs）/story.md) — open
<!-- /omt:children -->


## 进度记录

- 2026-08-20 **P2 评审中途暂停**：实现全部完成并提交（至 1de319a，303 测试全绿）。
  ce-code-review P2 段已完成评审+验证（run 20260820-085327-d745a2a7），**修复尚未应用**。
  待应用的已验证发现（9 项）：
  #2 打回不重开 ticket（P1，rpc.ts reject 分支改 core.update open + notify 文案）；
  #4 SSE debounce latest-wins 丢 run hint（P2，hint id 集合累积）；
  #7 joinRun 无重入守卫（P2）；#8 裸 done 绕过信任门（P2，awaiting_confirmation 项
  非 report 完成应跳过）；#9 claim/claim 跳过不发 run 事件（P2）；
  #10 disposed-hook 不含 paused（P2）；#11 子先父后销毁孤儿项（P2，handoff 跟踪）；
  #12 disposed-hook 提前返回泄漏 running 标记（P2）；#13 join 时无标记的
  in_progress 置 running 成楔形（P2，改置 pending）。
  验证器拒收 3 项：#1 core.ts 超千行拆分（无仓库契约支撑）、#3 omt_run_add
  （决策 14 明确无 add 工具）、#14 create 子树语义（决策 15 划归 UI）。
  另有 5 项测试补强降级在 testing_gaps（notice 计时器、applyRunMutation、
  RunsView 错误分支、disposed/notify 边界）。
  **恢复点**：从应用 #2/#4/#7/#8/#9/#10/#11/#12/#13 开始，产物目录
  /tmp/compound-engineering-501/ce-code-review/20260820-085327-d745a2a7/。
