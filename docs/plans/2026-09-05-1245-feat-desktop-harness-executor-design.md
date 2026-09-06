---
title: Desktop Harness 执行器与 Transcript 实时显示 - 分析与设计
type: design
date: 2026-09-05
status: accepted-with-supersessions（2026-09-06：被实施计划 Q1 取代的段落已就地标注，见「取代说明」）
superseded_by: docs/plans/2026-09-06-1100-feat-desktop-agents-dispatch-implementation-plan.md
review: docs/plans/2026-09-06-1530-review-desktop-agents-dispatch-plan-review.md
related:
  - docs/plans/2026-08-25-2209-feat-multi-surface-runtime-desktop-distribution-plan.md
  - docs/runtime/architecture.md
  - docs/runtime/protocol.md
---

# Desktop Harness 执行器与 Transcript 实时显示 - 分析与设计

## Goal Capsule

- **目标**：让 omt desktop 在执行 ticket 时可以选择系统当前安装的 agent harness（Claude Code / Codex / OpenCode / DSH 等）实际拉起执行，并把执行的 transcript 实时流式显示在桌面端。
- **性质**：本文是分析与设计文档（design draft），不是 implementation-ready plan；实施计划待设计评审后另立。

> **取代说明（2026-09-06）**：实施计划已定稿并做出 Q1–Q4 决策。以下段落被计划取代，实施以计划为准，本文保留原文供追溯：
> - §3.2 结论中「单 ticket 执行退化为 prompt 执行 + 状态收尾」与「模式 A 作兜底」→ **计划 Q1：统一 run 化，不做直派旁路**；
> - §5.1 `HarnessConfig{resolvedFrom:'install'|'dev'}` 与 dsh dev 模式 → **计划 §3.1 `HarnessUserConfig` + bag v2 `{defaultHarness}`**，dsh 出 v1；
> - §5.4「单 ticket 执行（非 run）」段 → 取代（无非 run 路径）；
> - §5.7「`omt mcp` 已支持 name」→ 实为硬编码（`mcp.rs:96`），计划 U3 改为 `omt mcp` 尊重 `--actor` 并以 `exec:<execId>` 为执行者身份；
> - §七 开放问题 2、5 → 已关闭（Q1；计划 U3 omp refcount 注入）；
> - §八 分期（P0 = 单 ticket prompt 执行、MCP 可选）→ **计划 §五分期（P0 = 统一 run 化，MCP 必选）**。
> - v1 provider 仅 opencode / omp（计划 Q3）；本文 claude / codex / dsh 相关内容作为 backlog #1 的输入保留。

---

## 一、现状分析

### 1.1 架构事实

当前进程拓扑（docs/runtime/architecture.md）：

```
DSH 插件(client-ts) │ Desktop(Tauri) │ MCP harness │ omt CLI
        └────── JSON-RPC over UDS（runtime dir 描述符发现）──────┘
                            ▼
              omt-daemon（唯一写者：SQLite+Markdown、run 状态机、
                          lease 租约、事件 outbox；纯协调面）
```

与本需求直接相关的三个事实：

1. **daemon 从不执行 ticket，也从不 spawn 任何 agent 进程。** 全 crates 树内唯一 `Command::new` 是 `omt daemon-start` 自举（`crates/omt-runtime/src/cli/mod.rs:813`）。run 机制的"执行"发生在外部模型会话里：agent 通过 `run/claim`（绑定连接凭据的 actor namespace + attempt-fenced lease，`crates/omt-runtime/src/dispatch.rs:2059`）认领 → 在自己的会话里干活 → `run/report` 回报（租约四重防栅）。`omt mcp`（`crates/omt-runtime/src/mcp.rs`）是外部 harness 接入的正式通道：stdio MCP server，enroll 为 `kind:"mcp"`、受限凭据（node/run/events 操作族，无 home/ui）。

2. **desktop 的"执行"按钮是空壳。** `DetailPanel.execute()` → `node/execute`（`crates/omt-runtime/src/dispatch.rs:1428-1470`）只做三件事：归档拒绝、置 `in_progress`、写 `exec:<nodeId>` 审计 meta。不 spawn 进程、不产生任何输出流。原因是 desktop 没有聊天会话——DSH 插件侧"执行 = 状态标记 + 引用塞输入框提交给当前会话模型"的语义（`src/client/components/DocPanel.tsx:287-301`）在 desktop 无处落地。

3. **协议与 UI 都没有 transcript 通道。** daemon 事件全集只有 `node.changed / run.changed / run.item_changed / attention.raised(保留未发射) / snapshot.resync / node.quarantined`（`schema/events.schema.json`）；desktop 的 `events_subscribe`（`apps/desktop/src-tauri/src/rpc_bridge.rs:321-370`）把事件转 `omt://event` 后，前端只用于 300ms 防抖整体重拉列表（`apps/desktop/src/store.ts:286-308`），不渲染内容本身。

### 1.2 已经有的积木

| 积木 | 位置 | 状态 |
|---|---|---|
| per-workspace harness 配置（type/mode/installPath/devPath） | `apps/desktop/src/HarnessSelect.tsx`，持久化于 `tauri:harness` bag | **已实现，但全仓无任何消费者**（"写后忘"配置） |
| harness 探测（PATH 查找 + `--version` 探针、增强 PATH） | `apps/desktop/src-tauri/src/rpc_bridge.rs:84-104`（`which_binary`/`augmented_path`/`probe_version`） | 已实现，但类型硬编码 `dsh \| opencode` 两处（TS 枚举 + Rust match） |
| 子进程 spawn 基建（piped stdout/stderr 读线程、超时 kill、setsid 脱离进程组） | `rpc_bridge.rs:125-182`（`run_with_timeout`）、`daemon.rs:74-100` | 已实现，但目前只服务 `dsh_plugin_install`（一次性命令，非流式） |
| 实时事件通道（daemon outbox → `events/resume` cursor → Tauri event） | `rpc_bridge.rs:321-370` | 已实现，仅用于刷新重拉 |
| run 控制面（create/control/claim/report 协议 + 状态机 + 租约 + 信任门） | daemon 全套 | 已实现，desktop 可调用 `run/control`，claim/report 协议层无 client kind 限制 |
| MCP agent 面（17 工具，含 `run__claim/run__report/node__update`） | `crates/omt-runtime/src/mcp.rs` | 已实现，随 `omt` CLI 二进制分发；**desktop 目前只打包 `omt-daemon` sidecar，未带 `omt`** |
| 租约 TTL 15min → 过期 → interrupted → retry 的崩溃恢复路径 | daemon | 已实现（desktop/执行器崩溃后的自然兜底） |

