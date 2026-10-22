---
title: 执行面下沉到 omt + 主控 agent 编排 - 设计
type: design
status: sub-project-1-done
date: 2026-10-20
supersedes_in_part: docs/plans/2026-09-05-1245-feat-desktop-harness-executor-design.md（KD-D1 / KD-D3 / KD-D7，见 §1.2）
related:
  - docs/plans/2026-10-19-harness-benchmark-design.md（§0 / §6.3 的 crate 搬迁与本设计子项目 1 合并）
---

# 执行面下沉到 omt + 主控 agent 编排 - 设计

## 0. 背景与目标

desktop agents dispatch 已全量落地（PR #9），但 harness 的拉起、执行、日志全在桌面端 Tauri 后端里：主控 agent（Claude Code）经 `omt mcp` 只能建 ticket / run、查进度，**拉不起任何 harness**（`exec_start` 是 tauri command；`node/execute` 为 adapter_only）。本设计把执行面下沉为 omt 的能力，让主控 agent 成为多 agent 协作的编排者，桌面端成为全过程的可视化与偶尔控制面。

**目标（用户需求原文的四点）**

1. **桌面端能力不变**：仍可视化查看 harness 运行情况、偶尔控制（派发、停止、续跑、裁决）。
2. **主控经 MCP / CLI 直接拉起其他 harness**，拉起细节（argv、MCP 注入、审批模式、流解析、收尾）由 omt 控制。
3. **主控经 MCP / CLI 获取可用 provider 与模型**，自行编排（哪个 harness 担任什么角色、用什么 provider / 模型）；编排文件在 omt 中可视化。
4. **主控自身也进入 omt 的 agent jsonl 日志**，从 ticket 到执行的全过程可在一处掌控。

**非目标**

- omt 不做调度引擎：编排决策由主控 agent 做，omt 只执行、记录、展示（KD-E8）。
- 不支持远程 / 多机执行：短期仅本地开发机使用，并发规模小（个位数 exec）。
- daemon 协议不新增 `exec/*` 方法（KD-E1、KD-E9）。

## 1. 决策记录

### 1.1 新决策

| ID | 决策 | 理由 |
|---|---|---|
| **KD-E1** | **执行面 = 每 exec 一个独立 shim 进程**（`omt exec-shim`，`omt` 二进制的隐藏子命令）；状态全落 runtime dir 文件；停止 = 信号；实时 = 尾随 jsonl | daemon 零改动——保住「纯协调面 / home 数据唯一写者 / idle 可退出」三条不变式；单 exec 崩溃隔离；任一面（桌面 / CLI / MCP）都能发起，发起方退出后执行继续。否决：放进 daemon（破坏 idle 退出、升级即孤儿化、执行面 bug 拖垮 DSH 共用协调面）、常驻 `omt-execd`（第二套生命周期，复杂度最高） |
| **KD-E2** | **存储按 agent 拆分**：`agents/<home>/<agentId>/agent.json` + 同目录每 exec 一个 jsonl | session 复用的单位是 agent（续跑 = 同 agent 追加 exec）；按 agent 拆后不同 agent 零争用，消除 `agents.json` 全局锁（F-U4a-5）；每 exec 独立 jsonl 保留各自 header / exit / 用量，失败归因与崩溃对账简单，链路由视图层拼接 |
| **KD-E3** | **jsonl 文件是 transcript 的唯一权威**；桌面端改为尾随文件推送前端，不再依赖进程内 Channel | 执行进程与查看进程解耦后，文件是唯一跨进程、可回放、可离线分析（评测分析器）的通道 |
| **KD-E4** | **CLI / MCP 新增「本地工具族」**（harness / exec / agent / orchestration），不经 daemon，在 parity 矩阵中单独声明 | 执行面不属于 home 数据，不应进 daemon 协议；`omt mcp` 进程本身就在本机，可直接调用共享 crate |
| **KD-E5** | **派发能力默认关闭**，由 `exec/policy.json` 显式开启并约束 | 一个 agent 以无人值守模式拉起另一个 agent 是权限放大——原设计刻意把 `node/execute` 定为 adapter_only、MCP 凭据去掉 home/ui 族，本设计必须保留等价的闸门 |
| **KD-E6** | **主控接入先做「附着」（方案 B）**：Claude Code SessionStart hook → `omt agent attach`，附着模式 shim 读取 Claude Code 会话 jsonl 转写入 omt | 保留用户在终端直接与主控交互的工作方式 |
| **KD-E7** | **「由 omt 拉起主控」（方案 A）设计保留、开发暂缓**（§3.8） | 防止需求随时间遗忘；A 依赖子项目 1、2 的全部成果，且交互形态需另议 |
| **KD-E8** | **编排文件 = 声明与关联记录，不是调度引擎** | 主控 agent 是引擎；omt 只记录「角色 → harness / 模型」与「ticket / run → 角色」，并把每个 exec 关联回编排 |
| **KD-E9** | **daemon 零改动**；唯一的 runtime 侧改动是 `omt` CLI / `omt mcp` 引入新 crate 与本地工具 | 与 KD-E1 同源 |

