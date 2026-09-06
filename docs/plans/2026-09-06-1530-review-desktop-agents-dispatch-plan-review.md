---
title: Desktop Agents 派发计划与 Harness 执行器设计 - 深度评审与搁置项方案分析
type: review
date: 2026-09-06
reviewed:
  - docs/plans/2026-09-06-1100-feat-desktop-agents-dispatch-implementation-plan.md（下称「计划」）
  - docs/plans/2026-09-05-1245-feat-desktop-harness-executor-design.md（下称「设计」）
method: 内联代码核验评审（非多 persona 流水线——计划 §九 已过一轮 6-persona 评审，本文聚焦事实核验、新增风险与搁置项方案对比）
---

# Desktop Agents 派发计划与 Harness 执行器设计 - 深度评审

## 0. 评审方式与结论摘要

**方式**：逐条对照 `crates/omt-runtime`、`crates/omt-storage`、`crates/omt-domain`、`apps/desktop/src-tauri` 现有代码核验两份文档的关键断言；对计划 §九 的 14 项搁置项、设计 §七 的开放问题、以及 §八 backlog 中会反向影响 v1 的条目，给出成因、影响与候选方案对比。

**总体结论**：

- **架构方向正确且稳固**。KD-D1～D8（执行器放 desktop、harness 经 MCP 自 claim/report、transcript 走 Channel、daemon 协议零变更、registry 表驱动、会话与 app 同生命周期）都在代码事实上站得住，与 daemon「纯协调面 / 唯一写者 / idle 可退出」不变式相容。Q1～Q4 四项产品决策也自洽。
- **计划已达可开工粒度**，§3.6 工程纪律（async command、setpgid vs setsid、cwd 剥 `.omt` 尾、写纪律、GC 冻结）是对仓库现实的准确提炼，本次核验全部属实。
- **但有 2 个新发现的阻断级问题**（不在 §九 之列），都在「终止 → 重派发可推进」这条 P0 验收链上：
  1. **interrupt 之后 item 不可再 claim**：`claim_next_run_item` 只取 `state='pending'`（`crates/omt-storage/src/store.rs:890-896`），而 `run/interrupt` 把 item 置 `interrupted`、run 置 `Paused`（`dispatch.rs:2634-2690`），`run/control resume` 只改 run 状态不动 item（`dispatch.rs:1811-1830`）。因此计划 §3.3「exec_stop 后立即重派发可推进」与 U4 测试场景「exec_stop 后立即重派发 → 新 Agent 可 claim」在现实现下**必然失败**——缺 `run/control retry(nodeId)` 一步（`retry` 才把 interrupted → pending，`dispatch.rs:1946-1965`）。§九 第 3 项只补了 resume，仍不完整。
  2. **MCP 执行者身份硬绑 pid，desktop 无法关联 exec ↔ item**：`omt mcp` 把 actor namespace 硬编码为 `mcp:<pid>`（`crates/omt-runtime/src/mcp.rs:95-97`），无视 CLI 已有的 `--actor` 全局参数（`cli/mod.rs:155-158`）。MCP 进程是 harness 的孙进程，desktop 不知道它的 pid → 不知道「本 exec 持有哪些 item 租约」，§3.3「对该 exec 持有的租约项显式 interrupt」与 §九 第 3 项的「guard：仅当全部 in-flight 属于被停 exec」都无法实现；Runs 面的 `mcp:<pid>` 谱系与 Agents 面的「Agent N」也无法互相跳转。此外 harness 若重启 MCP server（pid 变化），持有的租约 actor 不匹配 → report 被拒。**解法成本极低**（让 `omt mcp` 尊重 `--actor`，desktop 注入 `--actor exec:<execId>`），属于 CLI 参数而非 daemon 协议变更，详见 §4.3。
- **设计文档需同步标注被计划取代的段落**（§3.2 结论「单 ticket 执行退化为 prompt 执行」、§5.4「单 ticket 执行（非 run）」、开放问题 2、§八分期），否则两份文档并存会造成漂移；设计 `status: draft` 也应更新。

---

## 1. 代码事实核验表