### 1.3 缺失的三块

1. **执行器（executor）**：没有任何组件负责"拉起某个具体 harness 进程、喂 ticket 上下文、管理其生命周期"。
2. **Transcript 通道**：harness 进程的流式输出没有采集、规范化、推送、渲染的任何一环。
3. **Harness 注册表**：`dsh | opencode` 硬编码无法扩展到 claude/codex 等；配置结构与"headless 执行"所需信息（无头命令模板、流格式、MCP 注入方式）不匹配。

---

## 二、需求定义

- **R1** 执行 ticket 时可选择执行者：系统已安装的 agent harness。选项动态反映本机安装情况；每 workspace 可设默认，单次执行可覆盖。
- **R2** 执行实时可见：transcript 流式显示（assistant 文本、工具调用、错误等），无需等执行结束。
- **R3** 单 ticket 执行与 run 成员执行都支持；run 执行遵循既有状态机与租约纪律，不旁路。
- **R4** 执行会话可终止；desktop/执行器崩溃后，run 侧能通过既有机制恢复（interrupted → retry）。
- **R5** 不破坏 daemon 的安全模型与"纯协调面"定位；harness 获得的权限不高于本 OS 账户 + MCP 受限面。
- **R6**（隐含）transcript 可回看（至少限本次 app 生命周期；持久化回放为二期）。
- **R7** 会话连续性：首跑保留 harness 产生的 session 并记录其 id；后续对同一 ticket 的执行可指定在该 session 中继续（保留 harness 侧上下文）；不支持续跑的 harness（dsh headless）UI 明示降级，上下文由 ticket 正文/进度记录承载。

### 范围边界

- **不做**：daemon 侧 spawn agent（见 3.1 论证）；跨 surface 的 transcript 共享协议（远期扩展，见 §7）；harness 会话的交互式介入（中途注入指令，远期）。
- **降级接受**：无 headless 接口的 harness（如当前形态的 dsh）以 raw 输出透传显示，体验降级但可用。

---

## 三、方案选型

### 3.1 执行器放在哪：desktop Tauri Rust 侧（推荐）

| 方案 | 评估 |
|---|---|
| **A. desktop src-tauri 内新增执行器模块（推荐）** | desktop 已有全部积木（spawn/PATH/探测/事件通道）；执行会话与 UI 同生命周期，归属自然；不触碰 daemon 安全模型 |
| B. daemon 侧新增执行器 | 违背"daemon 纯协调面"的既定架构原则（mcp.rs R18、architecture.md 关键不变式、security.md OS 账户信任边界）；daemon 有 idle watchdog（无人连接即退出），长执行会钉住 daemon 或被误杀；单写者 + canary 安全态势被破坏 |
| C. 独立 executor 进程（desktop 拉起） | 多一个常驻进程与一套 IPC，收益仅是"desktop 关了执行继续"——但那恰恰与 R4 的租约恢复路径重复（desktop 崩溃后租约过期 → interrupted → retry 已兜底），v1 不值得 |

### 3.2 与 run 纪律的集成模式：harness 自 claim/report（推荐）

| | 模式 A：desktop 代 claim/report | **模式 B：harness 经 MCP 自 claim/report（推荐）** |
|---|---|---|
| 机制 | desktop 先 `run/claim`（executor=`desktop:<pid>`），把 ticket 上下文拼进 prompt，harness 纯干活，desktop 按退出码 `run/report` | desktop 给 harness 注入 `omt mcp` 配置 + 简短指令 prompt；harness 自己 `run__claim`（executor=其 MCP 进程的 `mcp:<pid>`）→ 干活 → `run__report` |
| 信任模型 | 退出码 ≠ 实际完成，desktop 代报告是"代言"；租约在 desktop 名下，harness 自己无法 report（fencing 校验 actor 不匹配会拒） | 认领者 = 执行者 = 报告者，租约/信任门/审计谱系全部自洽，与 DSH 侧模型会话执行完全同构 |
| 崩溃恢复 | desktop 崩溃 → 无人 report → 租约过期兜底 | 任意一方崩溃 → 租约过期 → interrupted → retry，路径最短 |
| 依赖 | 无新依赖 | 需要分发 `omt` CLI 二进制（第二 sidecar）；各 harness 的 MCP 注入方式不同（见 5.4） |
| transcript 之外的可观测 | 仅 desktop 侧 | desktop 经既有 `run.item_changed` 事件观察状态流转 |

**结论**：run 执行走模式 B；~~单 ticket 执行（不属于 run）退化为 prompt 执行 + 状态收尾（见 5.4）。模式 A 保留为实现兜底：若某 harness 无法注入 MCP，用 desktop 代 claim。~~ **［已被计划 Q1 取代：一切派发统一 run 化，无 MCP 则拒绝派发（U3「绝不 spawn 无 MCP 的 harness」）］**

### 3.3 Transcript 采集：结构化 adapter + raw 透传兜底（双层）

- **结构化层**：每 harness 一个 adapter，把其 headless 流格式解析成统一 transcript 事件（见 5.3）。
- **raw 兜底层**：解析失败或未知格式 → 原样透传 stdout（可选剥离 ANSI）。保证**任何**能打 stdout 的 CLI 都能"跑起来、看得见"，把适配风险限制在体验层而非可用性层。

---

## 四、总体设计