### 1.2 被修订的旧决策（执行器设计 2026-09-05）

| 旧决策 | 修订后 |
|---|---|
| KD-D1 执行器放 desktop、不放 daemon | 执行器**既不在 desktop 也不在 daemon**，而在每 exec 的 shim 里；「不放 daemon」的理由完整保留 |
| KD-D3 transcript 走 desktop 进程内 Channel | transcript 以 jsonl 文件为权威，桌面端尾随文件（KD-E3）；「不进 daemon 事件面」保留 |
| KD-D7 会话与 app 同生命周期（桌面退出 = 执行终止） | **桌面退出不再终止执行**（显式行为变化，§3.4）；租约过期 → interrupted → retry 的恢复路径保留，用于 shim 崩溃 |

## 2. 架构总览

```
 主控 agent（Claude Code，终端交互）        桌面端（Tauri，可视化 + 偶尔控制）
   │  hook: omt agent attach                 │  tauri command（薄封装）
   │  MCP:  omt mcp ──┐                      │
   │  CLI:  omt ...   │                      │
   ▼                  ▼                      ▼
 ┌──────────────────────── 共享 crate ─────────────────────────┐
 │ omt-harness：REGISTRY / 探测 / adapters / 模型列表 / 脱敏     │
 │ omt-exec：launcher / shim / mcp_injection / agent 存储 /      │
 │           policy / orchestration 存储                         │
 └───────────────┬──────────────────────────────────────────────┘
                 │ launcher 拉起（分离进程）
                 ▼
   omt exec-shim <execId> ──spawn──▶ harness（pi / omp / opencode / …）
     │ 写 agents/<home>/<agentId>/<execId>.jsonl         │ MCP（注入的 omt mcp）
     │ 收尾：更新 agent.json、释放注入、释放租约           ▼
     └──────── JSON-RPC（kind:"cli"）──────────▶ omt-daemon（home 数据唯一写者，零改动）
```

## 3. 组件设计

### 3.1 crate 拆分（子项目 1；与评测设计 §6.3 合并为一次搬迁）

- **`crates/omt-harness`**：`HarnessDescriptor` / `REGISTRY`、`which_binary` / `probe_version`、`adapters`（新增 claude-code 会话格式，§3.7）、`parse_models_output`、`sanitize_line` / `sanitize_args`。纯库，无进程管理。
- **`crates/omt-exec`**：launcher、shim 主循环、`mcp_injection`、agent 存储（原 `agents.rs` + `atomic_json.rs`）、policy、orchestration 存储。依赖 `omt-harness`、`omt-client`。
- **依赖方向**：`omt-runtime`（`omt` CLI、`omt mcp`、`omt exec-shim`）与 `apps/desktop/src-tauri` 依赖以上两者；**daemon 代码路径不引用**（`omt-daemon` 二进制不链接 `omt-exec`）。
- 桌面端 `harness.rs` / `executor.rs` / `adapters.rs` / `mcp_injection.rs` / `agents.rs` / `atomic_json.rs` 的内核迁出，只留 tauri command 包装。