| # | 文档断言 | 核验结果 | 证据 |
|---|---|---|---|
| F1 | `probe_version` 无超时会永久挂起（计划 §3.1） | ✅ 属实，`.output()` 直等 | `apps/desktop/src-tauri/src/rpc_bridge.rs:70-82` |
| F2 | `events_subscribe` 每订阅一线程永不退出（计划 §3.6.5） | ✅ 属实，loop 仅在 daemon 出错时 return，无取消信号 | `rpc_bridge.rs:321-370` |
| F3 | `omt_call` 同步 command、仅 `dsh_plugin_install` 是 async（§3.4） | ✅ 属实，全文件仅 :192 一处 `command(async)` | `rpc_bridge.rs` |
| F4 | journal 按 commandId 全局键控，同 id 不同输入 → CONFLICT（§九-1） | ✅ 属实 | `dispatch.rs:331-345, 376, 399-405` |
| F5 | `run/interrupt` 是 run 级：interrupt 全部 in-flight、清全部租约、置 Paused（§九-3） | ✅ 属实；且 in-flight 含 `AwaitingConfirmation` | `dispatch.rs:2634-2690`；`omt-domain/types.rs:255-260` |
| F6 | 租约 TTL 15min | ✅ `LEASE_TTL_MS = 15*60*1000` | `dispatch.rs:1534` |
| F7 | 信任门：同一执行者裸 done → awaiting_confirmation；显式 report 不落门 | ✅ | `omt-domain/runs.rs:50-68`；`dispatch.rs:1075-1095` |
| F8 | `node/execute` 接受 Epic/Story（计划 FYI） | ✅ 只拒 archived，无类型门 | `dispatch.rs:1428-1445` |
| F9 | claim 自动把非 ticket/subticket、archived 成员 drain 成 skipped | ✅ 与计划「成员 DFS 只收 Ticket/SubTicket」一致 | `store.rs:875-878` |
| F10 | desktop 自注册为 admin principal；admin 可无 leaseToken report（U14 可行性） | ✅ | `daemon.rs` 头注释；`dispatch.rs:2281-2290` |
| F11 | MCP 凭据受限 node/run/events，无 home/ui（R5） | ✅ | `mcp.rs:30-52, 98-104` |
| F12 | `sidecar_path` 硬编码 `omt-daemon`；`copy-sidecar.mjs` / `externalBin` 仅 daemon（U3 三处缺口） | ✅ | `daemon.rs:37-48`；`copy-sidecar.mjs`；`tauri.conf.json:36-38` |
| F13 | daemon spawn 用 `setsid`（与 harness 应用 `setpgid` 方向相反，§3.6.3） | ✅ | `daemon.rs:74-100` |
| F14 | `tauri:harness` bag 无执行消费者、`HarnessSelect` 可整体移除（§四） | ✅ 消费点仅 SettingsPanel/App/store 的读写与渲染 | `apps/desktop/src/{SettingsPanel,App,store}.tsx` |
| F15 | 300ms 防抖后 nodes+runs 双全量重拉（§四） | ✅ | `apps/desktop/src/store.ts:286-308` |
| F16 | 设计引用的 `docs/runtime/{architecture,protocol,security}.md` 存在 | ✅ | `ls docs/runtime` |
| F17 | 「exec_stop 后立即重派发可推进」（§3.3、U4） | ❌ **不成立**：claim 只取 pending；interrupted 需 retry | `store.rs:890-896`；`dispatch.rs:1811-1830, 1946-1965` |
| F18 | 「desktop 对该 exec 持有的租约项 interrupt」（§3.3） | ❌ **不可实现**：actor 为孙进程 pid，desktop 不可知；`omt mcp` 忽略 `--actor` | `mcp.rs:95-97`；`cli/mod.rs:155-158` |
| F19 | 「`omt mcp` 已支持 name，desktop 生成 config 时传入即可」（设计 §5.7） | ⚠️ name 硬编码 `omt-mcp`，无 CLI 参数 | `mcp.rs:96` |
| F20 | 「headless 走最严审批模式」（§九-10） | ⚠️ opencode 无 `--auto` 时权限请求一律 reject（设计 A.2），最严 = agent 无法干活；见 §4.10 | 设计 A.2、A.3 |

---

## 2. 对设计文档（base_design）的评审

### 2.1 成立的部分

- §1 现状分析三条事实（daemon 不 spawn、执行按钮空壳、无 transcript 通道）核验属实，是全文最有价值的锚点。
- §3.1 三方案对比理由充分，B（daemon 侧执行器）与 idle watchdog（`server.rs:240-245`）确实冲突。
- §3.2 模式 B「认领者=执行者=报告者」与信任门（F7）、租约 fencing（F10）的实现语义完全吻合。
- 附录 A 的外部调研粒度高（flag、事件类型、权限默认值、会话续跑矩阵），是 U11/U12 的直接输入。

### 2.2 需要修订的部分

| 位置 | 问题 | 建议 |
|---|---|---|
| frontmatter `status: draft` | 计划已「定稿」并以其为 base | 改 `status: accepted-with-supersessions`，加 `superseded_by` 指向计划 |
| §3.2 结论「单 ticket 执行退化为 prompt 执行 + 状态收尾」、§5.4「单 ticket 执行（非 run）」段、§七 开放问题 2 | 被计划 Q1「统一 run 化、不做直派旁路」取代 | 加删除线 + 「已被计划 Q1 取代」注记，避免实施者读到两套语义 |
| §八 分期（P0=单 ticket prompt 执行、MCP 可选） | 与计划分期（P0=统一 run 化 + MCP 必选）冲突 | 同上标注 |
| §5.1 `HarnessConfig { id, resolvedFrom:'install'|'dev', devPath }` | 计划 §3.1 已改为 `HarnessUserConfig{enabled,binary,extra_args,env}` + bag v2 `{defaultHarness}`；dsh dev 模式已出 v1 | 同上标注 |
| §5.4 表 dsh 行「不需要 MCP——插件直装」 | v1 不含 dsh，但该结论仍是 backlog #1 的依据，保留即可；需注意 dsh 直装后 executor 谱系是 dsh 会话（kind:dsh），**不是** mcp——Agents 面的 Agent↔item 关联将走另一条路径 | 在 backlog #1 注明 |
| §5.6 「回放：`exec_open(execId)`」 | 计划 §3.4 command 面无 `exec_open`；U10 回放走 `TranscriptView` 回放模式但未定 command 名 | 计划 §3.4 补 `transcript_open(execId, channel)` |
| §5.7 RunsPanel「item 行加『用 harness 执行』」 | 计划未纳入 | 明确进 backlog 或 U14 顺带 |
| §七 风险 5 / 计划 §3.6.3 Windows `taskkill /T` | CI 与产品均 macOS-only（`ci.yml` 注释） | v1 明确「仅 macOS」，删去半规格化的 Windows 分支，避免不可测的代码 |