```
┌─ apps/desktop 前端 (React) ──────────────────────────────┐
│  DetailPanel/RunsPanel ──执行──▶ store.executions        │
│  TranscriptView ◀── Tauri Channel<TranscriptEvent> ──┐   │
└──────────────────────────────────────────────────────┼───┘
                                                       │ invoke / Channel
┌─ apps/desktop src-tauri (Rust) ──────────────────────┼───┐
│  commands: exec_start / exec_stop / exec_list        │   │
│  ExecutionManager（会话注册表，Mutex<HashMap>）        │   │
│    ├─ spawn harness（进程组、增强 PATH、cwd=workspace）│   │
│    ├─ stdout 读线程 ──▶ HarnessAdapter（流格式解析）   │   │
│    │        └─未知/失败──▶ raw 透传                   │   │
│    └─ MCP 接线：临时 mcp-config 注入 omt mcp          │   │
└──────────────┬───────────────────────────────────────┘   │
               │ (子进程)                                   │
   ┌───────────▼────────────┐    ┌──────────────────────┐  │
   │ harness 进程           │───▶│ omt mcp（stdio MCP） │──┼──▶ omt-daemon
   │ claude/codex/opencode  │    │ claim/report/update  │  │   (既有协议零变更)
   └────────────────────────┘    └──────────────────────┘  │
```

关键点：

- **daemon 协议零变更**（v1）：claim/report/update/events 全部复用既有方法；desktop 新能力全部在自身进程内闭环（新 tauri command + Channel）。
- transcript 走 **Tauri 2 `Channel`**（`tauri::ipc::Channel`）：点对点、高吞吐、无跨窗口泄漏、自带背压处理，优于全局 `app.emit`（既有 `omt://event` 的线程泄漏问题不再复制到新通道）。
- run 状态观察复用既有 `events/resume` 订阅，前端已有防抖重拉逻辑直接生效。

---

## 五、详细设计

### 5.1 Harness 注册表（表驱动，替换硬编码）

Rust 侧内置描述符表（新增 `apps/desktop/src-tauri/src/harness.rs`）：

```rust
pub struct HarnessDescriptor {
    pub id: &'static str,          // "claude" | "codex" | "opencode" | "dsh" | ...
    pub label: &'static str,
    pub binary: &'static str,      // PATH 探测名
    pub probe: &'static [&'static str],          // ["--version"]
    pub headless: HeadlessSpec,    // 无头执行：argv 模板 + prompt 传法（arg | stdin）
    pub stream: StreamFormat,      // ClaudeStreamJson | CodexJsonl | OpenCodeJsonEvents | Raw
    pub mcp: McpWiring,            // None | CliFlag | ConfigFile | Todo(dsh)
    pub session: SessionSupport,   // 会话连续性：id 捕获方式 + resume argv 模板（见 5.4.1）
}

pub const REGISTRY: &[HarnessDescriptor] = &[
    /* claude:  `claude -p <prompt> --output-format stream-json --verbose
                 --mcp-config <file>`；MCP 注入 = 临时 config 文件          */
    /* codex:   `codex exec --json <prompt>`；MCP 注入 = config.toml      */
    /* opencode:`opencode run --format json <prompt>`；
                 MCP 注入 = env OPENCODE_CONFIG_CONTENT（内联，零污染）     */
    /* omp:     `omp --mode rpc`（NDJSON 双向）或 `omp -p --mode json`；
                 MCP 注入 = 项目 .omp/mcp.json                              */
    /* dsh:     `dsh --profile headless "<task>"`（one-shot，stderr 流式
                 reasoning）；不需要 MCP —— omt 即 dsh 插件，直接装进
                 headless profile（见 5.4）                                 */
];
```

- 新 command `harness_list()` → 返回注册表 + 逐项探测（installed/path/version，复用 `which_binary`/`probe_version`）。前端选项改为从 `harness_list` 拉取，替换 `HarnessSelect.tsx:14` 的硬编码枚举与 `rpc_bridge.rs:88-91` 的硬编码 match（`harness_detect` 保留为兼容或并入）。
- `HarnessConfig` 结构扩展：`{ id, resolvedFrom: 'install'|'dev', installPath?, version?, devPath? }`——dev 模式（dsh 源码 checkout）继续保留，解析为对应的启动命令。
- 二期再考虑用户自定义 harness（`tauri:harness.custom` bag 存 argv 模板 + `stream: raw`）。

### 5.2 ExecutionManager（desktop Rust 侧）

新增模块 `apps/desktop/src-tauri/src/executor.rs` + `state.rs` 中 `Mutex<HashMap<ExecId, ExecutionSession>>`：

```rust
pub struct ExecutionSession {
    pub id: String,                 // exec-<ts>-<rand>
    pub home_id: String,
    pub node_id: Option<String>,    // 关联 ticket（run 执行时为成员 id）
    pub run_id: Option<String>,
    pub harness: String,            // descriptor id
    pub pid: u32,
    pub harness_session_id: Option<String>,  // harness 侧会话 id（首跑捕获/预指派，供续跑）
    pub status: ExecStatus,         // starting | running | exited(i32) | killed | failed(String)
    pub started_at: SystemTime,
}
```

行为约定：

- **spawn**：`cwd` = workspace 根（home 路径）；PATH 用既有 `augmented_path`；Unix 下 `pre_exec(setpgid)` 自成进程组（daemon.rs 的 setsid 手法），终止时 `killpg(SIGTERM)` 再 `SIGKILL`，确保 harness 的孙进程（shell、editor）一并回收；Windows 走 `CREATE_NO_WINDOW` + `child.kill()`。
- **读线程**：stdout 逐行 → adapter 解析 → `Channel` 推送；stderr 单独通道（标记 `kind:"raw", stream:"stderr"`）；子进程退出 → 推 `kind:"exit"` 终结事件。
- **生命周期**：app 退出时统一 `killpg` 全部会话（不做守护式存活）；并发度 v1 不设限但不鼓励（run 侧 concurrency=1 由 daemon 保证，同一 run item 不会被双重认领——租约保证）。
- **审计衔接**：desktop 发起执行前仍调 `node/execute`（既有语义：`in_progress` + `exec:<nodeId>` meta 绑定 desktop principal），UI 立即有执行中反馈；后续状态由 harness 经 MCP 推进。