### 3.2 launcher 与 shim 生命周期

**启动（launcher，任一面调用，同步返回）**

1. 校验 policy（§3.6）→ 解析 harness（`resolve_launch`）→ 组装 argv（`compose_launch_args`）。
2. 新建或复用 agent（续跑 = 复用，§3.3），持 agent 目录锁写入 exec 记录（status = `starting`）。
3. 以 `setsid` 拉起分离的 `omt exec-shim --home <homeId> --agent <agentId> --exec <execId>`，记录 shim pid + 进程起始标识，返回 `{agentId, execId}`。发起方此后可退出。

**shim 主循环（每 exec 一个）**

1. 执行原 dispatch chain 的 daemon 侧步骤（ensure run 等），注入 MCP（原 `mcp_injection`，per-exec actor `exec:<execId>`）。
2. spawn harness（`setpgid`，子进程自成进程组），stdout / stderr 逐行经 `adapters::parse_line` → 追加 `<execId>.jsonl`（header / 事件 / exit 与现格式一致）。
3. maxDuration 计时、usage 去重（F-U12-3 逻辑）在 shim 内。
4. 收尾（原 `finish_terminal`）：分类终态、更新 agent.json、释放注入、经 daemon 释放该 exec 持有的租约，然后退出。

**控制**

- **停止**：向 shim pid 发 SIGTERM（先以进程起始标识校验，防 pid 复用）→ shim 对 harness 进程组做 TERM → 超时 KILL 升级，记 `killed(user)`。
- **状态**：读 agent.json + shim 存活检查（pid + 起始标识）。
- **对账**：任一面启动或 `omt exec list` 时，发现 status 为活但 shim 已死 → 按原 U10 对账逻辑收尾（释放租约、标记 failed / interrupted）。原 `kill_orphan` 降级为「shim 已死但 harness 进程组仍活」的清理。
- **daemon 连接**：shim 以 `kind:"cli"` 握手（`omt-client` 现有 discover-or-spawn），只用 agent_available 方法。

### 3.3 存储布局（KD-E2）

```
<runtime-dir>/
  agents/<homeId>/<agentId>/
    agent.json          # AgentRecord（原结构）+ execs[]；新增 parentAgentId / orchestrationId / role / kind
    <execId>.jsonl      # 每次执行一个
    .lock               # agent 目录锁（flock）
  exec/policy.json      # §3.6
  orchestrations/<homeId>/<orchId>.json   # §3.9
  harness/config.json   # 不变
```

- **session 复用**：续跑在同一 agent 下追加 exec、新增 jsonl；**同一 agent 同时只允许一个活跃 exec**（目录锁 + status 检查），防止两个 harness 进程 resume 同一 session id。
- **agent 序号**（`Agent N`）：`agents/<homeId>/seq` 单独计数文件，持锁自增。
- **迁移**：首次启动检测旧 `agents/<homeId>/agents.json` → 逐 agent 拆出目录、移动对应 `transcripts/<homeId>/<execId>.jsonl` → 旧文件改名 `agents.json.migrated`。迁移幂等、可中断重入。
- **前置修复**（多进程写入后变关键）：F-U4a-3（读路径 corrupt rename 移入锁内）、F-U4a-4（rename 前 `sync_all`）。F-U4a-5（全局锁）因按 agent 拆分而消解。

### 3.4 桌面端改造（子项目 1 验收面）

- `exec_start` / `exec_stop` / `exec_transcript_tail` / `exec_kill_orphan` / `agents_*` 改为调用 `omt-exec`；launcher 拉起的是桌面端已打包的 `omt` sidecar（`tauri.conf.json` `externalBin` 已含 `binaries/omt`）。
- 实时 transcript：后端尾随 jsonl（文件事件 + 200ms 轮询兜底），以既有事件形状推送前端——**前端组件与 vitest 不变**是验收门。
- **行为变化（显式）**：桌面退出不再终止执行。原「退出钩子 kill_all」删除；Agents 面板新增「停止全部」按钮作为替代控制。
- 新增视图（子项目 3、4）：Agents 面板显示 `kind`（harness / master）与父子树；编排视图（§3.9）。