---

## 3. 对实施计划的评审

### 3.1 架构增量（§三）

- **§3.1 Provider Contract**：三层分离合理。补充两点：
  - `HarnessUserConfig.binary: Option<String>` 存绝对路径即可，但「探测结果自动填入并持久化」会把机器路径写死；harness 升级（brew 换 Cellar 路径）后 config 指向失效二进制。建议存 `binary_source: Detected | Manual(path)`，Detected 每次启动重探测，Manual 才落死路径。
  - `env` 字面量 v1 可接受，但要明确 **desktop 自身的 env 是否透传**给 harness（GUI 进程 PATH 盲区是已知风险）：建议 spawn env = `augmented_path()` 覆盖 PATH + 用户 env 覆盖 + 强制注入 `OMT_RUNTIME_DIR`，且用户 env **不得**覆盖 `OMT_RUNTIME_DIR`（否则 MCP 连错 daemon，见 §九-11）。
- **§3.2 Agent 实体**：`execs[]` 内嵌于 Agent 记录，Agent 数 × exec 数增长后 agents.json 单文件读-合-写会变慢；v1 可接受，但 §3.6.6 的「read-merge-write under 锁」要加 **写合并**（同一 exec 的高频状态更新合并为一次落盘），否则 transcript 每个事件触发一次 agents.json 重写会成为 IO 热点。建议：Agent 状态变化才写，事件计数等派生量不落盘。
- **§3.3 派发链**：见 §0 阻断 1/2 与 §4.3。另外链序中 ⑤ 写 bindings 在 ⑥ Agent 记录之前，但 binding 的值是 agentId——agentId 在 ⑥ 才生成？需要先生成 id 再写 ⑤，或 ⑤⑥ 合并为一次写。
- **§3.4 commands**：缺 `transcript_open`（回放）与 `agent_dismiss/clear`（U9 悬空清理、U10 历史清理）；`exec_start` 返回 `execId` 但 UI 需要 `agentId` 才能选中列表项——建议返回 `{execId, agentId, runId}`。
- **§3.5 UI IA**：右侧「详情 | Agents」tab 化后，**派发成功自动切到 Agents tab 会丢失详情上下文**；建议 Agents tab 的 Agent 行显示「当前节点」可点回详情（§九-7 已部分覆盖）。
- **§3.6.9 单实例**：FYI 已建议前移 U2，同意——P0 双开即跨进程写坏 agents.json。

### 3.2 System-Wide Impact（§四）

- 事件分流（`node.*`→nodes、`run.*`→runs）正确。补一条：`run.item_changed` 高频时 Agents 面的「完成 3 / 失败 1」统计如果也挂在 refreshRuns 上会跟着全量重拉——建议 Agents 统计从 `run/get` 单 run 拉取而不是 `run/list`。
- 「DSH 面不可见」是预期，但要在 DSH 面的 Runs 展示里说明 `exec:<execId>`（若采纳 §4.3 方案 A）这类 executor 谱系的含义，否则 DSH 用户看到陌生前缀。

### 3.3 实施单元与依赖（§五）

依赖图核验：

```
U1 ─┬─ U2 ─┬─ U3 ─┬─ U4 ─┬─ U9 ─┐
    │      │      │      └─ U6 ◄┘   ← U6 依赖 U9、U9 修改 U6 创建的文件：环（§九-4）
    │      ├─ U5 ─┬─ U10 ─ U11
    │      │      └─ U14
    │      └─ U12
    └─ U7
U8、U15 独立
```

- **U4 步骤 ⑥ 需要 agents.json，但 agents.json 在 U10（P1）**——§九-2 已识别，同意前移。
- **U4 测试「exec_stop 后立即重派发 → 新 Agent 可 claim」按现实现必失败**（F17），需在 U4 Approach 中加入 resume + retry 链。
- **U13 parity 断言应含「`omt mcp` 尊重 `--actor`」**（若采纳 §4.3-A），并放在 `crates/omt-runtime` 的测试而非 desktop 测试。
- **P0 体量**：U1→U8 + U9 共 9 个单元 + 一条真实 harness 端到端。建议按「先无 UI 的纵向切片」排：U15(单实例) → U1(contract 层) → U2 → U3 → U4，用 Rust 内联测试 + 假 harness（`sh -c` 脚本调用 `omt mcp` 完成 claim/report）打通纯后端链路，再上 U5/U6/U7/U8/U9。这样 P0 最大的不确定性（MCP 注入、租约、补偿表）在没有 UI 干扰时暴露。

### 3.4 风险表（§七）遗漏

| 新增风险 | 说明 | 缓解 |
|---|---|---|
| MCP 冷启动 | 每次 exec spawn harness → harness spawn `omt mcp` → 5s 就绪等待（`mcp.rs:62-80`）；daemon 若因 idle 退出（无其他 actor 时）需重新自举 | desktop 自身是常驻 actor，daemon 不会 idle 退出；但 desktop 关闭后孤儿 harness 的 MCP 可能触发 daemon 重自举——KD-D7 下孤儿会被 killpg，可接受；记录即可 |
| harness 自身 MCP 超时 | claude/opencode 对 MCP server 初始化有超时（通常 10-60s）；daemon 冷启动 + 握手在慢机器上可能超时 | desktop 在 spawn harness 前确保 daemon live（已是 discover-or-spawn 前置） |
| prompt 模板与 harness 系统提示冲突 | 各 harness 自带 system prompt，可能与「不要虚构结果」「被拒即退出」冲突（如 claude 默认会尝试多种方法而非退出） | 模板 per-provider 可覆写（descriptor 携带 `prompt_overrides`），U12 fixture 里加「claim 被拒后是否退出」的行为断言 |
| 同 workspace 多 home | `.omt` 与全局 home 同时可见时，spawn cwd = workspace 根，但 `OMT_HOME` 未注入 → `omt mcp` 只带全局 home？ | 核验 `omt mcp` 的 home 解析：MCP 走 daemon 已注册的 home（homeId 在 claim 参数里），无需 `--home`；但 prompt 中要给 homeId，MCP 工具调用需带 `homeId` 参数——确认 `run__claim` 工具 schema 是否要求 homeId，模板需注入 |