### 5.3 Transcript 统一事件模型

Rust→前端的标准事件（serde 序列化，经 Channel）：

```jsonc
{ "seq": 12, "ts": "2026-09-05T04:12:33.120Z", "execId": "exec-...", "kind": "...", "payload": { } }
// kind 全集：
// session_start { harness, version, argv, cwd, mcp: bool }
// assistant_text { text }
// assistant_thinking { text }            // 可选披露
// tool_call     { name, input }          // 工具调用（折叠渲染）
// tool_result   { summary, isError }     // 截断展示
// usage         { tokens?, cost? }       // 有则显示
// error         { message }
// raw           { stream: "stdout"|"stderr", text }   // 兜底
// exit          { code: i32|null, signal: String|null, durationMs }
```

Adapter 层每 harness 一个（Rust 内，无外部依赖，手写行解析）。以下命令与事件格式均已对照本机安装版本与官方文档/源码逐一核实（证据见附录 A）：

| harness | 无头调用 | 流格式（adapter 输入） |
|---|---|---|
| claude | `claude -p <prompt> --output-format stream-json --verbose` | JSONL：`system(init)` / `assistant`（content blocks：text / thinking / tool_use）/ `user`（tool_result）/ `result` |
| codex | `codex exec --json <prompt>` | JSONL：`thread.started` → `turn.started` → `item.*`（agent_message / command_execution / file_change / mcp_tool_call…）→ `turn.completed` |
| opencode | `opencode run --format json <prompt>` | NDJSON `{type, timestamp, sessionID, …}`：`step_start` / `step_finish` / `text`（仅 finalized 块，非 token 级）/ `reasoning`（需 `--thinking`）/ `tool_use`（仅 completed|error 时发全量 part）/ `error`。**无终止事件**（idle 即 break）——以进程退出为结束信号 |
| omp (oh-my-pi) | 首选 RPC：`omp --mode rpc`（NDJSON 双向 over stdio）；简装路径 `omp -p --mode json` | RPC 帧：`ready`（protocolVersion 1/2）→ 命令 `prompt`/`abort`/`get_state`/`set_model`/`get_messages_page`…；事件 `agent_start` / `agent_end`（`isTerminal` 判终）/ `message_start|update|end`（`assistantMessageEvent` 含 **text/thinking/toolcall 增量**，唯一能拿到 token 级流式的）/ `tool_execution_start|update|end` |
| dsh | `dsh --profile headless "<task>"`（one-shot，官方 bundle `@deepseek-ai/dsh-headless`） | 无结构化 JSON 流：reasoning delta 流式写 **stderr**（`dsh: reasoning:` 分节标题），最终答案写 **stdout**，exit 0/1。实时显示走 stderr 分节解析（thinking 粒度）；**富 transcript 回放**走 session 日志 `~/.dsh/sessions/<munged-cwd>/session-<uuid>/session.jsonl.zstd`（typed JSONL：session/permission/model/agent 事件，zstd 压缩） |
| 未知/自定义 | 任意 CLI（prompt 经 arg 或 stdin） | Raw 透传 |

> 注：各 CLI flag 随版本漂移快，adapter 按 descriptor 携带的 `probe` 版本信息做能力判断，解析失败一律降级 raw——**契约是"降级保证可用"，不是"追平每个版本"**。桌面端应对长执行加进程级超时兜底（`omp --max-time` 原生支持；其余由 ExecutionManager 自己 kill）。

### 5.4 Prompt 构建与 MCP 接线

**run 成员执行（模式 B）**：prompt 不携带 ticket 正文——`run__claim` 的返回本身就含 claim context（ticket 全文 + 祖先链 16KiB 预算上下文，`dispatch.rs:2179`）。prompt 只需指令骨架（模板放 Rust 常量，后续可配置化）：

> 你在 OMT 系统中执行 run `<runId>` 的成员 `<nodeId>`。先调用 `run__claim` 认领（如已被认领则停止），按返回的上下文完成该 ticket 的工作，最后用 `run__report` 如实报告 done/failed/blocked/skipped。不要虚构结果。

MCP 注入（按 harness 逐一核实过的注入面，command 一律指向**随 app 分发的 `omt` CLI sidecar**，`["mcp"]` 子命令，见 5.8）：

| harness | 注入方式 |
|---|---|
| claude | `--mcp-config <临时JSON>`（`{"mcpServers":{"omt":{"command":<omt>,"args":["mcp"]}}}`） |
| codex | 临时 `config.toml` 片段（`[mcp_servers.omt]`）+ `-c` 覆盖或 `CODEX_HOME` 指向临时目录 |
| opencode | **env `OPENCODE_CONFIG_CONTENT`** 内联 `{"mcp":{"omt":{"type":"local","command":[<omt>,"mcp"],"enabled":true}}}`——一次性注入、不碰项目与全局配置（`OPENCODE_CONFIG` 文件路径亦可） |
| omp | 项目级 `.omp/mcp.json`（`{"mcpServers":{"omt":{"type":"stdio","command":<omt>,"args":["mcp"]}}}`）。注意与 omt home `.omt/` 是相邻的不同目录、无冲突；desktop 应在执行后清理或改用独立 `--profile` 的 profile 级 `~/.omp/profiles/<name>/agent/mcp.json` 避免写用户工作区 |
| dsh | **不需要 MCP**——omt 本身就是 dsh 插件：`dsh plugin --profile headless add dsh-oh-my-ticket@<ver>`（desktop 已有 `dsh_plugin_install` 同款命令封装基建），模型直接获得 `omt_*` 全套工具 + 内嵌 system prompt/skill，claim/report 纪律原生成立，executor 谱系即 dsh 会话（比 MCP 面更完整）。备选：`--patch` overlay 向任意 profile 插入插件 |