### 3.5 CLI / MCP 本地工具族（子项目 2）

| CLI | MCP 工具 | 说明 |
|---|---|---|
| `omt harness list [--json]` | `harness__list` | 已启用 harness、版本、能力（resume / json / 审批模式） |
| `omt harness models <id> [--json]` | `harness__models` | 该 harness 可用 provider / 模型 |
| `omt exec start --harness <id> [--model] [--run <runId> \| --node <nodeId>] [--agent <agentId>（续跑）] [--orchestration <id> --role <name>]` | `exec__start` | 返回 `{agentId, execId}` |
| `omt exec stop <execId>` | `exec__stop` | |
| `omt exec status <execId>` / `omt exec list` | `exec__status` / `exec__list` | |
| `omt exec tail <execId> [--since <seq>]` | `exec__tail` | 返回归一化事件，分页 |
| `omt agent attach --from-hook` / `omt agent detach --from-hook` | — | 仅供 hook 使用（§3.7） |
| `omt orchestration put / get / list` | `orchestration__put / get / list` | §3.9 |

- **parity**：本地工具在 `schema/parity.schema.json` 新增独立分类（如 `local_exec`），`mcp_spec` 的 tools/list 断言改为「daemon agent_available − home 族 ∪ 本地工具族（policy 开启时）」。
- **`node/execute` 问题**：原 dispatch chain 调用 `node/execute`（adapter_only，仅 dsh / desktop）。CLI / MCP 发起的派发**不调用它**，改由 run 路径承担（`run/create` + `run/claim` 标记 in_progress；审计由 agent.json 的 exec 记录 + 租约 actor `exec:<id>` 承担）。子项目 2 先实测 `run/claim` 是否已置 in_progress；若否，再议是否需要 daemon 侧补一个 agent_available 的轻量标记。

### 3.6 安全约束（KD-E5）

`<runtime-dir>/exec/policy.json`（桌面端设置页可编辑）：

```jsonc
{
  "allowFromCli": true,
  "allowFromMcp": false,            // 默认关闭；主控派发需显式开启
  "harnesses": ["pi", "omp", "opencode"],   // 白名单
  "workspaces": "homeWorkspaceOnly",        // 只允许在 home 所属 workspace 内执行
  "maxConcurrent": 4,
  "maxDurationSec": 1800,
  "approvalMode": "inherit"         // 沿用 harness 配置；不允许经 MCP 参数提升
}
```

- 派发参数**不能**放宽 policy（例如 MCP 调用不能指定更宽的审批模式）。
- 信任门（`awaiting_confirmation`）与 per-exec actor 身份照旧，人工裁决仍在桌面端。
- 每次派发在 agent.json 记录发起方（`launchedBy: desktop | cli | mcp`）与 `parentAgentId`。

### 3.7 主控附着（方案 B，子项目 3）

**接入**：用户在 Claude Code `settings.json` 配置 hook（omt 提供 `omt agent hook-config` 打印片段）：

```jsonc
{ "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "omt agent attach --from-hook" }] }],
    "SessionEnd":   [{ "hooks": [{ "type": "command", "command": "omt agent detach --from-hook" }] }]
} }
```

- `--from-hook` 从 stdin 读 hook 输入（`session_id`、`transcript_path`、`cwd`），按 cwd 解析 home（与 `docs/runtime/config.md` 同一规则）。
- **身份**：以 Claude Code `session_id` 为 agent 键（`kind: master`）。同一 session 再次附着（如 `claude --resume`）→ 同 agent 追加 exec；若 resume 产生新 session id，则按实测结果决定关联方式（§8 实测项）。
- **附着模式 shim**：不 spawn 子进程，而是尾随 `transcript_path`，经 claude-code adapter 转为统一事件写入 omt jsonl；SessionEnd hook 或 Claude 进程消失时收尾。
- **父子关联**：attach 记录调用方祖先进程链；`omt mcp` 处理 `exec__start` 时取自身祖先链，与已附着主控求最近公共祖先（即 Claude 进程，校验存活与起始标识），命中则自动写 `parentAgentId`。兜底：`exec__start` 接受显式 `parentAgentId`；SessionStart hook 的 stdout 会进入主控上下文，attach 输出 `omt agentId: <id>` 供主控引用。
- **claude-code adapter**：解析会话 jsonl（user / assistant 消息、text / thinking / tool_use / tool_result 块、`message.usage`）。该格式是 Claude Code 内部格式、可能随版本变化——纳入评测语料库（`fixtures/harness/claude-code/<version>/`），走同一套回放回归。