---

## 4. 搁置项逐条解析与候选方案

以下先列计划 §九 的 14 项（含拟议修复 1-13 与决策项 14），再列设计 §七 开放问题与 backlog 中反向影响 v1 的条目。每项按「搁置成因 → 不处理的影响 → 候选方案对比 → 建议」展开。

### 4.1 commandId 改 per-step 派生（§九-1，P1）

**成因**：计划 §3.3 把「一次逻辑派发 = 一个 commandId 贯穿 ②③④」当作幂等纪律，但 journal 以 commandId 为全局唯一键（F4）：②`node/execute` 成功后，③`run/create` 带同一 id 但输入不同 → `CONFLICT command-id-reuse`。评审后用户暂缓，可能是希望和其他链路修正（§九-3）一并决策。

**影响**：P0 主链路在第三步**必然**失败，属阻断级；没有替代路径。

**候选方案**：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **A. dispatchId + per-step 派生 id** | 派发生成 `dispatchId`；② 用 `<d>:execute`、③ `<d>:create`、④ `<d>:start`；重试复用各自 id，journal 命中即回放存储结果 | 每步真幂等；重试语义清晰；零协议变更；desktop 可凭 dispatchId 前缀在 `operations` 表反查整链 | commandId 可读性略降；需在 desktop 侧持久化 dispatchId 以支持跨重启重试（v1 可不跨重启） |
| B. 仅 `run/create` 用稳定 id，②④ 用随机 id | 只保护最容易重复副作用的建 run 一步 | 最少改动 | ② 重试会重复写 `exec:` 审计 meta（幂等覆盖，无害）；④ 重试在 run 已 Running 时撞 `run-status-gate` CONFLICT，需把「目标态已达」视为成功——把幂等责任推给调用方判断错误码 |
| C. daemon 新增 `run/ensure` 复合原语 | 一次 RPC 完成 execute+create+start | 链路最短，无补偿表 | 违反 KD-D3 协议零变更；把 desktop 编排逻辑推入 daemon；需独立评审（对应 backlog #7 的一部分） |

**建议**：采纳 **A**，并在 ④ 加「run 已是 Running 视为成功」的容错（应对复用 Running run 的分支，本来就要跳过 ④）。C 记 backlog，与 #7 一并评审。

### 4.2 agents.json 最小存储前移 P0（§九-2，P1）

**成因**：U4 第 ⑥ 步「写 Agent 记录 status=Starting」与 U9「GC 仅在 agents.json 成功解析后执行」都依赖 agents.json，但 agents.json 的全部实现（schema、锁、原子写、reconcile、回放、持久序号）打包在 P1 的 U10。搁置原因推测是 P0 体量压力。

**影响**：不前移则 P0 的 Agent 列表只能是内存态——app 重启后 Failed 记录消失、bindings GC 无法判断 agentId 是否悬空（会误清全部绑定或完全不 GC）、§3.2「先落盘」不变式在 P0 不成立、U4 补偿表「⑥ 之后失败一律留 Failed 记录」也不成立。

**候选方案**：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **A. 拆 U10：最小存储进 P0（新 U4a）** | U4a = schema + 进程锁 + tmp/rename + `.corrupt-<ts>` 降级 + `agents_list` 读；U10 只剩 reconcile、回放、持久序号 | 不变式在 P0 成立；U9 GC 规则可落地；U10 体量减半 | P0 多一个单元（约 1-2 天） |
| B. P0 纯内存 Agent 注册表 | agents.json 推到 P1 | P0 最轻 | 上述影响全部发生；U9 GC 必须先禁用（绑定只增不减）；P0 验收「起动失败呈现 Failed」重启即丢 |
| C. 整个 U10 前移 P0 | 一次做完 | 无二次返工 | P0 再增 reconcile/pid 校验/回放，明显超重 |

**建议**：**A**。并把 `meta.agentSeq` 持久序号也放进 U4a（只是一个计数器字段，代价极小，避免 P0「Agent 3」重启后变「Agent 1」的体验割裂）。

### 4.3 租约死区组合修正（§九-3，P1）——含本次新发现

**成因**：计划 §3.3 设计「exec_stop 成功后对该 exec 持有的租约项显式 `run/interrupt`」消除 15min 死区。评审事实核验发现 `run/interrupt` 是 run 级原语（F5），会把同 run 其他 agent 的 in-flight 也打断并置 run Paused，因此加了 guard 与 resume 补丁，用户暂缓决策。

**本次新增两个事实**使问题更完整：
- **F17**：interrupt 后 item 是 `interrupted`，claim 只取 `pending`，resume 不复原 item —— 必须对每个被 interrupt 的 item 调 `run/control retry(nodeId)`，否则新 Agent 一 claim 就拿到 `None` 并退出。
- **F18**：desktop 根本不知道「该 exec 持有哪些 item」——`executor_session_id = mcp:<孙进程 pid>`，且 `omt mcp` 忽略 `--actor`。