desktop 若以非默认 runtime dir 运行 daemon，spawn harness 时必须把 `OMT_RUNTIME_DIR` 注入子进程环境（`omt mcp` 按既有"参数 > 环境变量 > 默认值"契约解析），否则 MCP 会连错 daemon——需加 parity 测试。
- 权限面：MCP 凭据天然受限（node/run/events，无 home/ui，R7）——harness 越权面最小，符合 R5。
- **信任门语义**：harness 经 MCP 的显式 `run__report` 是受信任路径，不落 `awaiting_confirmation`；这与"模型自己报告"的既有产品语义一致。

**［已被计划 Q1 取代——以下段落仅供追溯］单 ticket 执行（非 run）**：desktop `node/get`（含祖先链）本地拼 prompt（预算规则对齐 claim context：16KiB、最近父级优先、截断显式标记）；同样注入 MCP，prompt 指示 harness 用 `node__update` 置 `in_progress`/完成后置 `done`。desktop 发起时仍先 `node/execute` 做意图标记。MCP 不可用时的 v1 简化路径：执行结束后前端弹"标记为 done / 保持 in_progress"人工确认——对齐信任门"裸 done 需人确认"的精神。

### 5.4.1 会话连续性（session 复用，2026-09-06 补充调研）

需求：首跑保留 harness 产生的 session；后续对同一 ticket 的执行可指定在该 session 中继续（保留上下文）。逐家核实的支持矩阵（证据见附录 A.5）：

| harness | 首跑获取 session id | 指定 session 续跑 | 能力 |
|---|---|---|---|
| claude | **可预指派**：`--session-id <uuid>`（desktop 自己生成 UUID 传入，无需从输出捕获）；也可从 stream-json `system/init` 事件的 `session_id` 捕获 | `-r/--resume <id>`、`-c`（最近）、`--fork-session`（resume 时换新 id） | ✅ 最完善 |
| codex | `codex exec --json` 的 `thread.started` 事件携带 thread id | `codex exec resume <id> --json`（resume 子命令完整继承 `--json`/`-o`）；`--last` 取最近；`fork` 子命令 | ✅ |
| opencode | **每个事件都带 `sessionID`** 字段，天然可捕获 | `opencode run -s <id>`、`-c`（最近）、`--fork` | ✅ |
| omp | RPC `get_state` 返回 `sessionId`/`sessionFile`；RPC 模式下进程内存活即可多轮 `prompt`，天然续跑 | one-shot：`-r/--resume <id前缀/路径>`、`-c`；RPC：`switch_session <sessionPath>`、`new_session` | ✅ |
| dsh headless | ❌ runner 内部 `randomUUID()` 新建（`dsh-headless` index.js:134），**不向 stdout/stderr 打印**，只能 diff `~/.dsh/sessions/<munged-cwd>/` 发现新 session 目录（best-effort） | ❌ config 仅 `task` 一个字段，无 resume/sessionId 设置 | ❌ v0.1.2-rc.1 不支持 |

设计落点：

1. **ExecutionSession 增加 `harness_session_id`**；首跑由 adapter 按上表捕获（claude 走预指派最稳），写入执行记录与 transcript 文件头。
2. **id ↔ ticket 的映射持久化**在 omt 侧（bag 或执行记录），DetailPanel 执行入口对"已有 harness session 的 ticket"提供"继续上次会话"选项（传 resume flag）与"新开会话"两种；跨 harness 复用同 ticket 时各存各的（映射键 = nodeId + harness id）。
3. **descriptor 增加 `session: SessionSupport`**（捕获方式 + resume argv 模板），不支持者（dsh）UI 上隐藏续跑选项。
4. **dsh 的替代语义**：session 不连续，但 omt 插件原生安装后上下文本来就该由 ticket 正文/进度记录承载（OMT 的设计哲学即"状态在 ticket 不在会话"）；同时向 dsh 侧提出诉求——session store 本身支持 resume（web/tui 面有 `--resume`），headless runner 增加 `resume`/`sessionId` 设置即可补齐。
5. **租约/审计不受影响**：续跑与否只改变 harness 侧上下文，claim/report 仍按 run 纪律走。

### 5.5 与 daemon 协议的关系

**v1 零协议变更**。明确不新增的东西及理由：

- 不新增 transcript 类 RPC/事件：transcript 属于执行会话（desktop 进程内的子进程输出），不是 home 数据；进 daemon outbox 会把高吞吐低价值数据灌进所有 surface 的事件流。
- 不新增 exec 生命周期 RPC：会话注册表是 desktop 私有态。
- （远期可选，§7）：若 DSH 面要回看 desktop 发起的执行，再议 `exec/*` 方法族或 transcript 落盘后的只读 RPC。

### 5.6 Transcript 持久化与回放（二期，先定形态）

- 落盘位置：`<runtime-dir>/transcripts/<home-id>/<execId>.jsonl`（runtime dir 是机器本地目录，天然不进项目 git——**不落 `.omt`**，避免 ticket 库被执行日志污染）。文件首行为 header：`{execId, nodeId, runId, harness, harnessSessionId, argv, startedAt}`——执行记录与 harness 会话 id 的持久锚点，app 重启后恢复"继续上次会话"选项的依据（§5.4.1）。
- 同时保留 raw 原始流（`<execId>.raw.log`），adapter 升级后可重放解析。
- 回放：`exec_open(execId)` 读文件按 seq 重推 Channel；会话注册表加载近期索引（文件名即元数据，无需 SQLite）。

### 5.7 前端设计

- **store**（`store.ts`）新增 executions 切片：`Map<execId, { meta, status, events[] }> `；Channel 回调按 execId 归并；事件仅保尾窗（如最近 2000 条），全量靠回放文件。
- **TranscriptView**（新组件）：虚拟滚动列表；按 kind 渲染——文本块直接渲染（复用 `MarkdownText`）、tool_call 折叠条（名称 + 展开看 input/result 摘要）、error 红条、raw 等宽文本、底部状态栏（running/exited/时长/退出码）；自动跟随滚动 + 用户上滚即暂停跟随。
- **入口**：
  - `DetailPanel`：Execute 按钮旁 harness 选择器（默认取 workspace 的 `tauri:harness` 配置，可单次覆盖/临时选择任意已安装 harness）；该 ticket 在所选 harness 下已有 session 时，提供「继续上次会话 / 新开会话」选择，不支持续跑的 harness 灰置并说明（§5.4.1）。
  - `RunsPanel`：item 行加"用 harness 执行"（run 需先 `run/control start`；对 `pending` 成员可用）。
  - App 级"执行中"指示（顶栏或侧栏小列表，多会话可见）。