### 3.8 主控由 omt 拉起（方案 A，设计保留、开发暂缓）

**目的**：脚本化或无人值守的编排场景——从桌面端或 CLI 启动一个主控，由它编排下游 harness。

**设计要点**

- REGISTRY 新增 `claude` descriptor：binary `claude`；headless `-p`；json `--output-format stream-json --verbose`；model `--model`；会话 `--session-id <uuid>` 预指派（KD-D8 已论证最稳）、续跑 `--resume <id>`；审批经 `--permission-mode` / `--allowedTools`。
- **MCP 注入**：用 `--mcp-config <临时文件>`，不改写 workspace 文件（比 omp / pi 的工作区注入简单），per-exec actor 同其他 harness。
- **流格式**：stream-json（与会话文件不同）→ 新增 `claude-stream` adapter；与 §3.7 的会话文件 adapter 共享内容块解析。
- **身份**：与其他 exec 一致（`kind: master`、`launchedBy: desktop | cli`）；下游派发的父子关联直接按 shim 祖先链命中，无需 hook。
- **交互限制**：headless 主控不能在终端对话。是否在桌面端提供「向主控追加指令」（经 `--resume` 新 exec）另行设计。
- **前置**：子项目 1、2、3 完成。**状态：暂缓开发**，本节仅防需求遗忘。

### 3.9 编排文件与可视化（子项目 4）

`orchestrations/<homeId>/<orchId>.json`（schema：`schema/orchestration.schema.json`）：

```jsonc
{
  "id": "ORCH-0001", "homeId": "h_…", "title": "Epic 12 实施",
  "createdBy": "agent-…",                     // 主控 agentId
  "roles": [
    { "name": "implementer", "harness": "pi", "model": "kimi-coding/k3",
      "instructions": "按 ticket 实施并自测", "approvalMode": "inherit" },
    { "name": "reviewer", "harness": "opencode", "model": "glm-5.2",
      "instructions": "审查 implementer 的改动" }
  ],
  "assignments": [
    { "target": { "runId": "RUN-0031" }, "role": "implementer" },
    { "target": { "nodeId": "TICKET-0042" }, "role": "reviewer", "after": ["RUN-0031"] }
  ],
  "updatedAt": "…"
}
```

- 主控经 `orchestration__put` 写入；`exec__start --orchestration --role` 时，launcher 从角色取默认 harness / 模型（参数可覆盖，但须在 policy 内），并在 exec 记录写 `orchestrationId` / `role`。
- `after` 只做展示与提示，不由 omt 强制调度（KD-E8）。
- **桌面端编排视图**：角色卡片（harness / 模型）→ 分配的 ticket / run → 各 exec 实时状态与 transcript 入口；与 Agents 面板的「主控 → 子 agent → ticket」树互相跳转。

## 4. 典型数据流（主控派发一次）

1. 主控 `harness__list` / `harness__models` → 选定 pi + k3。
2. `orchestration__put`（角色 implementer = pi / k3）；`run__create`（daemon，既有）。
3. `exec__start {runId, orchestration, role: "implementer"}` → `omt mcp` 校验 policy → launcher 建 agent（`parentAgentId` = 主控）→ 拉起 shim → 立即返回 execId。
4. shim 注入 MCP、拉起 pi；pi 经注入的 omt MCP `run__claim` → 干活 → `run__report`。
5. 主控轮询 `exec__status` / `run__get`，或 `exec__tail` 查看过程；桌面端实时显示同一 jsonl。
6. pi 退出 → shim 收尾。若走入信任门，桌面端人工裁决。