**影响**：P0 验收「可终止（终止后重派发可推进，租约死区已消）」目前**不可达**；同 run 多 agent 时 guard 无法计算；Agents 面与 Runs 面无法互相定位。

**候选方案**（可组合）：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **A. 稳定 actor 注入（身份层）** | `omt mcp` 尊重已存在的全局 `--actor`（`cli/mod.rs:155`，`mcp.rs:97` 改为 `globals.actor.clone().unwrap_or(mcp:<pid>)`）；desktop 生成 MCP config 时传 `args: ["--actor", "exec:<execId>", "mcp"]` | 一次改动解决三件事：desktop 精确知道本 exec 持有的 item（`executor_session_id == exec:<execId>`）；MCP 进程重启不丢身份；Runs 面 `exec:<execId>` 可直接跳 Agents 面对应 Agent。零 daemon 协议变更（是 CLI 参数），`auth.rs:126-133` 已支持 requested namespace | 需要 `omt` CLI 一个小改 + 一条 runtime 测试；`--actor` 命名空间由 MCP 客户端自报，DSH 面看到的谱系前缀从 `mcp:` 变 `exec:`（或保留 `mcp:exec-<id>` 形式以示来源） |
| **B. item 级 interrupt 原语** | daemon 新增 `run/interrupt` 的 `nodeIds?` 参数或 `run/control interrupt(nodeId)` | 语义最干净：只打断本 exec 的 item、不置 run Paused、不影响他人 | 协议变更（additive），需 schema/contracts/client-ts 三处再生 + 独立评审；与 KD-D3 冲突 |
| **C. desktop 以 admin 身份 report + retry** | desktop 是 admin principal（F10），对本 exec 持有的 item 直接 `run/report failed`（note「用户终止」，无需 leaseToken）→ `run/control retry(nodeId)` → item 回 pending | item 精确；不打断他人；零协议变更；历史如实（attempts+1、last_error=用户终止）；不置 run Paused，**无需 resume/retry 链** | 语义上把「用户终止」记为 `failed` 一次尝试；依赖 admin 路径（审计 reason=`admin-grant`，可接受）；仍需 A 才知道「哪些 item 是本 exec 的」 |
| D. 等 15min TTL | 不做 | 零工作量 | 终止后 15 分钟内无法重派发同 item，P0 验收不达 |
| E. run 级 interrupt + resume + 逐项 retry（计划现方案补全） | 仅当本 exec 独占 run 的全部 in-flight 时：`run/interrupt` → `run/control resume` → 对每个 interrupted 项 `retry` | 复用现有原语 | 三步链、三个补偿点；多 agent 共 run 时回退到 D；仍需 A 判断独占 |

**建议**：**A + C**。A 是所有方案的前置（身份），C 提供零协议变更、item 精确、无 Paused 副作用的释放路径；把「用户终止」如实记为一次失败尝试与 OMT「如实报告」哲学一致。E 作为 C 不可用时（desktop 非 admin，例如 admin-grants 被禁用）的兜底；B 记 backlog #7 独立评审。**同时把该机制扩展到 Exited/Failed 终态**（harness 崩溃未 report 时同样释放），以及 `AwaitingConfirmation` 项（F5 显示它也算 in-flight，用户终止时应一并处理——建议保留其 awaiting 状态而不是 failed，让 U14 处理）。

### 4.4 U6/U9 解环（§九-4，P1）

**成因**：U6（ExecutionDialog）依赖 U9（绑定预选），U9 又修改 U6 创建的 `ExecutionDialog.tsx`；DetailPanel 接线放在 U4。

**影响**：排期上谁先谁后都不自洽；两单元并行时必冲突。

**候选方案**：A. U6 创建弹窗（基础选择器 + DetailPanel 接线），U9 只追加「绑定预选」（推荐，改动最小）；B. 合并 U6+U9 为一单元（体量偏大，但 R-B/R-C 本就是同一交互）；C. U9 只导出纯函数 `resolveBinding(nodeId, bindings, tree)` + Rust 读写，U6 消费——本质等于 A。

**建议**：A，并把 DetailPanel 接线从 U4 移入 U6，U4 只暴露 `exec_start`。

### 4.5 U1 验收收缩（§九-5，P2）

**成因**：U1 Verification 写了「设置页能完成检测→启用」，但设置页在 U7。

**候选方案**：A. U1 只验 contract（`harness_list` 投影、config 读写、bag v2 迁移，Rust 内联测试 + bridge 单测）；B. U1 顺带做一个最简设置面（增大 U1）。**建议 A**。

### 4.6 重启对账终态语义统一（§九-6，P2）

**成因**：§3.2 状态机把 Exited/Failed/Killed 重启后都迁到 `Attached`，与「Attached = 进程仍活、只读尾随」的原义混淆；超时 kill 归 Failed 还是 Killed 也不一致。

**影响**：UI 无法区分「历史记录」与「仍在跑但不受控」；筛选/统计/badge 逻辑全部含糊。