- **执行记录**：RunsPanel/detail 已有的 executor 谱系展示会自然显示执行者谱系。~~`omt mcp` 已支持 name，desktop 生成 config 时传入即可~~ **［勘误：`mcp.rs:95-97` 将 name/actor 均硬编码（`omt-mcp` / `mcp:<pid>`）并忽略 CLI 全局 `--actor`。计划 U3 改为尊重 `--actor`，desktop 注入 `exec:<execId>`，谱系随之显示为 `exec:<execId>` 并可定位 Agents 面对应 Agent。］**

### 5.8 打包与分发影响

- `omt` CLI 成为第二 sidecar：`copy-sidecar.mjs` 增加 `omt` 产物；`tauri.conf.json` `externalBin` 增加 `binaries/omt`；sidecar 解析复用 `current_exe().parent()` 约定（`daemon.rs:37-48`）。
- MCP config 中的 command 需绝对路径指向该 sidecar。
- 体积影响：`omt` CLI 与 daemon 同 crate 构建，增量可接受；发布流水线（GitHub Releases 归档、brew formula）同步补第二二进制。

---

## 六、关键决策记录

- **KD-D1 执行器放 desktop，不放 daemon**：保住"daemon 纯协调面、唯一写者、idle 可退出"三条既定架构不变式；desktop 已有 spawn/探测/PATH 积木（§3.1）。
- **KD-D2 run 执行 = harness 经 MCP 自 claim/report**：认领者=执行者=报告者，租约/信任门/审计自洽，与 DSH 侧同构；desktop 代 claim 仅为降级路径（§3.2）。**dsh 例外**：omt 本身是 dsh 插件，直接装进 headless profile 走原生 `omt_*` 工具面（§5.4、附录 A.1/A.4）。
- **KD-D3 transcript 走 desktop 进程内 Channel，不进 daemon 事件面**：高吞吐流不属于 home 数据；协议零变更（§5.5）。
- **KD-D4 结构化 adapter + raw 兜底双层**：适配风险降级为体验风险；任何 stdout CLI 至少"可跑可见"（§3.3）。
- **KD-D5 harness 注册表表驱动**：探测/无头命令/流格式/MCP 注入四元组集中于 Rust 单表，前端动态拉取，消除两处硬编码（§5.1）。
- **KD-D6 transcript 不落项目 `.omt`**：落 runtime dir，避免 ticket 库（可进 git）被日志污染（§5.6）。
- **KD-D7 会话与 app 同生命周期**：desktop 退出 = 执行终止，run 恢复交给既有租约过期 → interrupted → retry 路径，不引入守护进程（§5.2、§3.1-C）。
- **KD-D8 会话连续性按 descriptor 能力驱动**：首跑捕获 harness session id（claude 走 `--session-id` 预指派最稳，其余从事件流捕获，dsh 只能 diff sessions 目录 best-effort），按 `nodeId + harness id` 持久化映射，续跑经 descriptor 的 resume argv 模板；dsh headless v0.1.2-rc.1 不支持续跑，UI 隐藏该选项、以 ticket 承载上下文兜底，并向 dsh 侧提出 headless resume 诉求（§5.4.1、附录 A.5）。

---

## 七、风险与开放问题

**风险**

| # | 风险 | 缓解 |
|---|---|---|
| 1 | dsh headless 无结构化输出流（仅 stderr reasoning 分节 + stdout 最终答案），transcript 无工具调用粒度 | 实时显示走 stderr 分节解析；富 transcript/回放关联 session 日志（`session.jsonl.zstd`，typed JSONL）；启动时 diff `~/.dsh/sessions/<munged-cwd>/` 新增 session 目录即得关联；与 dsh 侧推动 headless JSON 事件流为长期解 |
| 2 | harness CLI flag 版本漂移 | adapter 解析失败一律降级 raw；descriptor 携带版本探针 |
| 3 | GUI PATH 盲区（非标准安装位置） | 既有 `augmented_path` 启发式 + harness 配置允许显式 `installPath` 覆盖 PATH 查找 |
| 4 | 长输出吞吐压垮前端 | Channel 自带背压；前端尾窗截断 + 虚拟列表；raw 行做合并刷新（如 50ms 批） |
| 5 | Windows 进程组语义差异 | kill 链路按 cfg 分平台；Windows `taskkill /T` 兜底 |
| 6 | harness 权限策略差异大：**omp 默认 `tools.approvalMode: yolo`**（全放行，MCP 工具也在内），opencode 需 `--auto`，claude 需显式 permission 参数 | 每个 descriptor 显式声明默认权限策略并渲染给用户确认（R6 风险条目升级：omp 集成时 desktop 必须显式传 `--approval-mode write` 或让用户勾选 yolo，绝不静默继承默认） |
| 7 | opencode 事件流为块级（text 仅 finalized、tool_use 仅 completed/error），无终止事件 | transcript 粒度按块渲染（可接受）；结束信号 = 进程退出；想要更细粒度走二期 `serve`/ACP 模式 |
| 8 | `events_subscribe` 既有线程泄漏模式被放大 | 新代码不复用该模式（Channel 点对点）；顺带列入既有缺陷修复清单 |

**开放问题（需产品/相关侧输入）**