## 5. 错误处理

| 场景 | 处理 |
|---|---|
| shim 崩溃 | 下次任一面对账：释放租约 → item interrupted → 可 retry；harness 进程组若仍活，由 `kill_orphan` 清理 |
| harness 卡死 | shim 的 maxDuration → TERM → KILL |
| daemon 不可达 | shim 启动阶段失败即以 failed 收尾（不 spawn harness）；收尾阶段失败则记录待对账标记，下次对账重试租约释放 |
| policy 拒绝 | 结构化错误（含被拒字段），MCP 映射为 tool error |
| agent 已有活跃 exec 时续跑 | 拒绝（`agent-busy`） |
| 附着的会话文件格式不可解析 | adapter 回落 raw 事件，不中断（KD-D4 同理） |
| 旧存储迁移中断 | 迁移幂等，下次启动从断点续做 |

## 6. 测试

- 迁移的 executor / mcp_injection / agents 测试随代码进新 crate（继续用假 harness）。
- **shim 生命周期**：发起方退出后执行继续；SIGTERM → TERM / KILL 升级；shim 被 KILL 后的对账；pid 复用防护。
- **存储**：多进程并发写不同 agent 无争用；同 agent 并发续跑被拒；旧 `agents.json` 迁移幂等。
- **CLI / MCP**：本地工具族在 policy 关闭 / 开启下的 tools/list 与调用；parity 断言更新。
- **adapter**：claude-code 会话格式 fixture 回放（评测语料库机制）。
- **桌面端**：`apps/desktop` vitest 全绿 + 手工验收「派发 → 实时查看 → 停止 → 续跑 → 信任门裁决」与改造前一致。

## 7. 子项目拆分与顺序

每个子项目单独走 plan → 实施 → review，验收通过再进下一个。

| # | 子项目 | 验收 |
|---|---|---|
| 1 | **执行面下沉**：两个 crate、shim、存储拆分与迁移、前置修复、桌面端改走 crate | 桌面端功能不变（§3.4 验收门）；关闭桌面端后执行继续。**已完成（2026-10-21，实现序列 d6d07cd 起，自动化验证与待手工验收清单见 findings log「执行面子项目 1 验收」）** |
| 2 | **CLI / MCP 本地工具族 + policy** | 主控经 MCP 查询 harness / 模型并派发一次 pi 冒烟，桌面端可见 |
| 3 | **主控附着（方案 B）** | Claude Code 会话出现在 Agents 面板，其派发的 exec 挂在其下 |
| 4 | **编排文件与可视化** | 主控写入的编排在桌面端可视化，exec 与角色关联 |
| 5 | **主控由 omt 拉起（方案 A）** | **暂缓**（§3.8） |

## 8. 实测项与开放问题

- **resume 后的 usage 语义**：pi / omp 续跑时 usage 是从零计还是延续 session 累计——决定 agent 级用量是「末值」还是「差分求和」（子项目 1）。**已实测（2026-10-22）**：pi、omp 均为逐消息计、续跑不延续累计，agent 级用量取各 exec 合计之和（数据见 findings log「执行面子项目 1 验收」§8 实测结果）。
- **Claude Code resume 与 session id**：`claude --resume` 沿用原 session 文件还是新开 session id（子项目 3）。
- **`run/claim` 是否置 node in_progress**：决定 CLI / MCP 派发能否完全绕开 `node/execute`（子项目 2）。
- **hook 进程祖先链**：hook 经 shell 执行，确认最近公共祖先稳定命中 Claude 进程（子项目 3）。

## 9. 与评测设计的关系

- 评测设计 §0 / §6.3 的 `crates/omt-harness` 搬迁即本设计子项目 1 的一部分，合并执行。
- 评测 code review 的 F-BENCH-5（探针自带 registry 副本）在 crate 搬迁后自然消解；F-BENCH-3 / F-BENCH-4 应在搬迁前修复，以免随代码一起迁移。
- 评测 runner（§4.4）可直接复用本设计的 launcher 与 `exec tail`，不必另写派发逻辑。