**候选方案**：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| A. Attached 专属存活进程 | 已死 agent 重启后保持 Exited/Failed/Killed；只有 pid 活的 Running 才 Attached | 简单 | 「本 app 生命周期内产生」vs「重启恢复」的来源信息丢失 |
| **B. 正交字段（对齐 OMT 归档正交哲学）** | `status` 只表终态分类 {Starting, Running, Exited(code), Failed(reason), Killed}；新增 `attachment: Live \| Orphan \| None`（Orphan=pid 活但非本进程 spawn，只读尾随）；超时 kill = `Killed{reason: MaxDuration}` | 状态与「可控性」正交，与 OMT 的「archived 与生命周期正交」一致；UI 筛选简单；无信息丢失 | 多一个字段；状态机图要重画 |
| C. 保留 Attached 为伞状态 + sub-reason | `Attached{last: Exited\|Failed\|…}` | 改动小 | 嵌套枚举让前端判断变复杂 |

**建议**：**B**。超时 kill 统一归 `Killed` 并带 reason（它是 desktop 主动 kill，与 Failed「未按预期启动/异常」语义不同）。

### 4.7 弹窗提交态机（§九-7，P2）

**成因**：派发链 ①–⑧ 含多次 daemon RPC + spawn，弹窗在链执行期的状态未定义。

**候选方案**：A. 确认后禁用按钮 + 「派发中…」，成功关弹窗并切 Agents tab 选中新 Agent，失败就地内联报错（推荐）；B. 立即乐观关闭，Agents 列表先出现 Starting 行显示步骤进度——链失败只能在 Agents 行上报错，用户可能已离开；C. 弹窗内显示 ①–⑧ 步骤进度条，完成自动关——信息量最大但过重（链通常 <2s）。

**建议**：A，并加「超过 5s 未返回则展开步骤进度（C 的降级形态）」，覆盖 daemon 慢/MCP 就绪慢的情况；绑定写失败作为 Agent 行持久警告条。

### 4.8 终止控件（§九-8，P2）

**成因**：R-E 列表未定义「终止」入口；exec_stop 的副作用（interrupt/释放已认领项）需要向用户说明。

**候选方案**：A. 行内两段式确认（首次点击变「确认终止：将释放 N 个已认领项」）；B. 模态确认框；C. 无确认直接 kill（不可撤销，不推荐）。**建议 A**，文案中的 N 依赖 §4.3-A 的身份方案才能精确显示。

### 4.9 失败可见性（§九-9，P2）

**成因**：用户可能停在 Tickets 详情，链失败或 Agent Failed 发生在右侧未激活的 tab。

**候选方案**：A. Agents tab 标签未读失败 badge（按 home 计），选中即清；B. 系统通知（tauri-plugin-notification）；C. A+B。**建议 A** 进 P0（零依赖），B 记 backlog（需新插件与权限）。

### 4.10 prompt 不可信内容规则 + 默认审批模式（§九-10，P2，security）

**成因**：ticket 正文/claim context 是模型输入，可能含 prompt injection；各 harness 默认权限差异大（omp 默认 yolo）。评审建议「默认各 harness 最严审批」。

**本次核验的关键修正（F20）**：headless 执行**无人应答审批**，「最严模式」在 headless 下等于「拒绝一切需审批操作」——opencode 无 `--auto` 时权限请求一律 reject（设计 A.2），omp `always-ask` 在无 TTY 下要么阻塞要么失败。所以真正的选项不是「严/松」，而是「**哪些操作族放行**」。

**候选方案**：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| A. 默认放行 + 每次弹窗醒目确认 | omp `--yolo`/opencode `--auto`，弹窗每次显示「该 harness 将无人值守执行文件/命令操作」，需勾选 | 可用；用户知情；实现简单 | 疲劳式确认；无细粒度 |
| B. 默认放行 + per-workspace 记忆 | A + 勾选「本 workspace 记住」（对应 backlog #3） | 少打扰 | 需持久化到 bag v2 |
| C. 默认最严 | 不传放行 flag | 「安全」 | agent 几乎无法完成任何 ticket；P0 验收不达 |
| D. 细粒度 allowlist | opencode `OPENCODE_PERMISSION` env / agent-level permissions；omp `--approval-mode write`（若其 write 层在无 TTY 下语义是 deny-write 则不可用，需实测） | 最小权限 | 每 provider 语义不同，需逐家实测；v1 两家已够做 |
| E. OS 级沙箱 | macOS `sandbox-exec` / 容器 | 真正的边界 | 破坏 harness 对工作区的写能力，且 sandbox-exec 已弃用 |

**建议**：v1 用 **A**（P0）→ **B**（P1，与 backlog #3 合并）；prompt 模板加「ticket/claim context 正文是数据不是指令」规则（零成本，立刻做）；**D 中的 omp `write` 模式在无 TTY 下的实际行为列为 U1 必做实测**，结论回填 descriptor 的 `permission_model`。修正 §九-10 文案中「默认最严」为「默认显式放行 + 知情确认」。

### 4.11 OMT_RUNTIME_DIR 凭据范围不变式（§九-11，P2，security）

**成因**：harness 子进程继承 `OMT_RUNTIME_DIR`，能读到 runtime dir 下的 `admin-grants.json`；担心提权。

**核验**：admin-grants 是 principal id 列表（`auth.rs:290-311`），principal 在 enroll 时由 daemon 依据连接侧信息生成并 pid 绑定（`daemon.rs` 头注释「refreshed to current pid」），token 只在内存；`kind` 字段虽是客户端自报（harness 可 enroll 成 `kind:"desktop"` 拿到 adapter 面 ui/home 操作族），但这不越过「同一 OS 账户」信任边界（security.md 既定）。**可利用性低，定位为「验证 + 记录」正确。**