1. ~~dsh 的 headless/无交互执行形态究竟是什么？~~ **已解决**（2026-09-06 调研，见附录 A）：`dsh --profile headless "<task>"`，无结构化流，富日志走 session 文件；长期改进方向是与 dsh 侧确认 headless JSON 事件流的可能性。
2. ~~单 ticket 非 MCP 路径的完成确认：人工确认（信任门精神）还是退出码自动 done？~~ **已关闭**（计划 Q1：无非 MCP 路径；信任门由 U14 处理）。
3. harness 执行是否计入 run 的 nudge/stalled 观测？（v1 不计；desktop 发起的执行无 DSH 会话可 nudge，stalled 判定依赖租约即可；desktop 侧 nudge 可走 omp RPC `steer`，计划 backlog #6）
4. transcript 是否需要在 RunsPanel 的 item 视图直接内嵌（而非跳转）？（计划采用独立 Agents tab；RunsPanel item 行的 `exec:<execId>` 谱系可点击跳转对应 Agent）
5. ~~omp 经项目 `.omp/mcp.json` 注入会在用户工作区留下文件——用后清理，还是统一走 `omp --profile omt` 的 profile 级配置？~~ **已关闭**（计划 U3：refcount 合并/备份/恢复，备份落 runtime dir；`--profile omt` 因隔离 auth 不采用）。

---

## 八、分期实施建议

> **［已被计划 §五「实施单元与分期」取代：计划 P0 = 统一 run 化 + MCP 必选 + opencode/omp 两家；以下为原始建议，仅供追溯。］**

- **P0（骨架可用）**：harness 注册表 + `harness_list`；executor 骨架（spawn/进程组/Channel/raw 透传）；单 ticket prompt 执行（MCP 可选）+ TranscriptView；`omt` 第二 sidecar 打包。验收：任一已安装 harness 选中后能跑、能实时看到输出、能终止。
- **P1（纪律闭环）**：claude/codex/opencode 三个结构化 adapter；MCP 注入（run 成员执行走模式 B）；session 捕获与续跑（R7/§5.4.1：id 映射持久化、resume argv、UI 续跑选择）；transcript 落盘 + 回放；RunsPanel 入口。
- **P2（扩展）**：自定义 harness（bag 配置）；dsh 富 transcript（session 日志关联与 zstd 解析）；opencode `serve`/ACP 与 omp RPC 深度集成（中途 steer、双向控制）；内嵌终端形态（PTY）；（远期）跨 surface transcript 只读协议。

---

## 附录 A：外部调用机制调研记录（2026-09-06，基于本机安装版本实测 + 官方文档/源码）

### A.1 dsh（DeepSeek Harness 0.1.x，本机 nvm 全局安装）

- **调用形态**：`dsh --profile headless "<task>"`。headless 是普通 profile（`~/.dsh/profiles/headless/`，bundles = `@deepseek-ai/dsh-base` + `@deepseek-ai/dsh-headless`），launcher 语义："answer one task, print the result, and exit"。
- **输出契约**（`@deepseek-ai/dsh-headless` README，本机 `~/.dsh/profiles/node_modules/` 可读源码与 cordis.patch.yml）：one-shot、无交互跟进；每个非空 provider reasoning delta 流式写 **stderr**（`dsh: reasoning:` 分节标题、连续 delta 归一节）；**最终答案写 stdout** 后退出；exit 0 = 完成，1 = 中止/出错；空/空白 task 是 usage error（先于运行拒绝，exit 1）；`--help` 只打印帮助不执行。该 bundle 不挂 Host/HTTP/浏览器层，无端口残留。
- **无结构化输出模式**：headless profile 自身 flags 仅 `-h`；无 JSON 事件流选项。`DSH_TOOLS_MODE` env 控制 PTC 工具模式。
- **富 transcript 备选**：会话按 cwd 归档于 `~/.dsh/sessions/<munged-cwd>/session-<uuid>/session.jsonl.zstd`——typed JSONL（`session` / `permission/preset` / `sandbox/mode` / `approval/policy` / `model/selection` / `agent/inbox/spliced`…，含 seq/time/data），zstd 压缩。desktop 启动执行后 diff 出新增 session 目录即可关联本次运行。
- **OMT 集成的原生优势**：omt 即 dsh 插件。`dsh plugin --profile headless add dsh-oh-my-ticket@<ver>` 把全套 `omt_*` 工具与 system prompt/skill 直接装进 headless profile（desktop 已有 `dsh_plugin_install` 封装同款命令），claim/report 纪律原生成立，无需 MCP。备选 `--patch` overlay 插入。

### A.2 opencode（本机 `~/.opencode/bin/opencode`，官方文档 opencode.ai/docs/cli + GitHub sst/opencode 源码 run.ts）

- **无头调用**：`opencode run [message..] --format json`（`--format`: default | json，"raw JSON events"）。
- **JSON 事件流**（run.ts `emit()`，逐行 `{type, timestamp, sessionID, ...data}`）：`step_start` / `step_finish` / `text`（仅 `part.time?.end` 的 finalized 文本块，非 token 级）/ `reasoning`（仅 `--thinking` 开启时）/ `tool_use`（仅 part `state.status` 为 completed|error 时发全量 part）/ `error`。
- **关键行为**：session idle 时循环直接 `break`，**没有终止事件**——宿主以进程退出为结束信号；`--mini` 与 json 格式互斥；权限自动应答（`--auto` 发 "once"，否则 "reject"）不产生 JSON 事件。
- **权限**：`--auto`（auto-approve 未显式拒绝项）；agent 级 permissions allow-list（`opencode agent create --permissions`）；`OPENCODE_PERMISSION` env 内联权限配置。
- **MCP 配置**：config 的 `mcp` key——local `{type:"local", command:[…], cwd?, environment?, enabled?, timeout?}` / remote `{type:"remote", url, headers?, oauth?}`。**一次性注入**：env `OPENCODE_CONFIG`（配置文件路径）/ `OPENCODE_CONFIG_CONTENT`（内联内容）/ `OPENCODE_CONFIG_DIR`（源码 config.ts 证实三者皆支持）——无需写项目文件。
- **其他接口**：`opencode serve`（headless HTTP server）+ `run --attach <url>`（复用常驻实例，避免每次 run 的 MCP 冷启动）；`opencode acp`（Agent Client Protocol over stdio nd-JSON）；`opencode export [sessionID]`（会话 JSON 导出）、`session` 管理、`--session/-c/--fork`。