**候选方案**：A. U13 加断言：harness 子进程树内 enroll 得到的 principal 不在 admin 列表（推荐）；B. daemon 校验 enroll 的 peer pid 祖先链拒绝 desktop 后代自称 desktop——过度设计；C. 为 harness 用独立 runtime dir——破坏 descriptor 发现，不可行。**建议 A**，并在 §4.1 的 env 合并规则中禁止用户 env 覆盖 `OMT_RUNTIME_DIR`。

### 4.12 secrets 落盘治理（§九-12，P3，security）

**成因**：`HarnessUserConfig.env` 可能含 API key；transcript header/`execs[]` 记录 argv/env；Failed 附 stderr 尾可能带 token。

**候选方案**：A. `harness/config.json` 0600 + header 只记 binary 与非敏感 args、不记 env 值 + stderr 尾按键值形态脱敏（推荐 v1）；B. env 值进 OS keychain（tauri-plugin-stronghold / keyring crate），config 只存键名——更安全但增加依赖与 UX（钥匙串授权弹窗）；C. 不落 env，要求用户在系统环境配置——与 R-A「Env KV 表」需求冲突。**建议 A**，B 记 backlog；脱敏规则至少覆盖 `(?i)(key|token|secret|password)[=:]\s*\S+` 与 `sk-…`、`Bearer …` 形态。

### 4.13 transcript 自动跟随（§九-13，P3）

**候选方案**：A. 视口近底才跟随，上滚解除并显示「回到最新 (N)」pill（业界标准，推荐）；B. 始终跟随 + 手动暂停开关；C. 不跟随。**建议 A**，与虚拟滚动一起在 U5 落地。

### 4.14 omp 注入选型（§九-14，manual 决策项）

**成因**：omp 的 MCP 配置只认文件（项目 `.omp/mcp.json` / 用户级 / profile 级），没有 opencode 那样的 env 内联注入；写用户工作区留痕、并发 exec 冲突、crash 残留三个问题。评审已核验 `--config` overlay 不含 MCP、`--profile omt` 会隔离 auth。

**影响**：不决策则 U3 无法开工（omp 是 v1 两家之一）。

**候选方案**：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **A. refcount 合并/备份/恢复** | 首个 exec 备份原文件到 **runtime dir**（不留在工作区）、合并写入 omt server；按 workspace 记 exec 引用计数，最后一个结束恢复；启动清扫残留标记 | 工作区最终无痕；并发安全；已收敛 | 实现最重（refcount 文件、崩溃标记、清扫）；exec 期间用户手改 mcp.json 会被恢复覆盖（需检测 mtime 变化则放弃恢复并提示） |
| B. `--profile omt` 隔离 | 全部配置写 `~/.omp/profiles/omt/` | 工作区零写入；无并发冲突 | auth 一起隔离，用户需在 omt profile 重新登录 omp（重大 UX 成本） |
| C. 永久写入项目 `.omp/mcp.json`（幂等合并，不恢复） | 首次派发写入 omt server 条目并保留 | 实现最简；无 refcount/恢复/清扫 | 工作区留痕（若 `.omp` 未 gitignore 会进 git）；daemon 未运行时用户自己开 omp 会看到 omt MCP 连接失败噪音（`omt mcp` 会等 5s 后退出） |
| D. 用户级 `~/.omp/agent/mcp.json` 写入 | 同 C 但在用户级 | 不碰工作区 | 影响用户所有 omp 会话；同样有 daemon 未运行时的噪音 |
| E. 上游诉求：omp 支持 env/flag 内联 MCP 配置 | 类 `OPENCODE_CONFIG_CONTENT` | 根治 | 不受控的时间线 |

**建议**：**A 为 v1 默认**，加两条细则：备份与 refcount 文件放 `<runtime-dir>/harness/omp-injection/<workspace-hash>/`（不在工作区）；恢复前比对 mtime/hash，若用户在 exec 期间手改则不覆盖、改为提示。**C 作为用户可选的「在本项目持久保留 omt MCP」开关**（设置页 Harnesses → omp 详情），选了就不再做恢复。E 同步提给上游。B 仅文档化为退路。

### 4.15 设计 §七 开放问题的处置

| 开放问题 | 现状 | 建议 |
|---|---|---|
| OQ1 dsh headless 形态 | 已解决 | — |
| OQ2 单 ticket 非 MCP 路径完成确认 | **被 Q1 取代**（无非 MCP 路径） | 标记关闭 |
| OQ3 harness 执行是否计入 nudge/stalled | v1 不计 | 同意。DSH 面的 idle-hook nudge 依赖 DSH 会话（`src/host/idle-hook.ts`），desktop 的对应物是「长时间无输出」提示；omp RPC `steer` 可做 desktop 侧 nudge，记 backlog #6 |
| OQ4 transcript 是否内嵌 RunsPanel | 计划选择独立 Agents tab | 补一条：RunsPanel item 行的 executor 谱系可点击跳转 Agents 面对应 Agent（依赖 §4.3-A 身份方案） |
| OQ5 omp 注入 | 见 §4.14 | — |

### 4.16 backlog 中反向影响 v1 的条目