### A.3 omp（= oh-my-pi，can1357/oh-my-pi，Mario Zechner pi 的 fork；本机 brew can1357/tap，v18.0.9）

- **模式**（`omp --help` + docs/cli-reference.md）：`--mode text|json|rpc|acp|rpc-ui`；`-p/--print` one-shot（stdin 管道可并入 prompt：`echo x | omp -p`）。
- **`--mode json`**：结构化 JSON 事件流（继承 pi 的 print-json，pi docs/json.md），适合一次性管道。
- **`--mode rpc`（推荐的集成面，docs/rpc.md）**：NDJSON 双向协议 over stdio。启动即发 `ready` 帧（`protocolVersion`，v2 有 `rpc_chunk` base64 分块无损大帧）；入站命令全集含 `prompt`（`{id, type:"prompt", message, streamingBehavior}`）/ `steer` / `follow_up` / `abort` / `new_session` / `get_state` / `set_model` / `set_thinking_level` / `get_messages_page` / `export_html` / `switch_session` / `bash` / `set_host_tools` / `set_subagent_subscription`…；出站事件 `agent_start` / `agent_end`（`isTerminal !== false` 才是终态）/ `turn_start|end` / `message_start|update|end`（`assistantMessageEvent` 含 text/thinking/toolcall **增量**）/ `tool_execution_start|update|end` / `auto_compaction_*` / `auto_retry_*` / `model_changed`…。**唯一能拿到 token 级流式增量的一家**，且支持中途 steer 与完整会话控制。
- **审批**（docs/approval-mode.md）：`tools.approvalMode: always-ask | write | yolo`，**默认 yolo**（全放行）；`--approval-mode` / `--auto-approve` / `--yolo` 覆盖；MCP 工具声明为 `write` 层级。desktop 集成必须显式设定审批模式，不可静默继承默认。
- **MCP 配置**（docs/mcp-config.md）：OMP-native 文件为项目 `.omp/mcp.json` / 用户 `~/.omp/agent/mcp.json`（named profile 下 `~/.omp/profiles/<name>/agent/mcp.json`），结构 `{"mcpServers": {"name": {type:"stdio", command, args, env…}}, "disabledServers": [], "enabledServers": []}`；另自动翻译 `.claude/`、`.codex/config.toml`、`opencode.json` 等 8 类来源。运行期覆盖可用 `--config <file>`（可重复，config.yml-style overlay）。
- **会话/其他**：`--session-dir` / `--no-session` / `-r resume` / `--export`；profile 隔离（`--profile`）；`--max-time` 进程级超时；`--cwd` 指定目录；模型 `--model`、思考档 `--thinking`。本机 `~/.omp/run/daemons` 显示其有 IDE daemon 机制（headless 不依赖）。

### A.4 对设计结论的影响汇总

1. 四家全有可用的 headless 路径，设计的三层 adapter（结构化/raw 兜底）成立且已具体化；**omp 应升级为 RPC 集成**（增量流式 + steer 能力），其余 JSONL 一次性流。
2. **dsh 集成走"插件直装"而非 MCP**——比原设计更原生：工具面、system prompt、skill 全部原生，executor 谱系即 dsh 会话。`omt` CLI sidecar 仍需打包（claude/codex/opencode/omp 四家走 MCP）。
3. opencode 一次性 MCP 注入用 `OPENCODE_CONFIG_CONTENT` env，完全无文件污染；omp 需 `.omp/mcp.json`（与 omt home `.omt/` 相邻不同目录，无冲突，但有留痕清理问题，见开放问题 5）。
4. 新增硬性要求：desktop 必须为每家 harness 显式设定权限策略（尤其 omp 默认 yolo），并在 UI 呈现。

### A.5 会话连续性核实记录（2026-09-06，本机版本实测）

- **claude**（本机 `~/.local/bin/claude`，`claude --help`）：`--session-id <uuid>`——"Use a specific session ID for the conversation (must be a valid UUID)"（预指派，首跑无需捕获）；`-r/--resume [value]`——"Resume a conversation by session ID"；`-c/--continue`；`--fork-session`——"When resuming, create a new session ID instead of reusing the original"。stream-json 的 `system/init` 与 `result` 事件携带 `session_id`。
- **codex**（本机 brew，`codex exec --help` / `codex exec resume --help`）：`codex exec resume [SESSION_ID] [PROMPT]`——"Resume a previous session by id or pick the most recent with `--last`"；选项表确认 resume 子命令完整继承 `--json`（"Print events to stdout as JSONL"）与 `-o/--output-last-message`；另有 `fork` 子命令、`--ephemeral`（不落盘）。`--json` 事件流中 `thread.started` 携带 thread id。
- **opencode**（`opencode run --help` + run.ts 源码）：`-s/--session`——"Session ID to continue"；`-c/--continue`；`--fork`；`--format json` 每行事件都带 `sessionID`。
- **omp**（`omp --help` + docs/rpc.md）：`-r/--resume=<value>`——"Resume a session (by ID prefix, path, or picker if omitted)"；`-c/--continue`；`--session-dir`；`--no-session`。RPC 命令 `get_state` 返回 `sessionId`/`sessionFile`；`switch_session {sessionPath}`、`new_session {parentSession?}`；RPC 进程内多轮 `prompt` 天然同会话续跑。
- **dsh headless**（本机 `~/.dsh/profiles/node_modules/@deepseek-ai/dsh-headless/lib/index.js` 全文审读）：`Config = z.object({ task: z.string().required() })`（index.js:26）——唯一设置项；每次 `agents.create({ sessionId: brandString(\`session-${randomUUID()}\`), ... })`（index.js:134）新建会话；`sessions.flush()` 落盘后不打印 session id（index.js:162-166 只输出最终文本与退出码）；全文件无 resume 逻辑。会话归档于 `~/.dsh/sessions/<munged-cwd>/session-<uuid>/`，desktop 可 diff 目录 best-effort 关联。dsh 的 session store 本身支持续跑（launcher 帮助示例 `dsh --profile tui --resume <session>`），缺的只是 headless runner 的设置项——已列为向 dsh 侧的诉求。