- **排队语义（backlog #9）——「Running Agent 二次派发 → 拒绝」**：除拒绝（A）与 desktop 侧排队（B）之外，还有一个零协议变更的 **C：`run/add-members`**（daemon 已有，`dispatch.rs:242`）——向该 Agent 当前 run 追加成员，Agent 的 claim 循环自然会认领。这与「Agent 是一个持续认领的执行者」的心智模型更贴合，且 prompt 模板本就要求「继续下一项」。**建议 v1 弹窗三选项改为：查看运行中 Agent / 追加到该 Agent 的 run（add-members）/ 取消**，替代「追加新 Agent 同 run——默认禁止」。
- **home-id 变化后 agents/bindings 成孤儿（§3.1 v1 接受）**：A. 接受（v1）；B. 以 workspace 路径 hash 做二级索引，home-id 变化时提示「发现旧记录，是否迁移」；C. 存进 `.omt/`——违反 KD-D6。建议 A + 设置页「清理孤儿记录」按钮（P1 顺带）。
- **daemon 协议实体化（backlog #7）**：与 §4.1-C、§4.3-B 同源；建议合并成一份「运行时执行面协议扩展」评审：`run/ensure`、item 级 interrupt、`agent/*` 只读方法。v1 用本文各处的零变更方案即可，不阻塞。
- **provider 扩充（backlog #1）中 dsh 直装路径**：executor 谱系是 `kind:dsh` 会话而非 MCP，§4.3-A 的 `--actor` 注入对 dsh 不适用——dsh 插件侧的 actor namespace 由 DSH 会话决定。届时需要另一条关联路径（例如 desktop 以 `DSH_HOME` 隔离 + 读取 session 日志里的 session id）。提前记入 backlog #1 说明，避免届时发现 Agents 面对 dsh 失效。
- **Windows**：如 §2.2 所述，建议 v1 明示 macOS-only，删去半规格的 `taskkill` 分支。

---

## 5. 建议的文档修订清单（按优先级）

**阻断（开工前必改）**
1. 计划 §3.3 / U4：补「终止后释放」完整链——采纳 §4.3 A+C（`omt mcp --actor exec:<execId>` + admin report failed + retry），删除「run/interrupt 该 exec 持有的租约项」表述；U13 加 `omt mcp` 尊重 `--actor` 的 runtime 测试。
2. 计划 §3.3：commandId 改 per-step 派生（§4.1-A）。
3. 计划 §五：新增 U4a（agents.json 最小存储），U10 收缩（§4.2-A）；U6/U9 解环（§4.4-A）。
4. 计划 §九-14 → 正文：omp 注入定稿为 refcount + 备份落 runtime dir + 用户可选持久保留（§4.14）。

**高**
5. §3.2 状态模型改正交字段 `attachment`（§4.6-B），重画状态机图。
6. §九-10 文案修正为「默认显式放行 + 知情确认」，U1 加 omp `write` 模式无 TTY 实测项（§4.10）。
7. §3.4 `exec_start` 返回 `{execId, agentId, runId}`；补 `transcript_open`。
8. U6 弹窗三选项改为含「追加到该 Agent 的 run（add-members）」（§4.16）。
9. 设计文档标注被取代段落、更新 status（§2.2）。

**中/低**
10. §3.1 `binary_source: Detected|Manual`；env 合并规则与 `OMT_RUNTIME_DIR` 不可覆盖。
11. §3.6.6 agents.json 写合并（状态变化才写）。
12. §四 Agents 统计走 `run/get` 而非 `run/list`。
13. §七 风险表补 MCP 冷启动/MCP 初始化超时/prompt 模板与 harness 系统提示冲突/homeId 注入。
14. v1 明示 macOS-only。
15. §九 其余 P2/P3 项（7/8/9/12/13）按本文建议直接采纳，无需再议。

---

## 6. 决策记录（2026-09-06 逐项走查，全部已应用进计划正文）

| 项 | 决策 | 与本文建议的差异 | 落点 |
|---|---|---|---|
| 阻断 1–4 + §九-14 | 采纳本文方案 | — | §3.3 / U3 / U4 / U4a / U6 / U9 / U10 / 分期 |
| §九-5 U1 验收收缩 | 采纳 A | — | U1 Verification |
| §九-6 终态语义 | **B 正交字段** `attachment: Live\|Orphan\|None` + `Killed(KillReason)` | — | §3.2 struct + 状态机图 |
| §九-7 弹窗态机 | A 禁用+派发中，成功切 Agents，5s 降级展开进度 | — | U6 Approach |
| §九-8 终止控件 | A 行内两段式确认 | — | U5 Approach |
| §九-9 失败可见性 | **C badge + 系统通知** | 比建议多一层：系统通知作为 P1 新单元 | U5（badge）+ **新增 U16**（tauri-plugin-notification） |
| §九-10 审批策略 | **B 显式放行 + per-workspace 记住，P0 就做** | 比建议提前：per-workspace 记忆不等 P1 | U6 + bag v2 `approvalAck` + §3.6.11 + U1 实测 omp `write`；backlog #3 收缩为细粒度 allowlist |
| §九-11 凭据不变式 | A 仅测试断言 | — | U13 + §3.1 保留键 |
| §九-12 secrets | A 0600 + 不记 env 值 + stderr 脱敏 | — | §3.1 / §3.6.10 / U1 / U2 / U4a |
| §九-13 自动跟随 | A 贴底 + pill | — | U5 Approach |
| 非阻断五条 | 全部进正文 | — | §3.1 BinarySource + env 规则 / §四 run/get / §七 #8–11 / §3.6.3 macOS-only |
| 提交 | 新建 feature 分支提交，后续开发在该分支进行 | — | `feat/desktop-agents-dispatch` |

计划 §九 已无待决项；设计文档已加取代标注。下一步：在 feature 分支上按 P0 排序（U15 single-instance → U1 → U4a → U2 → U3 → U4 → U5 → U6 → U9 → U7 → U8）开工。
