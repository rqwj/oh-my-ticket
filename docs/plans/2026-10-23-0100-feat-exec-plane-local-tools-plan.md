---
title: 执行面下沉 · 子项目 2 实施计划（CLI / MCP 本地工具族 + policy；并入子项目 1 遗留 P3）
type: plan
status: draft
date: 2026-10-23
design: docs/plans/2026-10-20-0802-feat-omt-exec-plane-orchestration-design.md（§3.2、§3.5、§3.6、§5、§6、§7 子项目 2）
predecessor: docs/plans/2026-10-20-2300-feat-exec-plane-extraction-plan.md（子项目 1，已完成并手工验收）
findings_log: docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md
---

# 执行面下沉 · 子项目 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让主控 agent 和用户经 `omt` CLI / `omt mcp` 直接查询 harness 与模型、拉起 / 停止 / 查看执行（不再只能从桌面端派发），派发能力由 `exec/policy.json` 默认关闭并约束；同时清理子项目 1 验收与历轮 review 遗留的 P3。

**Architecture:** 本计划分两部分。**A 部分**在 `omt-exec` 新增 policy 与「本地工具」核心（纯函数，JSON 进 JSON 出），launcher 增加发起方（desktop / cli / mcp）与派发目标（node / run），CLI/MCP 发起时跳过 adapter_only 的 `node/execute`，并用跨进程 flock 串行化派发与并发计数。`omt-runtime` 的 lib 只定义一个不依赖执行面的 `LocalTools` trait，CLI 动词和 MCP 工具经它调用；实现放在只属于 `omt` 二进制的模块里注入，`omt-daemon` 不链接执行面（KD-E9）。桌面端新增「派发策略」设置页并定时刷新 agent 列表，外部发起的执行无需切换焦点即可见。**B 部分**把遗留 P3 按主题合成 8 个 Task。

**Tech Stack:** Rust 2021（workspace crates）、Tauri 2、`omt-client`（JSON-RPC over UDS）、libc（flock / killpg）、React + vitest（桌面前端）、JSON Schema（`schema/*.schema.json` → `pnpm gen`）。

## Global Constraints

- **KD-E9**：`omt-daemon` 不得链接 `omt_exec` 与 `omt_harness`。`crates/omt-runtime/src/` 下由 `lib.rs` 声明的模块一律不得引用这两个 crate；执行面接线只能出现在 `src/cli_main.rs` 及其私有子模块（`src/local_impl.rs`）。CI 的 `nm` 门（`.github/workflows/ci.yml`）在 Task A5 扩展为同时检查 `omt_harness`。
- **daemon 唯一改动**是 Task B1（P3-ACC-2 缺 token 的报错）。不新增 daemon 方法；不改 `parity_of`。
- **policy 默认值**（设计 §3.6 原样）：`allowFromCli: true`、`allowFromMcp: false`、`harnesses: ["pi","omp","opencode"]`、`workspaces: "homeWorkspaceOnly"`、`maxConcurrent: 4`、`maxDurationSec: 1800`、`approvalMode: "inherit"`。文件缺失 = 默认值；文件存在但无法解析或含未知字段 = **fail closed**（拒绝 CLI/MCP 的派发，错误码 `POLICY_INVALID`）。
- **派发参数不能放宽 policy**：调用方请求的 `maxDurationSec` 只能取 `min(请求, policy)`；不在白名单的 harness 一律拒绝；不接受任何审批模式参数。
- **CLI / MCP 派发总是无人值守**：每次都附加 harness descriptor 的无人值守参数（pi `--auto`、omp `--approval-mode=yolo` 等），与桌面端派发等效但**没有**桌面端派发弹窗那一步人工确认。`approvalMode: "inherit"` 的含义是「派发方不能改它」，不是「读取某个 harness 级开关」（harness 配置里没有这样的开关）。设置页与运维文档必须如实说明（Task A8 / A9）。
- **`workspaces: "homeWorkspaceOnly"` 生效**（Q-13）：CLI / MCP 派发只允许目标为 workspace 内 `.omt` 的 home；全局 home（handshake `kind: "global"`）或 workspace 根等于用户主目录时拒绝（`POLICY_DENIED`，field `workspaces`）。桌面端不受影响。
- **policy 只约束 CLI / MCP 发起的派发（`exec/start`）**；桌面端派发行为与子项目 1 完全一致（验收门不变）。**停止（`exec/stop`）不受 `allowFrom*` 与 policy 文件有效性约束**——止损方向不 fail closed（Q-4）；被派发的 agent 仍不能停止（Q-12）。
- **被派发的 agent 不能再派发或停止**（Q-12，权限放大闸门）：shim 向 harness 环境注入保留变量 `OMT_EXEC_ID=<execId>`（与 `OMT_RUNTIME_DIR` 同级，harness 配置不得覆盖）；带该变量的进程里，`LocalImpl` 拒绝 `exec/start` / `exec/stop`（`POLICY_DENIED`，field `nestedDispatch`），`omt mcp` 不暴露任何本地工具；`--actor exec:*` 的注入桥同样不暴露。
- **威胁模型**：policy 闸门约束的是经 omt 自身工具面（CLI / MCP）行事的 agent，是**纵深防御而非边界**——同一用户的进程（包括手握 shell 的无人值守 harness 或主控）仍可清除环境变量、直接改写 runtime dir 下的 `policy.json` / `admin-grants.json`。「允许经 CLI 派发」同样覆盖任何能执行 shell 的 agent（例如 Claude Code 的 Bash），只关 MCP 不能阻止这类主控派发——设置页与运维文档必须写明。
- **错误码**（`Problem.code`——执行面用 `omt_domain::error::Problem`，omt-runtime 里同一类型经 `omt_storage::Problem` 重导出；类型 `&'static str`；`details` 是 `Option<Value>`，测试里用 `details.as_ref().unwrap()["field"]`）：`POLICY_DENIED`（details 含 `field`）、`POLICY_INVALID`、`NOT_FOUND`、`INVALID_INPUT`、`LAUNCH_FAILED`（派发链的其他失败，如 agent 正忙、无可执行成员、run 状态不可派发——message 原样携带原因）、`IO`。MCP 侧照现有 `call_tool` 规则映射为 `isError: true` 的 `"CODE: message"`。
- **secrets 纪律**沿用：env 值绝不落盘、不进工具输出（`harness__list` 在本地工具路径上只输出 env 键名 `envKeys`，`extraArgs` 经 `sanitize_args`；桌面端设置页的投影保持原样）；argv 输出前经 `sanitize_args`。
- **平台**：v1 仅 macOS；保留现有 `#[cfg(target_os = "linux")]` 分支，不新增 Windows 支持。
- **提交**：每个 Task 结束提交一次；提交信息末尾加 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`；不要提交 `.tmp-tauri-research/`。
- **验证基线**：每个 Task 收尾都跑 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；桌面端相关 Task 加跑 `cd apps/desktop && pnpm test && pnpm exec tsc --noEmit`；改 schema 的 Task 加跑 `pnpm gen && git diff --exit-code packages/client-ts/src/generated`（生成物已提交）与 `pnpm test`。
- **验收环境**放在 `/tmp` 之外（macOS 每日清理 `/tmp` 中 3 天未访问的文件，2026-10-23 曾删掉验收环境的 `harness/config.json`）。

## 决策记录（本计划新增，设计未覆盖或需要细化的点）

| ID | 决策 | 理由 |
|---|---|---|
| **Q-1** | **本地工具经 lib 内的 `LocalTools` trait 注入**：`omt_runtime::local_tools` 只定义 trait、工具名表与参数解析；实现 `LocalImpl` 在 `src/local_impl.rs`，由 `src/cli_main.rs` 以 `mod local_impl;` 引入，构造后传给 `cli::real_run_code_with(Some(&impl))`；`omt-daemon` 不构造它 | `cli/mod.rs` 与 `mcp.rs` 都在 daemon 也链接的 lib 里；与 `exec-shim` 现有的接线方式一致 |
| **Q-2** | **CLI / MCP 发起的派发不调用 `node/execute`**（`LaunchOrigin != Desktop`）；ticket 状态在执行期间保持原样，由 `run/report` 落终态 | 用户已确认（2026-10-23，「接受现状」）。`node/execute` 是 adapter_only；实测 `run/claim` 只激活祖先节点。与 F-ACC-12 修复后的桌面复用路径行为一致 |
| **Q-3** | **home 路径取自 daemon 的 homeId→path 映射**（P-12）：新增 `daemon_link::served_homes`（返回 homeId、path、kind）；CLI 的全局 `--home <路径或 homeId>` 先在已服务的 home 里匹配，路径未被服务且是已初始化的 home 时经 `DaemonLink::connect` 声明——**声明分支只对 `LaunchOrigin::Cli` 开放**；MCP 只接受 `homeId`，且必须是已服务的 home（`home` 键被忽略） | 调用方输入的路径不可信；shim 依赖 spec 里的 `home_path` |
| **Q-4** | **policy 只约束 CLI / MCP 的派发**；桌面端豁免（但桌面端派发同样计入 `maxConcurrent` 的活跃数）。**停止不受 `allowFrom*` 与文件有效性约束**：拒绝启动是安全方向，拒绝停止反而危险（关掉派发后要能停掉失控执行） | 用户确认的验收门是「桌面端功能不变」；并发上限要反映机器上的真实负载 |
| **Q-5** | **MCP 工具暴露规则**：注入桥（actor 以 `exec:` 开头，或进程环境带 `OMT_EXEC_ID`）不暴露本地工具；顶层 MCP **始终列出全部 8 个本地工具**，`exec__start` 在调用时检查 policy（未开启 → `POLICY_DENIED`，提示指向桌面端设置） | `omt mcp` 不声明 `listChanged`，客户端缓存会话开始时的 tools/list——若按 policy 隐藏 `exec__start`，用户中途开启后主控仍看不到它，必须重启 MCP |
| **Q-6** | **跨进程派发锁**：`launch::start` 的进程内 `LAUNCH_LOCK` 改为 `<runtime>/exec/launch.lock` 上的 flock，覆盖「policy 并发检查 → ①–⑥ → spec → spawn_shim → 记录 shim 身份」；**在 `wait_ready` 之前释放**（顺带修掉 §39「锁覆盖整个 30s 就绪等待」） | CLI、MCP、桌面三个进程会同时派发；并发计数必须与登记在同一把锁内，否则会超额 |
| **Q-7** | **记录发起方**：`ExecRecord.launched_by: Option<LaunchOrigin>`（新 exec 必填，旧记录为 None）；`AgentRecord.parent_agent_id: Option<String>`，只在新建 agent 时由显式参数写入（自动祖先链关联属子项目 3） | 设计 §3.6「每次派发记录发起方与 parentAgentId」；serde `default` 保证旧文件可读 |
| **Q-8** | **`exec tail` 按 seq 分页**：输入 `since`（不含，缺省从头）与 `limit`（默认 200，上限 1000），输出 `{events, nextSeq, done}`；`done` = 已读到 `exit` 事件或该 exec 已不活跃且无更多事件 | 设计 §3.5「返回归一化事件，分页」；现有 `transcript_tail_from` 只能取尾部 |
| **Q-9** | **派发目标**：`LaunchTarget::Node(nodeId)`（与桌面端相同的成员收集与 run 复用）或 `LaunchTarget::Run(runId)`（直接用已有 run：pending 则 start，running 则复用，其余拒绝；不写 binding） | 设计 §3.5 `--run <runId> \| --node <nodeId>`；主控先 `run__create` 再派发是 §4 的典型数据流 |
| **Q-10** | **桌面端定时刷新 agent 列表**：窗口可见时每 5 秒 `refreshAgents({ silent: true })`——**静默刷新不清除全局错误**；外部发起的 agent 由此出现在列表中，选中后由 AgentsPanel 的 P-10 effect 接回实时流 | 子项目 2 的验收是「桌面端可见」；现在只有获得焦点 / exit / 派发后才重拉。现有 `refreshAgents` 成功后会清空 `state.error`，5 秒一次会让所有错误提示一闪而过 |
| **Q-12** | **嵌套派发标记**：执行器 spawn harness 时注入保留环境变量 `OMT_EXEC_ID=<execId>`（加入 `RESERVED_ENV_KEYS` 与 spawn 入口的保留键检查）；`LocalImpl` 与 `omt mcp` 以自身进程环境是否带它判定「我是被派发的 agent」 | `--actor exec:` 只是注入桥自报的参数；harness 手握 shell，可直接 `omt exec start`（默认 `allowFromCli: true`）。环境变量随进程继承，覆盖 CLI 与 MCP 两条路径；可被刻意清除，故定位为纵深防御（见威胁模型） |
| **Q-13** | **`workspaces: homeWorkspaceOnly` 的判定**：`local::exec_start` 解析出 home 后，`kind == global` 或 `workspace_root_of(home_path)` 等于 `$HOME` 时以 `POLICY_DENIED`（field `workspaces`）拒绝 | 否则全局 home（`~/.omt`）的 ticket 会让无人值守 harness 以用户主目录为 cwd 运行，字段形同虚设 |
| **Q-11** | **本地工具清单进 `schema/parity.schema.json` 的独立 `$defs.LocalToolMatrix`**（分类 `local_read` / `local_exec`），不进 26 条 daemon 方法的 `SeedActionParityMatrix` | 现有 contract / coverage 测试把 daemon 矩阵条目数固定为 26 并逐条走 live daemon；本地工具不经 daemon |

## 文件结构

```
crates/omt-exec/src/
  policy.rs              # 新建（A1）：ExecPolicy / LaunchOrigin / load / save / check_*
  local.rs               # 新建（A4）：本地工具核心（harness / exec / policy 的 JSON 进出）
  agents.rs              # 改（A2）：ExecRecord.launched_by、AgentRecord.parent_agent_id、NewExec 新字段
  executor.rs            # 改（A2）：dispatch_daemon_side 接 LaunchTarget + origin；transcript_page（A4）
  launch.rs              # 改（A2/A3）：LaunchRequest 新字段；跨进程锁 + policy 检查；锁在 wait_ready 前释放
  daemon_link.rs         # 改（A4）：served_homes
  layout.rs              # 改（A1）：exec_dir / policy_file / launch_lock_file
  lib.rs                 # 改：pub mod policy; pub mod local;
crates/omt-harness/src/harness.rs   # 改（A4）：list_value / models_value（桌面与 CLI 共用）
crates/omt-runtime/
  src/local_tools.rs     # 新建（A5）：LocalTools trait、LOCAL_TOOLS 表、CLI 参数解析（lib，零执行面依赖）
  src/local_impl.rs      # 新建（A5）：LocalImpl（仅 omt 二进制，经 cli_main.rs 引入）
  src/cli_main.rs        # 改（A5）：构造 LocalImpl 并注入
  src/cli/mod.rs         # 改（A5）：harness / exec 动词、real_run_code_with
  src/mcp.rs             # 改（A6）：serve 接 Option<&dyn LocalTools>，按 Q-5 暴露与路由
  src/lib.rs             # 改（A5）：pub mod local_tools;
  tests/local_tools_cli.rs   # 新建（A5/A9）：CLI 本地动词 e2e（假 harness）
  tests/mcp_spec.rs      # 改（A6）：tools/list 断言改为「daemon 子集 ∪ 本地工具（按 policy/actor）」
schema/parity.schema.json            # 改（A7）：$defs.LocalToolMatrix
crates/omt-contracts/tests/contract.rs  # 改（A7）：本地工具清单契约
packages/client-ts/src/generated/parity.ts  # 重新生成（A7）
apps/desktop/src-tauri/src/
  executor.rs            # 改（A2/A8）：派发传 LaunchOrigin::Desktop；exec_policy_get / exec_policy_set
  harness.rs             # 改（A4）：harness_list / harness_list_models 改调 omt-harness 共用函数
  lib.rs                 # 改（A8）：注册两个新 command
apps/desktop/src/
  ExecPolicySection.tsx  # 新建（A8）：设置页「派发策略」
  SettingsPanel.tsx      # 改（A8）：新增内层分段
  bridge.ts / types.ts   # 改（A8）：policy 类型与 invoke 包装
  store.ts               # 改（A8）：可见时 5s 定时 refreshAgents
  AgentsPanel.tsx        # 改（A8）：launchedBy 小标签（cli / mcp）
apps/desktop/tests/exec-policy.spec.tsx  # 新建（A8）
.github/workflows/ci.yml # 改（A5）：nm 门加 omt_harness
docs/runtime/operations.md, crates/omt-runtime/README.md  # 改（A9）：新动词
```

---

# A 部分：子项目 2（CLI / MCP 本地工具族 + policy）

### Task A1: policy 模型（`omt-exec::policy`）

**Files:**
- Create: `crates/omt-exec/src/policy.rs`
- Modify: `crates/omt-exec/src/lib.rs`（加 `pub mod policy;`）
- Modify: `crates/omt-exec/src/layout.rs`（加 `exec_dir` / `policy_file` / `launch_lock_file`）

**Interfaces:**
- Produces:
  - `pub enum LaunchOrigin { Desktop, Cli, Mcp }`（serde `"desktop" | "cli" | "mcp"`，`Copy`）
  - `pub struct ExecPolicy { allow_from_cli: bool, allow_from_mcp: bool, harnesses: Vec<String>, workspaces: WorkspaceScope, max_concurrent: u32, max_duration_sec: u64, approval_mode: ApprovalMode }`（serde camelCase）
  - `pub fn load(runtime_dir: &Path) -> Result<ExecPolicy, Problem>`（`Problem` = `omt_domain::error::Problem`；缺失 = 默认；无法解析 / 未知字段 → `POLICY_INVALID`）
  - `pub fn save(runtime_dir: &Path, policy: &ExecPolicy) -> Result<(), String>`（先 `validate` 再原子写）
  - `pub fn validate(policy: &ExecPolicy) -> Result<(), PolicyDenial>`
  - `pub fn check_launch(policy: &ExecPolicy, origin: LaunchOrigin, harness: &str) -> Result<(), PolicyDenial>`
  - `pub fn check_capacity(policy: &ExecPolicy, active: usize) -> Result<(), PolicyDenial>`
  - `pub fn effective_max_duration(policy: &ExecPolicy, requested: Option<Duration>) -> Duration`
  - `pub struct PolicyDenial { pub field: &'static str, pub reason: String }`，`impl PolicyDenial { pub fn into_problem(self) -> omt_domain::error::Problem }`
  - `layout::exec_dir(rt) -> PathBuf`、`layout::policy_file(rt) -> PathBuf`、`layout::launch_lock_file(rt) -> PathBuf`

- [ ] **Step 1: 让 `omt-exec` 能用 `Problem`（依赖 `omt-domain`）**

Run: `grep -n "omt-domain\|omt_domain" crates/omt-exec/Cargo.toml crates/omt-client/Cargo.toml`
Expected: 在 `crates/omt-exec/Cargo.toml` 的 `[dependencies]` 加 `omt-domain = { path = "../omt-domain" }`（`omt-client` 已依赖它）。**不要**依赖 `omt-storage`：它会把 rusqlite（bundled SQLite）、fs4 等拉进执行面与桌面端依赖图；`omt_storage::Problem` 只是 `omt_domain::error::Problem` 的重导出，两者是同一类型，lib 侧 `LocalTools` 与 `LocalImpl` 不受影响。

- [ ] **Step 2: 写失败测试**

在新文件 `crates/omt-exec/src/policy.rs` 底部写测试模块（先只放测试与空壳类型，`cargo test` 应编译失败或断言失败）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_design_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let p = load(tmp.path()).unwrap();
        assert!(p.allow_from_cli);
        assert!(!p.allow_from_mcp);
        assert_eq!(p.harnesses, vec!["pi", "omp", "opencode"]);
        assert_eq!(p.workspaces, WorkspaceScope::HomeWorkspaceOnly);
        assert_eq!(p.max_concurrent, 4);
        assert_eq!(p.max_duration_sec, 1800);
        assert_eq!(p.approval_mode, ApprovalMode::Inherit);
    }

    #[test]
    fn malformed_or_unknown_field_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::layout::exec_dir(tmp.path())).unwrap();
        let file = crate::layout::policy_file(tmp.path());
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load(tmp.path()).unwrap_err().code, "POLICY_INVALID");
        std::fs::write(&file, r#"{"allowFromMcp": true, "alowFromCli": false}"#).unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert_eq!(err.code, "POLICY_INVALID");
        assert!(err.message.contains("alowFromCli"), "{}", err.message);
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), &ExecPolicy { allow_from_mcp: true, ..ExecPolicy::default() }).unwrap();
        std::fs::write(crate::layout::policy_file(tmp.path()), r#"{"allowFromMcp": true}"#).unwrap();
        let p = load(tmp.path()).unwrap();
        assert!(p.allow_from_mcp);
        assert_eq!(p.max_concurrent, 4);
    }

    #[test]
    fn origin_and_whitelist_gate_launches() {
        let p = ExecPolicy::default();
        assert!(check_launch(&p, LaunchOrigin::Desktop, "anything").is_ok(), "桌面端豁免（Q-4）");
        assert!(check_launch(&p, LaunchOrigin::Cli, "pi").is_ok());
        let mcp = check_launch(&p, LaunchOrigin::Mcp, "pi").unwrap_err();
        assert_eq!(mcp.field, "allowFromMcp");
        let off_list = check_launch(&p, LaunchOrigin::Cli, "claude").unwrap_err();
        assert_eq!(off_list.field, "harnesses");
        let no_cli = ExecPolicy { allow_from_cli: false, ..ExecPolicy::default() };
        assert_eq!(check_launch(&no_cli, LaunchOrigin::Cli, "pi").unwrap_err().field, "allowFromCli");
    }

    #[test]
    fn capacity_and_duration_never_widen_policy() {
        let p = ExecPolicy::default();
        assert!(check_capacity(&p, 3).is_ok());
        assert_eq!(check_capacity(&p, 4).unwrap_err().field, "maxConcurrent");
        assert_eq!(effective_max_duration(&p, None), Duration::from_secs(1800));
        assert_eq!(effective_max_duration(&p, Some(Duration::from_secs(60))), Duration::from_secs(60));
        assert_eq!(effective_max_duration(&p, Some(Duration::from_secs(99_999))), Duration::from_secs(1800));
    }

    #[test]
    fn validate_rejects_nonsense_values() {
        let zero = ExecPolicy { max_concurrent: 0, ..ExecPolicy::default() };
        assert_eq!(validate(&zero).unwrap_err().field, "maxConcurrent");
        let unknown = ExecPolicy { harnesses: vec!["nope".into()], ..ExecPolicy::default() };
        assert_eq!(validate(&unknown).unwrap_err().field, "harnesses");
        let tiny = ExecPolicy { max_duration_sec: 10, ..ExecPolicy::default() };
        assert_eq!(validate(&tiny).unwrap_err().field, "maxDurationSec");
    }

    #[test]
    fn denial_maps_to_policy_denied_problem() {
        let problem = PolicyDenial { field: "allowFromMcp", reason: "x".into() }.into_problem();
        assert_eq!(problem.code, "POLICY_DENIED");
        assert_eq!(problem.details.as_ref().unwrap()["field"], "allowFromMcp");
    }
}
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p omt-exec --lib policy::tests`
Expected: 编译失败（`load` / `ExecPolicy` 等未定义）。

- [ ] **Step 4: 实现**

`crates/omt-exec/src/layout.rs` 追加：

```rust
/// 子项目 2：派发面的机器本地状态目录（policy / 跨进程派发锁 / 临时授权登记）。
pub fn exec_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("exec")
}

/// 设计 §3.6：`<runtime>/exec/policy.json`。
pub fn policy_file(runtime_dir: &Path) -> PathBuf {
    exec_dir(runtime_dir).join("policy.json")
}

/// Q-6：跨进程派发锁的目标路径（`atomic_json::lock_exclusive` 在其 `.lock` 上加 flock）。
pub fn launch_lock_file(runtime_dir: &Path) -> PathBuf {
    exec_dir(runtime_dir).join("launch")
}
```

`crates/omt-exec/src/policy.rs`（测试模块之上）：

```rust
//! 派发策略（设计 §3.6 / KD-E5）：`<runtime>/exec/policy.json`。
//!
//! 只约束 CLI / MCP 发起的派发（Q-4）；桌面端豁免。文件缺失 = 设计默认值；
//! 文件存在但无法解析或含未知字段 = fail closed（`POLICY_INVALID`），
//! 绝不静默回落到默认值——那会把「用户关掉的派发」重新打开。

use std::path::Path;
use std::time::Duration;

use omt_domain::error::Problem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaunchOrigin {
    Desktop,
    Cli,
    Mcp,
}

impl LaunchOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchOrigin::Desktop => "desktop",
            LaunchOrigin::Cli => "cli",
            LaunchOrigin::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceScope {
    /// 只允许在 workspace 内 `.omt` 的 home 上执行（Q-13）：全局 home 或 workspace
    /// 根等于用户主目录的派发被 `local::exec_start` 拒绝（harness 的 cwd 由 home 决定）。
    #[default]
    HomeWorkspaceOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalMode {
    /// 派发方不能改变审批模式（设计 §3.6）。注意 harness 配置里没有「是否无人值守」
    /// 开关：CLI / MCP 派发总是附加 descriptor 的无人值守参数（`--auto`、
    /// `--approval-mode=yolo` 等），与桌面端派发等效但没有那一步人工确认。
    #[default]
    Inherit,
}

const MIN_DURATION_SEC: u64 = 60;
const MAX_CONCURRENT_CAP: u32 = 32;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ExecPolicy {
    pub allow_from_cli: bool,
    pub allow_from_mcp: bool,
    pub harnesses: Vec<String>,
    pub workspaces: WorkspaceScope,
    pub max_concurrent: u32,
    pub max_duration_sec: u64,
    pub approval_mode: ApprovalMode,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        Self {
            allow_from_cli: true,
            allow_from_mcp: false,
            harnesses: vec!["pi".into(), "omp".into(), "opencode".into()],
            workspaces: WorkspaceScope::HomeWorkspaceOnly,
            max_concurrent: 4,
            max_duration_sec: 1800,
            approval_mode: ApprovalMode::Inherit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDenial {
    pub field: &'static str,
    pub reason: String,
}

impl PolicyDenial {
    pub fn into_problem(self) -> Problem {
        let field = self.field;
        Problem::with_details("POLICY_DENIED", self.reason, |d| {
            d.insert("field".into(), field.into());
        })
    }
}

fn denial(field: &'static str, reason: impl Into<String>) -> PolicyDenial {
    PolicyDenial { field, reason: reason.into() }
}

pub fn load(runtime_dir: &Path) -> Result<ExecPolicy, Problem> {
    let path = crate::layout::policy_file(runtime_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(ExecPolicy::default()),
        Err(err) => {
            return Err(Problem::new("POLICY_INVALID", format!("读取 {}: {err}", path.display())))
        }
    };
    let policy: ExecPolicy = serde_json::from_str(&text).map_err(|err| {
        Problem::new("POLICY_INVALID", format!("{} 无法解析：{err}", path.display()))
    })?;
    validate(&policy).map_err(|d| {
        Problem::new("POLICY_INVALID", format!("{} 的 {} 无效：{}", path.display(), d.field, d.reason))
    })?;
    Ok(policy)
}

pub fn save(runtime_dir: &Path, policy: &ExecPolicy) -> Result<(), String> {
    validate(policy).map_err(|d| format!("{}：{}", d.field, d.reason))?;
    let text = serde_json::to_string_pretty(policy).map_err(|e| e.to_string())?;
    let path = crate::layout::policy_file(runtime_dir);
    let _lock = omt_harness::atomic_json::lock_exclusive(&path, Duration::from_secs(10))?;
    omt_harness::atomic_json::write_atomic(&path, &text)
}

pub fn validate(policy: &ExecPolicy) -> Result<(), PolicyDenial> {
    if policy.max_concurrent == 0 || policy.max_concurrent > MAX_CONCURRENT_CAP {
        return Err(denial("maxConcurrent", format!("必须在 1..={MAX_CONCURRENT_CAP} 之间")));
    }
    if policy.max_duration_sec < MIN_DURATION_SEC {
        return Err(denial("maxDurationSec", format!("不得小于 {MIN_DURATION_SEC} 秒")));
    }
    if let Some(unknown) = policy
        .harnesses
        .iter()
        .find(|id| omt_harness::harness::descriptor_of(id).is_none())
    {
        return Err(denial("harnesses", format!("未知的 harness：{unknown}")));
    }
    Ok(())
}

pub fn check_launch(
    policy: &ExecPolicy,
    origin: LaunchOrigin,
    harness: &str,
) -> Result<(), PolicyDenial> {
    match origin {
        LaunchOrigin::Desktop => return Ok(()),
        LaunchOrigin::Cli if !policy.allow_from_cli => {
            return Err(denial("allowFromCli", "policy 未允许经 CLI 派发（exec/policy.json allowFromCli）"))
        }
        LaunchOrigin::Mcp if !policy.allow_from_mcp => {
            return Err(denial(
                "allowFromMcp",
                "policy 未允许经 MCP 派发——请在桌面端 设置 → 派发策略 中开启（exec/policy.json allowFromMcp）",
            ))
        }
        _ => {}
    }
    if !policy.harnesses.iter().any(|id| id == harness) {
        return Err(denial("harnesses", format!("harness {harness} 不在 policy 白名单内")));
    }
    Ok(())
}

pub fn check_capacity(policy: &ExecPolicy, active: usize) -> Result<(), PolicyDenial> {
    if active >= policy.max_concurrent as usize {
        return Err(denial(
            "maxConcurrent",
            format!("已有 {active} 个运行中的执行，达到 policy 上限 {}", policy.max_concurrent),
        ));
    }
    Ok(())
}

pub fn effective_max_duration(policy: &ExecPolicy, requested: Option<Duration>) -> Duration {
    let cap = Duration::from_secs(policy.max_duration_sec);
    requested.map_or(cap, |r| r.min(cap))
}
```

`crates/omt-exec/src/lib.rs` 加 `pub mod policy;`（按字母序插在 `pub mod mcp_injection;` 之后）。

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p omt-exec --lib policy::tests`
Expected: 7 passed。

- [ ] **Step 6: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: 全绿。

```bash
git add crates/omt-exec/src/policy.rs crates/omt-exec/src/layout.rs crates/omt-exec/src/lib.rs crates/omt-exec/Cargo.toml Cargo.lock
git commit -m "feat(exec): exec/policy.json model with fail-closed loading (sub-project 2 A1)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A2: launcher 发起方、派发目标与记录字段

**Files:**
- Modify: `crates/omt-exec/src/agents.rs`（`ExecRecord.launched_by`、`AgentRecord.parent_agent_id`、`NewExec` 新字段、`register_agent` / `append_exec` 写入）
- Modify: `crates/omt-exec/src/executor.rs`（`LaunchTarget`；`dispatch_daemon_side` 签名与 ②/③ 分支）
- Modify: `crates/omt-exec/src/launch.rs`（`LaunchRequest` 新字段；spec 的 `node_id`）
- Modify: `apps/desktop/src-tauri/src/executor.rs`（`exec_start` 传 `LaunchOrigin::Desktop` 与 `LaunchTarget::Node`）
- Modify: `crates/omt-runtime/tests/exec_launch.rs`、`crates/omt-runtime/tests/exec_survives_launcher.rs`（`LaunchRequest` 字面量补字段）
- Test: `crates/omt-exec/src/executor.rs` tests 模块

**Interfaces:**
- Consumes: `policy::LaunchOrigin`（A1）
- Produces:
  - `pub enum LaunchTarget<'a> { Node(&'a str), Run(&'a str) }`（`executor.rs`，`Copy`）
  - `LaunchRequest` 新增 `pub origin: LaunchOrigin`、`pub target: LaunchTarget<'a>`、`pub parent_agent_id: Option<&'a str>`；**删除** `node_id` 字段（由 `target` 取代）
  - `ExecRecord.launched_by: Option<LaunchOrigin>`；`AgentRecord.parent_agent_id: Option<String>`
  - `NewExec` 新增 `pub launched_by: Option<LaunchOrigin>`、`pub parent_agent_id: Option<String>`
  - `dispatch_daemon_side(rpc, runtime_dir, home_id, target: LaunchTarget, origin: LaunchOrigin, provider_id, unattended, model, resume_session, target_agent_id, parent_agent_id)`

- [ ] **Step 1: 写失败测试（executor.rs tests 模块，紧挨 `dispatch_skips_node_execute_when_reused_run_holds_pending_item`）**

```rust
    /// Q-2：CLI/MCP 发起时无论 run 新建与否都不调用 node/execute（adapter_only）。
    #[test]
    fn dispatch_from_cli_or_mcp_never_calls_node_execute() {
        let dir = tempfile::tempdir().unwrap();
        let program = write_script(dir.path(), "cli-origin", "exit 0");
        let (runtime, _home_path) = setup_dispatch_env(dir.path(), "pi", &program);
        for origin in [LaunchOrigin::Cli, LaunchOrigin::Mcp] {
            let fake = FakeDaemon::new(
                dispatch_node_get(serde_json::json!([])),
                dispatch_run_get("pending"),
                "run-new",
            );
            let rpc = |method: &str, params: serde_json::Value| fake.rpc(method, params);
            let plan = dispatch_daemon_side(
                &rpc, &runtime, "h_test", LaunchTarget::Node("t1"), origin, "pi",
                false, None, None, None, None,
            )
            .expect("派发应成功");
            assert!(!fake.method_names().iter().any(|m| m == "node/execute"), "{origin:?}");
            assert_eq!(plan.agent.execs[0].launched_by, Some(origin));
        }
    }

    /// Q-9：Run 目标直接用已有 run——不收集成员、不 create、不写 binding；
    /// pending 则 start。
    #[test]
    fn dispatch_run_target_reuses_given_run() {
        let dir = tempfile::tempdir().unwrap();
        let program = write_script(dir.path(), "run-target", "exit 0");
        let (runtime, _home_path) = setup_dispatch_env(dir.path(), "pi", &program);
        let fake = FakeDaemon::new(
            dispatch_node_get(serde_json::json!([])),
            dispatch_run_get("pending"),
            "run-should-not-be-created",
        );
        let rpc = |method: &str, params: serde_json::Value| fake.rpc(method, params);
        let plan = dispatch_daemon_side(
            &rpc, &runtime, "h_test", LaunchTarget::Run("RUN-0042"), LaunchOrigin::Mcp, "pi",
            false, None, None, None, Some("agent-master"),
        )
        .expect("Run 目标派发应成功");
        assert_eq!(plan.run_id, "RUN-0042");
        assert!(!plan.run_was_newly_created, "给定的 run 绝不能被补偿 cancel");
        let names = fake.method_names();
        assert!(!names.iter().any(|m| m == "node/get" || m == "run/create" || m == "node/execute"), "{names:?}");
        assert!(names.iter().any(|m| m == "run/control"), "pending run 需要 start: {names:?}");
        assert_eq!(plan.agent.execs[0].node_id, None);
        assert_eq!(plan.agent.parent_agent_id.as_deref(), Some("agent-master"));
        assert!(plan.prompt.contains("RUN-0042"));
    }

    /// Q-9：Run 目标在 paused / interrupted / 终态时拒绝，且不 cancel 它。
    #[test]
    fn dispatch_run_target_rejects_non_startable_run() {
        let dir = tempfile::tempdir().unwrap();
        let program = write_script(dir.path(), "run-bad", "exit 0");
        let (runtime, _home_path) = setup_dispatch_env(dir.path(), "pi", &program);
        for status in ["paused", "completed", "canceled"] {
            let fake = FakeDaemon::new(
                dispatch_node_get(serde_json::json!([])),
                dispatch_run_get(status),
                "unused",
            );
            let rpc = |method: &str, params: serde_json::Value| fake.rpc(method, params);
            let err = dispatch_daemon_side(
                &rpc, &runtime, "h_test", LaunchTarget::Run("RUN-0042"), LaunchOrigin::Cli, "pi",
                false, None, None, None, None,
            )
            .expect_err(status);
            assert!(err.contains(status), "{err}");
            assert!(fake.cancel_params().is_empty(), "{status}: 不得 cancel 调用方给定的 run");
        }
    }
```

同时把本文件 tests 模块里所有既有的 `dispatch_daemon_side(&rpc, &runtime, "h_test", "t1", "<harness>", false, None, None, None)` 调用改为新签名 `dispatch_daemon_side(&rpc, &runtime, "h_test", LaunchTarget::Node("t1"), LaunchOrigin::Desktop, "<harness>", false, None, None, None, None)`（`grep -n 'dispatch_daemon_side(' crates/omt-exec/src/executor.rs` 列出全部调用点逐个改），并在 tests 模块顶部 `use crate::policy::LaunchOrigin;`。

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p omt-exec --lib dispatch_`
Expected: 编译失败（`LaunchTarget`、新参数、`launched_by`、`parent_agent_id` 未定义）。

- [ ] **Step 3: 记录字段（agents.rs）**

在 `ExecRecord` 末尾（`lease_release_pending` 之后）加：

```rust
    /// Q-7：本次派发的发起方（desktop / cli / mcp）；子项目 2 之前的旧记录为 None。
    #[serde(default)]
    pub launched_by: Option<crate::policy::LaunchOrigin>,
```

在 `AgentRecord` 的 `execs` 之前加：

```rust
    /// Q-7：父 agent（主控）。只在新建 agent 时由显式参数写入；自动祖先链关联属子项目 3。
    #[serde(default)]
    pub parent_agent_id: Option<String>,
```

`NewExec` 加两个字段：

```rust
    /// Q-7：发起方，写入 ExecRecord.launched_by。
    pub launched_by: Option<crate::policy::LaunchOrigin>,
    /// Q-7：仅 register_agent 使用（续跑沿用 agent 既有的父关系）。
    pub parent_agent_id: Option<String>,
```

`register_agent` 的 `AgentRecord { ... }` 字面量加 `parent_agent_id: new_exec.parent_agent_id.clone(),`，其 `ExecRecord { ... }` 加 `launched_by: new_exec.launched_by,`；`append_exec` 的 `agent.execs.push(ExecRecord { ... })` 加 `launched_by: new_exec.launched_by,`（不改 `parent_agent_id`）。用 `grep -n "NewExec {" -r crates apps` 找到其余构造点（测试与 executor）补 `launched_by: None, parent_agent_id: None`（executor 的派发路径在 Step 4 填真实值）。

- [ ] **Step 4: 派发链（executor.rs）**

在 `DispatchPlan` 定义附近加：

```rust
/// Q-9：派发目标。Node = 与桌面端相同的成员收集 + run 复用；Run = 直接用给定 run。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchTarget<'a> {
    Node(&'a str),
    Run(&'a str),
}

impl LaunchTarget<'_> {
    pub fn node_id(&self) -> Option<&str> {
        match self {
            LaunchTarget::Node(id) => Some(id),
            LaunchTarget::Run(_) => None,
        }
    }
}
```

`dispatch_daemon_side` 签名改为：

```rust
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_daemon_side(
    rpc: DaemonRpc<'_>,
    runtime_dir: &Path,
    home_id: &str,
    target: LaunchTarget<'_>,
    origin: crate::policy::LaunchOrigin,
    provider_id: &str,
    unattended: bool,
    model: Option<&str>,
    resume_session: Option<&str>,
    target_agent_id: Option<&str>,
    parent_agent_id: Option<&str>,
) -> Result<DispatchPlan, String> {
```

函数体的改动（其余保持不变）：

1. 把「① 预检 … active_runs … reused_pending_item … ② node/execute」整段包进按目标分支的块，产出 `(active_runs, members)`：

```rust
    // ① 预检（仅 Node 目标）；Run 目标的 run 由调用方给定，③ 直接使用
    let (members, active_runs, reused_pending_item) = match target {
        LaunchTarget::Node(node_id) => {
            let node_resp = rpc(
                "node/get",
                serde_json::json!({"homeId": home_id, "nodeId": node_id}),
            )?;
            let members = collect_members(rpc, home_id, node_id)?;
            if members.is_empty() {
                return Err("无未完成可执行成员".into());
            }
            let run_links = node_resp["runs"].as_array().cloned().unwrap_or_default();
            let active_runs: Vec<String> = run_links
                .iter()
                .filter_map(|r| r["runId"].as_str().map(str::to_owned))
                .collect();
            // F-ACC-12（保留原注释）
            let reused_pending_item = run_links.first().is_some_and(|link| {
                link["status"].as_str() == Some("running")
                    && link["itemState"].as_str() == Some("pending")
            });
            (members, active_runs, reused_pending_item)
        }
        LaunchTarget::Run(run_id) => (Vec::new(), vec![run_id.to_string()], false),
    };

    // ② node/execute：仅桌面端（adapter_only，P-3）发起的 Node 目标调用；
    // CLI/MCP 一律跳过，由 exec 自己 claim（Q-2）
    if let (LaunchTarget::Node(node_id), crate::policy::LaunchOrigin::Desktop) = (target, origin) {
        if !reused_pending_item {
            rpc(
                "node/execute",
                serde_json::json!({
                    "homeId": home_id, "nodeId": node_id, "commandId": format!("{dispatch_id}:execute"),
                }),
            )?;
        }
    }
```

2. ③ ensure-run 的 `run/create` 分支里 `"title": format!("dispatch {node_id}")` 改为 `"title": format!("dispatch {}", target.node_id().unwrap_or_default())`（Run 目标走 `active_runs.first()` 分支，不会到这里）。

3. ④ 的 `match run_status.as_str()` 不变——Run 目标遇到 paused / interrupted / 终态时走既有的报错分支，`run_was_newly_created == false` 保证补偿不 cancel 给定的 run。把 `other =>` 分支文案改为包含状态名：`format!("run {run_id} 状态 {other}，无法派发")`（已含）。

4. ⑤ 的 `NewExec { ... }` 改为：

```rust
    let new_exec = crate::agents::NewExec {
        exec_id: exec_id.clone(),
        node_id: target.node_id().map(str::to_string),
        run_id: Some(run_id.clone()),
        binary_path: Some(plan.program.display().to_string()),
        binary_args: sanitize_args(&args),
        model: model.map(str::to_string),
        launched_by: Some(origin),
        parent_agent_id: parent_agent_id.map(str::to_string),
    };
```

5. ⑥ 绑定只对 Node 目标写：

```rust
    let warnings: Vec<String> = match target {
        LaunchTarget::Node(node_id) => agents::write_binding(runtime_dir, home_id, node_id, &agent.id)
            .err()
            .map(|e| format!("绑定写入失败：{e}"))
            .into_iter()
            .collect(),
        LaunchTarget::Run(_) => Vec::new(),
    };
```

6. 所有原先引用 `node_id` 参数的地方改用上面的分支变量；`cargo build -p omt-exec` 会指出遗漏。

- [ ] **Step 5: LaunchRequest（launch.rs）**

```rust
pub struct LaunchRequest<'a> {
    pub runtime_dir: &'a Path,
    /// `omt` 二进制：既是 shim，也是注入给 harness 的 MCP 桥
    pub omt_bin: &'a Path,
    pub home_id: &'a str,
    /// `.omt` 目录（Q-3：CLI/MCP 发起时取自 daemon 的 homeId→path 映射）
    pub home_path: &'a Path,
    /// Q-9：派发目标（取代原 node_id）
    pub target: crate::executor::LaunchTarget<'a>,
    /// Q-2 / Q-4：发起方；非 Desktop 时跳过 node/execute 并受 policy 约束（A3）
    pub origin: crate::policy::LaunchOrigin,
    pub harness: &'a str,
    pub unattended: bool,
    pub model: Option<&'a str>,
    pub resume_session: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
    /// Q-7：父 agent（仅新建 agent 时写入）
    pub parent_agent_id: Option<&'a str>,
    pub max_duration: Duration,
}
```

`start` 里对 `dispatch_daemon_side` 的调用改为传 `req.target, req.origin, req.harness, req.unattended, req.model, req.resume_session, req.target_agent_id, req.parent_agent_id`；spec 的 `node_id: Some(req.node_id.to_string())` 改为 `node_id: req.target.node_id().map(str::to_string)`。

- [ ] **Step 6: 调用方补字段**

- `apps/desktop/src-tauri/src/executor.rs` 的 `exec_start`：`node_id: &node_id` 改为 `target: omt_exec::executor::LaunchTarget::Node(&node_id), origin: omt_exec::policy::LaunchOrigin::Desktop, parent_agent_id: None,`。
- `crates/omt-exec/src/executor.rs` tests 里的 `LaunchRequest { ... }` 字面量（`grep -n "LaunchRequest {" -r crates apps`）同样替换 `node_id`，并补 `origin: LaunchOrigin::Desktop, parent_agent_id: None`。
- `crates/omt-runtime/tests/exec_launch.rs` 的 `fn request(..) -> LaunchRequest` 与 `exec_survives_launcher.rs` 的字面量同样替换。

- [ ] **Step 7: 运行，确认通过**

Run: `cargo test -p omt-exec --lib dispatch_ && cargo test -p omt-runtime --test exec_launch --test exec_survives_launcher && cargo test -p oh-my-ticket-desktop`
Expected: 全部通过；`dispatch_success_builds_dispatch_plan` 仍断言桌面路径调用 `node/execute`（未改动）。

- [ ] **Step 8: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && (cd apps/desktop && pnpm test && pnpm exec tsc --noEmit)`

```bash
git add crates/omt-exec apps/desktop/src-tauri/src/executor.rs crates/omt-runtime/tests/exec_launch.rs crates/omt-runtime/tests/exec_survives_launcher.rs
git commit -m "feat(exec): launch origin, node/run targets and launchedBy/parentAgentId records (A2)

CLI/MCP-originated dispatch never calls the adapter-only node/execute
(Q-2); a run target reuses the caller's run without collecting members
or writing a binding (Q-9).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A3: 跨进程派发锁 + policy 执行点

**Files:**
- Modify: `crates/omt-exec/src/launch.rs`
- Test: `crates/omt-exec/src/launch.rs`（新增 `#[cfg(test)] mod policy_gate_tests`）、`crates/omt-runtime/tests/exec_launch.rs`

**Interfaces:**
- Consumes: `policy::{load, check_launch, check_capacity, effective_max_duration}`（A1）、`control::active_exec_count`、`layout::launch_lock_file`（A1）
- Produces: `launch::start` 在 `origin != Desktop` 时返回 `Err(String)`，其文本以 `"POLICY_DENIED:"` / `"POLICY_INVALID:"` 开头（A5 的 `LocalImpl` 据此转回 `Problem`）；新增 `pub fn problem_from_launch_error(err: &str) -> (&'static str /*code*/, String /*message*/, Option<String> /*field*/)`（`Problem.code` 是 `&'static str`）

- [ ] **Step 1: 写失败测试**

`crates/omt-exec/src/launch.rs` 末尾：

```rust
#[cfg(test)]
mod policy_gate_tests {
    use super::*;
    use crate::policy::{ExecPolicy, LaunchOrigin};

    fn req<'a>(rt: &'a Path, home: &'a Path, bin: &'a Path, origin: LaunchOrigin) -> LaunchRequest<'a> {
        LaunchRequest {
            runtime_dir: rt,
            omt_bin: bin,
            home_id: "h_test",
            home_path: home,
            target: crate::executor::LaunchTarget::Node("t1"),
            origin,
            harness: "pi",
            unattended: true,
            model: None,
            resume_session: None,
            target_agent_id: None,
            parent_agent_id: None,
            max_duration: crate::executor::DEFAULT_MAX_DURATION,
        }
    }

    /// MCP 默认被拒，且被拒时不发出任何 daemon 调用。
    #[test]
    fn mcp_origin_denied_by_default_before_any_rpc() {
        let tmp = tempfile::tempdir().unwrap();
        let calls = std::sync::Mutex::new(Vec::<String>::new());
        let rpc = |m: &str, _p: serde_json::Value| {
            calls.lock().unwrap().push(m.to_string());
            Err::<serde_json::Value, String>("unreachable".into())
        };
        let bin = tmp.path().join("omt");
        let err = start(&rpc, &req(tmp.path(), tmp.path(), &bin, LaunchOrigin::Mcp)).unwrap_err();
        assert!(err.starts_with("POLICY_DENIED:"), "{err}");
        assert!(err.contains("allowFromMcp"), "{err}");
        assert!(calls.lock().unwrap().is_empty());
    }

    /// 无效 policy fail closed。
    #[test]
    fn invalid_policy_fails_closed_for_cli() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::layout::exec_dir(tmp.path())).unwrap();
        std::fs::write(crate::layout::policy_file(tmp.path()), "{bad").unwrap();
        let rpc = |_m: &str, _p: serde_json::Value| Ok(serde_json::json!({}));
        let bin = tmp.path().join("omt");
        let err = start(&rpc, &req(tmp.path(), tmp.path(), &bin, LaunchOrigin::Cli)).unwrap_err();
        assert!(err.starts_with("POLICY_INVALID:"), "{err}");
    }

    /// 桌面端豁免：policy 拒绝 CLI/MCP 时桌面端仍走原链（这里以 rpc 第一次被调用为证）。
    #[test]
    fn desktop_origin_ignores_policy() {
        let tmp = tempfile::tempdir().unwrap();
        crate::policy::save(tmp.path(), &ExecPolicy { allow_from_cli: false, ..ExecPolicy::default() }).unwrap();
        let first = std::sync::Mutex::new(None::<String>);
        let rpc = |m: &str, _p: serde_json::Value| {
            first.lock().unwrap().get_or_insert(m.to_string());
            Err::<serde_json::Value, String>("stop here".into())
        };
        let bin = tmp.path().join("omt");
        let err = start(&rpc, &req(tmp.path(), tmp.path(), &bin, LaunchOrigin::Desktop)).unwrap_err();
        assert_eq!(err, "stop here");
        assert_eq!(first.lock().unwrap().as_deref(), Some("node/get"));
    }

    #[test]
    fn launch_error_round_trips_to_problem_parts() {
        let (code, message, field) =
            problem_from_launch_error("POLICY_DENIED:maxConcurrent:已有 4 个运行中的执行");
        assert_eq!(code, "POLICY_DENIED");
        assert_eq!(field.as_deref(), Some("maxConcurrent"));
        assert_eq!(message, "已有 4 个运行中的执行");
        let (code, _, field) = problem_from_launch_error("无未完成可执行成员");
        assert_eq!(code, "LAUNCH_FAILED");
        assert!(field.is_none());
    }
}
```

`crates/omt-runtime/tests/exec_launch.rs` 加一个并发上限测试。按该文件真实的 helper 写：`setup(body) -> (TempDir, PathBuf /*runtime*/, PathBuf /*home_path*/)`、`isolated(&Path) -> MutexGuard`、`FakeDaemon { calls: Mutex::new(vec![]) }`（没有 `Default`）、`request(&runtime, &home_path, &omt) -> LaunchRequest`（A2 之后它的字面量已含 `target` / `origin: LaunchOrigin::Desktop` / `parent_agent_id: None`）。收尾必须照 `launch_returns_once_harness_is_spawned_and_exec_finishes_later` 的做法（review F2）：停止后等 shim 退出，再 `omt daemon-stop`——SIGTERM 后的 shim 会为释放租约拉起真 daemon，测试提前返回会把它泄漏在机器上。

```rust
#[test]
fn cli_launches_respect_max_concurrent_across_calls() {
    let (t, runtime, home_path) = setup("sleep 5\nexit 0");
    let _serial = isolated(t.path());
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    omt_exec::policy::save(
        &runtime,
        &omt_exec::policy::ExecPolicy { max_concurrent: 1, ..Default::default() },
    )
    .unwrap();
    let fake = FakeDaemon { calls: Mutex::new(vec![]) };
    let rpc = |m: &str, p: serde_json::Value| fake.rpc(m, p);
    let mut first = request(&runtime, &home_path, &omt);
    first.origin = omt_exec::policy::LaunchOrigin::Cli;
    let outcome = omt_exec::launch::start(&rpc, &first).expect("第一个在上限内");
    let mut second = request(&runtime, &home_path, &omt);
    second.origin = omt_exec::policy::LaunchOrigin::Cli;
    let err = omt_exec::launch::start(&rpc, &second).unwrap_err();
    assert!(err.starts_with("POLICY_DENIED:maxConcurrent"), "{err}");

    // 收尾（review F2）：停止 → 等 shim 退出 → 停掉它为释放租约拉起的 daemon
    let _ = omt_exec::control::stop_exec(&runtime, &outcome.exec_id);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let agent = omt_exec::agents::get_agent(&runtime, "h_test", &outcome.agent_id)
            .unwrap()
            .unwrap();
        if !omt_exec::control::shim_alive(&agent.execs[0]) {
            break;
        }
        assert!(Instant::now() < deadline, "shim 未在 30s 内退出");
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::process::Command::new(&omt)
        .args(["--runtime-dir", runtime.to_str().unwrap(), "daemon-stop"])
        .status();
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p omt-exec --lib policy_gate_tests && cargo test -p omt-runtime --test exec_launch cli_launches_respect_max_concurrent_across_calls`
Expected: FAIL（尚无 policy 检查；第二次派发成功）。

- [ ] **Step 3: 实现**

`launch.rs`：删除 `static LAUNCH_LOCK`，`start` 开头改为：

```rust
const LAUNCH_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// A3：policy 拒绝 / 无效时的错误文本前缀；LocalImpl 经
/// problem_from_launch_error 还原为结构化 Problem。
fn policy_err(problem: omt_domain::error::Problem) -> String {
    format!("{}:{}", problem.code, problem.message)
}

fn denial_err(d: crate::policy::PolicyDenial) -> String {
    format!("POLICY_DENIED:{}:{}", d.field, d.reason)
}

/// `"CODE:field:message"` / `"CODE:message"` / 其他 → ("LAUNCH_FAILED", 原文, None)。
pub fn problem_from_launch_error(err: &str) -> (&'static str, String, Option<String>) {
    if let Some(rest) = err.strip_prefix("POLICY_DENIED:") {
        if let Some((field, message)) = rest.split_once(':') {
            return ("POLICY_DENIED", message.to_string(), Some(field.to_string()));
        }
    }
    if let Some(rest) = err.strip_prefix("POLICY_INVALID:") {
        return ("POLICY_INVALID", rest.to_string(), None);
    }
    ("LAUNCH_FAILED", err.to_string(), None)
}

pub fn start(rpc: DaemonRpc<'_>, req: &LaunchRequest<'_>) -> Result<ExecStartOutcome, String> {
    if let Some(model) = req.model {
        harness::validate_model_id(model)?;
    }
    if let Some(session) = req.resume_session {
        harness::validate_model_id(session)?;
    }
    let runtime_dir = req.runtime_dir;
    let home_id = req.home_id;
    // Q-4：policy 只约束 CLI/MCP；先于任何 daemon 调用判定（被拒不留痕）
    let policy = match req.origin {
        crate::policy::LaunchOrigin::Desktop => None,
        _ => {
            let policy = crate::policy::load(runtime_dir).map_err(policy_err)?;
            crate::policy::check_launch(&policy, req.origin, req.harness).map_err(denial_err)?;
            Some(policy)
        }
    };
    let max_duration = match &policy {
        Some(p) => crate::policy::effective_max_duration(p, Some(req.max_duration)),
        None => req.max_duration,
    };
    // Q-6：跨进程派发锁——并发计数与登记、spawn_shim、身份记录同在锁内；
    // wait_ready 之前释放（§39：锁不再覆盖 30s 就绪等待）
    let launch_lock = omt_harness::atomic_json::lock_exclusive(
        &layout::launch_lock_file(runtime_dir),
        LAUNCH_LOCK_TIMEOUT,
    )?;
    if let Some(p) = &policy {
        crate::policy::check_capacity(p, crate::control::active_exec_count(runtime_dir))
            .map_err(denial_err)?;
    }
```

其后原有逻辑不变，但：
- spec 的 `max_duration_secs: req.max_duration.as_secs()` 改为 `max_duration_secs: max_duration.as_secs()`；
- 在「⑧ 记录 shim 身份成功」之后、「⑨ wait_ready」之前插入 `drop(launch_lock);`；
- 原 `let _guard = LAUNCH_LOCK…` 一行删除。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p omt-exec --lib policy_gate_tests && cargo test -p omt-runtime --test exec_launch`
Expected: 全部通过。

- [ ] **Step 5: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/omt-exec/src/launch.rs crates/omt-runtime/tests/exec_launch.rs
git commit -m "feat(exec): cross-process launch lock and policy gate for CLI/MCP launches (A3)

The in-process LAUNCH_LOCK becomes a flock on exec/launch.lock held from
the capacity check through shim identity, released before wait_ready.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A4: 本地工具核心（`omt-exec::local` + 共用的 harness 投影 + 分页 tail）

**Files:**
- Modify: `crates/omt-harness/src/harness.rs`（`list_value`、`models_value`）
- Modify: `apps/desktop/src-tauri/src/harness.rs`（`harness_list` / `harness_list_models` 改调共用函数）
- Modify: `crates/omt-exec/src/executor.rs`（`transcript_page`）
- Modify: `crates/omt-exec/src/daemon_link.rs`（`served_homes`）
- Create: `crates/omt-exec/src/local.rs`
- Modify: `crates/omt-exec/src/lib.rs`（`pub mod local;`）

**Interfaces:**
- Consumes: A1、A2、A3
- Produces（全部 `Result<serde_json::Value, omt_domain::error::Problem>`——与 omt-runtime 的 `omt_storage::Problem` 是同一类型——供 A5 的 `LocalImpl` 直接转发）：
  - `omt_harness::harness::list_value(runtime_dir: &Path) -> Result<Value, String>` → `{harnesses: [harness_entry…]}`
  - `omt_harness::harness::models_value(runtime_dir: &Path, harness: &str) -> Result<Value, String>` → `{models: [{id, group}]}`
  - `omt_exec::executor::transcript_page(runtime_dir, home_id, exec_id, since: Option<u64>, limit: usize) -> Result<Value, String>` → `{events, nextSeq, done}`
  - `omt_exec::daemon_link::served_homes(runtime_dir: &Path, daemon_bin: Option<&Path>) -> Result<Vec<ServedHome>, String>`，`pub struct ServedHome { pub home_id: String, pub path: PathBuf, pub kind: String /* "workspace" | "global" */ }`
  - `omt_exec::local::Ctx { runtime_dir: PathBuf, omt_bin: PathBuf, daemon_bin: Option<PathBuf> }`
  - `local::harness_list(&Ctx)`（本地工具路径脱敏：每个条目的 `env` 换成 `envKeys`（仅键名），`extraArgs` 经 `sanitize_args`）、`local::harness_models(&Ctx, harness: &str)`、`local::policy_show(&Ctx)`
  - `local::exec_start(&Ctx, origin: LaunchOrigin, args: &Value)`（args 键：`homeId`；仅 CLI 另认 `home`（路径或 homeId）；`harness`、`nodeId` | `runId`、`model?`、`agentId?`、`parentAgentId?`、`maxDurationSec?`）→ `{agentId, execId, runId, warnings}`
  - `local::exec_stop(&Ctx, exec_id: &str)` → `{execId, stopped: true}`（不受 `allowFrom*` / policy 有效性约束，Q-4；嵌套派发由 `LocalImpl` 拦截，Q-12）
  - `local::workspace_scope_denial(kind: &str, home_path: &Path, user_home: Option<&Path>) -> Option<PolicyDenial>`（Q-13 的纯判定，单测覆盖）
  - `local::exec_status(&Ctx, exec_id: &str)` → `{homeId, agent:{id, displayName, harness, status, attachment, parentAgentId}, exec:{…ExecRecord，binaryArgs 已脱敏}, active: bool}`
  - `local::exec_list(&Ctx, home_id: Option<&str>)` → `{execs: [{homeId, agentId, displayName, harness, status, execId, nodeId, runId, launchedBy, startedAt, active}]}`
  - `local::exec_tail(&Ctx, exec_id: &str, since: Option<u64>, limit: Option<usize>)` → `transcript_page` 的结果

- [ ] **Step 1: 写失败测试**

`crates/omt-harness/src/harness.rs` tests 模块（已有）加：

```rust
    #[test]
    fn list_value_projects_every_registry_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let value = list_value(tmp.path()).unwrap();
        let ids: Vec<&str> = value["harnesses"].as_array().unwrap().iter()
            .map(|h| h["id"].as_str().unwrap()).collect();
        let expected: Vec<&str> = REGISTRY.iter().map(|d| d.id).collect();
        assert_eq!(ids, expected);
        assert!(value["harnesses"][0].get("env").is_some(), "投影与桌面端 harness_list 同形");
    }

    #[test]
    fn models_value_rejects_unknown_harness() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(models_value(tmp.path(), "nope").unwrap_err().contains("nope"));
    }
```

`crates/omt-exec/src/local.rs` 底部测试（假 harness 与 agent 记录用 `agents::register_agent` 直接造）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{register_agent, NewExec};

    fn ctx(rt: &Path) -> Ctx {
        Ctx { runtime_dir: rt.to_path_buf(), omt_bin: rt.join("omt"), daemon_bin: None }
    }

    fn seed_exec(rt: &Path, exec_id: &str, lines: &[&str]) -> String {
        let agent = register_agent(rt, "h_t", "pi", NewExec {
            exec_id: exec_id.into(), node_id: Some("T-1".into()), run_id: Some("RUN-1".into()),
            binary_path: None, binary_args: vec!["--api-key".into(), "sk-secret".into()],
            model: None, launched_by: Some(LaunchOrigin::Cli), parent_agent_id: None,
        }).unwrap();
        let dir = crate::layout::agent_dir(rt, "h_t", &agent.id).unwrap();
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(crate::layout::exec_file(&dir, exec_id, crate::layout::ExecFile::Jsonl), body).unwrap();
        agent.id
    }

    #[test]
    fn exec_list_and_status_project_records() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = seed_exec(tmp.path(), "exec-a", &[]);
        let list = exec_list(&ctx(tmp.path()), None).unwrap();
        assert_eq!(list["execs"][0]["execId"], "exec-a");
        assert_eq!(list["execs"][0]["launchedBy"], "cli");
        assert_eq!(list["execs"][0]["agentId"], agent_id.as_str());
        let status = exec_status(&ctx(tmp.path()), "exec-a").unwrap();
        assert_eq!(status["homeId"], "h_t");
        assert_eq!(status["agent"]["harness"], "pi");
        let args = status["exec"]["binaryArgs"].to_string();
        assert!(!args.contains("sk-secret"), "argv 输出前必须脱敏：{args}");
    }

    #[test]
    fn exec_status_unknown_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(exec_status(&ctx(tmp.path()), "exec-nope").unwrap_err().code, "NOT_FOUND");
    }

    #[test]
    fn exec_tail_pages_by_seq_and_reports_done() {
        let tmp = tempfile::tempdir().unwrap();
        seed_exec(tmp.path(), "exec-t", &[
            r#"{"kind":"session_header","seq":0}"#,
            r#"{"kind":"session_start","execId":"exec-t","seq":1}"#,
            r#"{"kind":"raw","execId":"exec-t","seq":2,"text":"a"}"#,
            r#"{"kind":"raw","execId":"exec-t","seq":3,"text":"b"}"#,
            r#"{"kind":"exit","execId":"exec-t","seq":4,"status":"exited"}"#,
        ]);
        let first = exec_tail(&ctx(tmp.path()), "exec-t", None, Some(2)).unwrap();
        let seqs: Vec<u64> = first["events"].as_array().unwrap().iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(first["nextSeq"], 2);
        assert_eq!(first["done"], false);
        let rest = exec_tail(&ctx(tmp.path()), "exec-t", Some(2), Some(10)).unwrap();
        let seqs: Vec<u64> = rest["events"].as_array().unwrap().iter().map(|e| e["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, vec![3, 4]);
        assert_eq!(rest["done"], true);
    }

    #[test]
    fn exec_start_is_policy_gated_before_daemon_contact() {
        let tmp = tempfile::tempdir().unwrap();
        let err = exec_start(&ctx(tmp.path()), LaunchOrigin::Mcp, &serde_json::json!({
            "homeId": "h_t", "harness": "pi", "nodeId": "T-1"
        })).unwrap_err();
        assert_eq!(err.code, "POLICY_DENIED");
        assert_eq!(err.details.as_ref().unwrap()["field"], "allowFromMcp");
    }

    /// Q-4：停止不受 allowFrom* 与 policy 文件有效性约束（止损方向不 fail closed）。
    #[test]
    fn exec_stop_ignores_policy_even_when_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::layout::exec_dir(tmp.path())).unwrap();
        std::fs::write(crate::layout::policy_file(tmp.path()), "{broken").unwrap();
        // 不存在的 exec → NOT_FOUND，而不是 POLICY_INVALID / POLICY_DENIED
        assert_eq!(exec_stop(&ctx(tmp.path()), "exec-x").unwrap_err().code, "NOT_FOUND");
    }

    /// 本地工具路径不泄露 harness env 的值（secrets 纪律）。
    #[test]
    fn harness_list_redacts_env_values_and_extra_args() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("API_KEY".to_string(), "sk-secret-123".to_string());
        omt_harness::harness::upsert_config(tmp.path(), "pi", omt_harness::harness::HarnessUserConfig {
            enabled: true,
            extra_args: vec!["--api-key".into(), "sk-secret-456".into()],
            env,
            ..Default::default()
        }).unwrap();
        let out = harness_list(&ctx(tmp.path())).unwrap().to_string();
        assert!(!out.contains("sk-secret"), "{out}");
        assert!(out.contains("API_KEY"), "键名保留：{out}");
        assert!(!out.contains("\"env\""), "env 字段整体换成 envKeys：{out}");
    }

    /// Q-13：全局 home、或 workspace 根等于用户主目录 → 拒绝；workspace home 放行。
    #[test]
    fn workspace_scope_rejects_global_and_user_home_roots() {
        let user_home = Path::new("/Users/someone");
        assert_eq!(
            workspace_scope_denial("global", Path::new("/Users/someone/.omt"), Some(user_home)).unwrap().field,
            "workspaces"
        );
        assert!(workspace_scope_denial("workspace", Path::new("/Users/someone/.omt"), Some(user_home)).is_some(),
            "kind 报 workspace 但根目录就是 $HOME 也拒绝");
        assert!(workspace_scope_denial("workspace", Path::new("/Users/someone/dev/proj/.omt"), Some(user_home)).is_none());
        // 没有 .omt 尾段的 home（测试与 --home 直开的形态）：根就是它自己
        assert!(workspace_scope_denial("workspace", Path::new("/tmp/ws-home"), Some(user_home)).is_none());
    }

    #[test]
    fn exec_start_validates_target_shape() {
        let tmp = tempfile::tempdir().unwrap();
        for args in [
            serde_json::json!({"homeId": "h_t", "harness": "pi"}),
            serde_json::json!({"homeId": "h_t", "harness": "pi", "nodeId": "a", "runId": "b"}),
            serde_json::json!({"homeId": "h_t", "nodeId": "a"}),
        ] {
            assert_eq!(exec_start(&ctx(tmp.path()), LaunchOrigin::Cli, &args).unwrap_err().code, "INVALID_INPUT", "{args}");
        }
    }

    #[test]
    fn policy_show_reports_effective_policy_and_source() {
        let tmp = tempfile::tempdir().unwrap();
        let shown = policy_show(&ctx(tmp.path())).unwrap();
        assert_eq!(shown["policy"]["allowFromMcp"], false);
        assert_eq!(shown["source"], "default");
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p omt-harness list_value && cargo test -p omt-exec --lib local::tests`
Expected: 编译失败（函数未定义）。

- [ ] **Step 3: 共用的 harness 投影（omt-harness/src/harness.rs）**

把桌面端 `harness_list` / `harness_list_models` 的逻辑下沉。`apps/desktop/src-tauri/src/harness.rs:30` 的私有 helper `fn resolve_login_program(runtime_dir: &Path, provider_id: &str) -> Result<PathBuf, String>`（列模型不要求 `enabled`）原样移入 `crates/omt-harness/src/harness.rs`，改名为 `pub fn resolve_listing_program`，签名不变；它在桌面端的单测 `resolve_login_program_works_without_enabled_config`（同文件 :198）随之移入 omt-harness 的 tests 模块并改名：

```rust
/// 桌面端设置页与 `omt harness list` 共用的投影（A4）。
pub fn list_value(runtime_dir: &Path) -> Result<serde_json::Value, String> {
    let configs = load_configs(runtime_dir);
    let harnesses = probe_all(|d| harness_entry(d, configs.get(d.id)))?;
    Ok(serde_json::json!({ "harnesses": harnesses }))
}

const MODEL_LIST_TIMEOUT_SECS: u64 = 8;

/// `omt harness models <id>` / 桌面端模型下拉共用（不要求 enabled——列模型
/// 是启用前的探查动作，与原桌面端行为一致）。
pub fn models_value(runtime_dir: &Path, harness: &str) -> Result<serde_json::Value, String> {
    let d = descriptor_of(harness).ok_or_else(|| format!("unsupported harness type: {harness}"))?;
    if d.model_list_args.is_empty() {
        return Ok(serde_json::json!({ "models": [] }));
    }
    let program = resolve_listing_program(runtime_dir, harness)?;
    let output = capture_with_timeout(&program, d.model_list_args, MODEL_LIST_TIMEOUT_SECS)
        .ok_or_else(|| format!("{harness} 的模型列表命令 {MODEL_LIST_TIMEOUT_SECS}s 内无输出"))?;
    let models: Vec<serde_json::Value> = parse_models_output(harness, &output)
        .into_iter()
        .map(|m| serde_json::json!({ "id": m.id, "group": m.group }))
        .collect();
    Ok(serde_json::json!({ "models": models }))
}
```

（`capture_with_timeout(binary: &Path, args: &[&str], timeout_secs: u64)` 与 `model_list_args: &'static [&'static str]` 直接匹配。）桌面端 `apps/desktop/src-tauri/src/harness.rs` 的两个 command 改为：

```rust
#[tauri::command(async)]
pub fn harness_list() -> Result<serde_json::Value, String> {
    omt_harness::harness::list_value(&daemon::runtime_dir())
}

#[tauri::command(async)]
pub fn harness_list_models(harness: String) -> Result<serde_json::Value, String> {
    omt_harness::harness::models_value(&daemon::runtime_dir(), &harness)
}
```

桌面端 `harness_open_login` 里对 `resolve_login_program` 的调用改为 `omt_harness::harness::resolve_listing_program`。

- [ ] **Step 4: 分页 tail（executor.rs，紧挨 `transcript_tail_from`）**

```rust
pub const TAIL_PAGE_DEFAULT: usize = 200;
pub const TAIL_PAGE_MAX: usize = 1000;

/// Q-8：按 seq 正向分页读取 `<execId>.jsonl`（不含 session_header 与畸形行）。
/// `since` 不含；`nextSeq` = 本页最后一个 seq（无事件时回显 since）；`done` =
/// 已读到 exit，或该 exec 已不活跃且本页之后再无事件。
pub fn transcript_page(
    runtime_dir: &Path,
    home_id: &str,
    exec_id: &str,
    since: Option<u64>,
    limit: usize,
) -> Result<serde_json::Value, String> {
    crate::layout::validate_segment("exec_id", exec_id)?;
    let (agent, _) = crate::agents::find_exec_in_home(runtime_dir, home_id, exec_id)?
        .ok_or_else(|| format!("exec {exec_id} 不存在"))?;
    let dir = crate::layout::agent_dir(runtime_dir, home_id, &agent.id)?;
    let path = crate::layout::exec_file(&dir, exec_id, crate::layout::ExecFile::Jsonl);
    // 先判活、再读文件：读之前已不活跃 ⇒ 本次读到的内容已包含 shim 写出的全部事件
    // （shim 先写 exit 行再退出）。反过来的顺序会在两步之间漏掉 exit 却报 done。
    let active_before_read = crate::control::exec_active(runtime_dir, home_id, &agent.id, exec_id);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let limit = limit.clamp(1, TAIL_PAGE_MAX);
    let all: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|ev| ev["kind"].as_str().is_some_and(|k| k != "session_header"))
        .filter(|ev| match (since, ev["seq"].as_u64()) {
            (Some(after), Some(seq)) => seq > after,
            _ => true,
        })
        .collect();
    let more_after_page = all.len() > limit;
    let page: Vec<serde_json::Value> = all.into_iter().take(limit).collect();
    let saw_exit = page.iter().any(|ev| ev["kind"] == "exit");
    let next_seq = page.last().and_then(|ev| ev["seq"].as_u64()).or(since);
    let done = saw_exit || (!more_after_page && !active_before_read);
    Ok(serde_json::json!({ "events": page, "nextSeq": next_seq, "done": done }))
}
```

- [ ] **Step 5: `served_homes`（daemon_link.rs）**

```rust
/// Q-3：daemon 当前服务的 home（homeId, path）。daemon 不在线且给了二进制时拉起
/// （与 connect 同语义，不带 --home）。CLI/MCP 据此把 homeId 映射为路径——spec
/// 的 home_path 绝不取自调用方输入（P-12）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedHome {
    pub home_id: String,
    pub path: PathBuf,
    /// handshake 的 `kind`："workspace" | "global"（Q-13 据此拒绝全局 home）
    pub kind: String,
}

pub fn served_homes(
    runtime_dir: &Path,
    daemon_bin: Option<&Path>,
) -> Result<Vec<ServedHome>, String> {
    if live_descriptor(runtime_dir).is_none() {
        let bin = daemon_bin.ok_or("daemon 不在线，且没有可拉起的 omt-daemon")?;
        spawn_daemon(bin, runtime_dir)?;
    }
    let link = DaemonLink::enroll(runtime_dir, "cli")?;
    Ok(link.handshake["homes"]
        .as_array()
        .map(|homes| {
            homes
                .iter()
                .filter_map(|h| {
                    Some(ServedHome {
                        home_id: h["homeId"].as_str()?.to_string(),
                        path: PathBuf::from(h["path"].as_str()?),
                        kind: h["kind"].as_str().unwrap_or("workspace").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}
```

（文件顶部 `use std::path::{Path, PathBuf};` 若缺 `PathBuf` 则补上。）

- [ ] **Step 6: `omt-exec::local`**

新建 `crates/omt-exec/src/local.rs`（测试模块之上）：

```rust
//! 子项目 2 本地工具核心（设计 §3.5 / KD-E4）：harness / exec / policy 的
//! JSON 进、JSON 出。CLI 动词与 MCP 工具（omt-runtime 的 LocalImpl）只做
//! 参数搬运，语义全部在这里——两面行为一致，测试也只需一处。

use std::path::{Path, PathBuf};
use std::time::Duration;

use omt_domain::error::Problem;
use serde_json::{json, Value};

use crate::executor::LaunchTarget;
use crate::policy::LaunchOrigin;

pub struct Ctx {
    pub runtime_dir: PathBuf,
    /// 拉起 shim 的 `omt` 二进制（即当前进程的 current_exe）。
    pub omt_bin: PathBuf,
    /// 同目录的 `omt-daemon`（缺失时只能连接已在线的 daemon）。
    pub daemon_bin: Option<PathBuf>,
}

fn invalid(field: &str, message: impl Into<String>) -> Problem {
    let field = field.to_string();
    Problem::with_details("INVALID_INPUT", message.into(), |d| {
        d.insert("field".into(), field.into());
    })
}

fn not_found(message: impl Into<String>) -> Problem {
    Problem::new("NOT_FOUND", message.into())
}

fn io(message: impl Into<String>) -> Problem {
    Problem::new("IO", message.into())
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// 本地工具路径的 harness 投影：与桌面端设置页同源，但 env 只给键名、extraArgs
/// 脱敏——工具输出会进入主控 agent 的上下文与 transcript（secrets 纪律）。
pub fn harness_list(ctx: &Ctx) -> Result<Value, Problem> {
    let mut value = omt_harness::harness::list_value(&ctx.runtime_dir).map_err(io)?;
    if let Some(entries) = value["harnesses"].as_array_mut() {
        for entry in entries {
            let keys: Vec<Value> = entry["env"]
                .as_object()
                .map(|env| env.keys().map(|k| json!(k)).collect())
                .unwrap_or_default();
            let args: Vec<String> = entry["extraArgs"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            if let Some(obj) = entry.as_object_mut() {
                obj.remove("env");
                obj.insert("envKeys".into(), Value::Array(keys));
                obj.insert("extraArgs".into(), json!(omt_harness::sanitize::sanitize_args(&args)));
            }
        }
    }
    Ok(value)
}

pub fn harness_models(ctx: &Ctx, harness: &str) -> Result<Value, Problem> {
    if omt_harness::harness::descriptor_of(harness).is_none() {
        return Err(invalid("harness", format!("未知的 harness：{harness}")));
    }
    omt_harness::harness::models_value(&ctx.runtime_dir, harness).map_err(io)
}

pub fn policy_show(ctx: &Ctx) -> Result<Value, Problem> {
    let source = if crate::layout::policy_file(&ctx.runtime_dir).is_file() { "file" } else { "default" };
    let policy = crate::policy::load(&ctx.runtime_dir)?;
    Ok(json!({
        "policy": policy,
        "source": source,
        "path": crate::layout::policy_file(&ctx.runtime_dir).display().to_string(),
    }))
}

/// Q-3：解析目标 home → (homeId, .omt 路径, kind)，一律以 daemon 的映射为准。
/// MCP 只接受已服务的 `homeId`（`home` 键被忽略，MCP 不能借它声明 home，R7）；
/// CLI 另认 `home`（路径或 homeId），给出已初始化但未被服务的路径时经
/// DaemonLink::connect 声明。
fn resolve_home(ctx: &Ctx, origin: LaunchOrigin, args: &Value) -> Result<(String, PathBuf, String), Problem> {
    let cli_home = match origin {
        LaunchOrigin::Cli => opt_str(args, "home"),
        _ => None,
    };
    let wanted = opt_str(args, "homeId")
        .or(cli_home)
        .ok_or_else(|| invalid("homeId", "需要 homeId（CLI：全局 --home）"))?;
    let served = crate::daemon_link::served_homes(&ctx.runtime_dir, ctx.daemon_bin.as_deref())
        .map_err(io)?;
    let canonical = std::fs::canonicalize(wanted).ok();
    if let Some(home) = served.iter().find(|h| {
        h.home_id == wanted
            || canonical.as_ref().is_some_and(|c| std::fs::canonicalize(&h.path).ok().as_ref() == Some(c))
    }) {
        return Ok((home.home_id.clone(), home.path.clone(), home.kind.clone()));
    }
    if cli_home.is_some() {
        if let Some(path) = canonical.filter(|p| p.join("omt.db").is_file()) {
            let link = crate::daemon_link::DaemonLink::connect(
                &ctx.runtime_dir, "cli", ctx.daemon_bin.as_deref(), &path, None,
            )
            .map_err(io)?;
            if let Some(id) = link.home_id_for(&path) {
                let kind = crate::daemon_link::served_homes(&ctx.runtime_dir, ctx.daemon_bin.as_deref())
                    .map_err(io)?
                    .into_iter()
                    .find(|h| h.home_id == id)
                    .map(|h| h.kind)
                    .unwrap_or_else(|| "workspace".into());
                return Ok((id, path, kind));
            }
        }
    }
    Err(not_found(format!("home {wanted} 不在 daemon 当前服务的 home 中")))
}

/// Q-13：`workspaces: homeWorkspaceOnly` 的纯判定。全局 home，或 workspace 根
/// （与执行器同一规则 `executor::workspace_root_of`：尾段是 `.omt` 才剥掉）等于
/// 用户主目录，都会让无人值守 harness 以 $HOME 为 cwd 运行。
pub fn workspace_scope_denial(
    kind: &str,
    home_path: &Path,
    user_home: Option<&Path>,
) -> Option<crate::policy::PolicyDenial> {
    let root = crate::executor::workspace_root_of(home_path);
    let at_user_home = user_home.is_some_and(|h| {
        std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone())
            == std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf())
    });
    (kind == "global" || at_user_home).then(|| crate::policy::PolicyDenial {
        field: "workspaces",
        reason: format!(
            "policy 只允许在 workspace 内的 home 上执行（{} 的工作目录会是用户主目录）",
            home_path.display()
        ),
    })
}

pub fn exec_start(ctx: &Ctx, origin: LaunchOrigin, args: &Value) -> Result<Value, Problem> {
    let harness = opt_str(args, "harness").ok_or_else(|| invalid("harness", "需要 harness"))?;
    let target = match (opt_str(args, "nodeId"), opt_str(args, "runId")) {
        (Some(node), None) => LaunchTarget::Node(node),
        (None, Some(run)) => LaunchTarget::Run(run),
        _ => return Err(invalid("nodeId", "nodeId 与 runId 必须且只能给一个")),
    };
    // 先判 policy（不碰 daemon）：被拒的请求不留任何痕迹
    let policy = crate::policy::load(&ctx.runtime_dir)?;
    crate::policy::check_launch(&policy, origin, harness).map_err(|d| d.into_problem())?;
    let requested = args
        .get("maxDurationSec")
        .and_then(Value::as_u64)
        .map(Duration::from_secs);
    let (home_id, home_path, home_kind) = resolve_home(ctx, origin, args)?;
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(denial) = workspace_scope_denial(&home_kind, &home_path, user_home.as_deref()) {
        return Err(denial.into_problem());
    }
    let rpc = crate::daemon_link::plain_connector(ctx.runtime_dir.clone(), ctx.daemon_bin.clone())(
        &home_path, &home_id,
    )
    .map_err(io)?;
    let request = crate::launch::LaunchRequest {
        runtime_dir: &ctx.runtime_dir,
        omt_bin: &ctx.omt_bin,
        home_id: &home_id,
        home_path: &home_path,
        target,
        origin,
        harness,
        // CLI / MCP 派发总是无人值守：附加 descriptor 的无人值守参数（pi `--auto`、
        // omp `--approval-mode=yolo`），等效桌面端派发但没有派发弹窗那一步人工确认。
        // approvalMode=inherit 只表示派发方不能改它（harness 配置里没有该开关）。
        unattended: true,
        model: opt_str(args, "model"),
        resume_session: None,
        target_agent_id: opt_str(args, "agentId"),
        parent_agent_id: opt_str(args, "parentAgentId"),
        max_duration: crate::policy::effective_max_duration(&policy, requested),
    };
    let resume_session = match request.target_agent_id {
        Some(agent_id) => crate::agents::get_agent(&ctx.runtime_dir, &home_id, agent_id)
            .map_err(io)?
            .ok_or_else(|| not_found(format!("agent {agent_id} 不存在")))?
            .last_session_id,
        None => None,
    };
    let request = crate::launch::LaunchRequest { resume_session: resume_session.as_deref(), ..request };
    let outcome = crate::launch::start(&|m, p| rpc(m, p), &request).map_err(|err| {
        let (code, message, field) = crate::launch::problem_from_launch_error(&err);
        Problem::with_details(code, message, |d| {
            if let Some(field) = field {
                d.insert("field".into(), field.into());
            }
        })
    })?;
    serde_json::to_value(outcome).map_err(|e| io(e.to_string()))
}

/// Q-4：停止不受 allowFrom* 与 policy 文件有效性约束——拒绝启动是安全方向，拒绝
/// 停止反而危险。被派发的 agent 不能停止，由 LocalImpl 的嵌套标记检查拦截（Q-12）。
pub fn exec_stop(ctx: &Ctx, exec_id: &str) -> Result<Value, Problem> {
    crate::layout::validate_segment("exec_id", exec_id).map_err(|e| invalid("execId", e))?;
    if crate::control::find_exec(&ctx.runtime_dir, exec_id).map_err(io)?.is_none() {
        return Err(not_found(format!("exec {exec_id} 不存在")));
    }
    crate::control::stop_exec(&ctx.runtime_dir, exec_id).map_err(io)?;
    Ok(json!({ "execId": exec_id, "stopped": true }))
}

pub fn exec_status(ctx: &Ctx, exec_id: &str) -> Result<Value, Problem> {
    crate::layout::validate_segment("exec_id", exec_id).map_err(|e| invalid("execId", e))?;
    let loc = crate::control::find_exec(&ctx.runtime_dir, exec_id)
        .map_err(io)?
        .ok_or_else(|| not_found(format!("exec {exec_id} 不存在")))?;
    let mut exec = serde_json::to_value(&loc.exec).map_err(|e| io(e.to_string()))?;
    exec["binaryArgs"] = json!(omt_harness::sanitize::sanitize_args(&loc.exec.binary_args));
    let active = crate::control::exec_active(&ctx.runtime_dir, &loc.home_id, &loc.agent.id, exec_id);
    Ok(json!({
        "homeId": loc.home_id,
        "agent": {
            "id": loc.agent.id,
            "displayName": loc.agent.display_name,
            "harness": loc.agent.harness,
            "status": loc.agent.status,
            "attachment": loc.agent.attachment,
            "parentAgentId": loc.agent.parent_agent_id,
        },
        "exec": exec,
        "active": active,
    }))
}

pub fn exec_list(ctx: &Ctx, home_id: Option<&str>) -> Result<Value, Problem> {
    let homes: Vec<String> = match home_id {
        Some(id) => vec![id.to_string()],
        None => crate::layout::home_ids(&ctx.runtime_dir),
    };
    let mut rows = Vec::new();
    for home in homes {
        let (agents, _) = crate::agents::list_agents(&ctx.runtime_dir, &home).map_err(io)?;
        for agent in agents {
            for exec in &agent.execs {
                rows.push(json!({
                    "homeId": home,
                    "agentId": agent.id,
                    "displayName": agent.display_name,
                    "harness": agent.harness,
                    "status": agent.status,
                    "execId": exec.exec_id,
                    "nodeId": exec.node_id,
                    "runId": exec.run_id,
                    "launchedBy": exec.launched_by,
                    "startedAt": exec.started_at,
                    "active": crate::control::exec_active(&ctx.runtime_dir, &home, &agent.id, &exec.exec_id),
                }));
            }
        }
    }
    rows.sort_by(|a, b| b["startedAt"].as_str().cmp(&a["startedAt"].as_str()));
    Ok(json!({ "execs": rows }))
}

pub fn exec_tail(ctx: &Ctx, exec_id: &str, since: Option<u64>, limit: Option<usize>) -> Result<Value, Problem> {
    crate::layout::validate_segment("exec_id", exec_id).map_err(|e| invalid("execId", e))?;
    let loc = crate::control::find_exec(&ctx.runtime_dir, exec_id)
        .map_err(io)?
        .ok_or_else(|| not_found(format!("exec {exec_id} 不存在")))?;
    crate::executor::transcript_page(
        &ctx.runtime_dir,
        &loc.home_id,
        exec_id,
        since,
        limit.unwrap_or(crate::executor::TAIL_PAGE_DEFAULT),
    )
    .map_err(io)
}
```

（`omt_harness::sanitize::sanitize_args(&[String]) -> Vec<String>` 已是公开函数。）`AgentRecordStatus` / `AgentAttachment` 已 derive `Serialize`（camelCase 小写），`json!` 可直接嵌入。

`crates/omt-exec/src/lib.rs` 加 `pub mod local;`。

- [ ] **Step 7: 运行，确认通过**

Run: `cargo test -p omt-harness && cargo test -p omt-exec --lib local::tests && cargo test -p oh-my-ticket-desktop && (cd apps/desktop && pnpm test -- harness-manager)`
Expected: 全部通过（桌面端 harness 测试证明投影形状未变）。

- [ ] **Step 8: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/omt-harness/src/harness.rs crates/omt-exec/src apps/desktop/src-tauri/src/harness.rs
git commit -m "feat(exec): local tool core — harness/exec/policy JSON surface and seq-paged tail (A4)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A5: `LocalTools` trait、CLI 动词与二进制侧注入

**Files:**
- Create: `crates/omt-runtime/src/local_tools.rs`（lib；不得引用 omt_exec / omt_harness）
- Create: `crates/omt-runtime/src/local_impl.rs`（仅 `omt` 二进制，`cli_main.rs` 以 `mod local_impl;` 引入）
- Modify: `crates/omt-runtime/src/lib.rs`（`pub mod local_tools;`；**不**声明 `local_impl`）
- Modify: `crates/omt-runtime/src/cli/mod.rs`（`real_run_code_with`、`dispatch_args` 接 `Option<&dyn LocalTools>`、`harness` / `exec` 动词、USAGE）
- Modify: `crates/omt-runtime/src/cli_main.rs`
- Modify: `crates/omt-runtime/Cargo.toml`（`omt-harness` 从 dev-dependency 提为 dependency——仅 `local_impl.rs` 使用）
- Modify: `.github/workflows/ci.yml`（nm 门加 `omt_harness`）
- Modify: `crates/omt-exec/src/executor.rs`（Q-12：spawn 注入 `OMT_EXEC_ID`，保留键检查）
- Modify: `crates/omt-harness/src/harness.rs`（`RESERVED_ENV_KEYS` 加 `OMT_EXEC_ID`）
- Create: `crates/omt-runtime/tests/local_tools_cli.rs`

**Interfaces:**
- Consumes: `omt_exec::local::*`（A4）
- Produces:
  - `pub enum LocalCaller { Cli, Mcp }`（lib；`LocalImpl` 映射为 `LaunchOrigin`）
  - `pub trait LocalTools: Send + Sync { fn call(&self, caller: LocalCaller, tool: &str, args: Value) -> Result<Value, Problem>; }`
  - `pub struct LocalToolSpec { pub action: &'static str, pub mutating: bool, pub description: &'static str, pub input_schema: fn() -> Value }`
  - `pub const LOCAL_TOOLS: &[LocalToolSpec]`（action 形如 `"exec/start"`；MCP wire 名 `exec__start`）
  - `cli::real_run_code_with(local: Option<&dyn LocalTools>) -> i32`（`real_run_code()` 保留并等价于 `real_run_code_with(None)`）
  - `pub fn local_tool(action: &str) -> Option<&'static LocalToolSpec>`

- [ ] **Step 1: 写失败测试**

`crates/omt-runtime/src/local_tools.rs` 底部：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tool_table_is_the_designed_family() {
        let names: Vec<&str> = LOCAL_TOOLS.iter().map(|t| t.action).collect();
        assert_eq!(names, vec![
            "harness/list", "harness/models",
            "exec/start", "exec/stop", "exec/status", "exec/list", "exec/tail", "exec/policy",
        ]);
        let mutating: Vec<&str> = LOCAL_TOOLS.iter().filter(|t| t.mutating).map(|t| t.action).collect();
        assert_eq!(mutating, vec!["exec/start", "exec/stop"]);
    }

    #[test]
    fn every_local_tool_has_an_object_schema_with_properties() {
        for tool in LOCAL_TOOLS {
            let schema = (tool.input_schema)();
            assert_eq!(schema["type"], "object", "{}", tool.action);
            assert!(schema["properties"].as_object().is_some_and(|p| !p.is_empty()), "{}", tool.action);
            assert!(!schema.to_string().contains("$ref"), "{}", tool.action);
        }
    }

    #[test]
    fn cli_args_parse_into_tool_calls() {
        let call = |argv: &[&str]| parse_cli(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(call(&["harness", "list"]).unwrap(), ("harness/list", serde_json::json!({})));
        assert_eq!(call(&["harness", "models", "pi"]).unwrap(), ("harness/models", serde_json::json!({"harness": "pi"})));
        assert_eq!(
            call(&["exec", "start", "--harness", "pi", "--node", "TICKET-1", "--model", "k3"]).unwrap(),
            ("exec/start", serde_json::json!({"harness": "pi", "nodeId": "TICKET-1", "model": "k3"})),
        );
        assert_eq!(
            call(&["exec", "start", "--harness", "pi", "--run", "RUN-1", "--agent", "agent-x", "--parent", "agent-m", "--max-duration", "600"]).unwrap(),
            ("exec/start", serde_json::json!({"harness": "pi", "runId": "RUN-1", "agentId": "agent-x", "parentAgentId": "agent-m", "maxDurationSec": 600})),
        );
        assert_eq!(call(&["exec", "tail", "exec-1", "--since", "5", "--limit", "50"]).unwrap(),
            ("exec/tail", serde_json::json!({"execId": "exec-1", "since": 5, "limit": 50})));
        assert_eq!(call(&["exec", "list", "--all-homes"]).unwrap(), ("exec/list", serde_json::json!({"allHomes": true})));
        assert_eq!(call(&["exec", "policy"]).unwrap(), ("exec/policy", serde_json::json!({})));
        assert!(call(&["exec", "start", "--harness", "pi", "--node", "a", "--run", "b"]).is_err());
        assert!(call(&["exec", "stop"]).is_err());
        assert!(call(&["exec", "bogus"]).is_err());
    }
}
```

`crates/omt-runtime/tests/local_tools_cli.rs`（CLI e2e；复用 `cli_e2e.rs` 的 `omt_path` / `run_cli` 写法——在本文件复制这两个 5 行 helper，不改 common）：

```rust
//! 子项目 2 A5：CLI 本地动词 e2e（不经 daemon 的只读动词 + policy 拒绝路径）。
mod common;
use common::TestCtx;

fn omt_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_omt"))
}

fn run_cli(ctx: &TestCtx, args: &[&str]) -> std::process::Output {
    std::process::Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(&ctx.runtime_dir)
        .args(args)
        .env("OMT_RUNTIME_DIR", &ctx.runtime_dir)
        .output()
        .expect("run omt")
}

#[test]
fn harness_list_and_policy_work_without_a_daemon() {
    let ctx = TestCtx::spawn_named("local-cli-ro");
    let out = run_cli(&ctx, &["harness", "list", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["harnesses"].as_array().unwrap().iter().any(|h| h["id"] == "pi"));

    let out = run_cli(&ctx, &["exec", "policy", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["policy"]["allowFromMcp"], false);
    assert_eq!(value["source"], "default");
}

#[test]
fn exec_start_denied_by_policy_exits_3_with_structured_problem() {
    let ctx = TestCtx::spawn_named("local-cli-deny");
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(ctx.runtime_dir.join("exec/policy.json"), r#"{"allowFromCli": false}"#).unwrap();
    let out = run_cli(&ctx, &["exec", "start", "--harness", "pi", "--node", "TICKET-1"]);
    assert_eq!(out.status.code(), Some(3));
    let problem: serde_json::Value = serde_json::from_slice(out.stderr.trim_ascii_end()).unwrap();
    assert_eq!(problem["code"], "POLICY_DENIED");
    assert_eq!(problem["details"]["field"], "allowFromCli");
}

#[test]
fn exec_list_on_empty_runtime_is_empty_and_status_unknown_is_not_found() {
    let ctx = TestCtx::spawn_named("local-cli-list");
    let out = run_cli(&ctx, &["exec", "list", "--all-homes", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["execs"], serde_json::json!([]));
    let out = run_cli(&ctx, &["exec", "status", "exec-nope"]);
    assert_eq!(out.status.code(), Some(3));
}

/// Q-12：带 OMT_EXEC_ID 的进程（被派发的 harness）不能派发、不能停止。
#[test]
fn nested_dispatch_marker_denies_start_and_stop() {
    let ctx = TestCtx::spawn_named("local-cli-nested");
    for args in [
        vec!["exec", "start", "--harness", "pi", "--node", "TICKET-1"],
        vec!["exec", "stop", "exec-anything"],
    ] {
        let out = std::process::Command::new(omt_path())
            .arg("--runtime-dir")
            .arg(&ctx.runtime_dir)
            .args(&args)
            .env("OMT_RUNTIME_DIR", &ctx.runtime_dir)
            .env("OMT_EXEC_ID", "exec-parent")
            .output()
            .expect("run omt");
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        let problem: serde_json::Value = serde_json::from_slice(out.stderr.trim_ascii_end()).unwrap();
        assert_eq!(problem["code"], "POLICY_DENIED", "{args:?}");
        assert_eq!(problem["details"]["field"], "nestedDispatch", "{args:?}");
    }
    // 只读动词不受影响
    let out = std::process::Command::new(omt_path())
        .arg("--runtime-dir").arg(&ctx.runtime_dir)
        .args(["exec", "policy"])
        .env("OMT_EXEC_ID", "exec-parent")
        .output().unwrap();
    assert!(out.status.success());
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p omt-runtime --lib local_tools && cargo test -p omt-runtime --test local_tools_cli`
Expected: 编译失败 / 动词未知（exit 2）。

- [ ] **Step 3: lib 侧 `local_tools.rs`**

```rust
//! 子项目 2（设计 §3.5 / KD-E4）本地工具族的 lib 侧契约：工具表、MCP 输入
//! schema、CLI 参数解析与 `LocalTools` trait。**本模块被 omt-daemon 链接**——
//! 不得引用 omt_exec / omt_harness（KD-E9）；实现在 `local_impl.rs`（仅 omt
//! 二进制），经 `cli::real_run_code_with` 注入。

use omt_storage::Problem;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCaller {
    Cli,
    Mcp,
}

pub trait LocalTools: Send + Sync {
    fn call(&self, caller: LocalCaller, tool: &str, args: Value) -> Result<Value, Problem>;
}

pub struct LocalToolSpec {
    pub action: &'static str,
    /// 有副作用（parity 分类 local_exec；policy 在调用时检查，Q-5）
    pub mutating: bool,
    pub description: &'static str,
    pub input_schema: fn() -> Value,
}

fn schema_harness_list() -> Value {
    json!({"type": "object", "properties": {
        "verbose": {"type": "boolean", "description": "保留字段；当前总是返回完整投影"}
    }})
}
fn schema_harness_models() -> Value {
    json!({"type": "object", "required": ["harness"], "properties": {
        "harness": {"type": "string", "description": "harness id，如 pi / omp / opencode"}
    }})
}
fn schema_exec_start() -> Value {
    json!({"type": "object", "required": ["homeId", "harness"], "properties": {
        "homeId": {"type": "string"},
        "harness": {"type": "string", "description": "须在 exec/policy.json 的 harnesses 白名单内"},
        "nodeId": {"type": "string", "description": "与 runId 二选一：派发该节点（及其可执行子成员）"},
        "runId": {"type": "string", "description": "与 nodeId 二选一：派发到已有 run"},
        "model": {"type": "string", "description": "harness__models 返回的模型 id"},
        "agentId": {"type": "string", "description": "续跑：在该 agent 下追加 exec（沿用其会话）"},
        "parentAgentId": {"type": "string", "description": "主控自己的 agentId（关联父子）"},
        "maxDurationSec": {"type": "integer", "minimum": 60, "description": "不超过 policy.maxDurationSec"}
    }})
}
fn schema_exec_id() -> Value {
    json!({"type": "object", "required": ["execId"], "properties": {
        "execId": {"type": "string"}
    }})
}
fn schema_exec_list() -> Value {
    json!({"type": "object", "properties": {
        "homeId": {"type": "string", "description": "缺省列出全部 home"}
    }})
}
fn schema_exec_tail() -> Value {
    json!({"type": "object", "required": ["execId"], "properties": {
        "execId": {"type": "string"},
        "since": {"type": "integer", "minimum": 0, "description": "只返回 seq 大于它的事件（上一页的 nextSeq）"},
        "limit": {"type": "integer", "minimum": 1, "maximum": 1000}
    }})
}
fn schema_exec_policy() -> Value {
    json!({"type": "object", "properties": {
        "verbose": {"type": "boolean", "description": "保留字段"}
    }})
}

pub const LOCAL_TOOLS: &[LocalToolSpec] = &[
    LocalToolSpec { action: "harness/list", mutating: false, description: "列出 harness：是否安装、版本、是否启用、无人值守标志", input_schema: schema_harness_list },
    LocalToolSpec { action: "harness/models", mutating: false, description: "列出某个 harness 可用的模型", input_schema: schema_harness_models },
    LocalToolSpec { action: "exec/start", mutating: true, description: "拉起一个 harness 执行 ticket/run，立即返回 {agentId, execId, runId}", input_schema: schema_exec_start },
    LocalToolSpec { action: "exec/stop", mutating: true, description: "停止一个执行（SIGTERM → 宽限后 KILL）", input_schema: schema_exec_id },
    LocalToolSpec { action: "exec/status", mutating: false, description: "查询一个执行的状态", input_schema: schema_exec_id },
    LocalToolSpec { action: "exec/list", mutating: false, description: "列出执行（按开始时间倒序）", input_schema: schema_exec_list },
    LocalToolSpec { action: "exec/tail", mutating: false, description: "按 seq 分页读取执行的归一化事件", input_schema: schema_exec_tail },
    LocalToolSpec { action: "exec/policy", mutating: false, description: "查看当前生效的派发策略（exec/policy.json）", input_schema: schema_exec_policy },
];

pub fn local_tool(action: &str) -> Option<&'static LocalToolSpec> {
    LOCAL_TOOLS.iter().find(|t| t.action == action)
}

fn take_flag(rest: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(pos) = rest.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if pos + 1 >= rest.len() {
        return Err(format!("{flag} 需要一个值"));
    }
    let value = rest.remove(pos + 1);
    rest.remove(pos);
    Ok(Some(value))
}

fn take_switch(rest: &mut Vec<String>, flag: &str) -> bool {
    match rest.iter().position(|a| a == flag) {
        Some(pos) => {
            rest.remove(pos);
            true
        }
        None => false,
    }
}

fn int_flag(rest: &mut Vec<String>, flag: &str) -> Result<Option<u64>, String> {
    take_flag(rest, flag)?
        .map(|v| v.parse::<u64>().map_err(|_| format!("{flag} 需要非负整数")))
        .transpose()
}

/// `harness …` / `exec …`（已去掉全局参数）→ (action, args)。
pub fn parse_cli(argv: &[String]) -> Result<(&'static str, Value), String> {
    let mut rest: Vec<String> = argv.to_vec();
    if rest.len() < 2 {
        return Err("用法：omt harness list|models <id>；omt exec start|stop|status|list|tail|policy".into());
    }
    let group = rest.remove(0);
    let sub = rest.remove(0);
    let positional = |rest: &mut Vec<String>, what: &str| -> Result<String, String> {
        if rest.is_empty() || rest[0].starts_with("--") {
            return Err(format!("缺少 {what}"));
        }
        Ok(rest.remove(0))
    };
    let parsed = match (group.as_str(), sub.as_str()) {
        ("harness", "list") => ("harness/list", json!({})),
        ("harness", "models") => ("harness/models", json!({"harness": positional(&mut rest, "<harness>")?})),
        ("exec", "start") => {
            let mut args = serde_json::Map::new();
            let harness = take_flag(&mut rest, "--harness")?.ok_or("缺少 --harness")?;
            args.insert("harness".into(), json!(harness));
            let node = take_flag(&mut rest, "--node")?;
            let run = take_flag(&mut rest, "--run")?;
            match (node, run) {
                (Some(n), None) => { args.insert("nodeId".into(), json!(n)); }
                (None, Some(r)) => { args.insert("runId".into(), json!(r)); }
                _ => return Err("--node 与 --run 必须且只能给一个".into()),
            }
            for (flag, key) in [("--model", "model"), ("--agent", "agentId"), ("--parent", "parentAgentId")] {
                if let Some(v) = take_flag(&mut rest, flag)? {
                    args.insert(key.into(), json!(v));
                }
            }
            if let Some(v) = int_flag(&mut rest, "--max-duration")? {
                args.insert("maxDurationSec".into(), json!(v));
            }
            ("exec/start", Value::Object(args))
        }
        ("exec", "stop") => ("exec/stop", json!({"execId": positional(&mut rest, "<execId>")?})),
        ("exec", "status") => ("exec/status", json!({"execId": positional(&mut rest, "<execId>")?})),
        ("exec", "list") => {
            if take_switch(&mut rest, "--all-homes") {
                ("exec/list", json!({"allHomes": true}))
            } else {
                ("exec/list", json!({}))
            }
        }
        ("exec", "tail") => {
            let exec_id = positional(&mut rest, "<execId>")?;
            let mut args = serde_json::Map::new();
            args.insert("execId".into(), json!(exec_id));
            if let Some(v) = int_flag(&mut rest, "--since")? {
                args.insert("since".into(), json!(v));
            }
            if let Some(v) = int_flag(&mut rest, "--limit")? {
                args.insert("limit".into(), json!(v));
            }
            ("exec/tail", Value::Object(args))
        }
        ("exec", "policy") => ("exec/policy", json!({})),
        _ => return Err(format!("未知的子命令：{group} {sub}")),
    };
    if let Some(extra) = rest.first() {
        return Err(format!("多余的参数：{extra}"));
    }
    Ok(parsed)
}
```

`crates/omt-runtime/src/lib.rs` 加 `pub mod local_tools;`。

- [ ] **Step 4: CLI 接线（cli/mod.rs）**

1. `real_run()` 改名为 `real_run_with(local: Option<&dyn crate::local_tools::LocalTools>) -> i32`，内部 `dispatch_args(args, local)`；新增：

```rust
/// 子项目 2：`omt` 二进制注入本地工具实现（omt-daemon 链接的 lib 不构造它，KD-E9）。
pub fn real_run_code_with(local: Option<&dyn crate::local_tools::LocalTools>) -> i32 {
    real_run_with(local)
}
```

`real_run_code()` 改为 `real_run_with(None)`；`run()` 同理。

2. `fn dispatch_args(all: Vec<String>, local: Option<&dyn crate::local_tools::LocalTools>) -> CliResult<()>`，在 `match verb.as_str()` 里加：

```rust
        "harness" | "exec" => {
            let local = local.ok_or_else(|| usage_err("本地工具族只在 omt 二进制中可用"))?;
            let mut argv = vec![verb.clone()];
            argv.extend(rest.drain(..));
            let (action, mut args) = crate::local_tools::parse_cli(&argv).map_err(usage_err)?;
            // 全局 --home（路径或 homeId）承载 home 选择（Q-3）
            if matches!(action, "exec/start") {
                if let Some(home) = split.globals.homes.first() {
                    args["home"] = serde_json::json!(home);
                }
            }
            if action == "exec/list" && args.get("allHomes").is_none() {
                if let Some(home) = split.globals.homes.first() {
                    args["home"] = serde_json::json!(home);
                }
            }
            let result = local
                .call(crate::local_tools::LocalCaller::Cli, action, args)
                .map_err(CliError::Problem)?;
            print_result(result, split.globals.json)
        }
```

3. `"mcp" => crate::mcp::serve(&split.globals, local)…`（A6 改签名；本 Task 先传 `local` 并在 mcp.rs 把 `serve` 的第二参数加上、暂不使用，`let _ = local;`）。

4. `USAGE` 在 `daemon-status` 行之后加：

```text
LOCAL TOOLS (no daemon round-trip for reads; launches obey <runtime-dir>/exec/policy.json):
  harness list
  harness models <harness>
  exec start  --harness H (--node ID | --run ID) [--model M] [--agent AGENT_ID]
              [--parent AGENT_ID] [--max-duration SECS]   (home: global --home PATH|ID)
  exec stop   <execId>
  exec status <execId>
  exec list   [--all-homes]
  exec tail   <execId> [--since SEQ] [--limit N]
  exec policy
```

- [ ] **Step 5: 二进制侧实现（local_impl.rs + cli_main.rs）**

`crates/omt-runtime/src/local_impl.rs`：

```rust
//! `LocalTools` 的执行面实现——只由 `omt` 二进制（cli_main.rs）编译与构造，
//! omt-daemon 不链接（KD-E9；CI nm 门同时检查 omt_exec 与 omt_harness）。

use omt_exec::local::Ctx;
use omt_exec::policy::LaunchOrigin;
use omt_runtime::local_tools::{LocalCaller, LocalTools};
use omt_storage::Problem;
use serde_json::Value;

pub struct LocalImpl {
    ctx: Ctx,
}

impl LocalImpl {
    pub fn new(runtime_dir: std::path::PathBuf) -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("omt"));
        let daemon_bin = exe
            .parent()
            .map(|dir| dir.join("omt-daemon"))
            .filter(|path| path.is_file());
        Self { ctx: Ctx { runtime_dir, omt_bin: exe, daemon_bin } }
    }
}

fn origin(caller: LocalCaller) -> LaunchOrigin {
    match caller {
        LocalCaller::Cli => LaunchOrigin::Cli,
        LocalCaller::Mcp => LaunchOrigin::Mcp,
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, Problem> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| {
        let field = key.to_string();
        Problem::with_details("INVALID_INPUT", format!("需要 {key}"), |d| {
            d.insert("field".into(), field.into());
        })
    })
}

/// Q-12：本进程是被派发的 harness（或其子进程）？shim 在 spawn 时注入
/// `OMT_EXEC_ID`。纵深防御：手握 shell 的 agent 仍可清掉它（见威胁模型）。
pub fn nested_exec_id() -> Option<String> {
    std::env::var("OMT_EXEC_ID").ok().filter(|v| !v.is_empty())
}

impl LocalTools for LocalImpl {
    fn call(&self, caller: LocalCaller, tool: &str, args: Value) -> Result<Value, Problem> {
        let ctx = &self.ctx;
        if matches!(tool, "exec/start" | "exec/stop") {
            if let Some(parent) = nested_exec_id() {
                return Err(Problem::with_details(
                    "POLICY_DENIED",
                    format!("被派发的 agent（exec {parent}）不能再派发或停止执行"),
                    |d| {
                        d.insert("field".into(), "nestedDispatch".into());
                    },
                ));
            }
        }
        match tool {
            "harness/list" => omt_exec::local::harness_list(ctx),
            "harness/models" => omt_exec::local::harness_models(ctx, str_arg(&args, "harness")?),
            "exec/start" => omt_exec::local::exec_start(ctx, origin(caller), &args),
            "exec/stop" => omt_exec::local::exec_stop(ctx, str_arg(&args, "execId")?),
            "exec/status" => omt_exec::local::exec_status(ctx, str_arg(&args, "execId")?),
            "exec/list" => {
                // CLI 的 home 可能是路径：映射到 homeId；allHomes / 缺省 = 全部
                let home = args.get("homeId").or_else(|| args.get("home")).and_then(Value::as_str);
                let home_id = match home {
                    Some(h) if !args.get("allHomes").and_then(Value::as_bool).unwrap_or(false) => {
                        Some(resolve_home_id(ctx, h)?)
                    }
                    _ => None,
                };
                omt_exec::local::exec_list(ctx, home_id.as_deref())
            }
            "exec/tail" => omt_exec::local::exec_tail(
                ctx,
                str_arg(&args, "execId")?,
                args.get("since").and_then(Value::as_u64),
                args.get("limit").and_then(Value::as_u64).map(|n| n as usize),
            ),
            "exec/policy" => omt_exec::local::policy_show(ctx),
            other => Err(Problem::new("NOT_FOUND", format!("unknown local tool: {other}"))),
        }
    }
}

/// exec list 只读：homeId 原样；路径则按目录名之外的 agents/<homeId> 布局无法反推，
/// 所以经 daemon 映射（与 exec start 同一来源）。
fn resolve_home_id(ctx: &Ctx, home: &str) -> Result<String, Problem> {
    if !home.contains('/') {
        return Ok(home.to_string());
    }
    let served = omt_exec::daemon_link::served_homes(&ctx.runtime_dir, ctx.daemon_bin.as_deref())
        .map_err(|e| Problem::new("IO", e))?;
    let want = std::fs::canonicalize(home).ok();
    served
        .into_iter()
        .find(|h| std::fs::canonicalize(&h.path).ok() == want)
        .map(|h| h.home_id)
        .ok_or_else(|| Problem::new("NOT_FOUND", format!("home {home} 不在 daemon 当前服务的 home 中")))
}
```

`crates/omt-runtime/src/cli_main.rs` 顶部加 `mod local_impl;`，`main()` 末行改为：

```rust
    let runtime_dir = omt_runtime::paths::resolve(omt_runtime::cli::runtime_dir_arg(&args).as_deref());
    let local = local_impl::LocalImpl::new(runtime_dir);
    std::process::exit(omt_runtime::cli::real_run_code_with(Some(&local)));
```

在 `cli/mod.rs` 加一个只做参数提取的 pub 函数（复用 `split_globals`，失败时返回 None 让后续正常报 usage）：

```rust
/// cli_main.rs 构造 LocalImpl 需要 runtime dir：只取全局 `--runtime-dir`。
pub fn runtime_dir_arg(args: &[String]) -> Option<String> {
    split_globals(args).ok().and_then(|s| s.globals.runtime_dir)
}
```

（`paths::resolve` 若非 `pub`，以 `grep -n "pub fn resolve" crates/omt-runtime/src/paths.rs` 确认；它被 CLAUDE.md 列为权威实现，应已公开。`omt_storage` 是 omt-runtime 的依赖，二进制可直接 `use`。）

`crates/omt-runtime/Cargo.toml`：把 `omt-harness` 从 `[dev-dependencies]` 移到 `[dependencies]`，注释写明「仅 src/local_impl.rs（omt 二进制）使用；lib 不得引用（KD-E9）」。若 `local_impl.rs` 只经 `omt_exec` 调用而未直接 `use omt_harness`，则保持原状不动。

- [ ] **Step 5b: 嵌套派发标记的注入端（Q-12）**

`crates/omt-exec/src/executor.rs` 的 `spawn_inner`：保留键检查加上 `OMT_EXEC_ID`，并在 `cmd.env("OMT_RUNTIME_DIR", runtime_dir)` 之后注入：

```rust
        if req
            .env_additions
            .keys()
            .any(|k| matches!(k.as_str(), "OMT_RUNTIME_DIR" | "OMT_HOME" | "PATH" | "OMT_EXEC_ID"))
        {
            return Err(
                "env_additions 含保留键（PATH/OMT_RUNTIME_DIR/OMT_HOME/OMT_EXEC_ID 由执行面注入）".into(),
            );
        }
```

```rust
            .env("OMT_RUNTIME_DIR", runtime_dir)
            // Q-12：被派发的 harness 及其子进程（含注入的 omt mcp 桥）凭它识别自己
            .env("OMT_EXEC_ID", &req.exec_id);
```

`crates/omt-harness/src/harness.rs`：`const RESERVED_ENV_KEYS: &[&str] = &["OMT_RUNTIME_DIR", "OMT_HOME", "PATH", "OMT_EXEC_ID"];`，并把 `env_validation_rejects_reserved_and_illegal_keys` 的循环扩到 `OMT_EXEC_ID`。在 executor.rs 现有的「合法 env 注入生效 / 保留键被拒」测试（`grep -n "MY_FLAG" crates/omt-exec/src/executor.rs`）旁加一个用例：假 harness 脚本 `echo "EXEC=$OMT_EXEC_ID"`，断言 raw 输出含 `EXEC=<本次 exec_id>`；`env_additions` 含 `OMT_EXEC_ID` 时 spawn 返回含「保留键」的错误。

- [ ] **Step 6: CI 门**

`.github/workflows/ci.yml` 的 KD-E9 步骤改为：

```yaml
      - name: "KD-E9: omt-daemon must not link the exec plane"
        run: |
          cargo build -p omt-runtime --bins
          test "$(nm -a target/debug/omt-daemon | grep -c -E 'omt_exec|omt_harness')" = "0"
          test "$(nm -a target/debug/omt | grep -c omt_exec)" -gt "0"
```

- [ ] **Step 7: 运行，确认通过**

Run: `cargo test -p omt-exec --lib spawn && cargo test -p omt-harness env_validation && cargo test -p omt-runtime --lib local_tools && cargo test -p omt-runtime --test local_tools_cli --test cli_e2e && cargo build -p omt-runtime --bins && test "$(nm -a target/debug/omt-daemon | grep -c -E 'omt_exec|omt_harness')" = "0" && echo KD-E9-ok`
Expected: 全部通过并打印 `KD-E9-ok`。

- [ ] **Step 8: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/omt-runtime crates/omt-exec/src/executor.rs crates/omt-harness/src/harness.rs .github/workflows/ci.yml
git commit -m "feat(cli): harness/exec local verbs via an injected LocalTools (A5)

The lib only defines the trait, tool table and argv parsing; the exec
plane implementation lives in the omt binary (local_impl.rs). The CI nm
gate now also rejects omt_harness symbols in omt-daemon.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A6: MCP 暴露本地工具（Q-5）

**Files:**
- Modify: `crates/omt-runtime/src/mcp.rs`
- Modify: `crates/omt-runtime/tests/mcp_spec.rs`
- Modify: `crates/omt-runtime/src/local_impl.rs`（导出 `nested_exec_id`，供 mcp.rs 之外的判定复用时参考；mcp.rs 自己读环境变量，不跨 bin/lib 引用）
- Modify: `crates/omt-runtime/src/cli/mod.rs`（`"mcp"` 分支传入 `local`）

**Interfaces:**
- Consumes: `local_tools::{LOCAL_TOOLS, LocalTools, LocalCaller}`（A5）
- Produces: `pub fn serve(globals: &GlobalArgs, local: Option<&dyn LocalTools>) -> Result<i32, Problem>`。暴露规则（Q-5 / Q-12）：注入桥（`--actor exec:*` 或进程环境带 `OMT_EXEC_ID`）不暴露本地工具；顶层会话始终列出全部 8 个本地工具，`exec__start` 的 policy 在调用时由 `LocalImpl` 检查

- [ ] **Step 1: 写失败测试（mcp_spec.rs）**

1. 给 `McpProcess::spawn` 加一个变体 `spawn_with(ctx, extra_args: &[&str], envs: &[(&str, &str)])`，原 `spawn(ctx)` 委托 `spawn_with(ctx, &[], &[])`；`extra_args` 插在 `mcp` 之前（全局参数，如 `--actor exec:exec-1`），`envs` 追加到子进程环境。
2. 现有 tools/list 测试的期望集合改为「daemon agent_available − home/* ∪ 全部 8 个本地工具」（Q-5：顶层会话始终列出，`exec__start` 在调用时检查 policy）：

```rust
    const LOCAL: &[&str] = &[
        "harness__list", "harness__models", "exec__start", "exec__stop",
        "exec__status", "exec__list", "exec__tail", "exec__policy",
    ];
    expected_wire.extend(LOCAL.iter().map(|s| s.to_string()));
    expected_wire.sort();
    // … assert_eq!(actual, expected_wire, "tools/list == daemon 子集 ∪ 全部本地工具");
```

3. 新增三个测试：

```rust
/// Q-3 / R7：MCP 不能借 `home` 路径让 daemon 声明一个未被服务的 home。
#[test]
fn mcp_exec_start_cannot_declare_an_unserved_home() {
    let (ctx, homes) = TestCtx::spawn_with_homes("mcp-local-home", 2);
    // 先用 home-01 起一次 daemon 再停掉 → home-01 已初始化（有 omt.db）但随后不被服务
    {
        let mut first = common::DaemonProcess::spawn(&ctx, &["--home", homes[1].to_str().unwrap()]);
        common::wait_for_descriptor(&ctx.runtime_dir, std::time::Duration::from_secs(20)).expect("daemon");
        first.kill();
        assert!(common::wait_descriptor_gone(&ctx.runtime_dir, std::time::Duration::from_secs(20)));
    }
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", homes[0].to_str().unwrap()]);
    wait_endpoint(&ctx);
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(ctx.runtime_dir.join("exec/policy.json"), r#"{"allowFromMcp": true}"#).unwrap();
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    let result = call_tool(&mut mcp, "exec__start", serde_json::json!({
        "home": homes[1].join(".omt").to_str().unwrap(),
        "homeId": "h_not_served", "harness": "pi", "nodeId": "TICKET-1"
    }));
    assert_eq!(result["isError"], true, "{result}");
    assert!(result["content"][0]["text"].as_str().unwrap().starts_with("NOT_FOUND"), "{result}");
    // home-01 仍未被服务（没有被声明）
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_omt"))
        .args(["--runtime-dir", ctx.runtime_dir_str(), "daemon-status"])
        .output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("home-01"));
    mcp.shutdown();
}

/// Q-12：进程环境带 OMT_EXEC_ID 的 omt mcp（被派发 harness 自己起的）同样不暴露本地工具。
#[test]
fn mcp_with_nested_exec_marker_exposes_no_local_tools() {
    let ctx = TestCtx::spawn_named("mcp-local-nested");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    let mut mcp = McpProcess::spawn_with(&ctx, &[], &[("OMT_EXEC_ID", "exec-parent")]);
    initialize(&mut mcp);
    let names = tool_names(&mut mcp);
    assert!(!names.iter().any(|n| n.starts_with("harness__") || n.starts_with("exec__")), "{names:?}");
    mcp.shutdown();
}

#[test]
fn injected_harness_bridge_exposes_no_local_tools() {
    let ctx = TestCtx::spawn_named("mcp-local-bridge");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(ctx.runtime_dir.join("exec/policy.json"), r#"{"allowFromMcp": true}"#).unwrap();
    let mut mcp = McpProcess::spawn_with(&ctx, &["--actor", "exec:exec-1"], &[]);
    initialize(&mut mcp);
    let names = tool_names(&mut mcp);
    assert!(!names.iter().any(|n| n.starts_with("harness__") || n.starts_with("exec__")), "{names:?}");
    // 直接调用也拒绝（tools/list 之外的兜底）
    let reply = mcp.request("tools/call", serde_json::json!({"name": "exec__list", "arguments": {}}));
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    mcp.shutdown();
}

#[test]
fn exec_start_via_mcp_is_policy_denied_by_default() {
    let ctx = TestCtx::spawn_named("mcp-local-deny");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    wait_endpoint(&ctx);
    let mut mcp = McpProcess::spawn(&ctx);
    initialize(&mut mcp);
    // 未列出但被直接调用：调用时再检一次 policy（Q-5）
    let result = call_tool(&mut mcp, "exec__start", serde_json::json!({
        "homeId": "h_x", "harness": "pi", "nodeId": "TICKET-1"
    }));
    assert_eq!(result["isError"], true, "{result}");
    assert!(result["content"][0]["text"].as_str().unwrap().starts_with("POLICY_DENIED"), "{result}");
    mcp.shutdown();
}
```

（`call_tool` / `tool_names` / `initialize` / `wait_endpoint` 是该文件已有 helper；若 `call_tool` 只返回 `result` 字段，按其实际返回形状取 `isError` 与 `content`。）

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p omt-runtime --test mcp_spec`
Expected: FAIL（tools/list 不含本地工具；`spawn_with` / 新测试失败）。

- [ ] **Step 3: 实现（mcp.rs）**

1. `serve(globals: &GlobalArgs, local: Option<&dyn crate::local_tools::LocalTools>)`；在 `enroll_identity` 之后计算：

```rust
    // Q-5 / Q-12：注入给 harness 的桥（actor exec:*），或被派发 harness 自己起的
    // omt mcp（进程环境带 OMT_EXEC_ID）都不暴露本地工具——被派发的 agent 不能再派发
    let nested = globals.actor.as_deref().is_some_and(|a| a.starts_with("exec:"))
        || std::env::var("OMT_EXEC_ID").is_ok_and(|v| !v.is_empty());
    let local = local.filter(|_| !nested);
```

2. `tools/list` 分支改为 `"tools": all_tool_descriptors(local)`：

```rust
fn local_descriptors(local: Option<&dyn crate::local_tools::LocalTools>) -> Vec<serde_json::Value> {
    // Q-5：顶层会话始终列出全部本地工具（omt mcp 不声明 listChanged，按 policy
    // 隐藏会让中途开启后主控仍看不到 exec__start）；policy 在调用时检查
    if local.is_none() {
        return Vec::new();
    }
    crate::local_tools::LOCAL_TOOLS
        .iter()
        .map(|t| serde_json::json!({
            "name": wire_name(t.action),
            "description": t.description,
            "inputSchema": (t.input_schema)(),
        }))
        .collect()
}

fn all_tool_descriptors(local: Option<&dyn crate::local_tools::LocalTools>) -> Vec<serde_json::Value> {
    let mut tools = tool_descriptors();
    tools.extend(local_descriptors(local));
    tools
}
```

3. `tools/call` 分支：先查 daemon 工具，再查本地工具：

```rust
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap_or_default();
                let arguments = request["params"]["arguments"].clone();
                Some(match action_of(name) {
                    Some(action) => call_tool(&mut enrollment.client, action, arguments, &id),
                    None => match (local, local_action_of(name)) {
                        (Some(local), Some(action)) => call_local_tool(local, action, arguments, &id),
                        _ => error_reply(&id, -32602, &format!("unknown tool: {name}")),
                    },
                })
            }
```

```rust
fn local_action_of(wire: &str) -> Option<&'static str> {
    crate::local_tools::LOCAL_TOOLS.iter().map(|t| t.action).find(|a| wire_name(a) == wire)
}

/// 本地工具调用：Problem → isError（与 daemon 工具同一映射）。policy（含
/// allowFromMcp 未开启）由 LocalImpl / omt_exec::local 在调用时检查。
fn call_local_tool(
    local: &dyn crate::local_tools::LocalTools,
    action: &str,
    arguments: serde_json::Value,
    id: &serde_json::Value,
) -> serde_json::Value {
    let args = if arguments.is_object() { arguments } else { serde_json::json!({}) };
    let outcome = local.call(crate::local_tools::LocalCaller::Mcp, action, args);
    match outcome {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default() }], "isError": false },
        }),
        Err(problem) => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "content": [{ "type": "text", "text": format!("{}: {}", problem.code, problem.message) }], "isError": true },
        }),
    }
}
```

4. `cli/mod.rs` 的 `"mcp"` 分支传真实的 `local`（去掉 A5 的 `let _ = local;`）。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p omt-runtime --test mcp_spec && cargo test -p omt-runtime --lib mcp`
Expected: 全部通过。

- [ ] **Step 5: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add crates/omt-runtime/src/mcp.rs crates/omt-runtime/src/local_impl.rs crates/omt-runtime/src/cli/mod.rs crates/omt-runtime/tests/mcp_spec.rs
git commit -m "feat(mcp): expose the local tool family under policy (A6)

A top-level omt mcp always lists all eight local tools (no listChanged,
so hiding exec__start would strand a session after the user enables
dispatch); policy is checked at call time. Injected bridges (actor
exec:*) and processes carrying OMT_EXEC_ID get no local tools, and MCP
can no longer declare a home through a path argument.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A7: parity 清单（`$defs.LocalToolMatrix`）

**Files:**
- Modify: `schema/parity.schema.json`
- Modify: `crates/omt-contracts/tests/contract.rs`
- Modify: `crates/omt-runtime/src/local_tools.rs`（单测：与 schema 一致）
- Regenerate: `packages/client-ts/src/generated/parity.ts`（`pnpm gen`）

**Interfaces:**
- Produces: schema `$defs.LocalToolEntry { tool, classification: "local_read" | "local_exec", summary, since }` 与 `$defs.LocalToolMatrix`（`default.entries` 8 条，顺序同 `LOCAL_TOOLS`）

- [ ] **Step 1: 写失败测试**

`crates/omt-runtime/src/local_tools.rs` tests 加：

```rust
    #[test]
    fn local_tool_table_matches_parity_schema() {
        let parity: Value = serde_json::from_str(include_str!("../../../schema/parity.schema.json")).unwrap();
        let entries = parity["$defs"]["LocalToolMatrix"]["default"]["entries"].as_array().expect("LocalToolMatrix");
        let schema_tools: Vec<(&str, &str)> = entries.iter()
            .map(|e| (e["tool"].as_str().unwrap(), e["classification"].as_str().unwrap()))
            .collect();
        let table: Vec<(&str, &str)> = LOCAL_TOOLS.iter()
            .map(|t| (t.action, if t.mutating { "local_exec" } else { "local_read" }))
            .collect();
        assert_eq!(schema_tools, table);
    }
```

`crates/omt-contracts/tests/contract.rs` 加：

```rust
#[test]
fn local_tool_matrix_is_disjoint_from_daemon_actions() {
    let parity = load_schema("parity.schema.json");
    let daemon: std::collections::BTreeSet<String> = parity["$defs"]["SeedActionParityMatrix"]["default"]["entries"]
        .as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap().to_string()).collect();
    let local = parity["$defs"]["LocalToolMatrix"]["default"]["entries"].as_array().unwrap();
    assert_eq!(local.len(), 8);
    for entry in local {
        let tool = entry["tool"].as_str().unwrap();
        assert!(!daemon.contains(tool), "{tool} 不得与 daemon 方法同名");
        assert!(matches!(entry["classification"].as_str(), Some("local_read" | "local_exec")));
        assert_eq!(entry["since"], "v1");
    }
}
```

（`load_schema` 用该文件已有的 schema 读取 helper 的实际名字。）

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p omt-runtime --lib local_tool_table_matches_parity_schema && cargo test -p omt-contracts local_tool_matrix`
Expected: FAIL（schema 无 `LocalToolMatrix`）。

- [ ] **Step 3: 改 schema**

`schema/parity.schema.json` 的 `$defs` 内追加：

```json
    "LocalToolEntry": {
      "type": "object",
      "description": "子项目 2 本地工具（不经 daemon，KD-E4）。local_read 只读；local_exec 有副作用，受 exec/policy.json 约束。",
      "additionalProperties": false,
      "required": ["tool", "classification", "summary", "since"],
      "properties": {
        "tool": { "type": "string", "pattern": "^(harness|exec)/[a-z]+$" },
        "classification": { "enum": ["local_read", "local_exec"] },
        "summary": { "type": "string" },
        "since": { "type": "string", "pattern": "^v[0-9]+$" }
      }
    },
    "LocalToolMatrix": {
      "type": "object",
      "required": ["entries"],
      "properties": { "entries": { "type": "array", "items": { "$ref": "#/$defs/LocalToolEntry" } } },
      "default": {
        "entries": [
          { "tool": "harness/list", "classification": "local_read", "summary": "列出 harness 与安装/启用状态", "since": "v1" },
          { "tool": "harness/models", "classification": "local_read", "summary": "列出 harness 可用模型", "since": "v1" },
          { "tool": "exec/start", "classification": "local_exec", "summary": "拉起执行（policy 约束）", "since": "v1" },
          { "tool": "exec/stop", "classification": "local_exec", "summary": "停止执行（policy 约束）", "since": "v1" },
          { "tool": "exec/status", "classification": "local_read", "summary": "执行状态", "since": "v1" },
          { "tool": "exec/list", "classification": "local_read", "summary": "执行列表", "since": "v1" },
          { "tool": "exec/tail", "classification": "local_read", "summary": "按 seq 分页读取事件", "since": "v1" },
          { "tool": "exec/policy", "classification": "local_read", "summary": "查看生效的派发策略", "since": "v1" }
        ]
      }
    }
```

- [ ] **Step 4: 重新生成 TS 绑定并跑测试**

Run: `pnpm gen && cargo test -p omt-runtime --lib local_tools && cargo test -p omt-contracts && pnpm typecheck && pnpm test`
Expected: 全部通过；`git status` 显示 `packages/client-ts/src/generated/parity.ts` 有变化（新增类型）。

- [ ] **Step 5: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add schema/parity.schema.json crates/omt-contracts/tests/contract.rs crates/omt-runtime/src/local_tools.rs packages/client-ts/src/generated
git commit -m "feat(schema): LocalToolMatrix for the local tool family (A7)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A8: 桌面端——派发策略设置页、定时发现外部执行、发起方标签

**Files:**
- Modify: `apps/desktop/src-tauri/src/executor.rs`（`exec_policy_get` / `exec_policy_set`）
- Modify: `apps/desktop/src-tauri/src/lib.rs`（注册两个 command）
- Modify: `apps/desktop/src/types.ts`、`apps/desktop/src/bridge.ts`
- Create: `apps/desktop/src/ExecPolicySection.tsx`
- Modify: `apps/desktop/src/SettingsPanel.tsx`
- Modify: `apps/desktop/src/store.ts`（Q-10 定时刷新）
- Modify: `apps/desktop/src/AgentsPanel.tsx`（`launchedBy` 标签）
- Create: `apps/desktop/tests/exec-policy.spec.tsx`
- Modify: `apps/desktop/tests/store.spec.tsx`（定时刷新不清错误）
- Modify: `apps/desktop/tests/agents.spec.tsx`（发起方标签）

**Interfaces:**
- Consumes: `omt_exec::policy::{load, save, ExecPolicy}`（A1）、`ExecRecord.launched_by`（A2）
- Produces:
  - tauri `exec_policy_get() -> Result<Value, String>` → `{policy, source, path, defaults}`；`POLICY_INVALID` 时返回 `{error, path, defaults}` 而不是 Err。`defaults` 是 `ExecPolicy::default()`——前端不自己维护一份默认值副本
  - tauri `exec_policy_set(policy: ExecPolicy) -> Result<Value, String>` → 保存后的 `exec_policy_get` 结果
  - TS `interface ExecPolicy { allowFromCli: boolean; allowFromMcp: boolean; harnesses: string[]; workspaces: 'homeWorkspaceOnly'; maxConcurrent: number; maxDurationSec: number; approvalMode: 'inherit' }`
  - TS `execPolicyGet(): Promise<ExecPolicyView>`、`execPolicySet(policy: ExecPolicy): Promise<ExecPolicyView>`；`ExecPolicyView = { policy?: ExecPolicy; defaults: ExecPolicy; source?: 'file' | 'default'; path: string; error?: string }`
  - `AgentRecord` TS 类型的 exec 增加 `launchedBy?: 'desktop' | 'cli' | 'mcp' | null`；agent 增加 `parentAgentId?: string | null`
  - store：`refreshAgents(opts?: { silent?: boolean })`——`silent` 时不清除 `state.error`（Q-10 定时轮询用）
  - `export function originLabel(origin: 'cli' | 'mcp'): { text: string; title: string }`（`AgentsPanel.tsx`，供测试）

- [ ] **Step 1: 写失败测试**

`apps/desktop/tests/exec-policy.spec.tsx`（mock 边界照 `harness-manager.spec.tsx`：mock `@tauri-apps/api/core` 的 `invoke` 与 `Channel`、`@tauri-apps/api/event`、`@tauri-apps/api/app`、`@tauri-apps/plugin-dialog`——直接复制该文件头部 40 行的 mock 块）：

```tsx
// @vitest-environment jsdom
import { ExecPolicySection } from '../src/ExecPolicySection'

const defaults = {
  allowFromCli: true, allowFromMcp: false, harnesses: ['pi', 'omp', 'opencode'],
  workspaces: 'homeWorkspaceOnly', maxConcurrent: 4, maxDurationSec: 1800, approvalMode: 'inherit',
}
const ok = (extra: object = {}) => ({ policy: defaults, defaults, source: 'default', path: '/p', ...extra })

describe('ExecPolicySection', () => {
  beforeEach(() => invokeMock.mockReset())

  it('shows the effective policy, its source and the unattended/CLI warnings', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok()) : Promise.reject(new Error(cmd)))
    render(<ExecPolicySection />)
    expect(await screen.findByLabelText('允许经 MCP 派发')).not.toBeChecked()
    expect(screen.getByLabelText('允许经 CLI 派发')).toBeChecked()
    expect(screen.getByText(/默认值/)).toBeInTheDocument()
    expect(screen.getByText(/无人值守/)).toBeInTheDocument()
    expect(screen.getByText(/能执行 shell 的 agent/)).toBeInTheDocument()
    expect(screen.getByText(/只影响之后的派发/)).toBeInTheDocument()
  })

  it('saves toggles and numeric limits through exec_policy_set', async () => {
    invokeMock.mockImplementation((cmd: string, args?: { policy: typeof defaults }) => {
      if (cmd === 'exec_policy_get') return Promise.resolve(ok())
      if (cmd === 'exec_policy_set') return Promise.resolve(ok({ policy: args!.policy, source: 'file' }))
      return Promise.reject(new Error(cmd))
    })
    render(<ExecPolicySection />)
    fireEvent.click(await screen.findByLabelText('允许经 MCP 派发'))
    fireEvent.change(screen.getByLabelText('最大并发'), { target: { value: '2' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('exec_policy_set', {
      policy: { ...defaults, allowFromMcp: true, maxConcurrent: 2 },
    }))
    expect(await screen.findByRole('status')).toHaveTextContent('已保存')
  })

  it('reports validation errors as an alert tied to the field, without invoking', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok()) : Promise.reject(new Error(cmd)))
    render(<ExecPolicySection />)
    const field = await screen.findByLabelText('最大并发')
    fireEvent.change(field, { target: { value: '0' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/最大并发须在 1–32 之间/)
    expect(field).toHaveAttribute('aria-invalid', 'true')
    expect(invokeMock).not.toHaveBeenCalledWith('exec_policy_set', expect.anything())
  })

  it('warns inline when no harness is allowed', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve(ok({ policy: { ...defaults, harnesses: [] } })) : Promise.reject(new Error(cmd)))
    render(<ExecPolicySection />)
    expect(await screen.findByText(/一个 harness 都没选/)).toBeInTheDocument()
  })

  it('prefills documented defaults when the file is invalid and offers restore-defaults', async () => {
    invokeMock.mockImplementation((cmd: string) =>
      cmd === 'exec_policy_get' ? Promise.resolve({ error: 'POLICY_INVALID: 无法解析', defaults, path: '/p' }) : Promise.reject(new Error(cmd)))
    render(<ExecPolicySection />)
    expect(await screen.findByText(/策略文件无效/)).toBeInTheDocument()
    expect(screen.getByText(/CLI 和 MCP 派发已全部拒绝/)).toBeInTheDocument()
    expect(screen.getByText(/下面已填入默认值/)).toBeInTheDocument()
    expect(screen.getByLabelText('允许经 CLI 派发')).toBeChecked()
    fireEvent.click(screen.getByLabelText('允许经 CLI 派发'))
    fireEvent.click(screen.getByRole('button', { name: '恢复默认值' }))
    expect(screen.getByLabelText('允许经 CLI 派发')).toBeChecked()
  })

  it('shows the load error with a retry button instead of loading forever', async () => {
    invokeMock.mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce(ok())
    render(<ExecPolicySection />)
    expect(await screen.findByRole('alert')).toHaveTextContent(/读取派发策略失败/)
    fireEvent.click(screen.getByRole('button', { name: '重试' }))
    expect(await screen.findByLabelText('允许经 MCP 派发')).toBeInTheDocument()
  })
})
```

`apps/desktop/tests/agents.spec.tsx` 加（导入 `originLabel`）：

```tsx
  it('origin chip names who launched the displayed exec', () => {
    expect(originLabel('mcp')).toEqual({ text: 'MCP', title: '由 MCP（主控 agent）发起' })
    expect(originLabel('cli')).toEqual({ text: 'CLI', title: '由 CLI 发起' })
  })
```

并在该文件已有的「渲染 agent 列表」用例旁加一条：最新 exec `launchedBy: 'mcp'` 的 agent 行出现 `aria-label="由 MCP（主控 agent）发起"` 的标签，`launchedBy: 'desktop'` / 缺省的不出现。

`apps/desktop/tests/store.spec.tsx` 加（用 `vi.useFakeTimers()`；该文件已有的 store 渲染 helper 与 `agents_list` mock 照用）：

```tsx
  it('Q-10: refreshes agents every 5s while visible, without clearing a shown error', async () => {
    vi.useFakeTimers()
    // …按本文件现有方式挂载 store 并选中一个 home，再触发一次会写 state.error 的失败（如 exec_stop 拒绝）…
    const before = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
    await vi.advanceTimersByTimeAsync(5_000)
    const after = invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length
    expect(after).toBeGreaterThan(before)
    expect(result.current.state.error).not.toBeNull()  // 静默轮询不清错误
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true })
    await vi.advanceTimersByTimeAsync(10_000)
    expect(invokeMock.mock.calls.filter(c => c[0] === 'agents_list').length).toBe(after)
    Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true })
    vi.useRealTimers()
  })
```

- [ ] **Step 2: 运行，确认失败**

Run: `cd apps/desktop && pnpm test -- exec-policy store agents`
Expected: FAIL（组件不存在；无定时刷新；`originLabel` 未导出）。

- [ ] **Step 3: tauri commands（apps/desktop/src-tauri/src/executor.rs）**

```rust
/// A8：设置页「派发策略」。无效文件不报 Err——返回 {error, path, defaults} 让界面
/// 提示「CLI/MCP 派发已全部拒绝」并预填文档默认值，绝不伪装成文件内容。
#[tauri::command(async)]
pub fn exec_policy_get() -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let file = omt_exec::layout::policy_file(&runtime_dir);
    let path = file.display().to_string();
    let defaults = omt_exec::policy::ExecPolicy::default();
    match omt_exec::policy::load(&runtime_dir) {
        Ok(policy) => Ok(serde_json::json!({
            "policy": policy,
            "defaults": defaults,
            "source": if file.is_file() { "file" } else { "default" },
            "path": path,
        })),
        Err(problem) => Ok(serde_json::json!({
            "error": format!("{}: {}", problem.code, problem.message),
            "defaults": defaults,
            "path": path,
        })),
    }
}

#[tauri::command(async)]
pub fn exec_policy_set(policy: omt_exec::policy::ExecPolicy) -> Result<serde_json::Value, String> {
    omt_exec::policy::save(&daemon::runtime_dir(), &policy)?;
    exec_policy_get()
}
```

`lib.rs` 的 `generate_handler!` 加 `executor::exec_policy_get, executor::exec_policy_set,`。

- [ ] **Step 4: 前端**

`types.ts`：

```ts
/** A8：exec/policy.json（设计 §3.6）。只约束 CLI / MCP 发起的派发。 */
export interface ExecPolicy {
  allowFromCli: boolean
  allowFromMcp: boolean
  harnesses: string[]
  workspaces: 'homeWorkspaceOnly'
  maxConcurrent: number
  maxDurationSec: number
  approvalMode: 'inherit'
}

export interface ExecPolicyView {
  policy?: ExecPolicy
  /** ExecPolicy::default()（Rust 侧唯一来源） */
  defaults: ExecPolicy
  source?: 'file' | 'default'
  path: string
  error?: string
}
```

并在 agent / exec 类型上加 `parentAgentId?: string | null` 与 `launchedBy?: 'desktop' | 'cli' | 'mcp' | null`。

`bridge.ts`：

```ts
export async function execPolicyGet(): Promise<ExecPolicyView> {
  return invoke<ExecPolicyView>('exec_policy_get')
}

export async function execPolicySet(policy: ExecPolicy): Promise<ExecPolicyView> {
  return invoke<ExecPolicyView>('exec_policy_set', { policy })
}
```

`ExecPolicySection.tsx`：

```tsx
/**
 * A8 设置页「派发策略」（设计 §3.6 / KD-E5）：exec/policy.json 的界面编辑入口
 * （同一用户的进程也能直接改这个文件——见计划「威胁模型」）。只约束 CLI / MCP
 * 发起的派发；桌面端派发不受影响（Q-4）。
 */
import { useCallback, useEffect, useState } from 'react'
import { execPolicyGet, execPolicySet } from './bridge'
import type { ExecPolicy, ExecPolicyView } from './types'

const HARNESS_IDS = ['pi', 'omp', 'opencode'] as const

type FieldError = { field: 'maxConcurrent' | 'maxDurationSec'; text: string }

function validate(p: ExecPolicy): FieldError | null {
  if (!Number.isInteger(p.maxConcurrent) || p.maxConcurrent < 1 || p.maxConcurrent > 32) {
    return { field: 'maxConcurrent', text: '最大并发须在 1–32 之间' }
  }
  if (!Number.isInteger(p.maxDurationSec) || p.maxDurationSec < 60) {
    return { field: 'maxDurationSec', text: '最长时长不得小于 60 秒' }
  }
  return null
}

/** 空输入保持空串让校验报错，而不是被 Number('') 变成 0 */
const numberOrNaN = (raw: string) => (raw.trim() === '' ? Number.NaN : Number(raw))

export function ExecPolicySection() {
  const [view, setView] = useState<ExecPolicyView | null>(null)
  const [draft, setDraft] = useState<ExecPolicy | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [fieldError, setFieldError] = useState<FieldError | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const [saving, setSaving] = useState(false)

  const load = useCallback(() => {
    setLoadError(null)
    void execPolicyGet()
      .then(v => {
        setView(v)
        // 文件无效时预填文档默认值（不是「全部拒绝」）——保存即修复成默认策略
        setDraft(v.policy ?? v.defaults)
      })
      .catch(error => setLoadError(String(error)))
  }, [])

  useEffect(load, [load])

  if (loadError) {
    return (
      <div role="alert" className="error">
        读取派发策略失败：{loadError}
        <button className="chip" onClick={load}>重试</button>
      </div>
    )
  }
  if (!view || !draft) return <p className="muted">读取派发策略…</p>

  const policy = draft
  const update = (patch: Partial<ExecPolicy>) => {
    setFieldError(null)
    setSaveError(null)
    setSaved(false)
    setDraft({ ...policy, ...patch })
  }
  const save = async () => {
    const problem = validate(policy)
    if (problem) {
      setFieldError(problem)
      return
    }
    setSaving(true)
    try {
      const next = await execPolicySet(policy)
      setView(next)
      setDraft(next.policy ?? policy)
      setSaved(true)
    } catch (error) {
      setSaveError(String(error))
    } finally {
      setSaving(false)
    }
  }
  const invalid = (field: FieldError['field']) => fieldError?.field === field

  return (
    <section className="exec-policy">
      <h3>派发策略</h3>
      <p className="muted">
        约束主控 agent（MCP）和命令行（CLI）发起的派发；桌面端派发不受影响。保存后只影响之后的派发，不会停止正在运行的执行。文件：<code>{view.path}</code>
        {view.source === 'default' && '（当前为默认值，尚未保存过）'}
      </p>
      <p className="warning">
        经 CLI / MCP 派发的 harness 一律无人值守运行（pi <code>--auto</code>、omp <code>--approval-mode=yolo</code> 等），没有桌面端派发弹窗那一步确认。
      </p>
      {view.error && (
        <p className="error" role="alert">
          策略文件无效：{view.error}。在修复前 CLI 和 MCP 派发已全部拒绝；下面已填入默认值，保存将用它覆盖文件。
        </p>
      )}
      <label>
        <input type="checkbox" checked={policy.allowFromCli} onChange={e => update({ allowFromCli: e.target.checked })} />
        允许经 CLI 派发
      </label>
      <p className="muted">也覆盖所有能执行 shell 的 agent（例如 Claude Code 的 Bash 工具）——只关 MCP 派发拦不住这类主控。</p>
      <label>
        <input type="checkbox" checked={policy.allowFromMcp} onChange={e => update({ allowFromMcp: e.target.checked })} />
        允许经 MCP 派发
      </label>
      <p className="muted">开启后，接入 omt mcp 的主控 agent 可在下面的并发与时长上限内启动 harness 进程。</p>
      <fieldset>
        <legend>允许的 harness</legend>
        {HARNESS_IDS.map(id => (
          <label key={id}>
            <input
              type="checkbox"
              checked={policy.harnesses.includes(id)}
              onChange={e => update({
                harnesses: e.target.checked ? [...policy.harnesses, id] : policy.harnesses.filter(h => h !== id),
              })}
            />
            {id}
          </label>
        ))}
        {policy.harnesses.length === 0 && (
          <p className="warning">一个 harness 都没选——所有 CLI / MCP 派发都会被拒绝。</p>
        )}
      </fieldset>
      <label>
        最大并发
        <input type="number" min={1} max={32} value={Number.isNaN(policy.maxConcurrent) ? '' : policy.maxConcurrent}
          aria-invalid={invalid('maxConcurrent')}
          aria-describedby={invalid('maxConcurrent') ? 'exec-policy-error' : undefined}
          onChange={e => update({ maxConcurrent: numberOrNaN(e.target.value) })} />
      </label>
      <label>
        最长时长（秒）
        <input type="number" min={60} value={Number.isNaN(policy.maxDurationSec) ? '' : policy.maxDurationSec}
          aria-invalid={invalid('maxDurationSec')}
          aria-describedby={invalid('maxDurationSec') ? 'exec-policy-error' : undefined}
          onChange={e => update({ maxDurationSec: numberOrNaN(e.target.value) })} />
      </label>
      <button onClick={() => void save()} disabled={saving}>{saving ? '保存中…' : '保存'}</button>
      <button className="chip" onClick={() => update(view.defaults)} disabled={saving}>恢复默认值</button>
      {(fieldError || saveError) && (
        <p id="exec-policy-error" className="error" role="alert">{fieldError?.text ?? saveError}</p>
      )}
      <p role="status" aria-live="polite">{saved ? '已保存' : ''}</p>
    </section>
  )
}
```

`SettingsPanel.tsx`：`type SettingsSection = 'general' | 'harnesses' | 'policy'`；内层分段加按钮「派发策略」（与现有两个 `inner-tab` 按钮同形），`section === 'policy'` 时渲染 `<ExecPolicySection />`。

`store.ts`：`refreshAgents` 改为接受 `opts?: { silent?: boolean }`，`setState` 里把原来的 `error: null,` 换成 `...(opts?.silent ? {} : { error: null }),`（其余不变；`useCallback` 依赖不变）。在 boot/focus 那个 `useEffect` 之后加：

```ts
  // Q-10：CLI / MCP 发起的执行在桌面端无需切换焦点即可见——窗口可见时每 5s
  // 静默重拉 agents（不清除全局错误）。外部 agent 由此出现在列表中；选中后由
  // AgentsPanel 的 P-10 effect 接回实时流。
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') void refreshAgents({ silent: true })
    }, AGENTS_POLL_MS)
    return () => clearInterval(timer)
  }, [refreshAgents])
```

文件顶部常量区加 `const AGENTS_POLL_MS = 5_000`。

`AgentsPanel.tsx`：

```tsx
/** A8：发起方标签——按来源分别说明（cli / mcp）；desktop 与旧记录不显示 */
export function originLabel(origin: 'cli' | 'mcp'): { text: string; title: string } {
  return origin === 'mcp'
    ? { text: 'MCP', title: '由 MCP（主控 agent）发起' }
    : { text: 'CLI', title: '由 CLI 发起' }
}

function OriginChip({ origin }: { origin?: 'desktop' | 'cli' | 'mcp' | null }) {
  if (origin !== 'cli' && origin !== 'mcp') return null
  const { text, title } = originLabel(origin)
  return <span className="chip origin-chip" title={title} aria-label={title}>{text}</span>
}
```

在 agent 行的显示名旁渲染 `<OriginChip origin={latestExecOf(agent)?.launchedBy} />`；在右侧 transcript 头部（`selectedExec` 渲染处）同样渲染 `<OriginChip origin={selectedExec.launchedBy} />`。标签反映的是**当前显示的那次 exec**（Agents 面板总是显示最新 exec）：经 MCP 拉起、后来在桌面端续跑的 agent 标签会消失——这是如实反映「当前这次执行由谁发起」。

- [ ] **Step 5: 运行，确认通过**

Run: `cd apps/desktop && pnpm test && pnpm exec tsc --noEmit && cd ../.. && cargo test -p oh-my-ticket-desktop`
Expected: 全部通过；既有测试断言未改动。

- [ ] **Step 6: 基线验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add apps/desktop
git commit -m "feat(desktop): dispatch policy settings, silent 5s agent discovery, CLI/MCP origin chip (A8)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task A9: 端到端验证、文档与子项目 2 验收

**Files:**
- Modify: `crates/omt-runtime/tests/local_tools_cli.rs`（真 daemon + 假 harness 的 CLI 派发 e2e）
- Modify: `crates/omt-runtime/README.md`、`docs/runtime/operations.md`（新动词与 policy）
- Modify: `docs/plans/2026-10-20-0802-feat-omt-exec-plane-orchestration-design.md`（§7 子项目 2 状态、§8 `run/claim` 实测结论）
- Modify: `docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md`（「执行面子项目 2 验收」一节）

- [ ] **Step 1: 写 CLI 派发 e2e（真 daemon、假 harness）**

在 `local_tools_cli.rs` 追加。假 harness 的写法照 `crates/omt-runtime/tests/exec_shim.rs` 的 `rig_at`（`omt_harness::harness::upsert_config` + `BinarySource::Manual` 指向一个脚本）——在本文件内复制该 helper 的必要部分。脚本只 `echo` 一行 JSON 后 `exit 0`（不需要真的 claim：验证的是「CLI 能拉起、记录发起方、不调 node/execute、能 tail 到 exit」）。

```rust
#[test]
fn cli_exec_start_launches_tails_and_records_origin() {
    let ctx = TestCtx::spawn_named("local-cli-e2e");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    common::wait_for_descriptor(&ctx.runtime_dir, std::time::Duration::from_secs(20)).expect("daemon");
    install_fake_pi(&ctx, r#"echo '{"type":"session","id":"s-1"}'; sleep 1; exit 0"#);
    // 建一张 ticket（epic → story → ticket）
    let ticket = create_ticket_chain(&ctx);
    let out = run_cli(&ctx, &["--home", ctx.home_str(), "exec", "start", "--harness", "pi", "--node", &ticket, "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let started: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let exec_id = started["execId"].as_str().unwrap().to_string();

    // 等到 tail 报告 done（最多 30s）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut since: Option<u64> = None;
    let mut kinds = Vec::new();
    loop {
        let mut args = vec!["exec", "tail", exec_id.as_str(), "--json"];
        let since_text;
        if let Some(s) = since { since_text = s.to_string(); args.extend(["--since", since_text.as_str()]); }
        let page: serde_json::Value = serde_json::from_slice(&run_cli(&ctx, &args).stdout).unwrap();
        for ev in page["events"].as_array().unwrap() { kinds.push(ev["kind"].as_str().unwrap().to_string()); }
        since = page["nextSeq"].as_u64().or(since);
        if page["done"] == true { break; }
        assert!(std::time::Instant::now() < deadline, "tail 30s 内未结束：{kinds:?}");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    assert_eq!(kinds.last().map(String::as_str), Some("exit"), "{kinds:?}");

    let status: serde_json::Value = serde_json::from_slice(&run_cli(&ctx, &["exec", "status", &exec_id, "--json"]).stdout).unwrap();
    assert_eq!(status["exec"]["launchedBy"], "cli");
    // Q-2：node 状态没有被 node/execute 改成 in_progress（假 harness 不 claim，节点保持 open）
    let shown = run_cli(&ctx, &["--home", ctx.home_str(), "show", &ticket]);
    let node: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(node["node"]["status"], "open");
}
```

`install_fake_pi` 与 `create_ticket_chain`（经 `run_cli(.., "create", ..)` 三次）写在本文件内，各约 15 行。

- [ ] **Step 2: 运行**

Run: `cargo build -p omt-runtime --bins && cargo test -p omt-runtime --test local_tools_cli`
Expected: 全部通过。

- [ ] **Step 3: 文档**

- `crates/omt-runtime/README.md` 与 `docs/runtime/operations.md` 的动词清单补上 `harness …` / `exec …`（照 USAGE 文本），并加一节「派发策略」，必须写明：
  - 文件位置、默认值、fail closed（派发）与「停止不受 policy 约束」；
  - CLI / MCP 派发**总是无人值守**（附加 harness 的 `--auto` / `--approval-mode=yolo` 等），没有桌面端那一步确认；
  - 「允许经 CLI 派发」也覆盖任何能执行 shell 的 agent（如 Claude Code 的 Bash），只关 MCP 拦不住这类主控；
  - 只允许 workspace 内的 home（全局 home 被拒，Q-13）；
  - 被派发的 agent（带 `OMT_EXEC_ID`）不能再派发或停止，注入桥不暴露本地工具；
  - 威胁模型：这是对经 omt 工具面行事的 agent 的纵深防御，不是同一用户进程之间的安全边界（它们能改 `policy.json`、清环境变量）。
- 设计文档 §7 子项目 2 行追加「**已完成（日期，实现序列 <首个 commit> 起）**」；§8 的 `run/claim` 一条改为「**已实测（2026-10-23）**：只激活祖先、不改节点本身；按 Q-2 接受现状」。
- findings log 新增「### 执行面子项目 2 验收」一节，列出下面 Step 4 的手工验收矩阵（结果列先填「待验收」）。

- [ ] **Step 4: 手工验收矩阵（写入 findings log；验收环境放在 `/tmp` 之外，如 `~/omt-accept/`）**

| # | 场景 | 期望 |
|---|---|---|
| 1 | 设置页「派发策略」：默认显示 MCP 关闭；打开并保存 | `exec/policy.json` 写出；`omt exec policy` 显示 `source: file`、`allowFromMcp: true` |
| 2 | 主控（Claude Code 配置 `omt mcp`）调用 `harness__list`、`harness__models pi` | 返回已启用 harness 与 pi 的模型列表 |
| 3 | 主控 `exec__start {homeId, harness: "pi", model: "kimi-coding/k3", nodeId: <快速 ticket>}`（ticket 在 workspace 内的 `.omt` 里） | 立即返回 `{agentId, execId, runId}`；**5 秒内**桌面端 Agents 面板出现新 agent，带 `MCP` 标签；选中它后实时 transcript 可见；跑完 run completed |
| 4 | 主控 `exec__tail` 循环直到 `done` | 能拿到完整事件，末尾是 exit |
| 5 | 设置页关闭 MCP 派发后，主控再 `exec__start`（不重启 MCP 会话）；再在设置页重新开启并再调一次 | 关闭时 `POLICY_DENIED`（field `allowFromMcp`）、`exec__start` 仍在 tools/list 中；重新开启后无需重启会话即可派发成功 |
| 6 | 被派发的 harness 自己的 MCP 工具列表 | 不含任何 `harness__*` / `exec__*` |
| 7 | `maxConcurrent: 1` 时连续两次 `omt exec start` | 第二次 `POLICY_DENIED`（field `maxConcurrent`） |
| 8 | `omt exec stop <execId>` | 3 秒内 `killed`，item 回 pending |
| 9 | 桌面端派发（任一 harness） | 与子项目 1 完全一致，不受 policy 影响 |
| 10 | 故意写坏 `exec/policy.json` | 设置页显示「策略文件无效」并预填默认值；CLI/MCP 派发 `POLICY_INVALID`；`omt exec stop` 仍可用；桌面端派发不受影响 |
| 11 | 被派发的 harness 在自己的 shell 里执行 `omt exec start …` 与 `omt exec stop …`（可让测试 ticket 要求它这样做） | 均 `POLICY_DENIED`（field `nestedDispatch`）；它的 MCP 工具列表里没有任何 `harness__*` / `exec__*` |
| 12 | 设置页关闭 CLI 派发后，在终端执行 `omt exec start --harness pi --node <ticket>` | `POLICY_DENIED`（field `allowFromCli`）——同时证明关 MCP 不等于关掉 agent 经 shell 的派发 |
| 13 | 对全局 home（`~/.omt`）里的 ticket 执行 `omt --home ~/.omt exec start …` | `POLICY_DENIED`（field `workspaces`），没有在用户主目录下拉起 harness |
| 14 | 主控 `harness__list` | 返回的条目只有 `envKeys`（键名），看不到任何 env 值；`extraArgs` 已脱敏 |

- [ ] **Step 5: 全量验证并提交**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && pnpm test && (cd apps/desktop && pnpm test && pnpm exec tsc --noEmit) && cargo build -p omt-runtime --bins && test "$(nm -a target/debug/omt-daemon | grep -c -E 'omt_exec|omt_harness')" = "0"`
Expected: 全绿。

```bash
git add crates/omt-runtime/tests/local_tools_cli.rs crates/omt-runtime/README.md docs/runtime/operations.md docs/plans/2026-10-20-0802-feat-omt-exec-plane-orchestration-design.md docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md
git commit -m "test(exec): CLI launch e2e, docs and sub-project 2 acceptance matrix (A9)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

# B 部分：P3 清理（子项目 1 验收与 §39–§47 遗留）

> **执行顺序与基线**：B 部分在 A 部分全部完成后执行（每个 B Task 开工前先 `git log --oneline -5` 确认 A9 已提交）。B 的代码片段已按 A 落地后的接口写好，下表列出 A 改动过、B 片段直接依赖的接口；实施时若与代码不符，以下表为准：
>
> | A 的改动 | B 片段的写法 | 涉及位置 |
> |---|---|---|
> | A2：`NewExec` 新增 `launched_by` / `parent_agent_id` | 每个 `NewExec { .. }` 字面量都带 `launched_by: None, parent_agent_id: None` | B3 `pending_target`、B4 `kill_orphan_kills_the_whole_harness_process_group`、B5 `transcript_tail_falls_back_to_legacy_dir_mid_migration`、B7 `force_release_is_bounded_by_an_overall_budget`（各测试模块里既有的 `new_exec` / `reg` / `register` helper 已由 A2 补齐，B 直接复用） |
> | A2：`dispatch_daemon_side` 11 参数 | `dispatch_daemon_side(rpc, runtime, home, LaunchTarget::Node(id), LaunchOrigin::Desktop, harness, unattended, model, resume, target_agent, None)`；executor tests 模块已有 `use crate::policy::LaunchOrigin;` | B2 `dispatch_on_done_ticket_explains_it_is_done` |
> | A2：① 预检挪进 `match target` 的 `LaunchTarget::Node(node_id)` 分支 | B2 的 `no_members_error(node_id, &node_resp["node"])` 写在该分支里（`node_resp` 只在这里有效）；`LaunchTarget::Run` 分支不受影响 | B2 Step 3 |
> | A3：`launch::start` 的跨进程 `launch_lock`，在 ⑧ 身份落档之后、⑨ `wait_ready` 之前 `drop(launch_lock)` | B2 的 `record_shim_identity(...)` 调用放在 `drop(launch_lock);` **之前**（身份落档仍受锁保护），`wait_ready` 与新增的 `Ready::Stopped` 分支在锁外 | B2 Step 5 |
> | A4：`omt_exec::local::exec_stop(ctx, exec_id)`（不带 `origin`；停止不受 policy 约束，被派发的 agent 由 `OMT_EXEC_ID` 拦截，Q-12） | B2 让 `control::stop_exec` 在拉起宽限期内返回 `Err`；`local::exec_stop` 经既有的 `.map_err(io)` 原样透传为 `IO` 错误（消息「执行仍在拉起中，请稍后再停止」），无需改 `local.rs` | B2 Step 6 |
> | A4：`daemon_link::served_homes(runtime_dir, daemon_bin) -> Vec<ServedHome { home_id, path, kind }>`，内部调用 `DaemonLink::enroll(runtime_dir, "cli")` | B8 给 `enroll` 加超时参数时，`served_homes` 里的调用同步改为 `DaemonLink::enroll(runtime_dir, "cli", rpc_timeout())` | B8 Step 3 |
> | A5：KD-E9 的 `nm` 门同时检查两个 crate | 一律写 `nm -a target/debug/omt-daemon \| grep -c -E 'omt_exec\|omt_harness'`，期望 `0` | B1 Step 4、B8 Step 7、§48 |

> 范围：执行面子项目 1 收口后遗留的 P3 项（findings log §39 / §41 / §43 / §45 / §47 的 P3 段与「执行面子项目 1 验收」节的 P3-ACC-1..8）。每个 Task 结束时都有可独立验证的交付物，按 B1 → B8 顺序执行（B5 改写的 `delete_agent` 会在 B7 再加一行；B2 引入的 `agents::update_agent` 被 B3 / B4 复用；B2 引入的 `ShimOptions` 被 B8 复用）。
>
> **与子项目 1 的约束差异**：子项目 1 的「daemon 零改动」到此为止——B1 修改 `omt-domain` 与 `omt-runtime` 的 daemon 侧报错（P3-ACC-2 当时因此被推迟）。**KD-E9 仍然成立**：`crates/omt-runtime` 的库（omt-daemon 链接的部分）不得引用 `omt_exec` / `omt_harness`；B1 只动 `omt-domain`，不触碰这条边界。每个 Task 收尾仍跑 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings` 与对应 crate 的测试；动到前端的 Task 加跑 `cd apps/desktop && pnpm test && pnpm exec tsc --noEmit`。

### B 部分分诊表

| 来源 | 条目 | 去向 |
|---|---|---|
| 验收 P3 | P3-ACC-1 派发 prompt 未要求 leaseToken | 已修（fe84f27），B8 只补总览行 |
| 验收 P3 | P3-ACC-2 缺 token 的报错文案误导 | **B1** |
| 验收 P3 | P3-ACC-3 run 完成后的多余认领 | 已修（fe84f27），B8 只补总览行 |
| 验收 P3 | P3-ACC-4 已完成 ticket 续跑的报错不说明原因 | **B2** |
| 验收 P3 | P3-ACC-5 agent 状态从不进入 running | **B2** |
| 验收 P3 | P3-ACC-6 退出 / 关窗确认框 Esc 无效 | **B7** |
| 验收 P3 | P3-ACC-7 sidecar 复制带 provenance 导致 EPERM | **B8** |
| 验收 P3 | P3-ACC-8 对账释放遇已取消 run 报失败 | **B3** |
| §41 | stop-requested-before-spawn（launcher SIGTERM 到达时 shim 仍会拉起 harness，Killed 覆盖 Failed） | **B2** |
| §39 | grace-stop-noop（启动宽限期内停止静默成功） | **B2** |
| §41 | launcher 身份写入失败的 SIGTERM + 补偿分支无测试 | **B2** |
| §41 | item-stuck-failed-after-retry-timeout | **B3** |
| §39 | reconcile-snapshot-writeback（`reconcile_agent` 用快照回写状态） | **B3** |
| §39 | dual-startup-reconcilers（两个对账器同时处理同一目标） | **B3** |
| §45 / §47 | double-release-false-negative（身份检查假阴性时仍双重释放） | **B3** |
| §47 | force-double-release（force 不跳过存活 shim 时的双重释放残余） | won't-fix（已接受的取舍：§45 ① 的设计选择——用户定向 force 时不释放的代价是对账依据随记录消失，比重复释放更糟；B3 的假阴性修复不改变 force 路径） |
| §39 | daemon-link-stale-descriptor | **B4** |
| §39 | shim-daemon-zombie | **B4** |
| §39 | keepalive-zero | **B4** |
| §39 | tempgrant-drop-leak | **B4** |
| 验收 #7 遗留 + §41 | kill-orphan-single-pid + kill-orphan-two-step | **B4** |
| §39 | transcript-tail-traversal | **B5** |
| §39 | tail-no-legacy-fallback-midmigration | **B5** |
| §39 | mcp-sweep-snapshot-race | **B5** |
| §39 | mcp-write-atomic-fsync | **B5** |
| §41 | delete-loser-empty-dir / trash-never-swept / remove-dir-under-lock / quarantined-delete-says-missing | **B5** |
| §41 | legacy-degraded-permanent（降级永久、无恢复入口、横幅文案不符） | **B6** |
| §47 | release-results-hidden | **B7** |
| §47 | force-release-no-bound | **B7** |
| 验收「强制删除」注 | stale-binding-after-force-delete | **B7** |
| §39 | fe-resume-fail-reattach / fe-stopall-toast / fe-a11y-stopall | **B7** |
| §39 | housekeeping-before-single-instance | **B7** |
| §41 | test-env-var-parallel | **B8** |
| BENCH | F-BENCH-8 余项（capture 未过 sanitize_line） | **B8** |
| BENCH | F-BENCH-12 余项 | **B8**（只做核实与记账：`bench.rs` 的「上两级」在 Task 1 搬迁后已经正确——`crates/omt-harness` 上两级就是仓库根，fixtures 路径是 `../../fixtures`；改成「上三级」反而会错。`harness_probe.rs` 模块注释已不再提「MCP 握手」） |
| 日志 | 总览表补行 / 翻状态 / 关闭过期行 | **B8** |
| §39 | dock-quit-bypass（从 dock 退出、注销、关机绕过退出确认） | won't-fix（效果等同「继续在后台运行」，仅信息性） |
| §45 | run-on-main-thread-drop（`run_on_main_thread` 失败时插件丢回调，DIALOG_OPEN 卡住） | won't-fix（只有事件循环已退出后才可达；B7 改用 GCD 主队列后此路径也不再经过插件） |
| §39 | `LAUNCH_LOCK` 在整个 `wait_ready` 期间持有（最长 30s，派发被串行化） | **已由 Task A3（Q-6）覆盖**：跨进程派发锁在 `wait_ready` 之前释放 |
| §39 | follow：`Ready::Timeout` 后 `APPEAR_TIMEOUT` 先到期不再接回；webview 重载后第二个跟随线程空转 | **B7**（Step 6–7：exec 仍活跃时不因 APPEAR_TIMEOUT 放弃；桌面后端每个 exec 只保留一个跟随者，新的接管、旧的取消） |
| §41 | ② `node/execute` 先于校验（配置错误时节点留在 in_progress，改造前既有） | 后续项：CLI/MCP 发起已由 Q-2/A2 绕开 node/execute；桌面端派发仍先 node/execute，校验前置列入后续项 |
| daemon | janitor 未接线（过期租约不回收） | 后续项（不在本计划） |
| §17 | 孤儿 transcript 采纳 / pid 复用 cmdline 校验 / 从 transcript 重建损坏 agents.json | 后续项（增强性质而非缺陷 / 窄崩溃路径，不在 P3 清理范围；B6 只提供「确认并归档」出口，不做重建） |
| U1 | F-U1-4（omp 无 TTY 放行 flag 实测） | 已由 F-ACC-12 实测回填（`--approval-mode=yolo` + `-p`），B8 记 `superseded(F-ACC-12)` |
| U1 | F-U1-7（`probe_version` 丢弃 stderr） | **B2**（Step 8：stdout 首行为空时回落 stderr 首行；核实仍开放：`harness.rs::probe_version` 仍是 `.stderr(Stdio::null())`） |
| U12 | omp title 事件 → `Agent.title` | 后续项（增强性质而非缺陷，不在 P3 清理范围；核实仍开放：print-json 流无 title 事件，`AgentRecord.title` 无写入方，需 RPC 模式） |
| 验收 | ACC-14（只开 DSH、desktop 已关，shim 拉起 daemon 后全局 home 仍正确） | 待手工验收（非代码） |
| 验收 | ACC-6 UI 确认（「运行中 / 实时流接回 / 可停止」） | 待手工验收（非代码；B2 之后状态显示为 `running`，验收时一并确认） |

### Part B 任务一览

| Task | 交付 | 主要文件 |
|---|---|---|
| B1 | 缺 leaseToken 的 report 报 `lease-token-missing` | `omt-domain/src/runs.rs`、`omt-runtime/src/dispatch.rs` |
| B2 | 派发链与拉起：done ticket 文案、agent 进入 Running、拉起前停止、宽限期停止报错、launcher 补偿分支有测试；版本探测回落 stderr（F-U1-7） | `omt-exec/src/{agents,executor,shim,launch,control}.rs`、`omt-harness/src/harness.rs` |
| B3 | 租约释放与对账：终态 run 不释放、failed 残项补 retry、对账 compare-and-write、对账器串行（进程内排队、跨进程跳过）、假阴性按存活 | `omt-exec/src/{executor,control}.rs` |
| B4 | daemon_link 与 shim 健壮性：活 descriptor、daemon 回收、保活下限、授权泄漏、orphan 整组强杀 | `omt-exec/src/{daemon_link,shim,grants,executor}.rs` |
| B5 | 存储与安全：tail 防穿越与回落、MCP 清扫锁内快照与 fsync、删除四项 + 残留清扫 | `omt-exec/src/{executor,mcp_injection,agents}.rs` |
| B6 | 旧版 home 降级：结构化降级说明 + 「确认并归档」 | `omt-exec/src/agents.rs`、桌面 `agents.rs` / `lib.rs`、前端 |
| B7 | 桌面 UI：Esc 取消、释放结果展示（全局通知列表 + 前往 Runs）、强删总时限、删绑定、前端三项、清扫挪进 setup、跟随者不提前放弃 / 每 exec 唯一 | 桌面 `lib.rs` / `quit_dialog.rs` / `executor.rs`、`omt-exec/src/follow.rs`、前端 |
| B8 | 工具链、测试与记账 | `copy-sidecar.mjs`、测试注入、`sanitize.rs`、findings log |

---

### Task B1: 缺 leaseToken 的 run/report 单列规则（P3-ACC-2）

**Files:**
- Modify: `crates/omt-domain/src/runs.rs`（`authorize_report` 的 `ExecutorLease` 分支 + 文档注释）
- Modify: `crates/omt-runtime/src/dispatch.rs`（`run_report` 的 `None` 分支注释，行为不变）
- Test: `crates/omt-domain/tests/properties.rs`、`crates/omt-runtime/tests/run_plane_ops.rs`

**Interfaces:**
- Produces: 新的拒绝规则 `CONFLICT` + `details.rule = "lease-token-missing"`，message 为 `run/report requires leaseToken: pass the lease.token returned by run/claim (only an administrator may report without one)`。
- 判定顺序：`report-state-gate`（不在途）→ **`lease-token-missing`（新）** → `lease-stale` → `lease-attempt` → `lease-token` → `actor-mismatch`。管理员路径（`ReportAuthority::Administrator`）不受影响。
- 落点说明：不在 `dispatch.rs` 里提前返回，而是放进 `authorize_report`——这样 state gate 仍先判，规则也能由 domain 属性测试直接钉住。`dispatch.rs` 继续为非管理员传空 token，空串在 domain 里即「缺失」（合法 token 是 32 位 hex，不可能为空）。
- 核实过的影响面：`git grep "does not fence"` 只命中 `runs.rs` 本身与 findings log；`corpus/`、`schema/`、TS 侧都没有钉旧文案；`envelope_gates.rs` 的错误 token 用例带了非空 token，仍走 `lease-token`。

- [ ] **Step 1: 写失败测试（domain）**

在 `crates/omt-domain/tests/properties.rs` 的 `stale_or_mismatched_leases_are_denied_with_distinct_rules` 之后加：

```rust
/// P3-ACC-2：非管理员未带 leaseToken（dispatch 传入空串）单列规则
/// `lease-token-missing`，消息点名缺少的参数与来源——此前按空串与存储
/// token 比对，报 `lease-token`「presented token does not fence this item」，
/// 模型无从得知是漏传了参数（omp 首次 report 即撞上）。
#[test]
fn missing_token_is_denied_with_its_own_rule() {
    let rule_of = |problem: &omt_domain::error::Problem| {
        problem
            .details
            .as_ref()
            .and_then(|d| d.get("rule"))
            .and_then(|r| r.as_str())
            .map(str::to_string)
    };
    let no_token = ReportAuthority::ExecutorLease {
        token: String::new(),
        actor: "session-a".into(),
    };
    // 有活租约：缺 token 不再被当成「token 不匹配」
    let stored = Some(grant(1, 20_000));
    let problem = authorize_report(
        RunItemState::Running,
        1,
        stored.as_ref(),
        &no_token,
        10_000,
        Some("lease-session-a-1"),
    )
    .expect_err("missing token must be denied");
    assert_eq!(problem.code, "CONFLICT");
    assert_eq!(rule_of(&problem).as_deref(), Some("lease-token-missing"));
    assert!(
        problem.message.contains("leaseToken") && problem.message.contains("run/claim"),
        "消息必须点名参数与来源：{}",
        problem.message
    );
    // 无租约：缺 token 仍报缺 token（比 lease-stale 更可行动）
    let problem = authorize_report(RunItemState::Running, 1, None, &no_token, 10_000, None)
        .expect_err("missing token without grant must be denied");
    assert_eq!(rule_of(&problem).as_deref(), Some("lease-token-missing"));
    // 不在途：state gate 仍然优先
    let problem = authorize_report(
        RunItemState::Pending,
        1,
        stored.as_ref(),
        &no_token,
        10_000,
        Some("lease-session-a-1"),
    )
    .expect_err("pending item must be denied by the state gate");
    assert_eq!(rule_of(&problem).as_deref(), Some("report-state-gate"));
    // 管理员路径不受影响
    assert_eq!(
        authorize_report(
            RunItemState::Running,
            1,
            None,
            &ReportAuthority::Administrator {
                reason: "admin-grant".into()
            },
            10_000,
            None,
        )
        .unwrap(),
        ReportVerdict::AdminAuthorized
    );
}
```

```bash
cargo test -p omt-domain --test properties missing_token_is_denied_with_its_own_rule 2>&1 | tail -5
```
Expected：FAIL——第一处断言得到 `Some("lease-token")`。

- [ ] **Step 2: 写失败测试（真 daemon 端到端）**

在 `crates/omt-runtime/tests/run_plane_ops.rs` 末尾加：

```rust
/// P3-ACC-2（端到端）：非管理员、未带 leaseToken 的 run/report 返回
/// CONFLICT + rule `lease-token-missing`；带上 run/claim 返回的 lease.token
/// 即成功。
#[test]
fn report_without_lease_token_names_the_missing_parameter() {
    let (_ctx, mut proc, endpoint) = ready();
    let (mut client, cred) = connected_client(&endpoint, "cli").expect("client");
    let epic = client
        .call(
            "node/create",
            authed(json!({ "type": "epic", "title": "token epic" }), &cred),
        )
        .expect("epic");
    let story = client
        .call(
            "node/create",
            authed(
                json!({ "type": "story", "title": "token story", "parentId": epic["node"]["nodeId"] }),
                &cred,
            ),
        )
        .expect("story");
    let ticket = client
        .call(
            "node/create",
            authed(
                json!({ "type": "ticket", "title": "token ticket", "parentId": story["node"]["nodeId"] }),
                &cred,
            ),
        )
        .expect("ticket");
    let ticket_id = ticket["node"]["nodeId"].as_str().unwrap().to_string();
    let run = client
        .call(
            "run/create",
            authed(json!({ "nodeIds": [&ticket_id], "config": {} }), &cred),
        )
        .expect("run");
    let run_id = run["run"]["runId"].as_str().unwrap().to_string();
    client
        .call(
            "run/control",
            authed(json!({ "runId": run_id, "action": "start" }), &cred),
        )
        .expect("start");
    let claim = client
        .call("run/claim", authed(json!({ "runId": run_id }), &cred))
        .expect("claim");
    assert_eq!(claim["item"]["nodeId"], json!(ticket_id));
    let token = claim["lease"]["token"].as_str().expect("lease.token").to_string();

    let err = client
        .call(
            "run/report",
            authed(
                json!({ "runId": run_id, "nodeId": ticket_id, "outcome": "done" }),
                &cred,
            ),
        )
        .unwrap_err();
    match &err {
        RpcError::Problem { code, details } => {
            assert_eq!(code, "CONFLICT", "{err:?}");
            assert_eq!(details["rule"], json!("lease-token-missing"), "{err:?}");
        }
        other => panic!("expected a Problem, got {other:?}"),
    }

    client
        .call(
            "run/report",
            authed(
                json!({ "runId": run_id, "nodeId": ticket_id, "outcome": "done", "leaseToken": token }),
                &cred,
            ),
        )
        .expect("report with the claim's lease.token");

    proc.kill();
}
```

```bash
cargo test -p omt-runtime --test run_plane_ops report_without_lease_token 2>&1 | tail -5
```
Expected：FAIL——`details["rule"]` 为 `"lease-token"`。

- [ ] **Step 3: 实现**

`crates/omt-domain/src/runs.rs`，`authorize_report` 的文档注释把规则列表改为：

```rust
/// Denial rules (each a structured CONFLICT with a `rule` detail so the
/// registry stays untouched):
/// - not in flight → `report-state-gate`;
/// - administrator override → allowed regardless of lease state, returning
///   [`ReportVerdict::AdminAuthorized`] so callers can audit it distinctly;
/// - executor presented no token (empty) → `lease-token-missing` (P3-ACC-2:
///   names the missing `leaseToken` parameter instead of a fence mismatch);
/// - missing/expired grant → `lease-stale`;
/// - wrong attempt (stale attempt-one token against attempt-two work) →
///   `lease-attempt`;
/// - wrong token → `lease-token`;
/// - foreign actor namespace → `actor-mismatch`.
```

`ExecutorLease` 分支开头（`let Some(grant) = grant else {` 之前）插入：

```rust
        ReportAuthority::ExecutorLease { token, actor } => {
            // P3-ACC-2：非管理员没带 leaseToken 时 dispatch 传入空串——单列
            // 规则并点名参数来源，不再按空串与存储 token 比对后报「token
            // 不匹配」。合法 token 恒为非空，空串只可能是「没传」。
            if token.is_empty() {
                return Err(conflict_rule(
                    "lease-token-missing",
                    "run/report requires leaseToken: pass the lease.token returned by run/claim (only an administrator may report without one)",
                ));
            }
            let Some(grant) = grant else {
```

`crates/omt-runtime/src/dispatch.rs` 的 `run_report`，`None` 分支的非管理员臂只改注释：

```rust
            } else {
                // P3-ACC-2：空 token = 未传 leaseToken——authorize_report 据此
                // 报 `lease-token-missing`（点名参数来源），而不是 token 不匹配。
                omt_domain::runs::ReportAuthority::ExecutorLease {
                    token: String::new(),
                    actor: ctx.auth.actor_namespace.clone(),
                }
            }
```

`crates/omt-runtime/README.md` 第 185 行的规则清单补上新规则：

```markdown
  (`lease-stale`/`lease-attempt`/`lease-token`/`lease-token-missing`/`actor-mismatch`/
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p omt-domain --test properties 2>&1 | grep -E '^test result|FAILED'
cargo test -p omt-runtime --test run_plane_ops 2>&1 | grep -E '^test result|FAILED'
cargo test -p omt-runtime --test coverage 2>&1 | grep -E '^test result|FAILED'
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
cargo build -p omt-runtime --bin omt-daemon && nm -a target/debug/omt-daemon | grep -c -E 'omt_exec|omt_harness'
```
Expected：三组测试全部 `0 failed`（coverage 里 run/report 仍是 semantic 拒绝，只是规则名变了）；fmt / clippy 干净；`nm` 计数为 `0`（KD-E9）。

- [ ] **Step 5: 提交**

```bash
git add crates/omt-domain crates/omt-runtime/src/dispatch.rs crates/omt-runtime/tests/run_plane_ops.rs crates/omt-runtime/README.md
git commit -m "fix(daemon): name the missing leaseToken on run/report (P3-ACC-2)

A non-admin report without leaseToken used to be fenced as an empty
token and rejected with 'presented token does not fence this item'.
authorize_report now denies it with rule lease-token-missing and a
message pointing at run/claim's lease.token; state gate still wins.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B2: 派发链与拉起（P3-ACC-4 / P3-ACC-5 / §41 拉起前停止 / §39 宽限期停止 / §41 launcher 补偿分支）

**Files:**
- Modify: `crates/omt-exec/src/agents.rs`（新增 `update_agent`、`mark_exec_spawned`）
- Modify: `crates/omt-exec/src/executor.rs`（`dispatch_daemon_side` 预检文案；`spawn_inner` 写 pid 改走 `mark_exec_spawned`）
- Modify: `crates/omt-exec/src/shim.rs`（`ShimOptions` / `execute_with`；spawn 前检查停止标志）
- Modify: `crates/omt-exec/src/launch.rs`（抽出 `record_shim_identity`；`Ready::Stopped`）
- Modify: `crates/omt-exec/src/control.rs`（`stop_exec_signaled` 宽限期报错）
- Modify: `crates/omt-runtime/tests/exec_launch.rs`（launcher 返回时状态改为 Running）
- Modify: `crates/omt-harness/src/harness.rs`（`probe_version` 回落 stderr，F-U1-7）
- Test: 各文件的 `mod tests`

**Interfaces:**
- Produces: `pub fn agents::update_agent(runtime_dir: &Path, home_id: &str, agent_id: &str, apply: impl FnOnce(&mut AgentRecord) -> bool) -> Result<bool, String>`——单事务 compare-and-write 的公共入口（B3 / B4 复用）。
- Produces: `pub fn agents::mark_exec_spawned(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str, pid: u32, process_start_id: Option<String>) -> Result<bool, String>`。
- Produces: `pub(crate) struct shim::ShimOptions<'a> { pub stop: &'a AtomicBool, pub lease_timeout: Duration }` 与 `pub(crate) fn shim::execute_with(..., opts: ShimOptions<'_>) -> i32`；`shim::execute` / `shim::run` 签名不变（B8 把单测切到 `execute_with`）。
- 行为变化：harness spawn 成功即 `status = running`（此前执行期间一直是 `starting`）；`exec_active` / `is_active` / 前端分组本来就把 Running 与 Starting 同等对待（`control.rs::is_active`、`AgentsPanel.tsx::groupAgents`、`TranscriptView.tsx:334`），无需改动。
- 行为变化：启动宽限期内（shim 还没记下 pid）`stop_exec` 返回 `Err("执行仍在拉起中，请稍后再停止")`，`stop_all` 把它记入 `errors`；A4 的 `local::exec_stop` 原样透传为 `IO` 错误。
- 行为变化（F-U1-7）：`omt_harness::harness::probe_version` 同时读 stdout 与 stderr；stdout 首行为空时回落 stderr 首行（把版本号打到 stderr 的 CLI 不再被判「待验证」）。stdout 有内容时行为不变。签名不变。

- [ ] **Step 1: 写失败测试**

`crates/omt-exec/src/agents.rs` 的 `mod tests` 加：

```rust
    /// P3-ACC-5：harness spawn 后同一事务写 pid / 起始身份并把 Starting
    /// 推进到 Running；终态不回退，非最新 exec 不推进。
    #[test]
    fn mark_exec_spawned_promotes_starting_to_running_once() {
        let (_dir, runtime, home) = temp_runtime();
        let agent = register_agent(&runtime, &home, "pi", new_exec("exec-run")).unwrap();
        assert!(mark_exec_spawned(
            &runtime,
            &home,
            &agent.id,
            "exec-run",
            4242,
            Some("macos:1:1".into())
        )
        .unwrap());
        let rec = get_agent(&runtime, &home, &agent.id).unwrap().unwrap();
        assert_eq!(rec.status, AgentRecordStatus::Running);
        assert_eq!(rec.execs[0].pid, Some(4242));
        assert_eq!(rec.execs[0].process_start_id.as_deref(), Some("macos:1:1"));
        // 终态不回退
        update_agent_status(
            &runtime,
            &home,
            &agent.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        mark_exec_spawned(&runtime, &home, &agent.id, "exec-run", 4242, None).unwrap();
        let rec = get_agent(&runtime, &home, &agent.id).unwrap().unwrap();
        assert_eq!(rec.status, AgentRecordStatus::Exited, "终态不得被推回 Running");
    }
```

`crates/omt-exec/src/executor.rs` 的 `mod tests`（`collect_members_walks_subtree_in_daemon_view_shape` 之后）加：

```rust
    /// P3-ACC-4：派发 / 续跑的目标本身是已完成的 ticket 时，报错直说是它
    /// 完成了（续跑沿用最后一个 exec 的 nodeId，旧文案「无未完成可执行成员」
    /// 让人以为子树出了问题）；且在 ② node/execute 之前返回。非 ticket 目标
    /// 子树全部完成时保留原文案。
    #[test]
    fn dispatch_on_done_ticket_explains_it_is_done() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let fake = FakeDaemon::new(
            node_get_shape("t1", "ticket", "done", false, &[]),
            dispatch_run_get("running"),
            "run-unused",
        );
        let rpc = |method: &str, params: serde_json::Value| fake.rpc(method, params);
        let err = dispatch_daemon_side(
            &rpc,
            &runtime,
            "h_test",
            LaunchTarget::Node("t1"),
            LaunchOrigin::Desktop,
            "pi",
            false,
            None,
            None,
            Some("agent-x"),
            None,
        )
        .expect_err("done ticket 不可派发");
        assert_eq!(
            err,
            "关联的 ticket t1 已完成（done），无需续跑；如需重做请先将其改回 open"
        );
        assert!(
            !fake.method_names().iter().any(|m| m == "node/execute"),
            "预检失败必须先于 ②: {:?}",
            fake.method_names()
        );

        let epic = FakeDaemon::new(
            node_get_shape("e1", "epic", "open", false, &[]),
            dispatch_run_get("running"),
            "run-unused",
        );
        let rpc = |method: &str, params: serde_json::Value| epic.rpc(method, params);
        let err = dispatch_daemon_side(
            &rpc,
            &runtime,
            "h_test",
            LaunchTarget::Node("e1"),
            LaunchOrigin::Desktop,
            "pi",
            false,
            None,
            None,
            None,
            None,
        )
        .expect_err("空子树不可派发");
        assert_eq!(err, "无未完成可执行成员");
    }
```

`crates/omt-exec/src/shim.rs` 的 `mod tests` 加（`rig` / `recording_leaser` / `idle_keepalive` 已存在）：

```rust
    /// §41：停止请求先于 harness spawn 到达（launcher 身份落档失败后的
    /// SIGTERM，或用户在拉起窗口里停止）——不得再拉起 harness；仍活跃的
    /// agent 落 Killed(user)，MCP 注入已释放，jsonl 从未创建。
    #[test]
    fn stop_requested_before_spawn_does_not_launch_harness() {
        let (_t, runtime, spec) = rig("echo SHOULD-NOT-RUN\nexit 0", Some("RUN-1"));
        let stop = AtomicBool::new(true);
        let code = execute_with(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            recording_leaser(Default::default(), &runtime),
            idle_keepalive(),
            ShimOptions {
                stop: &stop,
                lease_timeout: Duration::from_secs(5),
            },
        );
        assert_eq!(code, 0);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(agent.status, crate::agents::AgentRecordStatus::Killed);
        assert_eq!(agent.execs[0].kill_reason.as_deref(), Some("user"));
        assert!(agent.execs[0].pid.is_none(), "harness 不得被拉起");
        let dir = crate::layout::agent_dir(&runtime, "h_test", &spec.agent_id).unwrap();
        assert!(!crate::layout::exec_file(&dir, "exec-s", crate::layout::ExecFile::Jsonl).exists());
    }

    /// §41：launcher 已经 fail_and_compensate 落了 Failed——拉起前停止只退出，
    /// 不把 Failed 覆盖成 Killed。
    #[test]
    fn stop_before_spawn_keeps_launcher_failed_status() {
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        crate::agents::update_agent_status(
            &runtime,
            "h_test",
            &spec.agent_id,
            crate::agents::AgentRecordStatus::Failed,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        let stop = AtomicBool::new(true);
        execute_with(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            recording_leaser(Default::default(), &runtime),
            idle_keepalive(),
            ShimOptions {
                stop: &stop,
                lease_timeout: Duration::from_secs(5),
            },
        );
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(agent.status, crate::agents::AgentRecordStatus::Failed);
        assert!(agent.execs[0].kill_reason.is_none());
    }
```

`crates/omt-exec/src/launch.rs` 的 `mod tests` 加：

```rust
    fn sleeper() -> std::process::Child {
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap()
    }

    fn terminated_by_sigterm(child: &mut std::process::Child) -> bool {
        use std::os::unix::process::ExitStatusExt;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.signal() == Some(libc::SIGTERM);
            }
            if Instant::now() >= deadline {
                unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
                let _ = child.wait();
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// §41（F-EXEC-2 的补偿分支）：读 shim 起始身份失败（含重试一次）→
    /// SIGTERM 刚拉起的 shim 并返回 Err（调用方随即走 fail_and_compensate）。
    #[test]
    fn identity_read_failure_terminates_the_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let agent = register(tmp.path(), "exec-idfail");
        let mut shim = sleeper();
        let failing = |_: u32| -> Result<String, String> { Err("proc_pidinfo: EPERM".into()) };
        let err = record_shim_identity(tmp.path(), "h_t", &agent.id, "exec-idfail", shim.id(), &failing)
            .expect_err("身份读不出必须失败");
        assert!(err.contains("读取 exec-shim 起始身份失败"), "{err}");
        assert!(terminated_by_sigterm(&mut shim), "shim 必须被 SIGTERM");
    }

    /// §41：身份写不进记录（记录已不存在——update_exec 得 Ok(false)）同样
    /// SIGTERM shim 并返回 Err；shim 已自写了同一身份（Ok(false) 但记录一致）
    /// 则视为成功、不发信号。
    #[test]
    fn identity_write_failure_terminates_the_shim_but_self_written_identity_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let mut shim = sleeper();
        let real = |pid: u32| crate::executor::process_start_identity(pid);
        let err = record_shim_identity(tmp.path(), "h_t", "agent-gone", "exec-gone", shim.id(), &real)
            .expect_err("记录不存在必须失败");
        assert!(err.contains("记录 exec-shim 身份失败"), "{err}");
        assert!(terminated_by_sigterm(&mut shim));

        let agent = register(tmp.path(), "exec-self");
        let mut shim = sleeper();
        let start = crate::executor::process_start_identity(shim.id()).unwrap();
        agents::update_exec(tmp.path(), "h_t", &agent.id, "exec-self", |e| {
            e.shim_pid = Some(shim.id());
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        record_shim_identity(tmp.path(), "h_t", &agent.id, "exec-self", shim.id(), &real)
            .expect("shim 已自写同一身份 = 成功");
        assert!(shim.try_wait().unwrap().is_none(), "成功路径不得发信号");
        unsafe { libc::kill(shim.id() as i32, libc::SIGKILL) };
        let _ = shim.wait();
    }
```

`crates/omt-exec/src/control.rs` 的 `mod tests` 加：

```rust
    /// §39 grace-stop-noop：拉起宽限期内（shim 还没记下 pid）没有可发信号
    /// 的目标——必须如实报错，而不是返回 Ok 让界面显示「已停止」。
    #[test]
    fn stop_during_launch_grace_reports_not_yet_stoppable() {
        let tmp = tempfile::tempdir().unwrap();
        let id = reg(tmp.path(), "exec-fresh");
        let err = stop_exec(tmp.path(), "exec-fresh").expect_err("宽限期内不得假装停止成功");
        assert!(err.contains("仍在拉起中"), "{err}");
        let a = crate::agents::get_agent(tmp.path(), "h_t", &id).unwrap().unwrap();
        assert_eq!(a.status, AgentRecordStatus::Starting, "记录不被改写");
        let outcome = stop_all(tmp.path(), "h_t").unwrap();
        assert_eq!(outcome.signaled_count, 0);
        assert_eq!(outcome.errors.len(), 1, "{outcome:?}");
        assert!(outcome.errors[0].contains("exec-fresh"), "{outcome:?}");
    }
```

```bash
cargo test -p omt-exec -- mark_exec_spawned dispatch_on_done_ticket stop_requested_before_spawn stop_before_spawn_keeps identity_ stop_during_launch_grace 2>&1 | tail -5
```
Expected：编译失败（`mark_exec_spawned`、`execute_with`、`ShimOptions`、`record_shim_identity` 未定义）。

- [ ] **Step 2: agents.rs——`update_agent` 与 `mark_exec_spawned`**

在 `update_exec` 之后加：

```rust
/// 单事务读改写的公共入口（P3：compare-and-write）。`apply` 在 agent.json
/// 的进程内锁 + flock 内拿到整条记录，可先复核当前状态再改；返回 true
/// 才落盘。agent.json 不存在 / id 不符 → Ok(false)。
pub fn update_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    apply: impl FnOnce(&mut AgentRecord) -> bool,
) -> Result<bool, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Ok(false);
    }
    update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id {
            return false;
        }
        apply(agent)
    })
    .map(|(_, outcome)| outcome.changed)
}

/// P3-ACC-5：harness 已 spawn——同一事务写入 harness pid / 起始身份，并把
/// agent 从 Starting 推进到 Running（此前执行器没有任何写 Running 的路径，
/// 执行期间 agent.json 一直是 starting）。只在 agent 仍是 Starting 且该
/// exec 是最新 exec 时推进：终态与旧 exec 都不回退。返回是否落盘。
pub fn mark_exec_spawned(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    pid: u32,
    process_start_id: Option<String>,
) -> Result<bool, String> {
    update_agent(runtime_dir, home_id, agent_id, |agent| {
        let Some(exec) = agent.execs.iter_mut().find(|e| e.exec_id == exec_id) else {
            return false;
        };
        exec.pid = Some(pid);
        exec.process_start_id = process_start_id;
        if agent.status == AgentRecordStatus::Starting
            && agent.last_exec_id.as_deref() == Some(exec_id)
        {
            agent.status = AgentRecordStatus::Running;
        }
        true
    })
}
```

- [ ] **Step 3: executor.rs——预检文案与 Running**

`dispatch_daemon_side` 的 ① 预检在 A2 之后位于 `match target` 的 `LaunchTarget::Node(node_id)` 分支里（`node_resp` 只在这个分支里有效）；把该分支里的

```rust
            let members = collect_members(rpc, home_id, node_id)?;
            if members.is_empty() {
                return Err("无未完成可执行成员".into());
            }
```

改为：

```rust
            let members = collect_members(rpc, home_id, node_id)?;
            if members.is_empty() {
                // P3-ACC-4：目标本身是已完成 / 已跳过的 ticket 时直说
                return Err(no_members_error(node_id, &node_resp["node"]));
            }
```

`LaunchTarget::Run` 分支不变（不收集成员）。

在 `collect_members` 之后加：

```rust
/// P3-ACC-4：「无未完成可执行成员」的可行动说明——派发 / 续跑目标本身就是
/// 已完成（或已跳过）的 ticket 时直说是它完成了；其余情形保留原文案。
fn no_members_error(node_id: &str, node: &serde_json::Value) -> String {
    let node_type = node["type"].as_str().unwrap_or("");
    let status = node["status"].as_str().unwrap_or("");
    if matches!(node_type, "ticket" | "subticket") {
        match status {
            "done" => {
                return format!(
                    "关联的 ticket {node_id} 已完成（done），无需续跑；如需重做请先将其改回 open"
                )
            }
            "skipped" => {
                return format!(
                    "关联的 ticket {node_id} 已跳过（skipped），无需续跑；如需重做请先将其改回 open"
                )
            }
            _ => {}
        }
    }
    "无未完成可执行成员".into()
}
```

`ExecutionManager::spawn_inner` 中，把

```rust
        let process_start_id = process_start_identity(pid).ok();
        let _ = agents::update_exec(
            runtime_dir,
            &req.home_id,
            &req.agent_id,
            &req.exec_id,
            |exec| {
                exec.pid = Some(pid);
                exec.process_start_id.clone_from(&process_start_id);
            },
        );
```

替换为：

```rust
        let process_start_id = process_start_identity(pid).ok();
        // P3-ACC-5：pid / 起始身份与 Starting → Running 同一事务落盘——launcher
        // 的就绪握手看到 pid 时状态已是 Running
        let _ = agents::mark_exec_spawned(
            runtime_dir,
            &req.home_id,
            &req.agent_id,
            &req.exec_id,
            pid,
            process_start_id,
        );
```

- [ ] **Step 4: shim.rs——`ShimOptions` 与 spawn 前停止**

在 `install_stop_handler` 之后加：

```rust
/// 测试缝（§41 / §41 test-env-var-parallel）：停止标志与租约释放上限从参数
/// 注入——单测不再改进程级静态量或环境变量。生产入口 `execute` 用
/// `STOP_REQUESTED` + `lease_timeout()`。
pub(crate) struct ShimOptions<'a> {
    pub stop: &'a AtomicBool,
    pub lease_timeout: Duration,
}
```

`execute` 改为委托 `execute_with`：

```rust
/// shim 主体（不装信号处理，单测直接调用）；0 = 走完收尾，1 = 启动阶段失败。
pub fn execute(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Leaser,
    keepalive: Connector,
) -> i32 {
    execute_with(
        runtime_dir,
        home_id,
        agent_id,
        exec_id,
        lease,
        keepalive,
        ShimOptions {
            stop: &STOP_REQUESTED,
            lease_timeout: lease_timeout(),
        },
    )
}

pub(crate) fn execute_with(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Leaser,
    keepalive: Connector,
    opts: ShimOptions<'_>,
) -> i32 {
    match run_inner(runtime_dir, home_id, agent_id, exec_id, lease, keepalive, &opts) {
        Ok(()) => 0,
        Err(detail) => {
            eprintln!("exec-shim {exec_id}: {detail}");
            let _ = agents::update_exec(runtime_dir, home_id, agent_id, exec_id, |exec| {
                if exec.failure_detail.is_none() {
                    exec.failure_detail = Some(detail.clone());
                }
            });
            let _ = agents::update_agent_status(
                runtime_dir,
                home_id,
                agent_id,
                AgentRecordStatus::Failed,
                AgentAttachment::None,
            );
            1
        }
    }
}
```

在 `write_own_identity` 之后加：

```rust
/// §41：停止请求先于 harness spawn 到达——不再拉起 harness。只在 agent 仍处
/// 活跃态时落 Killed(user)；launcher 已经 fail_and_compensate 落了 Failed 的
/// 保持 Failed（此前 spawn 照常进行，最终 Killed 覆盖 Failed）。
fn finish_stopped_before_spawn(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str) {
    let _ = agents::update_agent(runtime_dir, home_id, agent_id, |agent| {
        if !matches!(
            agent.status,
            AgentRecordStatus::Starting | AgentRecordStatus::Running
        ) {
            return false;
        }
        agent.status = AgentRecordStatus::Killed;
        agent.attachment = AgentAttachment::None;
        if let Some(exec) = agent.execs.iter_mut().find(|e| e.exec_id == exec_id) {
            exec.kill_reason = Some(crate::executor::KillReason::User.key());
        }
        true
    });
}
```

`run_inner` 签名加 `opts: &ShimOptions<'_>`，并做四处修改：

```rust
fn run_inner(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Leaser,
    keepalive: Connector,
    opts: &ShimOptions<'_>,
) -> Result<(), String> {
```

（1）`let env_additions = plan.sanitized_env();` 之后、`mcp_injection::prepare` 之前：

```rust
    // §41：launcher 的 SIGTERM（身份落档失败）可能在这里之前就到了——不做
    // MCP 注入、不拉起 harness
    if opts.stop.load(Ordering::SeqCst) {
        finish_stopped_before_spawn(runtime_dir, home_id, agent_id, exec_id);
        return Ok(());
    }
```

（2）`manager.set_lease_release(lease_releaser(...))` 的调用加上界参数：

```rust
    manager.set_lease_release(lease_releaser(
        runtime_dir.to_path_buf(),
        spec.home_path.clone(),
        lease,
        opts.lease_timeout,
    ));
```

（3）`let session = manager.spawn(...)?;` 之前：

```rust
    // §41：MCP 注入与 prompt 落盘期间到达的停止——释放注入后退出，不 spawn
    if opts.stop.load(Ordering::SeqCst) {
        let _ = crate::mcp_injection::release(runtime_dir, &request.mcp);
        finish_stopped_before_spawn(runtime_dir, home_id, agent_id, exec_id);
        return Ok(());
    }
    // 事件已由执行器写入 jsonl（KD-E3）；进程内 sink 无人消费
    let session = manager.spawn(runtime_dir, request, Arc::new(|_| {}))?;
```

（4）主循环里的 `STOP_REQUESTED.swap(false, Ordering::SeqCst)` 改为 `opts.stop.swap(false, Ordering::SeqCst)`。

`lease_releaser` 加参数 `timeout: Duration`，函数体里 `run_bounded(bounded, lease_timeout())` 改为 `run_bounded(bounded, timeout)`：

```rust
fn lease_releaser(
    runtime_dir: PathBuf,
    home_path: PathBuf,
    leaser: Leaser,
    timeout: Duration,
) -> crate::executor::LeaseRelease {
```

- [ ] **Step 5: launch.rs——抽出 `record_shim_identity`，加 `Ready::Stopped`**

文件头的 `use crate::executor::{...}` 加上 `compensate_new_run`（A2/A3 之后该行的其余条目保持原样）：

```rust
use crate::executor::{
    compensate_new_run, dispatch_daemon_side, fail_and_compensate, DaemonRpc, ExecStartOutcome,
};
```

`enum Ready` 加一个变体：

```rust
enum Ready {
    Spawned,
    Failed(String),
    /// §41：harness 拉起前 agent 已被停止（Killed）——只补偿 run，不落 Failed
    Stopped,
    Timeout,
}
```

`start()` 中从 `let shim_start_id = match ...` 到身份写入 `if let Err(err) = agents::update_exec(...)` 结束的整段替换为下面的调用。位置要求（A3 之后）：它仍在 A3 的 `drop(launch_lock);` **之前**——身份落档受跨进程派发锁保护，与并发计数同在锁内；`drop(launch_lock);` 与其后的 `wait_ready` 保持原位不动：

```rust
    if let Err(err) = record_shim_identity(
        runtime_dir,
        home_id,
        &plan.agent.id,
        &plan.exec_id,
        shim_pid,
        &crate::executor::process_start_identity,
    ) {
        return Err(compensate(err));
    }
    // ↓ 紧接着就是 A3 已有的 `drop(launch_lock);`，然后是 ⑨ wait_ready（都不动）
```

`match wait_ready(...)` 加分支：

```rust
        Ready::Stopped => {
            compensate_new_run(rpc, home_id, &plan.run_id, plan.run_was_newly_created);
            return Err("执行在 harness 拉起前已被停止".into());
        }
```

在 `shim_reported_dead` 之前加：

```rust
/// ⑧ 后半（F-EXEC-2 / §41）：记录 shim 身份。读取起始身份失败（重试一次，
/// 覆盖 spawn 与 exec 之间的瞬时窗口）或写不进记录，都 SIGTERM 刚拉起的
/// shim 并返回 Err，由调用方走补偿表。`update_exec` 得 Ok(false) 有两种
/// 含义：shim 已自写了同一身份（成功），或记录不存在（失败）——落档后复核
/// 区分。`read_identity` 是测试缝（生产 = `executor::process_start_identity`）。
fn record_shim_identity(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    shim_pid: u32,
    read_identity: &dyn Fn(u32) -> Result<String, String>,
) -> Result<(), String> {
    let start_id = match read_identity(shim_pid)
        .or_else(|first| read_identity(shim_pid).map_err(|_| first))
    {
        Ok(id) => id,
        Err(err) => {
            terminate_shim(shim_pid);
            return Err(format!("读取 exec-shim 起始身份失败: {err}"));
        }
    };
    let written = agents::update_exec(runtime_dir, home_id, agent_id, exec_id, |e| {
        e.shim_pid = Some(shim_pid);
        e.shim_start_id = Some(start_id.clone());
    });
    let recorded = match written {
        Ok(true) => true,
        Ok(false) => agents::get_agent(runtime_dir, home_id, agent_id)
            .ok()
            .flatten()
            .and_then(|agent| agent.execs.into_iter().find(|e| e.exec_id == exec_id))
            .is_some_and(|e| {
                e.shim_pid == Some(shim_pid) && e.shim_start_id.as_deref() == Some(start_id.as_str())
            }),
        Err(_) => false,
    };
    if !recorded {
        terminate_shim(shim_pid);
        let detail = match written {
            Err(err) => err,
            _ => "exec 记录不存在".to_string(),
        };
        return Err(format!("记录 exec-shim 身份失败: {detail}"));
    }
    Ok(())
}
```

`wait_ready` 里 `if agent.status == AgentRecordStatus::Failed {` 之前加：

```rust
                // §41：拉起前被停止（shim 落了 Killed、没有 pid）
                if agent.status == AgentRecordStatus::Killed && exec.pid.is_none() {
                    return Ready::Stopped;
                }
```

- [ ] **Step 6: control.rs——宽限期停止报错**

`stop_exec_signaled` 里 `if shim_alive(&loc.exec) { ... }` 之后、`reconcile_agent(...)` 之前加：

```rust
    // §39 grace-stop-noop：拉起宽限期内（shim 还没记下身份）没有可发信号的
    // 目标，对账也会跳过它——此前直接返回 Ok，界面显示「已停止」而实际什么
    // 也没停
    if within_launch_grace(&loc.exec) {
        return Err("执行仍在拉起中，请稍后再停止".to_string());
    }
```

- [ ] **Step 7: exec_launch 端到端改断言**

`crates/omt-runtime/tests/exec_launch.rs` 的 `launch_returns_once_harness_is_spawned_and_exec_finishes_later`：

```rust
    assert_eq!(
        at_return.status,
        omt_exec::agents::AgentRecordStatus::Running,
        "launcher 不应等执行结束；harness 已 spawn 即 Running（P3-ACC-5）"
    );
```

- [ ] **Step 8: F-U1-7——版本探测回落 stderr**

先写失败测试。`crates/omt-harness/src/harness.rs` 的 `mod tests`（`probe_version_times_out_hanging_binary` 之后）加：

```rust
    /// F-U1-7：把版本号打到 stderr 的 CLI（stdout 为空）——回落 stderr 首行，
    /// 不再被判「待验证」；stdout 有内容时仍取 stdout（stderr 噪声不干扰）。
    #[cfg(unix)]
    #[test]
    fn probe_version_falls_back_to_stderr_when_stdout_is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stderr_only = dir.path().join("stderr-only");
        std::fs::write(&stderr_only, "#!/bin/sh\necho 'tool 4.5.6' >&2\n").unwrap();
        std::fs::set_permissions(&stderr_only, std::fs::Permissions::from_mode(0o755)).unwrap();
        // F-BENCH-22：现写脚本首跑可能有 exec 评估停顿，预算放宽到 20s
        assert_eq!(
            probe_version(&stderr_only, &["--version"], 20).as_deref(),
            Some("tool 4.5.6")
        );
        let both = dir.path().join("both");
        std::fs::write(&both, "#!/bin/sh\necho v1.0.0\necho 'warning: noise' >&2\n").unwrap();
        std::fs::set_permissions(&both, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(probe_version(&both, &["--version"], 20).as_deref(), Some("v1.0.0"));
    }
```

```bash
cargo test -p omt-harness probe_version_falls_back_to_stderr 2>&1 | tail -3
```
Expected：FAIL——第一个断言得到 `None`（stderr 被丢弃）。

`probe_version` 替换为：

```rust
pub fn probe_version(binary: &Path, args: &[&str], timeout_secs: u64) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary)
        .args(args)
        .stdout(Stdio::piped())
        // F-U1-7：stderr 也读——有的 CLI 把版本号打到 stderr
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .env("PATH", augmented_path())
        .spawn()
        .ok()?;
    // 两路各起一个读线程（先于等待退出——F-BENCH-21：退出后才读会在大输出时卡死）
    let read_all = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    };
    let out_reader = read_all(Box::new(child.stdout.take()?));
    let err_reader = read_all(Box::new(child.stderr.take()?));
    let first_line = |text: String| {
        text.lines()
            .next()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let line = loop {
        match child.try_wait() {
            // 进程已退出：两路管道落 EOF，读线程随即结束。stdout 首行优先，
            // 为空时回落 stderr 首行（F-U1-7）
            Ok(Some(_)) => {
                let out = out_reader.join().ok().and_then(first_line);
                break out.or_else(|| err_reader.join().ok().and_then(first_line));
            }
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    // 超时路径先 kill 再返回——读线程随管道 EOF 收尾，不悬挂。
    let _ = child.kill();
    let _ = child.wait();
    line
}
```

函数上方的文档注释（若提到「只读 stdout」）同步改为「stdout 首行优先，空则回落 stderr 首行（F-U1-7）」。

```bash
cargo test -p omt-harness probe_version 2>&1 | grep -E '^test result|FAILED'
```
Expected：`probe_version_times_out_hanging_binary` 与新测试都通过（hanging 脚本超时后 kill，两路读线程随 EOF 退出）。

- [ ] **Step 9: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
cargo test -p omt-harness 2>&1 | grep -E '^test result|FAILED'
cargo test -p omt-runtime --test exec_launch --test exec_shim --test exec_survives_launcher 2>&1 | grep -E '^test result|FAILED'
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```
Expected：全部 `0 failed`；fmt / clippy 干净。

- [ ] **Step 10: 提交**

```bash
git add crates/omt-exec crates/omt-harness/src/harness.rs crates/omt-runtime/tests/exec_launch.rs
git commit -m "fix(exec): launch-path P3s — done-ticket message, Running status, stop before spawn

- P3-ACC-4: dispatching a done/skipped ticket says so instead of
  'no executable members'.
- P3-ACC-5: the harness pid write also moves the agent Starting -> Running
  in one transaction.
- §41: a stop that arrives before spawn no longer launches the harness;
  a launcher-written Failed is not overwritten by Killed.
- §39: stopping inside the launch grace window reports an error instead
  of a silent success.
- §41: the launcher's identity-failure SIGTERM + compensation branch is
  extracted (record_shim_identity) and tested.
- F-U1-7: probe_version falls back to the first stderr line when stdout
  is empty.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B3: 租约释放与对账（P3-ACC-8 / §41 failed 残项 / §39 快照回写 / §39 双对账器 / 假阴性）

**Files:**
- Modify: `crates/omt-exec/src/executor.rs`（`release_items_for_exec`、新增 `held_failed_by_release` / `release_note` / `reconcile_single_flight`、`target_shim_alive`、两个对账入口）
- Modify: `crates/omt-exec/src/control.rs`（`reconcile_agent` 改为锁内 compare-and-write；新增 `shim_maybe_alive`）
- Test: 两文件的 `mod tests`

**Interfaces:**
- Consumes: B2 的 `agents::update_agent`。
- Produces: `pub fn control::shim_maybe_alive(exec: &ExecRecord) -> bool`——对账「是否跳过」用的保守判定（未知 = 存活）。
- 行为变化：
  - `release_items_for_exec` 在 run 为 `canceled` / `completed` 时直接 Ok（不 report、不 retry）。`completed_with_failures` **不在此列**：该状态只能经逐项 retry 重开，下面的 failed 残项正需要 retry。
  - 本 exec 的 item 若处于 `failed` 且 `lastError` 恰为释放说明 `exec <id> terminated (kill/app-exit)`（上次释放 report 成功、retry 超时或失败），只补 retry。harness 自己报的 failed（lastError 不同）不动。
  - `reconcile_with_connector` 与 `reconcile_interrupted_execs_with` 串行化（`reconcile_single_flight`）：
    - **进程内**（同一 desktop 里的启动对账线程、首个会话建立后的 U10 线程、F-EXEC-17 的执行丢失收尾）：后到者在进程内互斥锁上排队（上限 60s），轮到后**跑自己的一轮**。每轮对已处理的目标幂等——成功的目标已 `mark_reconciled`、不再是对账目标——所以排队不会重复释放，也不会漏掉后到者自己才处理得了的目标（例如 U10 路径才能处理的、没有 spec 的旧记录）。
    - **跨进程**（dev 多实例、将来的 CLI 对账）：拿到进程内锁后再取 `<runtime>/exec/reconcile.lock` 的 flock，最多等 1s；仍被别的进程持有就跳过本轮（返回 0），目标留给下一次触发。进程内的竞争者已被互斥锁排开，这里的 flock 竞争只可能来自别的进程。
  - 对账跳过判定从 `shim_alive`（身份未知 = 死）换成 `shim_maybe_alive`（身份未知但 pid 仍在 = 活）。代价：start-id 缺失且 pid 恰被复用时，该目标会一直被跳过，直到那个 pid 消失。
- 不变：`force_release_agent_leases` 仍不跳过存活 shim（§47 force-double-release 已接受，见分诊表）。

- [ ] **Step 1: 写失败测试（executor.rs）**

`crates/omt-exec/src/executor.rs` 的 `mod tests` 加（放在 `held_running_nodes_matches_executor_actor_suffix` 之后）：

```rust
    /// 记账型 rpc 替身：run/get 返回给定响应，其余调用记 (method, action)。
    fn recording_release_rpc(
        run_get: serde_json::Value,
    ) -> (
        Arc<Mutex<Vec<String>>>,
        impl Fn(&str, serde_json::Value) -> Result<serde_json::Value, String>,
    ) {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = calls.clone();
        let rpc = move |method: &str, params: serde_json::Value| {
            if method == "run/get" {
                return Ok(run_get.clone());
            }
            let label = match params["action"].as_str() {
                Some(action) => format!("{method}:{action}"),
                None => method.to_string(),
            };
            seen.lock().unwrap().push(label);
            Ok(serde_json::json!({}))
        };
        (calls, rpc)
    }

    /// P3-ACC-8：run 已 canceled / completed——没有可释放的租约，直接 Ok，
    /// 不再 report + retry（retry 在 canceled run 上必然 CONFLICT，首条对账
    /// 路径因此误记「释放失败，保留为对账目标」）。
    #[test]
    fn release_skips_terminal_runs() {
        for status in ["canceled", "completed"] {
            let (calls, rpc) = recording_release_rpc(serde_json::json!({
                "run": { "runId": "RUN-1", "status": status },
                "items": [{
                    "nodeId": "node-1", "state": "running",
                    "executorActor": "mcp:1/exec:exec-x",
                }],
            }));
            release_items_for_exec(&rpc, "h_t", "RUN-1", "exec-x")
                .unwrap_or_else(|e| panic!("{status}: {e}"));
            assert!(calls.lock().unwrap().is_empty(), "{status}: {:?}", calls.lock().unwrap());
        }
    }

    /// §41 item-stuck-failed-after-retry-timeout：上次释放 report 成功、
    /// retry 超时——item 停在 failed（lastError = 释放说明）。下一次释放只补
    /// retry；harness 自己报的 failed（lastError 不同）不动。
    /// completed_with_failures 仍会处理（它只能经逐项 retry 重开）。
    #[test]
    fn release_retries_items_failed_by_a_previous_release() {
        let (calls, rpc) = recording_release_rpc(serde_json::json!({
            "run": { "runId": "RUN-1", "status": "completed_with_failures" },
            "items": [
                {
                    "nodeId": "node-stuck", "state": "failed",
                    "executorActor": "mcp:1/exec:exec-x",
                    "lastError": "exec exec-x terminated (kill/app-exit)",
                },
                {
                    "nodeId": "node-own-fail", "state": "failed",
                    "executorActor": "mcp:1/exec:exec-x",
                    "lastError": "tests failed",
                },
            ],
        }));
        release_items_for_exec(&rpc, "h_t", "RUN-1", "exec-x").unwrap();
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["run/control:retry".to_string()],
            "只对释放留下的 failed 残项补 retry"
        );
    }

    /// pending 对账目标（无 spec——U10 会话路径）。
    fn pending_target(rt: &Path, exec_id: &str) -> crate::agents::AgentRecord {
        let agent = crate::agents::register_agent(
            rt,
            "h_t",
            "pi",
            crate::agents::NewExec {
                exec_id: exec_id.into(),
                node_id: None,
                run_id: Some("RUN-1".into()),
                binary_path: None,
                binary_args: vec![],
                model: None,
                launched_by: None,
                parent_agent_id: None,
            },
        )
        .unwrap();
        crate::agents::update_exec(rt, "h_t", &agent.id, exec_id, |e| {
            e.lease_release_pending = true
        })
        .unwrap();
        agent
    }

    /// 带 launch spec 的 pending 对账目标（shim 路径 reconcile_with_connector
    /// 只处理有 spec 的目标）。
    fn pending_target_with_spec(rt: &Path, exec_id: &str) -> crate::agents::AgentRecord {
        let agent = pending_target(rt, exec_id);
        let mut spec = crate::spec::sample(&rt.join("ws").join(".omt"), Path::new("/bin/omt"));
        spec.home_id = "h_t".into();
        spec.agent_id = agent.id.clone();
        spec.exec_id = exec_id.into();
        let adir = crate::layout::agent_dir(rt, "h_t", &agent.id).unwrap();
        spec.write(&crate::layout::exec_file(
            &adir,
            exec_id,
            crate::layout::ExecFile::Spec,
        ))
        .unwrap();
        agent
    }

    /// §39 dual-startup-reconcilers（进程内）：shim 路径对账器正在跑时，U10
    /// 会话对账器排队等它跑完，再跑**自己的一轮**——没有 spec 的旧目标只有
    /// U10 路径处理得了，排队后必须照常处理；带 spec 的目标已被前一轮标记
    /// reconciled，不会被二次释放。
    #[test]
    fn in_process_reconcilers_queue_and_each_runs_its_own_pass() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().to_path_buf();
        pending_target_with_spec(&rt, "exec-spec");
        pending_target(&rt, "exec-nospec");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let entered_tx = Mutex::new(entered_tx);
        let slow: crate::daemon_link::Connector = Arc::new(move |_: &Path, _: &str| {
            let _ = entered_tx.lock().unwrap().send(());
            std::thread::sleep(Duration::from_millis(500));
            let rpc: RpcFn =
                Arc::new(|_: &str, _: serde_json::Value| Ok(serde_json::json!({ "items": [] })));
            Ok(rpc)
        });
        let rt_a = rt.clone();
        let shim_path = std::thread::spawn(move || reconcile_with_connector(&slow, &rt_a));
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("shim 路径对账器应已持锁并开始释放");

        let released = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = released.clone();
        let rpc = move |method: &str, params: serde_json::Value| -> Result<serde_json::Value, String> {
            if method == "run/get" {
                seen.lock().unwrap().push(params["runId"].as_str().unwrap_or_default().to_string());
            }
            Ok(serde_json::json!({ "items": [] }))
        };
        let started = Instant::now();
        let handled = reconcile_interrupted_execs_with(&rpc, &rt);
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "U10 对账器必须排队等 shim 路径那一轮结束：{:?}",
            started.elapsed()
        );
        assert_eq!(handled, 1, "排队后跑自己的一轮：只剩没有 spec 的目标");
        assert_eq!(released.lock().unwrap().len(), 1, "带 spec 的目标不得被二次释放");
        assert_eq!(shim_path.join().unwrap(), 1);
        assert!(crate::agents::list_reconcile_targets(&rt).unwrap().is_empty());
    }

    /// §39 dual-startup-reconcilers（跨进程）：别的进程持有 reconcile.lock
    /// （测试用独立 fd 直接持 flock 模拟）时，两条对账路径都短等后跳过——
    /// 不发 RPC、不标记；锁释放后照常处理。
    #[test]
    fn reconcile_skips_when_another_process_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path();
        pending_target(rt, "exec-xproc");
        let held = omt_harness::atomic_json::lock_exclusive(
            &rt.join("exec").join("reconcile"),
            Duration::from_secs(1),
        )
        .unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let seen = calls.clone();
        let rpc = move |_: &str, _: serde_json::Value| -> Result<serde_json::Value, String> {
            *seen.lock().unwrap() += 1;
            Ok(serde_json::json!({ "items": [] }))
        };
        let started = Instant::now();
        assert_eq!(reconcile_interrupted_execs_with(&rpc, rt), 0, "跨进程持锁时跳过");
        let connector: crate::daemon_link::Connector =
            Arc::new(|_: &Path, _: &str| Err("must not connect".to_string()));
        assert_eq!(reconcile_with_connector(&connector, rt), 0);
        assert!(started.elapsed() < Duration::from_secs(5), "只短等：{:?}", started.elapsed());
        assert_eq!(*calls.lock().unwrap(), 0, "跳过的一方不得发 RPC");
        drop(held);
        assert_eq!(reconcile_interrupted_execs_with(&rpc, rt), 1, "锁释放后照常对账");
        assert!(crate::agents::list_reconcile_targets(rt).unwrap().is_empty());
    }

    /// §45/§47 double-release-false-negative：shim_pid 在、start-id 缺失（或
    /// proc_pidinfo 读不出）且 pid 仍存在 → 按「未知 → 存活」跳过，绝不释放；
    /// pid 已不存在 → 照常释放。
    #[test]
    fn reconcile_treats_unknown_shim_identity_as_alive() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path();
        let agent = pending_target(rt, "exec-unknown");
        crate::agents::update_exec(rt, "h_t", &agent.id, "exec-unknown", |e| {
            e.shim_pid = Some(std::process::id()); // 存在的 pid
            e.shim_start_id = None; // 身份缺失
        })
        .unwrap();
        let ok = |_: &str, _: serde_json::Value| -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({ "items": [] }))
        };
        assert_eq!(reconcile_interrupted_execs_with(&ok, rt), 0, "身份未知必须跳过");
        assert_eq!(crate::agents::list_reconcile_targets(rt).unwrap().len(), 1);

        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap(); // 已收割：pid 不再存在
        crate::agents::update_exec(rt, "h_t", &agent.id, "exec-unknown", |e| {
            e.shim_pid = Some(gone_pid);
        })
        .unwrap();
        assert_eq!(reconcile_interrupted_execs_with(&ok, rt), 1, "pid 已不在 → 释放");
    }
```

`crates/omt-exec/src/control.rs` 的 `mod tests` 加：

```rust
    /// §39 reconcile-snapshot-writeback：shim 存活分支不得用过时快照回写
    /// 状态——快照是 Running、磁盘上 shim 已落 Exited 时，对账后仍是 Exited。
    #[test]
    fn reconcile_alive_branch_does_not_write_back_a_stale_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let id = reg(rt, "exec-snap");
        let me = std::process::id();
        let start = crate::executor::process_start_identity(me).unwrap();
        update_exec(rt, "h_t", &id, "exec-snap", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        crate::agents::update_agent_status(
            rt,
            "h_t",
            &id,
            AgentRecordStatus::Running,
            crate::agents::AgentAttachment::Orphan,
        )
        .unwrap();
        let snapshot = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        // 快照之后 shim 收尾写了终态
        crate::agents::update_agent_status(
            rt,
            "h_t",
            &id,
            AgentRecordStatus::Exited,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        assert!(!reconcile_agent(rt, "h_t", &snapshot));
        let now = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        assert_eq!(now.status, AgentRecordStatus::Exited, "不得被快照覆盖回 Running");
        assert_eq!(now.attachment, crate::agents::AgentAttachment::None);
    }

    /// 同一问题的 shim 已死分支：快照活跃、磁盘已是终态（shim 写完终态后才
    /// 退出）——锁内复核后不得改成 Failed + SHIM_LOST。
    #[test]
    fn reconcile_dead_branch_rechecks_status_under_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        let id = reg(rt, "exec-done-late");
        update_exec(rt, "h_t", &id, "exec-done-late", |e| {
            e.shim_pid = Some(u32::MAX);
            e.shim_start_id = Some("macos:0:0".into());
        })
        .unwrap();
        backdate(rt, &id, "exec-done-late");
        let snapshot = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        crate::agents::update_agent_status(
            rt,
            "h_t",
            &id,
            AgentRecordStatus::Exited,
            crate::agents::AgentAttachment::None,
        )
        .unwrap();
        assert!(!reconcile_agent(rt, "h_t", &snapshot));
        let now = crate::agents::get_agent(rt, "h_t", &id).unwrap().unwrap();
        assert_eq!(now.status, AgentRecordStatus::Exited);
        assert!(now.execs[0].failure_detail.is_none());
    }

    /// shim_maybe_alive 判定表：无 pid → 否；身份匹配 → 是；身份不匹配 →
    /// 否；身份缺失但 pid 存在 → 是（未知按存活）；pid 已不存在 → 否。
    #[test]
    fn shim_maybe_alive_treats_unknown_identity_as_alive() {
        let me = std::process::id();
        let start = crate::executor::process_start_identity(me).unwrap();
        let rec = |pid: Option<u32>, start: Option<&str>| ExecRecord {
            shim_pid: pid,
            shim_start_id: start.map(str::to_string),
            ..Default::default()
        };
        assert!(!shim_maybe_alive(&rec(None, None)));
        assert!(shim_maybe_alive(&rec(Some(me), Some(&start))));
        assert!(!shim_maybe_alive(&rec(Some(me), Some("macos:0:0"))));
        assert!(shim_maybe_alive(&rec(Some(me), None)));
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        assert!(!shim_maybe_alive(&rec(Some(gone_pid), None)));
    }
```

```bash
cargo test -p omt-exec -- release_skips_terminal_runs release_retries_items_failed in_process_reconcilers_queue reconcile_skips_when_another_process reconcile_treats_unknown reconcile_alive_branch reconcile_dead_branch shim_maybe_alive 2>&1 | tail -5
```
Expected：编译失败（`shim_maybe_alive` 未定义）；补一个空壳 `pub fn shim_maybe_alive(_: &ExecRecord) -> bool { false }` 后再跑，其余 7 个测试 FAIL（canceled 仍 report + retry、failed 残项无 retry、U10 不排队就与 shim 路径并发跑（耗时 < 300ms、带 spec 的目标被二次释放）、跨进程持锁时仍对账、未知身份被释放、快照覆盖回 Running、终态被改成 Failed）。

- [ ] **Step 2: executor.rs——释放序列**

`release_items_for_exec` 整体替换为：

```rust
/// 租约释放写进 item.lastError 的说明（run/report 的 note 原样落
/// last_error）。`held_failed_by_release` 据此认出「本 exec 的上次释放已
/// report failed、retry 未完成」的残项（§41）。
fn release_note(exec_id: &str) -> String {
    format!("exec {exec_id} terminated (kill/app-exit)")
}

/// run/get 挑出本 exec 仍持有的 running item 并逐个释放（admin report
/// failed + retry 回 pending）。终态线程（shim 经 set_lease_release 注入的
/// 租约释放）与启动对账（reconcile_interrupted_execs）共用。run/get 失败
/// 返回 Err；逐项释放失败记首个错误、其余项继续处理。
pub fn release_items_for_exec(
    rpc: &dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String>,
    home_id: &str,
    run_id: &str,
    exec_id: &str,
) -> Result<(), String> {
    // run/get 查 items 中 executor_actor 以 /exec:<exec_id> 结尾的 in-flight 项
    // （run/get 失败 = 无法证明持有项，直接上报 Err）
    let resp = rpc(
        "run/get",
        serde_json::json!({"homeId": home_id, "runId": run_id}),
    )?;
    // P3-ACC-8：run 已 canceled / completed——没有可释放的租约（canceled 上的
    // retry 必然 CONFLICT），视为释放完成。completed_with_failures 不在此列：
    // 它只能经逐项 retry 重开，下面的 failed 残项正需要 retry。
    if matches!(
        resp["run"]["status"].as_str(),
        Some("canceled") | Some("completed")
    ) {
        return Ok(());
    }
    let note = release_note(exec_id);
    let retry = |node_id: &str| {
        rpc(
            "run/control",
            serde_json::json!({
                "homeId": home_id, "runId": run_id,
                "action": "retry", "nodeId": node_id,
            }),
        )
    };
    let mut first_err: Option<String> = None;
    for node_id in held_running_nodes(&resp, exec_id) {
        // admin report failed（无 leaseToken，Administrator 授权路径）
        if let Err(e) = rpc(
            "run/report",
            serde_json::json!({
                "homeId": home_id, "runId": run_id, "nodeId": node_id,
                "outcome": "failed",
                "note": note,
            }),
        ) {
            first_err.get_or_insert(e);
        }
        // retry 回 pending
        if let Err(e) = retry(&node_id) {
            first_err.get_or_insert(e);
        }
    }
    // §41：上一次释放 report 已成功、retry 超时 / 失败留下的 failed 残项——
    // 只补 retry（此前对账只挑 running，残项永远停在 failed）
    for node_id in held_failed_by_release(&resp, exec_id) {
        if let Err(e) = retry(&node_id) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}
```

`held_running_nodes` 之后加：

```rust
/// §41：本 exec 的上一次释放留下的 failed 残项——actor 后缀匹配、state =
/// failed、lastError 恰为释放说明（harness 自己报的 failed 不算）。
fn held_failed_by_release(resp: &serde_json::Value, exec_id: &str) -> Vec<String> {
    let suffix = format!("/exec:{exec_id}");
    let note = release_note(exec_id);
    resp["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item["executorActor"]
                        .as_str()
                        .is_some_and(|actor| actor.ends_with(&suffix))
                        && item["state"].as_str() == Some("failed")
                        && item["lastError"].as_str() == Some(note.as_str())
                })
                .filter_map(|item| item["nodeId"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 3: executor.rs——单飞与假阴性**

在 `target_shim_alive` 之前加：

```rust
/// §39 dual-startup-reconcilers：两条对账路径（shim 路径 / U10 会话路径）
/// 在同一 desktop 进程里会并发（启动对账线程、首个会话建立后的 U10 线程、
/// F-EXEC-17 的执行丢失收尾），dev 多实例时还会跨进程并发——同一目标被
/// 两个对账器同时释放 = 双重释放。
/// - 进程内：在 RECONCILE_IN_PROCESS 上排队（上限 60s），轮到后跑**自己的**
///   一轮。每轮幂等——成功的目标已 mark_reconciled、不再是对账目标——排队
///   既不会重复释放，也不会漏掉只有后到者处理得了的目标（无 spec 的旧
///   记录只走 U10 路径）。
/// - 跨进程：拿到进程内锁后再取 `<runtime>/exec/reconcile.lock` 的 flock，
///   最多等 RECONCILE_CROSS_PROCESS_WAIT；仍被占用就跳过本轮（返回 0），
///   目标留给下一次触发。进程内的竞争者已被互斥锁排开，这里的 flock 竞争
///   只可能来自别的进程。
static RECONCILE_IN_PROCESS: Mutex<()> = Mutex::new(());
const RECONCILE_IN_PROCESS_WAIT: Duration = Duration::from_secs(60);
const RECONCILE_CROSS_PROCESS_WAIT: Duration = Duration::from_secs(1);

fn reconcile_single_flight(runtime_dir: &Path, pass: impl FnOnce() -> usize) -> usize {
    let deadline = Instant::now() + RECONCILE_IN_PROCESS_WAIT;
    let _in_process = loop {
        match RECONCILE_IN_PROCESS.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    eprintln!(
                        "对账：等待本进程内另一轮对账超过 {}s，本轮跳过（目标留给下一次触发）",
                        RECONCILE_IN_PROCESS_WAIT.as_secs()
                    );
                    return 0;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let Ok(_cross_process) = omt_harness::atomic_json::lock_exclusive(
        &runtime_dir.join("exec").join("reconcile"),
        RECONCILE_CROSS_PROCESS_WAIT,
    ) else {
        eprintln!("对账：另一个进程正在对账，本轮跳过（目标留给下一次触发）");
        return 0;
    };
    pass()
}
```

`target_shim_alive` 改用保守判定：

```rust
fn target_shim_alive(runtime_dir: &Path, t: &agents::ReconcileTarget) -> bool {
    agents::find_exec_in_home(runtime_dir, &t.home_id, &t.exec_id)
        .ok()
        .flatten()
        // §45/§47：身份未知（start-id 缺失 / proc_pidinfo 读不出）但 pid 仍在
        // → 按存活跳过——宁可留到下一轮，也不双重释放
        .map(|(_, exec)| crate::control::shim_maybe_alive(&exec))
        .unwrap_or(false)
}
```

`reconcile_interrupted_execs_with` 的函数体包进单飞：

```rust
pub fn reconcile_interrupted_execs_with(
    rpc: &dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String>,
    runtime_dir: &Path,
) -> usize {
    reconcile_single_flight(runtime_dir, || {
        let targets = match agents::list_reconcile_targets(runtime_dir) {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let mut handled = 0usize;
        for t in &targets {
            // F-EXEC-14(3)：shim 仍存活的目标由 shim 自己收尾——跳过（不释放、
            // 不标记），保持为下一轮的对账目标。
            if target_shim_alive(runtime_dir, t) {
                continue;
            }
            if let Err(err) = release_items_for_exec(rpc, &t.home_id, &t.run_id, &t.exec_id) {
                eprintln!(
                    "对账：exec {} 租约释放失败（{err}），保留为对账目标",
                    t.exec_id
                );
                continue;
            }
            mark_reconciled(runtime_dir, t);
            handled += 1;
        }
        handled
    })
}
```

`reconcile_with_connector` 同样处理：

```rust
pub fn reconcile_with_connector(
    connector: &crate::daemon_link::Connector,
    runtime_dir: &Path,
) -> usize {
    reconcile_single_flight(runtime_dir, || {
        let Ok(targets) = agents::list_reconcile_targets(runtime_dir) else {
            return 0;
        };
        let mut handled = 0usize;
        for t in &targets {
            // F-EXEC-14(3)：shim 仍存活的目标由 shim 自己收尾——跳过（不释放、
            // 不标记），保持为下一轮的对账目标。
            if target_shim_alive(runtime_dir, t) {
                continue;
            }
            let Ok(dir) = crate::layout::agent_dir(runtime_dir, &t.home_id, &t.agent_id) else {
                continue;
            };
            let spec_path =
                crate::layout::exec_file(&dir, &t.exec_id, crate::layout::ExecFile::Spec);
            let Ok(spec) = crate::spec::LaunchSpec::read(&spec_path) else {
                continue;
            };
            let outcome = connector(&spec.home_path, &t.home_id)
                .and_then(|rpc| release_items_for_exec(&*rpc, &t.home_id, &t.run_id, &t.exec_id));
            if let Err(err) = outcome {
                eprintln!(
                    "对账：exec {} 经 shim 路径释放失败（{err}），保留为对账目标",
                    t.exec_id
                );
                continue;
            }
            mark_reconciled(runtime_dir, t);
            handled += 1;
        }
        handled
    })
}
```

- [ ] **Step 4: control.rs——`shim_maybe_alive` 与 compare-and-write**

`shim_alive` 之后加：

```rust
/// §45/§47 double-release-false-negative：对账「是否跳过」用的保守判定——
/// 宁可把死 shim 当活的（目标留到下一轮），也不把活 shim 当死的（双重
/// 释放 + 陈旧快照的 admin report 误伤别的 exec 刚认领的 item）。与
/// `shim_alive`（发信号前必须确知身份，未知 = 否）方向相反：
/// - 无 shim_pid → 否；
/// - 身份可读：匹配 → 是，不匹配 → 否（确已换人，pid 复用不算假阴性）；
/// - start-id 缺失或 proc_pidinfo 读不出 → pid 仍存在即按「未知 → 存活」。
pub fn shim_maybe_alive(exec: &ExecRecord) -> bool {
    let Some(pid) = exec.shim_pid else {
        return false;
    };
    let pid_alive = || omt_client::pid_live(i64::from(pid));
    match exec.shim_start_id.as_deref() {
        None => pid_alive(),
        Some(expected) => match crate::executor::process_start_identity(pid) {
            Ok(actual) => actual == expected,
            Err(_) => pid_alive(),
        },
    }
}
```

`reconcile_agent` 整体替换为：

```rust
/// 单个活跃 agent 的对账（P-8）。返回是否改成了终态。
///
/// §39 reconcile-snapshot-writeback：两个分支都是锁内 compare-and-write——
/// `agent` 是调用方读到的快照，shim 可能在快照之后已落终态。存活分支只在
/// 记录仍活跃时把 attachment 拉回 Live、绝不改 status（此前用快照的 status
/// 回写，可能把 Exited 覆盖回 Running）；已死分支在锁内复核「仍活跃且最新
/// exec 未变」后，才在同一事务里写 failure_detail + Failed + attachment。
fn reconcile_agent(runtime_dir: &Path, home_id: &str, agent: &AgentRecord) -> bool {
    let Some(exec) = latest_exec(agent) else {
        return false;
    };
    let exec_id = exec.exec_id.clone();
    if shim_alive(exec) {
        let _ = agents::update_agent(runtime_dir, home_id, &agent.id, |current| {
            if !is_active(current) || current.attachment == AgentAttachment::Live {
                return false;
            }
            current.attachment = AgentAttachment::Live;
            true
        });
        return false;
    }
    if within_launch_grace(exec) {
        return false;
    }
    let harness_alive = identity_alive(exec.pid, exec.process_start_id.as_deref());
    let attachment = if harness_alive {
        AgentAttachment::Orphan
    } else {
        AgentAttachment::None
    };
    agents::update_agent(runtime_dir, home_id, &agent.id, |current| {
        if !is_active(current) || current.last_exec_id.as_deref() != Some(exec_id.as_str()) {
            return false;
        }
        if let Some(e) = current.execs.iter_mut().find(|e| e.exec_id == exec_id) {
            if e.failure_detail.is_none() {
                e.failure_detail = Some(agents::SHIM_LOST_MARKER.to_string());
            }
        }
        current.status = AgentRecordStatus::Failed;
        current.attachment = attachment;
        true
    })
    .unwrap_or(false)
}
```

- [ ] **Step 5: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
cargo test -p omt-runtime --test exec_shim --test exec_survives_launcher 2>&1 | grep -E '^test result|FAILED'
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```
Expected：全部 `0 failed`（`settle_lost_exec_*`、`startup_reconcile_*`、`stop_all_does_not_count_reconciled_dead_shims` 在新的 compare-and-write 下语义不变）；fmt / clippy 干净。

- [ ] **Step 6: 提交**

```bash
git add crates/omt-exec
git commit -m "fix(exec): lease release and reconcile P3s

- P3-ACC-8: a canceled/completed run has nothing to release.
- §41: items a previous release left failed (report ok, retry timed out)
  get their retry; harness-reported failures are untouched.
- §39: reconcile_agent writes compare-and-set under the agent lock and
  never writes a stale snapshot status back.
- §39: reconcilers queue in-process (each then runs its own idempotent
  pass) and skip only when another process holds exec/reconcile.lock.
- §45/§47: unknown shim identity with a live pid counts as alive, so the
  reconciler skips instead of double-releasing.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B4: daemon_link 与 shim 健壮性（§39 陈旧 descriptor / daemon 僵尸 / 保活为 0 / 授权泄漏；orphan 整组强杀）

**Files:**
- Modify: `crates/omt-exec/src/daemon_link.rs`（`wait_for_live_descriptor`、`spawn_daemon_process` + 回收线程）
- Modify: `crates/omt-exec/src/shim.rs`（`KEEPALIVE_FLOOR`、`spawn_keepalive` 外层查 done）
- Modify: `crates/omt-exec/src/grants.rs`（`TempGrant::drop` 只在撤销成功后删登记）
- Modify: `crates/omt-exec/src/executor.rs`（`kill_orphan`：整组 SIGKILL + 单事务写终态）
- Test: 各文件的 `mod tests`

**Interfaces:**
- Consumes: B2 的 `agents::update_agent`；`omt_client::{read_descriptor, pid_live, endpoint_live}`（omt-exec 不能用 `omt_runtime::descriptor`，用 omt-client 里的等价判据，与已有的 `daemon_link::live_descriptor` 相同）。
- 行为变化：
  - `spawn_daemon` 只接受**存活**的 descriptor（pid 存活 + endpoint 可连）；崩溃 daemon 残留的 descriptor 不再让「拉起后第一次 connect」必然失败。
  - shim 拉起的 daemon 由回收线程 `wait()`，退出后不留僵尸（此前僵尸让 `pid_live` 恒真，`omt daemon-stop` 每次 stop-timeout）。
  - 保活间隔下限 100ms（= 内层轮询粒度）。`OMT_SHIM_KEEPALIVE_MS=0`、idleQuietMs 为 1 或 2 时不再忙等；外层循环也检查 done。e2e 的 `idleQuietMs=1500` 推导出 500ms，不受影响。
  - `TempGrant::drop` 撤销失败时保留 `exec/temp-grants.json` 登记，交给启动清扫（`sweep_dead_temp_grants`）在 pid 死后补删。
  - `kill_orphan`：pid 的起始身份校验通过、且它仍是自己进程组的组首（`getpgid(pid) == pid`，harness spawn 时 `setpgid(0,0)`）时整组 SIGKILL——MCP 桥 `omt mcp` 等同组子进程一并结束；组首不是它时退回单 pid。status / attachment / kill_reason 一次写入。

- [ ] **Step 1: 写失败测试**

`crates/omt-exec/src/daemon_link.rs` 的 `mod tests` 加：

```rust
    fn write_descriptor(runtime: &Path, endpoint: &Path, pid: u32) {
        std::fs::create_dir_all(runtime).unwrap();
        std::fs::write(
            omt_client::descriptor_path(runtime),
            serde_json::json!({
                "schemaVersion": 1,
                "endpoint": endpoint.display().to_string(),
                "generation": 1,
                "pid": pid,
                "bootToken": "test",
            })
            .to_string(),
        )
        .unwrap();
    }

    /// §39 daemon-link-stale-descriptor：崩溃 daemon 残留的 descriptor（pid
    /// 已死、socket 不可连）不算就绪——等待必须超时返回 None；pid 存活且
    /// socket 可连才返回。
    #[test]
    fn wait_for_live_descriptor_ignores_stale_descriptors() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let dead_pid = gone.id();
        gone.wait().unwrap();
        write_descriptor(&runtime, &tmp.path().join("missing.sock"), dead_pid);
        assert!(
            wait_for_live_descriptor(&runtime, Duration::from_millis(300)).is_none(),
            "陈旧 descriptor 不得被当成就绪"
        );
        let socket = tmp.path().join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        write_descriptor(&runtime, &socket, std::process::id());
        assert!(wait_for_live_descriptor(&runtime, Duration::from_millis(300)).is_some());
    }

    /// §39 shim-daemon-zombie：shim 拉起的 daemon 退出后必须被回收——僵尸
    /// 进程对 kill(pid, 0) 仍成功，pid_live 恒真。
    #[test]
    fn spawned_daemon_is_reaped_after_exit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("fake-daemon");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pid = spawn_daemon_process(&bin, &tmp.path().join("runtime")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while omt_client::pid_live(i64::from(pid)) {
            assert!(Instant::now() < deadline, "daemon 退出后 5s 仍是僵尸（未回收）");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
```

`crates/omt-exec/src/shim.rs` 的 `mod tests` 加：

```rust
    /// §39 keepalive-zero：间隔有下限（0 / idleQuietMs=2 不再忙等），保活
    /// 线程在外层循环也检查 done——置 done 后能在界内退出，且连接次数有界。
    #[test]
    fn keepalive_has_a_floor_and_honours_done() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        assert_eq!(keepalive_every_from(rt, Some("0")), Some(KEEPALIVE_FLOOR));
        std::fs::write(rt.join("daemon.json"), r#"{"idleQuietMs": 2}"#).unwrap();
        assert_eq!(keepalive_every_from(rt, None), Some(KEEPALIVE_FLOOR));

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let connector: Connector = Arc::new(move |_: &Path, _: &str| {
            seen.fetch_add(1, Ordering::SeqCst);
            Err("down".to_string())
        });
        let done = Arc::new(AtomicBool::new(false));
        let keeper = spawn_keepalive(
            Some(Duration::ZERO),
            PathBuf::from("/nonexistent/.omt"),
            "h_t".to_string(),
            connector,
            done.clone(),
        );
        std::thread::sleep(Duration::from_millis(350));
        done.store(true, Ordering::SeqCst);
        assert!(join_bounded(keeper, Duration::from_secs(2)), "置 done 后必须退出");
        let n = calls.load(Ordering::SeqCst);
        assert!(n <= 5, "不得忙等连接 daemon：350ms 内连了 {n} 次");
    }
```

`crates/omt-exec/src/grants.rs` 的 `mod tests` 加：

```rust
    /// §39 tempgrant-drop-leak：撤销（admin-grants.json 写回）失败时必须保留
    /// 临时登记——启动清扫凭它在 pid 死后补删授权；此前登记被无条件删除，
    /// 授权从此永久泄漏。手法：runtime 目录只读（admin-grants.json 的 tmp
    /// 建不出来），exec/ 子目录仍可写（登记表在那里）。
    #[test]
    fn failed_revoke_keeps_the_temp_registry_entry() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path().join("rt");
        std::fs::create_dir_all(&rt).unwrap();
        let grant = TempGrant::add(&rt, "cli:999999").unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o555)).unwrap();
        drop(grant);
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            principals(&rt).contains(&"cli:999999".to_string()),
            "前提：撤销确实失败了"
        );
        let (registry, _) = read_json::<Vec<String>>(&temp_registry(&rt)).unwrap();
        assert!(
            registry.contains(&"cli:999999".to_string()),
            "撤销失败时登记必须保留: {registry:?}"
        );
        // 启动清扫据登记补删（999999 不是存活 pid）
        assert_eq!(sweep_dead_temp_grants(&rt).unwrap(), 1);
        assert!(!principals(&rt).contains(&"cli:999999".to_string()));
    }
```

`crates/omt-exec/src/executor.rs` 的 `mod tests`（`kill_orphan_rejects_non_orphan_even_with_valid_pid` 之后）加：

```rust
    /// 验收 #7 遗留 + §41：orphan 强杀整个 harness 进程组（MCP 桥等同组子
    /// 进程一并结束），且 status / attachment / kill_reason 一次写入。
    #[test]
    fn kill_orphan_kills_the_whole_harness_process_group() {
        use std::io::BufRead as _;
        use std::os::unix::process::CommandExt as _;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_id = "h_test";
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 30 & echo $!; wait")
            .stdout(std::process::Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let grandchild: i64 = line.trim().parse().unwrap();
        let pid = child.id();
        let start_id = process_start_identity(pid).unwrap();
        let agent = agents::register_agent(
            &runtime,
            home_id,
            "fake",
            NewExec {
                exec_id: "exec-pg".into(),
                node_id: None,
                run_id: None,
                binary_path: None,
                binary_args: vec![],
                model: None,
                launched_by: None,
                parent_agent_id: None,
            },
        )
        .unwrap();
        agents::update_exec(&runtime, home_id, &agent.id, "exec-pg", |exec| {
            exec.pid = Some(pid);
            exec.process_start_id = Some(start_id.clone());
        })
        .unwrap();
        agents::update_agent_status(
            &runtime,
            home_id,
            &agent.id,
            AgentRecordStatus::Failed,
            AgentAttachment::Orphan,
        )
        .unwrap();

        kill_orphan(&runtime, home_id, &agent.id, "exec-pg").unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let alive = wait_for(
            || (!omt_client::pid_live(grandchild)).then_some(()),
            Duration::from_secs(5),
        );
        assert!(alive.is_some(), "同组子进程（sleep {grandchild}）必须一并被杀");
        let record = agents::get_agent(&runtime, home_id, &agent.id).unwrap().unwrap();
        assert_eq!(record.status, AgentRecordStatus::Killed);
        assert_eq!(record.attachment, AgentAttachment::None);
        assert_eq!(record.execs[0].kill_reason.as_deref(), Some("orphan"));
    }
```

（孙进程 sleep 不是本测试进程的子进程，sh 被杀后由 launchd 收养并回收，所以 `pid_live` 会转为 false；修复前它一直活着，30s 后才自然结束。）

```bash
cargo test -p omt-exec -- wait_for_live_descriptor spawned_daemon_is_reaped keepalive_has_a_floor failed_revoke_keeps kill_orphan_kills_the_whole 2>&1 | tail -5
```
Expected：编译失败（`wait_for_live_descriptor`、`spawn_daemon_process`、`KEEPALIVE_FLOOR` 未定义）；补上空壳后其余断言 FAIL（陈旧 descriptor 被接受、僵尸不消失、keeper 忙等不退出、登记被删、孙进程存活）。

- [ ] **Step 2: daemon_link.rs**

`use std::time::Duration;` 改为 `use std::time::{Duration, Instant};`，`spawn_daemon` 替换为：

```rust
/// §39 daemon-link-stale-descriptor：轮询**存活**的 descriptor（pid 存活 +
/// endpoint 可连，与 `live_descriptor` 同一判据）。`omt_client::
/// wait_for_descriptor` 只看文件存在，会接受崩溃 daemon 残留的旧
/// descriptor，拉起后的第一次 connect 因此必然失败。
fn wait_for_live_descriptor(
    runtime_dir: &Path,
    timeout: Duration,
) -> Option<omt_client::Descriptor> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(descriptor) = live_descriptor(runtime_dir) {
            return Some(descriptor);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 以新会话拉起 daemon 并返回其 pid。§39 shim-daemon-zombie：起一个
/// `wait()` 回收线程（同 `launch::spawn_shim`）——daemon 退出后不留僵尸
/// （僵尸让 pid_live 恒真，`omt daemon-stop` 每次 stop-timeout）。shim 先
/// 退出时线程随进程结束，daemon 由 launchd 收养。
fn spawn_daemon_process(bin: &Path, runtime_dir: &Path) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(bin);
    command
        .arg("--runtime-dir")
        .arg(runtime_dir)
        // 不带 --home：打开全局 home，与其他面拉起的 daemon 一致（review R2-1）
        .env("OMT_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid 在 fork 与 exec 之间脱离会话，daemon 不随 shim 退出
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn omt-daemon: {e}"))?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn spawn_daemon(bin: &Path, runtime_dir: &Path) -> Result<omt_client::Descriptor, String> {
    spawn_daemon_process(bin, runtime_dir)?;
    wait_for_live_descriptor(runtime_dir, Duration::from_secs(10))
        .ok_or_else(|| "omt-daemon 10s 内未产出可用的 descriptor".to_string())
}
```

- [ ] **Step 3: shim.rs——保活下限**

`keepalive_every` 之前加常量，并改 `keepalive_every_from` 的两处返回：

```rust
/// §39 keepalive-zero：保活间隔下限。间隔为 0（`OMT_SHIM_KEEPALIVE_MS=0`，或
/// idleQuietMs 为 1 / 2 时 1/3 取整为 0）时内层等待一次都不执行，外层循环
/// 忙等连 daemon，且永远看不到 done。取 100ms = 内层轮询粒度。
const KEEPALIVE_FLOOR: Duration = Duration::from_millis(100);

fn keepalive_every_from(runtime_dir: &Path, override_ms: Option<&str>) -> Option<Duration> {
    if let Some(ms) = override_ms.and_then(|v| v.parse::<u64>().ok()) {
        return Some(Duration::from_millis(ms).max(KEEPALIVE_FLOOR));
    }
    let idle_ms = std::fs::read_to_string(runtime_dir.join("daemon.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|config| config["idleQuietMs"].as_i64())
        .unwrap_or(1_800_000);
    if idle_ms <= 0 {
        return None;
    }
    Some(
        Duration::from_millis((idle_ms as u64 / 3).min(5 * 60 * 1000)).max(KEEPALIVE_FLOOR),
    )
}
```

`spawn_keepalive` 的线程体改为：

```rust
    std::thread::spawn(move || {
        let Some(every) = every else {
            return;
        };
        // §39：调用方传入的间隔同样受下限约束（防御直接构造的 Some(0)）
        let every = every.max(KEEPALIVE_FLOOR);
        let mut rpc: Option<RpcFn> = None;
        loop {
            // §39：外层也查 done——内层等待被跳过时仍能退出
            if done.load(Ordering::SeqCst) {
                return;
            }
            let deadline = Instant::now() + every;
            while Instant::now() < deadline {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if rpc.is_none() {
                rpc = connector(&home_path, &home_id).ok();
            }
            let alive = rpc
                .as_ref()
                .map(|call| call("run/list", serde_json::json!({ "homeId": home_id })).is_ok())
                .unwrap_or(false);
            if !alive {
                rpc = None;
            }
        }
    })
```

- [ ] **Step 4: grants.rs——只在撤销成功后删登记**

```rust
impl Drop for TempGrant {
    fn drop(&mut self) {
        // §39 tempgrant-drop-leak：撤销失败时保留临时登记——启动清扫
        // （sweep_dead_temp_grants）凭它在 pid 死后补删授权。先删登记会让
        // admin 授权永久泄漏（pid 复用即提权窗口）。
        if let Err(err) = remove(&self.runtime_dir, &self.principal) {
            eprintln!(
                "临时授权 {} 撤销失败（{err}），保留登记，留给启动清扫",
                self.principal
            );
            return;
        }
        let _ = update_json(
            &temp_registry(&self.runtime_dir),
            |list: &mut Vec<String>| {
                let before = list.len();
                list.retain(|p| p != &self.principal);
                list.len() != before
            },
        );
    }
}
```

- [ ] **Step 5: executor.rs——`kill_orphan`**

文档注释与身份校验之后的部分替换为（函数前半到 `let pid_i = ...` 不变）：

```rust
/// Orphan 强制 kill：只接受持久化 agent/exec 身份，pid 与内核进程起始标识
/// 均由 Rust 侧读取并比对。验收 #7 遗留：harness spawn 时 `setpgid(0,0)`，
/// pid 通过身份校验且仍是自己进程组的组首时整组 SIGKILL（MCP 桥 `omt mcp`
/// 等同组子进程一并结束）；组首不是它（重新分组 / 旧记录）时退回单 pid。
pub fn kill_orphan(
```

```rust
    let pid_i = i32::try_from(pid).map_err(|_| format!("pid 越界: {pid}"))?;
    // SAFETY: 只查询进程组 / 发信号；pid 已通过内核起始身份校验
    let pgid = unsafe { libc::getpgid(pid_i) };
    let rc = if pgid == pid_i {
        unsafe { libc::killpg(pid_i, libc::SIGKILL) }
    } else {
        unsafe { libc::kill(pid_i, libc::SIGKILL) }
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("kill({pid}, SIGKILL) 失败: {err}"));
    }
    // §41 kill-orphan-two-step：status / attachment / kill_reason 同一事务——
    // 读方不会看到「Killed 但 kill_reason 为空」
    let _ = agents::update_agent(runtime_dir, home_id, agent_id, |record| {
        record.status = AgentRecordStatus::Killed;
        record.attachment = AgentAttachment::None;
        if let Some(exec) = record.execs.iter_mut().find(|e| e.exec_id == exec_id) {
            exec.kill_reason = Some("orphan".to_string());
        }
        true
    });
    Ok(())
}
```

（`kill_orphan_resolves_persisted_identity_and_kills_living_process` 的子进程没有自成进程组——`getpgid` 得到测试进程的组——走单 pid 分支，绝不会 killpg 测试进程自己。）

- [ ] **Step 6: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
cargo test -p omt-runtime --test exec_daemon_link --test exec_shim 2>&1 | grep -E '^test result|FAILED'
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```
Expected：全部 `0 failed`；`exec_daemon_link` 三个测试不再各多耗约 10s（daemon-stop 不再 stop-timeout）；fmt / clippy 干净。

- [ ] **Step 7: 提交**

```bash
git add crates/omt-exec
git commit -m "fix(exec): daemon link, keepalive, temp grant and orphan-kill robustness

- §39: spawn_daemon waits for a live descriptor and reaps the daemon
  it spawned (no zombie keeping pid_live true).
- §39: keepalive interval has a 100ms floor and the outer loop checks
  the done flag.
- §39: TempGrant only drops its registry entry after a successful revoke.
- §41 + acceptance #7: kill_orphan SIGKILLs the harness process group
  and writes status + kill_reason in one update.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B5: 存储与安全（§39 tail 穿越与迁移中断回落 / §39 MCP 清扫与原子写 / §41 删除四项 + 残留清扫）

**Files:**
- Modify: `crates/omt-exec/src/executor.rs`（`transcript_tail_from`；`startup_housekeeping` 调用残留清扫）
- Modify: `crates/omt-exec/src/mcp_injection.rs`（`sweep_spec_crash_residue` 锁内读快照；`write_atomic` fsync）
- Modify: `crates/omt-exec/src/agents.rs`（`delete_agent`、`append_exec`；新增 `remove_residue_dir` / `sweep_agent_residue`）
- Test: 三个文件的 `mod tests`

**Interfaces:**
- Produces: `pub fn agents::sweep_agent_residue(runtime_dir: &Path) -> usize`——清扫各 home 下的 `.trash-*` 目录，以及只含 `agent.json.lock`、且锁文件 60s 未修改的 agent 目录；返回清掉的目录数，失败 `eprintln!` 不中止。由 `startup_housekeeping` 调用。
- 行为变化：
  - `transcript_tail_from` 先用 `layout::validate_segment` 校验 home_id / exec_id，不合法返回空——旧目录回落分支不能再被 `..` 穿越读取任意 `*.jsonl` / `*.raw.log`。
  - 候选目录依次为 agent 目录、旧 `transcripts/<home>/`，取第一个存在 jsonl 或 raw.log 的目录——迁移中断（`agent.json` 已写、文件还没搬，见 `migrate.rs::migrate_home` 的顺序）时回落旧目录。
  - MCP 崩溃残留清扫在持有进程内锁 + flock 之后才读 `state.json`；整个清扫持锁（只在启动时跑一次）。
  - `mcp_injection::write_atomic`：rename 前 `sync_all`、rename 后 best-effort 同步目录，写失败时删除 tmp。文件权限保持默认（这里写的是用户工作区的 `mcp.json`，不能套用 `atomic_json` 的 0600）。
  - `delete_agent`：已隔离（`agent.json` → `.corrupt-*`）的 agent 恢复报「已损坏」；并发删除 / 续跑的败方不再留下只含锁文件的空目录，`append_exec` 的败方报「不存在」；改名后立即放锁再 `remove_dir_all`；`.trash-*` 删除失败改为 `eprintln!` 并交给启动清扫。
- 无单测可钉的两项：`write_atomic` 的 fsync（耐久性不可观测，靠既有 mcp_injection 测试回归）、「改名后放锁」（只影响同进程等待时长，靠既有 delete 测试回归）。

- [ ] **Step 1: 写失败测试**

`crates/omt-exec/src/executor.rs` 的 `mod tests`（`transcript_tail_prefers_normalized_jsonl` 之后）加：

```rust
    /// §39 transcript-tail-traversal：home_id / exec_id 必须先过
    /// validate_segment——旧目录回落分支直接拼路径，`..` 可以读到
    /// transcripts/<home>/ 之外的 *.jsonl / *.raw.log。
    #[test]
    fn transcript_tail_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let event = r#"{"kind":"assistant_text","execId":"x","seq":1,"ts":"1","text":"SECRET"}"#;
        // exec_id 穿越：transcripts/h_t/../secret.jsonl
        std::fs::create_dir_all(runtime.join("transcripts").join("h_t")).unwrap();
        std::fs::write(runtime.join("transcripts").join("secret.jsonl"), format!("{event}\n")).unwrap();
        assert!(transcript_tail_from(&runtime, "h_t", "../secret", None).is_empty());
        // home_id 穿越：transcripts/../exec-esc.jsonl
        std::fs::write(runtime.join("exec-esc.jsonl"), format!("{event}\n")).unwrap();
        assert!(transcript_tail_from(&runtime, "..", "exec-esc", None).is_empty());
    }

    /// §39 tail-no-legacy-fallback-midmigration：迁移中断——agent.json 已在新
    /// 布局，transcript 文件还留在旧 transcripts/<home>/——回放回落旧目录。
    #[test]
    fn transcript_tail_falls_back_to_legacy_dir_mid_migration() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        crate::agents::register_agent(
            &runtime,
            "h_m",
            "pi",
            NewExec {
                exec_id: "exec-mid".into(),
                node_id: None,
                run_id: None,
                binary_path: None,
                binary_args: vec![],
                model: None,
                launched_by: None,
                parent_agent_id: None,
            },
        )
        .unwrap();
        let legacy = legacy_transcripts_dir(&runtime, "h_m");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("exec-mid.jsonl"),
            r#"{"kind":"assistant_text","execId":"exec-mid","seq":1,"ts":"1","text":"OLD"}"#,
        )
        .unwrap();
        let events = transcript_tail_from(&runtime, "h_m", "exec-mid", None);
        assert_eq!(events.len(), 1, "新目录无文件时必须回落旧目录");
        assert_eq!(events[0]["text"], "OLD");
    }
```

`crates/omt-exec/src/mcp_injection.rs` 的 `mod tests`（`sweep_skips_workspaces_with_live_execs` 之后）加：

```rust
    /// §39 mcp-sweep-snapshot-race：清扫必须在锁内读 state 快照。持锁期间
    /// 另一个 shim 的 prepare 把新 exec 记进 launchers——清扫拿到锁后必须看到
    /// 它并跳过该 workspace；此前清扫在拿锁前读快照，按旧快照把正在运行的
    /// exec 的注入摘掉。
    #[test]
    fn sweep_reads_state_under_the_lock() {
        let rig = rig();
        let _inj = prepare(&rig.fake_omt, &rig.runtime, &rig.workspace, "omp", "exec-old", false)
            .unwrap();
        let mcp = rig.workspace.join(".omp").join("mcp.json");
        let state_file = state_dir(&rig.runtime, &rig.workspace, OMP_SPEC).join("state.json");
        let held = project_flock(&rig.runtime, OMP_SPEC).unwrap();
        let runtime = rig.runtime.clone();
        let sweeper =
            std::thread::spawn(move || sweep_crash_residue(&runtime, &|id| id == "exec-new"));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
        state["launchers"]
            .as_array_mut()
            .expect("launchers 数组")
            .push(serde_json::json!("exec-new"));
        std::fs::write(&state_file, state.to_string()).unwrap();
        drop(held);
        assert_eq!(sweeper.join().unwrap().unwrap(), 0, "存活 exec 的 workspace 必须跳过");
        assert!(
            std::fs::read_to_string(&mcp).unwrap().contains("omt"),
            "存活 exec 的注入必须保留"
        );
    }
```

`crates/omt-exec/src/agents.rs` 的 `mod tests` 加：

```rust
    fn exited_agent(runtime: &Path, exec_id: &str) -> AgentRecord {
        let agent = register_agent(runtime, "h_test", "pi", new_exec(exec_id)).unwrap();
        update_agent_status(
            runtime,
            "h_test",
            &agent.id,
            AgentRecordStatus::Exited,
            AgentAttachment::None,
        )
        .unwrap();
        agent
    }

    /// 模拟「胜方已把记录移走」：测试持有 agent.json 的 flock，败方在锁上
    /// 等待；期间删掉 agent.json，再放锁。
    fn race_against_removal<T: Send + 'static>(
        runtime: &Path,
        agent_id: &str,
        loser: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let file = layout::agent_file(runtime, "h_test", agent_id).unwrap();
        let held = lock_exclusive(&file, LOCK_ACQUIRE_TIMEOUT).unwrap();
        let handle = std::thread::spawn(loser);
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::remove_file(&file).unwrap();
        drop(held);
        handle.join().unwrap()
    }

    /// §41 delete-loser-empty-dir：并发删除的败方报「不存在」，且不留下只含
    /// 锁文件的空 agent 目录。
    #[test]
    fn delete_loser_reports_missing_and_leaves_no_residue_dir() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = exited_agent(&runtime, "exec-race");
        let adir = layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
        let (rt, id) = (runtime.clone(), agent.id.clone());
        let err = race_against_removal(&runtime, &agent.id, move || {
            delete_agent(&rt, "h_test", &id, false)
        })
        .unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        assert!(!adir.exists(), "败方不得留下空目录");
    }

    /// §41：落败的 append_exec 报「不存在」（此前报「不可追加（harness 不
    /// 一致/运行中）」），同样不留空目录。
    #[test]
    fn append_loser_reports_missing() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = exited_agent(&runtime, "exec-race2");
        let adir = layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
        let (rt, id) = (runtime.clone(), agent.id.clone());
        let err = race_against_removal(&runtime, &agent.id, move || {
            append_exec(&rt, "h_test", &id, "pi", new_exec("exec-late"))
        })
        .unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        assert!(!adir.exists());
    }

    /// §41 quarantined-delete-says-missing：agent.json 已被隔离为 .corrupt-*
    /// 时，删除报「已损坏」（修复前报「不存在」，以前的实现报「已损坏」）。
    #[test]
    fn deleting_a_quarantined_agent_says_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let agent = exited_agent(&runtime, "exec-q");
        let file = layout::agent_file(&runtime, "h_test", &agent.id).unwrap();
        std::fs::rename(&file, file.with_file_name("agent.json.corrupt-1")).unwrap();
        let err = delete_agent(&runtime, "h_test", &agent.id, false).unwrap_err();
        assert!(err.contains("已损坏"), "{err}");
    }

    /// §41 trash-never-swept + 残留：启动清扫删掉 `.trash-*` 与锁文件超过
    /// 60s 未动的空 agent 目录；刚建的（可能正在注册）与健康 agent 不动。
    #[test]
    fn sweep_agent_residue_removes_trash_and_stale_lock_only_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let keep = exited_agent(&runtime, "exec-keep");
        let home = layout::home_dir(&runtime, "h_test").unwrap();
        let trash = home.join(".trash-agent-exec-x-1");
        std::fs::create_dir_all(trash.join("nested")).unwrap();
        std::fs::write(trash.join("nested").join("exec-x.jsonl"), "x").unwrap();
        let stale = home.join("agent-exec-gone");
        std::fs::create_dir_all(&stale).unwrap();
        let stale_lock = stale.join("agent.json.lock");
        std::fs::write(&stale_lock, "").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&stale_lock)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
            .unwrap();
        let fresh = home.join("agent-exec-fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("agent.json.lock"), "").unwrap();

        assert_eq!(sweep_agent_residue(&runtime), 2);
        assert!(!trash.exists());
        assert!(!stale.exists());
        assert!(fresh.exists(), "刚建的目录可能正在注册，不动");
        assert!(get_agent(&runtime, "h_test", &keep.id).unwrap().is_some());
    }
```

```bash
cargo test -p omt-exec -- transcript_tail_rejects transcript_tail_falls_back sweep_reads_state_under delete_loser append_loser deleting_a_quarantined sweep_agent_residue 2>&1 | tail -5
```
Expected：编译失败（`sweep_agent_residue` 未定义）；补空壳后其余 6 个 FAIL（穿越读到 SECRET、中途迁移返回空、清扫摘掉注入、败方留目录、append 报「不可追加」、隔离 agent 报「不存在」）。

- [ ] **Step 2: executor.rs——`transcript_tail_from`**

函数开头到 `let jsonl = dir.join(...)` 之前替换为：

```rust
pub fn transcript_tail_from(
    runtime_dir: &Path,
    home_id: &str,
    exec_id: &str,
    max_lines: Option<usize>,
) -> Vec<serde_json::Value> {
    // §39 transcript-tail-traversal：路径段先校验——旧目录回落分支直接拼
    // home_id / exec_id，`..` 可穿越读取任意 *.jsonl / *.raw.log（改造前既有）
    if crate::layout::validate_segment("home id", home_id).is_err()
        || crate::layout::validate_segment("exec id", exec_id).is_err()
    {
        return Vec::new();
    }
    // KD-E2：新布局按 exec 定位 agent 目录；§39：候选目录依次为 agent 目录、
    // 旧 transcripts/<home>/，取第一个有 jsonl 或 raw.log 的——迁移中断
    // （agent.json 已写、文件还没搬）时回落旧目录
    let mut candidates = Vec::new();
    if let Ok(Some((agent, _))) = crate::agents::find_exec_in_home(runtime_dir, home_id, exec_id) {
        if let Ok(dir) = crate::layout::agent_dir(runtime_dir, home_id, &agent.id) {
            candidates.push(dir);
        }
    }
    candidates.push(legacy_transcripts_dir(runtime_dir, home_id));
    let Some(dir) = candidates.into_iter().find(|dir| {
        dir.join(format!("{exec_id}.jsonl")).is_file()
            || dir.join(format!("{exec_id}.raw.log")).is_file()
    }) else {
        return Vec::new(); // 无日志（未产出/已清理）→ 空
    };
    let jsonl = dir.join(format!("{exec_id}.jsonl"));
```

（函数其余部分不变。）

`startup_housekeeping` 末尾（MCP 清扫之后）加：

```rust
    // §41 trash-never-swept：删除失败遗留的 .trash-* 与竞争败方的空目录
    let residue = crate::agents::sweep_agent_residue(runtime_dir);
    if residue > 0 {
        eprintln!("启动清扫：清理了 {residue} 个 agent 删除残留目录");
    }
```

- [ ] **Step 3: mcp_injection.rs——锁内快照 + fsync**

`sweep_spec_crash_residue` 替换为：

```rust
fn sweep_spec_crash_residue(
    runtime_dir: &Path,
    spec: ProjectSpec,
    is_exec_live: &dyn Fn(&str) -> bool,
) -> Result<usize, String> {
    let root = spec.injection_root(runtime_dir);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    // §39 mcp-sweep-snapshot-race：state 快照必须在锁内读取——此前先读
    // state.json 再拿锁，等锁期间另一个 shim 的 prepare 已把新 exec 记进
    // launchers，清扫仍按旧快照判「无存活 exec」并 finalize_release，摘掉
    // 正在运行的 exec 的注入。整个清扫持有进程内锁 + flock（只在启动时跑
    // 一次；孤儿 launcher 的兜底清扫也在锁内）。
    let _guard = PROJECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _flock = project_flock(runtime_dir, spec)?;
    let mut swept = 0usize;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let state = omp_state(&entry.path());
        let workspace = state["workspace"].as_str().unwrap_or_default();
        if workspace.is_empty() {
            continue;
        }
        let has_live = state["launchers"]
            .as_array()
            .map(|ids| ids.iter().filter_map(|v| v.as_str()).any(is_exec_live))
            .unwrap_or(false);
        if has_live {
            continue; // 该 workspace 仍被存活 exec 使用，由其 shim 收尾时释放
        }
        match finalize_release(
            runtime_dir,
            &entry.path(),
            Path::new(workspace),
            &state,
            spec,
        ) {
            Ok(()) => swept += 1,
            Err(err) => eprintln!(
                "{} 注入残留清扫失败（{}）：{err}（本次跳过，下次启动重试）",
                spec.ns,
                entry.path().display()
            ),
        }
    }
    // 上面每个 workspace 的 finalize_release 已经清了自己追踪到的
    // launcher；这里只兜底真正的孤儿（例如 state 目录在记录 `workspace`
    // 之前就整个丢失）。绝不动 persistent.sh，也不动存活 exec 的 launcher
    // （P-9）。
    if let Ok(entries) = std::fs::read_dir(spec.launchers_dir(runtime_dir)) {
        let persistent = launcher_path(runtime_dir, spec, "persistent");
        for entry in entries.flatten() {
            let path = entry.path();
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if path != persistent && !is_exec_live(&id) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(swept)
}
```

`write_atomic` 替换为：

```rust
/// 原子写：同目录 tmp + fsync + rename（F-U3-8——用户工作区文件不能半截写
/// 坏）。§39 mcp-write-atomic-fsync：rename 前 `sync_all`（F-U4a-4 同款），
/// 断电不留 0 字节的 backup.json / state.json / 用户 mcp.json；rename 后
/// best-effort 同步目录项。权限保持默认（用户工作区文件，不套 0600）。
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp-{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("json"),
        std::process::id()
    ));
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write tmp: {e}"));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        // F-U3-13：rename 失败不留 tmp——否则每次失败都在用户工作区（或
        // state 目录）多一个孤儿文件。
        let _ = std::fs::remove_file(&tmp);
        format!("rename: {e}")
    })?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}
```

- [ ] **Step 4: agents.rs——删除四项与残留清扫**

在 `delete_agent` 之前加：

```rust
/// 删除残留目录判定的最小年龄：锁文件这么久没动才算残留（更新的可能是
/// 一次正在进行的 register_agent——update_json 先建目录与锁、后写 agent.json）。
const RESIDUE_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// §41 delete-loser-empty-dir：只含 companion 锁文件（无 agent.json、无
/// `.corrupt-*` 留证）的 agent 目录是竞争残留——删锁文件后 `remove_dir`
/// （目录非空即失败，不会误删）。
fn remove_residue_dir(dir: &Path) {
    let file = dir.join("agent.json");
    if file.exists() || has_unrecovered_corrupt(&file) {
        return;
    }
    let _ = std::fs::remove_file(dir.join("agent.json.lock"));
    let _ = std::fs::remove_dir(dir);
}

fn is_stale_residue(dir: &Path) -> bool {
    let only_lock = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .all(|e| e.file_name().to_string_lossy() == "agent.json.lock")
        })
        .unwrap_or(false);
    only_lock
        && std::fs::metadata(dir.join("agent.json.lock"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .is_some_and(|age| age >= RESIDUE_MIN_AGE)
}

/// §41 trash-never-swept：启动清扫——各 home 下删除失败遗留的 `.trash-*`
/// 目录，以及竞争败方留下的、锁文件超过 RESIDUE_MIN_AGE 未动的空 agent
/// 目录。返回清掉的目录数；单项失败 `eprintln!` 后继续。
pub fn sweep_agent_residue(runtime_dir: &Path) -> usize {
    let mut swept = 0usize;
    for home_id in layout::home_ids(runtime_dir) {
        let Ok(home) = layout::home_dir(runtime_dir, &home_id) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&home) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".trash-") {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => swept += 1,
                    Err(err) => eprintln!("启动清扫：删除 {} 失败：{err}", path.display()),
                }
            } else if !name.starts_with('.') && is_stale_residue(&path) {
                remove_residue_dir(&path);
                if !path.exists() {
                    swept += 1;
                }
            }
        }
    }
    swept
}
```

`delete_agent` 函数体替换为（守卫逻辑不变；原守卫上方关于 §45 P3-3 收尾窗口的长注释可原样保留在 `if !force` 之前）：

```rust
pub fn delete_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    force: bool,
) -> Result<(), String> {
    let agent_file = layout::agent_file(runtime_dir, home_id, agent_id)?;
    let dir = layout::agent_dir(runtime_dir, home_id, agent_id)?;
    let corrupt_err = || {
        format!(
            "删除失败：{} 已损坏，拒绝解析进程身份",
            agent_file.display()
        )
    };
    if !agent_file.is_file() {
        // §41 quarantined-delete-says-missing：agent.json 已被隔离为
        // .corrupt-* 时报「已损坏」而不是「不存在」
        if has_unrecovered_corrupt(&agent_file) {
            return Err(corrupt_err());
        }
        remove_residue_dir(&dir);
        return Err(format!("agent {agent_id} 不存在"));
    }
    let guard = lock_exclusive(&agent_file, LOCK_ACQUIRE_TIMEOUT)?;
    // 锁内复核：文件可能已被并发删除赢走，或损坏（含粘性留证）——后者拒绝
    // 删除，与 get_agent 的语义一致（无法证明进程身份就不动它）。
    let corrupt = has_unrecovered_corrupt(&agent_file);
    let record = match std::fs::read(&agent_file) {
        Ok(bytes) => match serde_json::from_slice::<AgentRecord>(&bytes) {
            Ok(record) => record,
            Err(_) => return Err(corrupt_err()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if corrupt {
                return Err(corrupt_err());
            }
            // §41 delete-loser-empty-dir：并发删除的胜方已把目录改名，本方的
            // lock_exclusive（create_dir_all + 建锁文件）在原位置重建了只含
            // 锁文件的空目录——锁内清掉
            remove_residue_dir(&dir);
            return Err(format!("agent {agent_id} 不存在"));
        }
        Err(e) => return Err(format!("删除失败：read agent.json: {e}")),
    };
    if corrupt {
        return Err(corrupt_err());
    }
    if record.id != agent_id {
        return Err(format!("agent {agent_id} 不存在"));
    }
    if matches!(
        record.status,
        AgentRecordStatus::Starting | AgentRecordStatus::Running
    ) {
        return Err(format!("agent {agent_id} 仍在运行——请先停止再删除"));
    }
    if record.attachment == AgentAttachment::Orphan {
        return Err(format!(
            "agent {agent_id} 处于不受控态（pid 存活）——请先强制终止再删除"
        ));
    }
    // F-EXEC-13(2) / F-EXEC-14(1)：任一 exec 仍是待对账目标（谓词 =
    // is_reconcile_target，与 list_reconcile_targets 同一函数）时拒绝删除；
    // F-EXEC-14(4)：force=true 绕过本守卫（放弃租约强制删除，调用方先经
    // executor::force_release_agent_leases 尽力释放）。
    if !force && record.execs.iter().any(is_reconcile_target) {
        return Err(format!(
            "agent {agent_id} 存在待对账的租约——请等待对账完成后再删除（或放弃租约强制删除）"
        ));
    }
    // P-7：按 agent 拆目录后，transcript 随 agent 一起删除。改名仍在锁内：
    // append_exec 在锁的另一侧只会看到「文件不存在」，不会写进将删的目录。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let parent = dir
        .parent()
        .ok_or_else(|| format!("删除失败：invalid agent dir {}", dir.display()))?;
    let trash = parent.join(format!(".trash-{agent_id}-{ts}"));
    std::fs::rename(&dir, &trash).map_err(|e| format!("删除失败：rename: {e}"))?;
    // §41 remove-dir-under-lock：改名完成即放锁——remove_dir_all 可能很慢，
    // 持锁期间同进程的 update_json 会拿着全局 STORE_LOCK 干等
    drop(guard);
    if let Err(err) = std::fs::remove_dir_all(&trash) {
        // §41 trash-never-swept：失败不再静默——由 startup_housekeeping 的
        // sweep_agent_residue 清扫
        eprintln!(
            "删除 agent {agent_id}：清理 {} 失败（{err}），留给启动清扫",
            trash.display()
        );
    }
    Ok(())
}
```

把 `delete_agent` 文档注释里「改名后仍在锁内 best-effort 移除；移除失败只留下对列表隐形的 .trash 残留，不报错」这一句改为「改名后立即放锁，再 best-effort 移除；移除失败记日志，`.trash-*` 由启动清扫兜底」。

`append_exec` 中 `if !outcome.changed {` 一段替换为：

```rust
    if !outcome.changed {
        // §41：并发删除赢了——记录在等锁期间被移走，本方的 update_json 在原
        // 位置重建了只含锁文件的空目录。报「不存在」并清掉它
        if !path.is_file() {
            if let Some(dir) = path.parent() {
                remove_residue_dir(dir);
            }
            return Err(format!("append_exec: agent {agent_id} 不存在"));
        }
        if orphan_rejected {
            return Err(format!(
                "append_exec: agent {agent_id} 处于不受控态（pid 存活）——请先强制终止再续跑"
            ));
        }
        return Err(format!(
            "append_exec: agent {agent_id} 不可追加（harness 不一致/运行中）"
        ));
    }
```

- [ ] **Step 5: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result|FAILED'
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```
Expected：全部 `0 failed`（`delete_agent_guards_and_removes` 二删仍报「不存在」；既有 `transcript_tail_*` 与 mcp_injection 测试不变）；fmt / clippy 干净。

- [ ] **Step 6: 提交**

```bash
git add crates/omt-exec
git commit -m "fix(exec): storage and security P3s

- §39: transcript_tail validates path segments (no ../ traversal) and
  falls back to the legacy dir when a migration stopped half-way.
- §39: MCP crash-residue sweep reads state under the lock; write_atomic
  fsyncs before rename.
- §41: delete_agent reports quarantined records as corrupt, cleans the
  lock-only dir a losing racer recreates, releases the lock before
  remove_dir_all, and logs trash failures; startup sweeps .trash-* and
  stale lock-only agent dirs.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B6: 旧版 home 降级的说明与恢复入口（§41 legacy-degraded-permanent）

**Files:**
- Modify: `crates/omt-exec/src/agents.rs`（`agents_list_value` 增字段；新增 `archive_legacy_store` 与两个私有 helper）
- Modify: `apps/desktop/src-tauri/src/agents.rs`（新 command `agents_archive_legacy`）、`apps/desktop/src-tauri/src/lib.rs`（注册 command）
- Modify: `apps/desktop/src/types.ts`、`apps/desktop/src/bridge.ts`、`apps/desktop/src/store.ts`、`apps/desktop/src/App.tsx`、`apps/desktop/src/AgentsPanel.tsx`、`apps/desktop/src/styles.css`
- Test: `agents.rs` 的 `mod tests`；新建 `apps/desktop/tests/exec-plane-p3.spec.tsx`（B7 继续往里加用例）

**Interfaces:**
- Produces（`agents_list` 响应新增两个字段，既有字段不变）：
  - `legacyUnmigrated: null | { path: string, present: boolean, parseable: boolean, corruptEvidence: string[] }`——home 级旧 `agents.json` 仍在（`present`），或只剩它的 `.corrupt-*` 留证；`parseable` = 旧文件能按 `AgentsFile` 解析（能解析的由启动迁移处理，不提供归档）。
  - `corruptAgents: { agentId: string, dir: string, evidence: string[] }[]`——`agent.json` 解析失败或留有 `.corrupt-*` 证据的 agent：目录绝对路径与留证文件名（横幅据此告诉用户删哪个文件、或改名回 `agent.json` 即可解冻）。
- Produces: `pub fn agents::archive_legacy_store(runtime_dir: &Path, home_id: &str) -> Result<Vec<String>, String>`——在 `agents/.migrate` 全局锁内，把不可解析的旧 `agents.json` 改名为 `<home>/.recovered/agents.json.<epoch-ms>`，并把 `agents.json.corrupt-*` 留证原名移入 `<home>/.recovered/`；返回移走的文件名。旧文件可解析时拒绝。归档后 `list_agents` 的 parseFailed 不再因旧文件置位，GC 冻结随之解除。
  - 与任务描述的差异：归档目录用点前缀的 `.recovered/` 而不是 `recovered/`。`layout::subdirs` 跳过点前缀目录，否则 `recovered` 会被当成一个 agent id 列出来（B5 的残留清扫同样跳过它）。
- Produces: tauri command `agents_archive_legacy(home_id) -> { archived: string[] }`；`bridge.agentsArchiveLegacy(homeId)`；`store.archiveLegacyAgents(): Promise<string[]>`——成功后重拉 agents 与 bindings 并返回移走的文件名；失败**抛出**，不写 `state.error`（A8 的 5s 静默轮询保留全局错误，但别的刷新会清掉它——归档失败要留在横幅里，见下）。
- 前端：`AgentsPanel` 新增可选 props `legacyUnmigrated` / `corruptAgents` / `onArchiveLegacy`。两处横幅「agents.json 解析失败——已降级为空列表（损坏文件已留证 .corrupt-*）」改为 `DegradedBanner`，按来源如实说明（列表并不为空、旧文件没有被改名）。横幅出现条件为 `parseFailed || legacyUnmigrated || corruptAgents.length > 0`。
  - 旧文件损坏：两步控件，与 `ConfirmStopAllButton` 同形——「确认并归档」→「确认归档」/「取消」；进行中按钮显示「归档中…」并禁用；失败时错误**就地**显示在横幅里、按钮旁（「归档失败：…」），不依赖全局 `state.error`；成功后面板显示可关闭的状态行「已归档 N 个文件到 .recovered/：<文件名>」（横幅随降级解除而消失，状态行由面板持有）。
  - 单个 agent 记录损坏：每个 agent 一行，写明目录路径与 `.corrupt-*` 留证文件名，并说明解冻办法：删除该留证文件，或修复后改名回 `agent.json`，下次刷新生效。
- 既有测试：`git grep -n "已降级为空列表" apps/desktop/tests` 为空，旧文案没有被既有用例钉住，改动不触碰既有断言。

- [ ] **Step 1: 写失败测试（Rust）**

`crates/omt-exec/src/agents.rs` 的 `mod tests` 加：

```rust
    /// §41 legacy-degraded-permanent：旧版 home 级文件损坏 → agents_list
    /// 结构化说明降级来源；「确认并归档」后移入 .recovered/、降级解除、
    /// .recovered 不进 agent 列表。
    #[test]
    fn corrupt_legacy_store_is_reported_and_archivable() {
        let (_dir, runtime, home) = temp_runtime();
        register_agent(&runtime, &home, "pi", new_exec("exec-new")).unwrap();
        let home_dir = layout::home_dir(&runtime, &home).unwrap();
        let legacy = home_dir.join(format!("{}.json", "agents"));
        std::fs::write(&legacy, "{ not json").unwrap();

        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["parseFailed"], true);
        assert_eq!(value["agents"].as_array().unwrap().len(), 1, "列表并不为空");
        let info = &value["legacyUnmigrated"];
        assert_eq!(info["present"], true);
        assert_eq!(info["parseable"], false);
        assert!(info["path"].as_str().unwrap().ends_with(".json"), "{info}");
        assert_eq!(value["corruptAgents"], serde_json::json!([]));

        let moved = archive_legacy_store(&runtime, &home).unwrap();
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert!(!legacy.exists());
        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["parseFailed"], false, "归档后冻结解除");
        assert!(value["legacyUnmigrated"].is_null());
        let recovered: Vec<_> = std::fs::read_dir(home_dir.join(".recovered"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(recovered.len(), 1);
        assert_eq!(layout::agent_ids(&runtime, &home).unwrap().len(), 1, ".recovered 不是 agent");
    }

    /// 可解析的旧文件由启动迁移处理——拒绝归档（否则其中的 agent 会从列表
    /// 消失）；损坏的单个 agent.json 进 corruptAgents（目录路径 + 留证文件名）。
    #[test]
    fn parseable_legacy_store_refuses_archive_and_corrupt_agents_are_listed() {
        let (_dir, runtime, home) = temp_runtime();
        let agent = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let home_dir = layout::home_dir(&runtime, &home).unwrap();
        let legacy = home_dir.join(format!("{}.json", "agents"));
        std::fs::write(&legacy, serde_json::to_string(&AgentsFile::default()).unwrap()).unwrap();
        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["legacyUnmigrated"]["parseable"], true);
        let err = archive_legacy_store(&runtime, &home).unwrap_err();
        assert!(err.contains("自动迁移"), "{err}");
        assert!(legacy.exists());

        std::fs::remove_file(&legacy).unwrap();
        std::fs::write(layout::agent_file(&runtime, &home, &agent.id).unwrap(), "{ broken").unwrap();
        let value = agents_list_value(&runtime, &home).unwrap();
        assert!(value["legacyUnmigrated"].is_null());
        let corrupt = value["corruptAgents"].as_array().unwrap();
        assert_eq!(corrupt.len(), 1, "{value}");
        assert_eq!(corrupt[0]["agentId"], serde_json::json!(agent.id));
        assert!(corrupt[0]["dir"].as_str().unwrap().ends_with(&agent.id), "{value}");
        let evidence = corrupt[0]["evidence"][0].as_str().unwrap();
        assert!(evidence.starts_with("agent.json.corrupt-"), "{value}");
        // 解冻办法如横幅所说：删掉留证文件，下次读取即不再降级
        let dir = layout::agent_dir(&runtime, &home, &agent.id).unwrap();
        std::fs::remove_file(dir.join(evidence)).unwrap();
        let value = agents_list_value(&runtime, &home).unwrap();
        assert_eq!(value["parseFailed"], false);
        assert_eq!(value["corruptAgents"], serde_json::json!([]));
    }
```

```bash
cargo test -p omt-exec -- corrupt_legacy_store parseable_legacy_store 2>&1 | tail -5
```
Expected：编译失败（`archive_legacy_store` 未定义）。

- [ ] **Step 2: 实现（Rust 存储）**

`crates/omt-exec/src/agents.rs`，`agents_list_value` 替换为：

```rust
/// Agents 面板数据面（U5 消费）：记录列表 + `parseFailed` 标志（前端提示
/// 数据已降级；U9 据此冻结 GC）。§41：附带降级来源的结构化说明——
/// `legacyUnmigrated`（旧版 home 级文件未迁移 / 只剩留证）与
/// `corruptAgents`（单个 agent.json 损坏：目录 + 留证文件名），横幅据此
/// 如实呈现、说明解冻办法，并提供「确认并归档」。
pub fn agents_list_value(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let (agents, parse_failed) = list_agents(runtime_dir, home_id)?;
    Ok(serde_json::json!({
        "agents": agents,
        "agentSeq": agent_seq(runtime_dir, home_id)?,
        "parseFailed": parse_failed,
        "legacyUnmigrated": legacy_unmigrated(runtime_dir, home_id)?,
        "corruptAgents": corrupt_agents(runtime_dir, home_id)?,
    }))
}

/// `path` 同目录下的 `<文件名>.corrupt-*` 留证（文件名，排序）。
fn corrupt_evidence_of(path: &Path) -> Vec<String> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.corrupt-", name.to_string_lossy());
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn legacy_unmigrated(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let legacy = legacy_agents_monolith(runtime_dir, home_id)?;
    let evidence = corrupt_evidence_of(&legacy);
    let present = legacy.is_file();
    if !present && evidence.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let parseable = std::fs::read(&legacy)
        .ok()
        .is_some_and(|bytes| serde_json::from_slice::<AgentsFile>(&bytes).is_ok());
    Ok(serde_json::json!({
        "path": legacy.display().to_string(),
        "present": present,
        "parseable": parseable,
        "corruptEvidence": evidence,
    }))
}

/// §41：单个 agent.json 损坏（解析失败或留有 `.corrupt-*` 证据）的 agent——
/// 目录绝对路径 + 留证文件名。list_agents 的 parseFailed 正是因这些留证
/// 粘性置位；删掉留证（或修复后改名回 agent.json）即解冻。
fn corrupt_agents(runtime_dir: &Path, home_id: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    for agent_id in layout::agent_ids(runtime_dir, home_id)? {
        let path = layout::agent_file(runtime_dir, home_id, &agent_id)?;
        let unreadable = std::fs::read(&path)
            .ok()
            .is_some_and(|bytes| serde_json::from_slice::<AgentRecord>(&bytes).is_err());
        let evidence = corrupt_evidence_of(&path);
        if unreadable || !evidence.is_empty() {
            out.push(serde_json::json!({
                "agentId": agent_id,
                "dir": layout::agent_dir(runtime_dir, home_id, &agent_id)?.display().to_string(),
                "evidence": evidence,
            }));
        }
    }
    Ok(out)
}

/// §41 legacy-degraded-permanent：「确认并归档」——把未能迁移的旧版 home 级
/// 存储文件及其 `.corrupt-*` 留证移入 `<home>/.recovered/`（点前缀：agent
/// 列表与残留清扫都跳过它），解除 F-EXEC-1 的 GC 冻结（atomic_json 注释里
/// 说的「移入 recovered/」从此真实存在）。只在旧文件**不可解析**或只剩
/// 留证时允许：可解析的旧文件由启动迁移处理，归档会让其中的 agent 从列表
/// 消失。与迁移共用 `agents/.migrate` 全局锁。返回移走的文件名。
pub fn archive_legacy_store(runtime_dir: &Path, home_id: &str) -> Result<Vec<String>, String> {
    let legacy = legacy_agents_monolith(runtime_dir, home_id)?;
    let _guard = lock_exclusive(
        &runtime_dir.join("agents").join(".migrate"),
        LOCK_ACQUIRE_TIMEOUT,
    )?;
    if let Ok(bytes) = std::fs::read(&legacy) {
        if serde_json::from_slice::<AgentsFile>(&bytes).is_ok() {
            return Err("旧版文件可以解析——重启桌面端会自动迁移，无需归档".to_string());
        }
    }
    let home = layout::home_dir(runtime_dir, home_id)?;
    let recovered = home.join(".recovered");
    std::fs::create_dir_all(&recovered).map_err(|e| format!("create .recovered: {e}"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let mut moved = Vec::new();
    if legacy.is_file() {
        let name = format!("{}.json.{ts}", "agents");
        std::fs::rename(&legacy, recovered.join(&name))
            .map_err(|e| format!("归档 {} 失败：{e}", legacy.display()))?;
        moved.push(name);
    }
    for name in corrupt_evidence_of(&legacy) {
        std::fs::rename(home.join(&name), recovered.join(&name))
            .map_err(|e| format!("归档 {name} 失败：{e}"))?;
        moved.push(name);
    }
    let _ = std::fs::remove_file(home.join(format!("{}.json.lock", "agents")));
    if moved.is_empty() {
        return Err("没有需要归档的旧版文件".to_string());
    }
    Ok(moved)
}
```

（`format!("{}.json", "agents")` 拼名沿用本文件既有约定——迁移 grep 不命中字面量。）

```bash
cargo test -p omt-exec -- corrupt_legacy_store parseable_legacy_store 2>&1 | grep -E '^test result|FAILED'
```
Expected：`2 passed`。

- [ ] **Step 3: 写失败测试（前端）**

新建 `apps/desktop/tests/exec-plane-p3.spec.tsx`：

```tsx
/**
 * Part B（P3 清理）的前端用例：降级横幅与「确认并归档」（B6）、强制删除的
 * 释放结果、停止全部提示与焦点、续跑失败后补接（B7）。mock 边界与
 * exec-plane.spec 相同；既有 spec 不改。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { AgentRecord } from '../src/types'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: class {
    onmessage: ((message: unknown) => void) | null = null
    toJSON() {
      return '__CHANNEL__'
    }
  },
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn().mockResolvedValue(true),
  requestPermission: vi.fn().mockResolvedValue('granted'),
  sendNotification: vi.fn(),
}))

import { AgentsPanel } from '../src/AgentsPanel'

function agent(over: Partial<AgentRecord> = {}, runId: string | null = null): AgentRecord {
  return {
    id: 'agent-exec-1',
    displayName: 'Agent 1',
    title: null,
    harness: 'pi',
    status: 'starting',
    attachment: 'live',
    lastExecId: 'exec-1',
    lastSessionId: null,
    execs: [
      {
        execId: 'exec-1',
        nodeId: null,
        runId,
        startedAt: '1760000000000',
        exitCode: null,
        killReason: null,
        failureDetail: null,
        harnessSessionId: null,
        transcriptFile: null,
        binaryPath: null,
        binaryArgs: [],
        pid: 123,
        processStartId: 'macos:1:1',
      },
    ],
    ...over,
  }
}

function renderPanel(props: Partial<Parameters<typeof AgentsPanel>[0]> = {}) {
  return render(
    <AgentsPanel
      agents={[agent()]}
      parseFailed={false}
      selectedAgentId="agent-exec-1"
      nodes={[]}
      fetchRun={vi.fn().mockResolvedValue({ run: { id: 'RUN-1', status: 'running' }, items: [] })}
      eventsOf={() => []}
      homeId="h_test"
      loadHistoryTail={vi.fn()}
      onSelectAgent={vi.fn()}
      onStopAgent={vi.fn()}
      onResumeAgent={vi.fn()}
      onDeleteAgent={vi.fn()}
      onKillOrphan={vi.fn()}
      onOpenHarnessSettings={vi.fn()}
      {...props}
    />,
  )
}

describe('B6 legacy-degraded banner (§41)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const corruptLegacy = {
    path: '/rt/agents/h_test/agents.json',
    present: true,
    parseable: false,
    corruptEvidence: [],
  }

  it('旧文件损坏：如实说明路径与冻结，不再声称「已降级为空列表」', () => {
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: vi.fn() })
    expect(screen.getByText(/\/rt\/agents\/h_test\/agents\.json/)).toBeTruthy()
    expect(screen.getByText(/绑定清理已冻结/)).toBeTruthy()
    expect(screen.queryByText(/已降级为空列表/)).toBeNull()
  })

  it('「确认并归档」是两步控件：先确认、可取消，确认后才调用；成功显示可关闭的状态行', async () => {
    const archive = vi.fn().mockResolvedValue(['agents.json.1760000000000', 'agents.json.corrupt-1'])
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: archive })
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    expect(archive).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: '取消' }))
    expect(screen.getByRole('button', { name: '确认并归档' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    expect(archive).toHaveBeenCalledTimes(1)
    const done = screen.getByText(/已归档 2 个文件到 \.recovered\//)
    expect(done.textContent).toContain('agents.json.corrupt-1')
    fireEvent.click(screen.getByRole('button', { name: '知道了' }))
    expect(screen.queryByText(/已归档 2 个文件/)).toBeNull()
  })

  it('归档进行中按钮禁用', async () => {
    const archive = vi.fn(() => new Promise<string[]>(() => {}))
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: archive })
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    expect(screen.getByRole('button', { name: '归档中…' })).toHaveProperty('disabled', true)
  })

  it('归档失败：错误就地显示在横幅里、按钮旁，按钮复位可重试', async () => {
    const archive = vi.fn().mockRejectedValue('rename: Permission denied')
    renderPanel({ parseFailed: true, legacyUnmigrated: corruptLegacy, onArchiveLegacy: archive })
    fireEvent.click(screen.getByRole('button', { name: '确认并归档' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认归档' }))
    })
    const inline = screen.getByText(/归档失败：rename: Permission denied/)
    expect(inline.closest('.degraded-banner')).not.toBeNull()
    expect(screen.getByRole('button', { name: '确认并归档' })).toBeTruthy()
  })

  it('空列表分支同样显示横幅', () => {
    renderPanel({
      agents: [],
      parseFailed: true,
      legacyUnmigrated: corruptLegacy,
      onArchiveLegacy: vi.fn(),
    })
    expect(screen.getByRole('button', { name: '确认并归档' })).toBeTruthy()
  })

  it('可解析的旧文件：提示重启自动迁移，无归档入口', () => {
    renderPanel({
      parseFailed: true,
      legacyUnmigrated: { ...corruptLegacy, parseable: true },
      onArchiveLegacy: vi.fn(),
    })
    expect(screen.getByText(/重启桌面端会自动迁移/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: '确认并归档' })).toBeNull()
  })

  it('单个 agent 记录损坏：写明目录与留证文件名和解冻办法；横幅不依赖 parseFailed', () => {
    const { unmount } = renderPanel({
      parseFailed: false,
      corruptAgents: [
        {
          agentId: 'agent-exec-9',
          dir: '/rt/agents/h_test/agent-exec-9',
          evidence: ['agent.json.corrupt-1760000000000'],
        },
      ],
    })
    const line = screen.getByText(/\/rt\/agents\/h_test\/agent-exec-9/)
    expect(line.textContent).toContain('agent.json.corrupt-1760000000000')
    expect(line.textContent).toContain('下次刷新')
    unmount()
    renderPanel({ parseFailed: true })
    expect(screen.getByText(/部分 agent 记录读取失败；绑定清理已冻结/)).toBeTruthy()
  })
})

describe('B6 archive store action (§41)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const setupStore = async () => {
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    await act(async () => {
      await result.current.selectHome(home)
    })
    return { result, unmount }
  }

  it('archiveLegacyAgents → agents_archive_legacy，随后重拉 agents 与 bindings，返回文件名', async () => {
    const { result, unmount } = await setupStore()
    invokeMock.mockClear()
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === 'agents_archive_legacy' ? { archived: ['agents.json.1'] } : {},
    )
    let archived: unknown
    await act(async () => {
      archived = await result.current.archiveLegacyAgents()
    })
    expect(archived).toEqual(['agents.json.1'])
    const cmds = invokeMock.mock.calls.map(c => c[0])
    expect(invokeMock).toHaveBeenCalledWith('agents_archive_legacy', { homeId: 'h1' })
    expect(cmds.indexOf('agents_archive_legacy')).toBeLessThan(cmds.lastIndexOf('agents_list'))
    expect(cmds).toContain('bindings_get')
    unmount()
  })

  it('归档失败：archiveLegacyAgents 抛出，不写全局 error（错误由横幅就地呈现）', async () => {
    const { result, unmount } = await setupStore()
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === 'agents_archive_legacy') throw 'rename: Permission denied'
      return {}
    })
    await act(async () => {
      await expect(result.current.archiveLegacyAgents()).rejects.toBe('rename: Permission denied')
    })
    expect(result.current.state.error).toBeNull()
    unmount()
  })
})
```

```bash
cd apps/desktop && pnpm test -- exec-plane-p3 2>&1 | tail -15
```
Expected：B6 的 9 个用例 FAIL（找不到「确认并归档」/ 留证文件名、`archiveLegacyAgents` 不是函数）。

- [ ] **Step 4: 实现（desktop 后端）**

`apps/desktop/src-tauri/src/agents.rs` 末尾加：

```rust
/// §41 legacy-degraded-permanent：「确认并归档」未能迁移的旧版存储文件
/// （见 omt_exec::agents::archive_legacy_store）。返回 `{ archived: [文件名] }`。
#[tauri::command(async)]
pub fn agents_archive_legacy(home_id: String) -> Result<serde_json::Value, String> {
    let archived = omt_exec::agents::archive_legacy_store(&daemon::runtime_dir(), &home_id)?;
    Ok(serde_json::json!({ "archived": archived }))
}
```

`apps/desktop/src-tauri/src/lib.rs` 的 `generate_handler!` 在 `agents::agents_delete,` 之后加一行：

```rust
            agents::agents_archive_legacy,
```

- [ ] **Step 5: 实现（前端）**

`apps/desktop/src/types.ts`，`AgentsListResult` 之前加并扩展接口：

```ts
/** §41：未能迁移的旧版 home 级 agents.json（损坏 / 尚未迁移 / 只剩 .corrupt-* 留证）。 */
export interface LegacyUnmigrated {
  path: string
  /** 旧文件仍在原位 */
  present: boolean
  /** 可按旧格式解析（启动迁移会处理；不提供归档） */
  parseable: boolean
  /** 同目录的 `.corrupt-*` 留证文件名 */
  corruptEvidence: string[]
}

/** §41：agent.json 已损坏的 agent——目录与留证文件名（解冻：删留证或改名回 agent.json）。 */
export interface CorruptAgent {
  agentId: string
  dir: string
  evidence: string[]
}

export interface AgentsListResult {
  agents: AgentRecord[]
  agentSeq: number
  parseFailed: boolean
  /** §41：旧 fixture / 旧后端缺省按 null 处理 */
  legacyUnmigrated?: LegacyUnmigrated | null
  /** §41：缺省按空处理 */
  corruptAgents?: CorruptAgent[]
}
```

`apps/desktop/src/bridge.ts`，`agentsList` 之后加：

```ts
/** §41：「确认并归档」未能迁移的旧版 agents.json（移入 `<home>/.recovered/`，
 * 解除绑定清理冻结）。旧文件可解析时 Rust 侧拒绝（由启动迁移处理）。 */
export async function agentsArchiveLegacy(homeId: string): Promise<{ archived: string[] }> {
  return invoke<{ archived: string[] }>('agents_archive_legacy', { homeId })
}
```

`apps/desktop/src/store.ts`：

1. bridge import 列表加 `agentsArchiveLegacy,`；types import 加 `CorruptAgent`、`LegacyUnmigrated`：

```ts
import type {
  AgentRecord,
  CorruptAgent,
  HomeInfo,
  LegacyUnmigrated,
  OmtNode,
  RunDetail,
  RunSummary,
  SavedFilters,
} from './types'
```

（A8 若已在这行加过别的类型，保留它们，只补这两个。）

2. `TreeState` 在 `parseFailed: boolean` 之后加：

```ts
  /** §41：旧版存储未迁移的说明（横幅 + 「确认并归档」） */
  legacyUnmigrated: LegacyUnmigrated | null
  /** §41：agent.json 已损坏的 agent（目录 + 留证文件名） */
  corruptAgents: CorruptAgent[]
```

`INITIAL` 在 `parseFailed: false,` 之后加 `legacyUnmigrated: null,` 与 `corruptAgents: [],`；`selectHome` 的重置块（`parseFailed: false,` 之后）同样加这两行。

3. `refreshAgents`（A8 之后签名为 `refreshAgents(opts?: { silent?: boolean })`）的 `setState` 在 `parseFailed: result.parseFailed ?? false,` 之后加：

```ts
        legacyUnmigrated: result.legacyUnmigrated ?? null,
        corruptAgents: result.corruptAgents ?? [],
```

4. `gcBindings` 之后加：

```ts
  // §41：「确认并归档」旧版存储——成功后重拉 agents（降级解除）与绑定表
  //（GC 冻结解除后悬空计数才可信），返回移走的文件名供面板显示。失败**抛出**、
  // 不写 state.error：全局错误会被其他刷新清掉，归档失败由横幅就地呈现。
  const archiveLegacyAgents = useCallback(async (): Promise<string[]> => {
    const home = activeHomeRef.current
    if (!home) throw new Error('无活动 workspace')
    const { archived } = await agentsArchiveLegacy(home.homeId)
    await refreshAgents()
    await refreshBindings()
    return archived
  }, [refreshAgents, refreshBindings])
```

返回对象在 `gcBindings,` 之后加 `archiveLegacyAgents,`。

`apps/desktop/src/App.tsx` 的 `<AgentsPanel` 在 `parseFailed={state.parseFailed}` 之后加：

```tsx
                    legacyUnmigrated={state.legacyUnmigrated}
                    corruptAgents={state.corruptAgents}
                    onArchiveLegacy={store.archiveLegacyAgents}
```

`apps/desktop/src/AgentsPanel.tsx`：

1. types import 加 `CorruptAgent`、`LegacyUnmigrated`：

```ts
import type {
  AgentRecord,
  CorruptAgent,
  ExecRecord,
  LegacyUnmigrated,
  OmtNode,
  RunDetail,
  RunItemView,
} from './types'
```

2. `statsOf` 之后加组件：

```tsx
/**
 * §41 legacy-degraded-permanent：降级横幅如实说明来源——旧版 home 级文件
 * 未迁移（损坏 / 尚未迁移 / 只剩留证）与单个 agent 记录损坏分别呈现。旧文案
 * 「已降级为空列表（损坏文件已留证 .corrupt-*）」与事实不符：列表并不为空，
 * 旧文件也没有被改名。
 * - 旧文件不可解析：两步控件「确认并归档」→「确认归档」/「取消」（与
 *   ConfirmStopAllButton 同形），进行中禁用；失败就地显示在按钮旁（不依赖
 *   全局 error——其他刷新会清掉它）。成功提示由面板持有（横幅随降级解除消失）。
 * - 单个 agent 损坏：写明目录与留证文件名，以及解冻办法。
 */
export function DegradedBanner({
  legacy,
  corruptAgents,
  onArchive,
}: {
  legacy: LegacyUnmigrated | null
  corruptAgents: CorruptAgent[]
  onArchive?: () => Promise<void>
}) {
  const [step, setStep] = useState<'idle' | 'armed' | 'busy'>('idle')
  const [archiveError, setArchiveError] = useState<string | null>(null)
  const lines: string[] = []
  if (legacy && legacy.parseable) {
    lines.push(
      `旧版 agents.json 尚未迁移（${legacy.path}）：重启桌面端会自动迁移；迁移完成前绑定清理已冻结。`,
    )
  } else if (legacy) {
    const evidence =
      legacy.corruptEvidence.length > 0 ? `；损坏留证：${legacy.corruptEvidence.join('、')}` : ''
    lines.push(
      legacy.present
        ? `旧版 agents.json 已损坏、未能迁移（${legacy.path}${evidence}）：其中的 agent 不在下方列表中，为防误删，绑定清理已冻结。`
        : `旧版 agents.json 曾损坏，留证尚未处理（${legacy.path}${evidence}）：为防误删，绑定清理已冻结。`,
    )
  }
  for (const corrupt of corruptAgents) {
    const evidence =
      corrupt.evidence.length > 0 ? `留证 ${corrupt.evidence.join('、')}` : 'agent.json 无法解析'
    lines.push(
      `agent 记录已损坏：${corrupt.dir}（${evidence}），该 agent 未在列表中显示，绑定清理已冻结。删除该留证文件，或修复后改名回 agent.json，下次刷新即解冻。`,
    )
  }
  if (lines.length === 0) lines.push('部分 agent 记录读取失败；绑定清理已冻结。')
  const archivable = legacy !== null && !legacy.parseable && onArchive !== undefined
  const runArchive = async () => {
    if (!onArchive) return
    setStep('busy')
    setArchiveError(null)
    try {
      await onArchive()
    } catch (err) {
      setArchiveError(err instanceof Error ? err.message : String(err))
    } finally {
      setStep('idle')
    }
  }
  return (
    <div className="error-banner degraded-banner" role="alert">
      {lines.map(line => (
        <p key={line}>{line}</p>
      ))}
      {archivable && (
        <>
          <p className="hm-hint">
            「确认并归档」会把旧文件与留证移入同目录的 .recovered/，绑定清理随之解冻——指向这些旧 agent
            的绑定会被视为失效。
          </p>
          <div className="chip-row">
            {step === 'idle' && (
              <button
                className="chip"
                onClick={() => {
                  setArchiveError(null)
                  setStep('armed')
                }}
              >
                确认并归档
              </button>
            )}
            {step === 'armed' && (
              <>
                <button className="chip active" onClick={() => void runArchive()}>
                  确认归档
                </button>
                <button className="chip" onClick={() => setStep('idle')}>
                  取消
                </button>
              </>
            )}
            {step === 'busy' && (
              <button className="chip" disabled>
                归档中…
              </button>
            )}
            {archiveError && <span className="archive-error">归档失败：{archiveError}</span>}
          </div>
        </>
      )}
    </div>
  )
}
```

3. `Props` 在 `parseFailed: boolean` 之后加：

```ts
  /** §41：旧版存储未迁移的说明（缺省 null） */
  legacyUnmigrated?: LegacyUnmigrated | null
  /** §41：agent.json 已损坏的 agent（缺省空） */
  corruptAgents?: CorruptAgent[]
  /** §41：「确认并归档」（App → store.archiveLegacyAgents），返回移走的文件名；失败抛出 */
  onArchiveLegacy?: () => Promise<string[]>
```

解构参数在 `parseFailed,` 之后加 `legacyUnmigrated = null,`、`corruptAgents = EMPTY_CORRUPT,`、`onArchiveLegacy,`；文件顶部（`latestExecOf` 之前）加 `const EMPTY_CORRUPT: CorruptAgent[] = []`。

4. 其他 `useState` 旁边加归档成功提示的状态：

```tsx
  /** §41：归档成功提示（横幅随降级解除消失，提示由面板持有，手动关闭） */
  const [archiveNotice, setArchiveNotice] = useState<string | null>(null)
```

`const dangling = ...` 之前加：

```tsx
  // §41：降级横幅（空列表分支与列表分支共用）。条件包含 corruptAgents——单个
  // agent 损坏时 parseFailed 也为真，但横幅不依赖这一点
  const handleArchive = onArchiveLegacy
    ? async () => {
        const archived = await onArchiveLegacy()
        setArchiveNotice(`已归档 ${archived.length} 个文件到 .recovered/：${archived.join('、')}`)
      }
    : undefined
  const degradedBanner =
    parseFailed || legacyUnmigrated || corruptAgents.length > 0 ? (
      <DegradedBanner
        legacy={legacyUnmigrated}
        corruptAgents={corruptAgents}
        onArchive={handleArchive}
      />
    ) : null
  const archiveStatus = archiveNotice ? (
    <p className="agents-notice" role="status">
      {archiveNotice}
      <button className="chip" onClick={() => setArchiveNotice(null)}>
        知道了
      </button>
    </p>
  ) : null
```

5. 两处

```tsx
        {parseFailed && (
          <p className="error-banner">agents.json 解析失败——已降级为空列表（损坏文件已留证 .corrupt-*）</p>
        )}
```

都替换为：

```tsx
        {degradedBanner}
        {archiveStatus}
```

6. `apps/desktop/src/styles.css` 末尾加：

```css
/* §41：归档失败就地提示（降级横幅内、按钮旁） */
.degraded-banner .archive-error {
  margin-left: 8px;
  font-weight: 600;
}
```

- [ ] **Step 6: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED'
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result|FAILED'
cd apps/desktop && pnpm test -- exec-plane-p3 2>&1 | tail -5
cd apps/desktop && pnpm test 2>&1 | grep -E 'Tests +[0-9]+' && pnpm exec tsc --noEmit
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```
Expected：Rust 全绿；`exec-plane-p3` B6 段 9 个用例通过；桌面 vitest 全部通过、tsc 干净；fmt / clippy 干净。

- [ ] **Step 7: 提交**

```bash
git add crates/omt-exec/src/agents.rs apps/desktop/src-tauri/src/agents.rs apps/desktop/src-tauri/src/lib.rs \
  apps/desktop/src/types.ts apps/desktop/src/bridge.ts apps/desktop/src/store.ts apps/desktop/src/App.tsx \
  apps/desktop/src/AgentsPanel.tsx apps/desktop/src/styles.css apps/desktop/tests/exec-plane-p3.spec.tsx
git commit -m "feat(desktop): explain degraded agent storage and offer archive (§41)

agents_list now reports why it is degraded (unmigrated/corrupt legacy
store, corrupt agent records with their dir and evidence file). The
banner says what actually happened instead of 'degraded to an empty
list' and how to clear a corrupt record; a corrupt legacy store can be
archived into .recovered/ through a two-step control that shows its
result or error inline, lifting the GC freeze.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B7: 桌面 UI（P3-ACC-6 Esc / §47 释放结果与总时限 / 强删后的悬空绑定 / §39 前端三项 / §39 清扫挪进 setup / §39 跟随者）

**Files:**
- Create: `apps/desktop/src-tauri/src/quit_dialog.rs`
- Modify: `apps/desktop/src-tauri/Cargo.toml`（macOS 专属的 objc2 依赖）、`apps/desktop/src-tauri/src/lib.rs`
- Modify: `crates/omt-exec/src/executor.rs`（`force_release_agent_leases` 总时限）、`crates/omt-exec/src/shim.rs`（`run_bounded` 改 `pub(crate)`）、`crates/omt-exec/src/agents.rs`（`remove_bindings_to`；`delete_agent` 调用）
- Modify: `crates/omt-exec/src/follow.rs`（`FollowOptions` / `follow_jsonl_with`）、`apps/desktop/src-tauri/src/executor.rs`（每 exec 一个跟随者）
- Modify: `apps/desktop/src/types.ts`、`apps/desktop/src/store.ts`、`apps/desktop/src/App.tsx`、`apps/desktop/src/AgentsPanel.tsx`、`apps/desktop/src/agentUi.tsx`、`apps/desktop/src/styles.css`
- Test: `quit_dialog.rs` / `executor.rs`（omt-exec 与桌面端）/ `agents.rs` / `follow.rs` 的 `mod tests`；`apps/desktop/tests/exec-plane-p3.spec.tsx`（续写）

**Interfaces:**
- Produces: `quit_dialog::{QuitChoice, choice_from_response, confirm_quit_dialog}`。`confirm_quit_dialog(app: &tauri::AppHandle, title: String, message: String, on_choice: impl FnOnce(QuitChoice) + Send + 'static)`：macOS 上投递到 GCD 主队列，直接构造 `NSAlert`（按钮顺序 KEEP / STOP / CANCEL，KEEP 为首个按钮 = Return，CANCEL 显式 `setKeyEquivalent("\u{1b}")` = Esc），`runModal` 返回后回调；其他平台保留 tauri-plugin-dialog 路径。
  - 根因核实（rfd 0.16.0 源码）：无父窗口时 rfd 走的不是 `NSAlert`，而是 `utils/user_alert.rs` 的 `CFUserNotificationDisplayAlert`——三个按钮都不绑 Esc，也不是 app-modal。findings log 里引用的 `message_dialog.rs:64`（`addButtonWithTitle` 逐个加按钮）是有父窗口时的 sheet 路径，同样不设 keyEquivalent。直调 `NSAlert` 同时解决两条路径。
  - 依赖版本复用 Cargo.lock 里 tauri / rfd 已经拉进来的：`objc2 0.6.4`、`objc2-app-kit 0.3.2`、`objc2-foundation 0.3.2`、`dispatch2 0.3.1`。这几个 crate 的方法在 0.3 里是安全 fn，不需要 `unsafe`（写了 `unsafe` 会触发 `unused_unsafe`，clippy `-D warnings` 失败）。
- `DIALOG_OPEN` / `QUIT_CONFIRMED` 语义不变：只有回到正常运行的分支清零 `DIALOG_OPEN`，会退出的分支保持置位直到进程退出。
- Produces: `pub(crate) fn executor::force_release_agent_leases_within(..., budget: Duration)`；`force_release_agent_leases` = 30s 总时限的包装。每个目标的释放在 `run_bounded(剩余预算)` 里跑；预算耗尽后，剩余目标记为失败 `exec <id>: 释放总时限（30s）已到，未尝试`。
- Produces: `pub fn agents::remove_bindings_to(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<usize, String>`；`delete_agent` 删除成功后调用它（普通删除与 force 删除都调用，失败只记日志）。指向已删除 agent 的绑定没有任何用处，按 agent id 精确移除不受 GC 冻结约束（冻结防的是「无法证明 agent 表有效时批量清空」）。
- 前端：
  - 被放弃租约的通知改为 tree store 里的**列表** `leaseNotices: LeaseNotice[]`（按被删 agent 键控；第二次强制删除追加、不覆盖；换 home 不清），`store.deleteAgent` 返回 `AgentsDeleteResult` 并在有 `leaseReleaseFailures` 时登记一条（文案列出 exec 与 run id，「请到 Runs 页手动取消该 run」）；`store.dismissLeaseNotice(agentId)` 逐条关闭。新组件 `agentUi.LeaseNoticeBanner({ notices, onDismiss, onOpenRuns })` 渲染在 `App.tsx` 的应用级横幅区（任何页面都可见，直到关闭），每条带「前往 Runs」（`onOpenRuns={() => setView('runs')}`，与 `onOpenHarnessSettings` 同一写法）与「知道了」。`AgentsPanel` 的删除流程与 `onDeleteAgent` 类型不变。
  - 「停止全部」的结果：成功 / 无事可做写入**常驻挂载**的 `<p role="status" aria-live="polite">`，只换文字、不重新挂载（不用 `key`），5 秒计时器由依赖 `seq` 的 effect 重置；含失败的结果（调用失败 / 全部失败 / 部分失败）进需手动关闭的 alert，不随 5 秒消失。文案与原来逐字相同。
  - 两步确认：出现时焦点落在「取消」；第二步结束（确认、取消、3s 自动复位）后焦点回到「停止全部」；Esc 取消。
  - 续跑派发失败后，派发期间被推迟的接回请求立即补接（`deferredAttachRef`），不再等下一次列表重拉。
- §39 跟随者（Step 6–7）：
  - Produces: `omt_exec::follow::{FollowOptions { appear_timeout, cancel }, follow_jsonl_with}`；`follow_jsonl` 签名不变（= 默认选项）。文件尚不存在时，exec 仍活跃就一直等，`APPEAR_TIMEOUT` 只约束已不活跃的情形（修 launcher `Ready::Timeout` 之后跟随者先放弃）；`cancel` 置位后在下一次轮询退出且不调 `on_lost`。
  - 桌面端 `executor.rs`：`claim_follower(exec_id)` / `release_follower(exec_id, token)`，`follow()`（`exec_start` 与 `exec_attach` 共用）同一 exec 只保留一个跟随者——webview 重载后的第二次 `exec_attach` 接管，旧跟随者被取消。`store.ts` 的 `attachTranscript` 无需改动。
- §39 housekeeping-before-single-instance：`startup_housekeeping` 与 `release_reconcile_targets_in_background` 从 `run()` 开头挪进 `.setup(...)`。single-instance 插件在自己的 plugin setup 里对第二实例 `exit(0)`，plugin setup 先于 app setup，所以第二实例不再跑清扫和对账。

- [ ] **Step 1: 写失败测试（Rust）**

`crates/omt-exec/src/agents.rs` 的 `mod tests` 加：

```rust
    /// 验收「强制删除」注：删除 agent 时一并移除指向它的绑定（此前留下
    /// 悬空绑定，要用户手动触发 bindings_gc）；指向别的 agent 的绑定不动。
    #[test]
    fn delete_agent_drops_bindings_that_point_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let a = exited_agent(&runtime, "exec-bind-a");
        let b = exited_agent(&runtime, "exec-bind-b");
        write_binding(&runtime, "h_test", "TICKET-1", &a.id).unwrap();
        write_binding(&runtime, "h_test", "TICKET-2", &b.id).unwrap();
        write_binding(&runtime, "h_test", "TICKET-3", &a.id).unwrap();
        delete_agent(&runtime, "h_test", &a.id, false).unwrap();
        let value = bindings_get_value(&runtime, "h_test").unwrap();
        assert_eq!(value["bindings"], serde_json::json!({ "TICKET-2": b.id }));
    }
```

`crates/omt-exec/src/executor.rs` 的 `mod tests`（`force_release_agent_leases_*` 之后）加：

```rust
    /// §47 force-release-no-bound：force 删除前的尽力释放有总时限——半卡死
    /// daemon 下「删除中…」不得持续数分钟。超出预算的目标记为失败（不阻塞），
    /// 预算耗尽后剩余目标不再尝试。
    #[test]
    fn force_release_is_bounded_by_an_overall_budget() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path();
        let target = crate::agents::register_agent(
            rt,
            "h_t",
            "pi",
            crate::agents::NewExec {
                exec_id: "exec-budget".into(),
                node_id: None,
                run_id: Some("RUN-1".into()),
                binary_path: None,
                binary_args: vec![],
                model: None,
                launched_by: None,
                parent_agent_id: None,
            },
        )
        .unwrap();
        crate::agents::update_exec(rt, "h_t", &target.id, "exec-budget", |e| {
            e.lease_release_pending = true
        })
        .unwrap();
        let mut spec = crate::spec::sample(&rt.join("ws").join(".omt"), Path::new("/bin/omt"));
        spec.home_id = "h_t".into();
        spec.agent_id = target.id.clone();
        spec.exec_id = "exec-budget".into();
        let adir = crate::layout::agent_dir(rt, "h_t", &target.id).unwrap();
        spec.write(&crate::layout::exec_file(
            &adir,
            "exec-budget",
            crate::layout::ExecFile::Spec,
        ))
        .unwrap();

        let slow: crate::daemon_link::Connector = Arc::new(|_: &Path, _: &str| {
            std::thread::sleep(Duration::from_secs(5));
            Err("late".to_string())
        });
        let started = Instant::now();
        let outcome =
            force_release_agent_leases_within(rt, &slow, "h_t", &target.id, Duration::from_millis(300))
                .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert_eq!(outcome.released, 0);
        assert_eq!(outcome.failures.len(), 1, "{outcome:?}");
        assert!(
            outcome.failures[0].contains("exec-budget") && outcome.failures[0].contains("超时"),
            "{outcome:?}"
        );

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let counting: crate::daemon_link::Connector = Arc::new(move |_: &Path, _: &str| {
            seen.fetch_add(1, Ordering::SeqCst);
            Err("unused".to_string())
        });
        let outcome =
            force_release_agent_leases_within(rt, &counting, "h_t", &target.id, Duration::ZERO)
                .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "预算耗尽后不再尝试");
        assert!(outcome.failures[0].contains("未尝试"), "{outcome:?}");
        assert!(
            crate::agents::is_reconcile_target(
                &crate::agents::get_agent(rt, "h_t", &target.id).unwrap().unwrap().execs[0]
            ),
            "失败不标记——仍是对账目标"
        );
    }
```

新建 `apps/desktop/src-tauri/src/quit_dialog.rs` 时先只写测试模块（Step 4 再补实现）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// P3-ACC-6：NSAlert 返回码按按钮**添加顺序**映射——1000 = 第一个（KEEP，
    /// Return 默认键）、1001 = 第二个（STOP）、1002 = 第三个（CANCEL，Esc）；
    /// 未知返回码按安全方向归 Cancel（不退出）。
    #[test]
    fn quit_dialog_maps_alert_responses_safely() {
        assert_eq!(choice_from_response(1000), QuitChoice::Keep);
        assert_eq!(choice_from_response(1001), QuitChoice::Stop);
        assert_eq!(choice_from_response(1002), QuitChoice::Cancel);
        assert_eq!(choice_from_response(-1000), QuitChoice::Cancel);
        assert_eq!(choice_from_response(0), QuitChoice::Cancel);
    }
}
```

`lib.rs` 加 `mod quit_dialog;`，然后：

```bash
cargo test -p omt-exec -- delete_agent_drops_bindings force_release_is_bounded 2>&1 | tail -5
cargo test -p oh-my-ticket-desktop quit_dialog 2>&1 | tail -5
```
Expected：编译失败（`force_release_agent_leases_within`、`choice_from_response`、`QuitChoice` 未定义）；`delete_agent_drops_bindings_that_point_to_it` 补齐编译后 FAIL（TICKET-1/3 仍在）。

- [ ] **Step 2: 写失败测试（前端）**

在 `apps/desktop/tests/exec-plane-p3.spec.tsx` 顶部的 import 区加一行：

```tsx
import { LeaseNoticeBanner } from '../src/agentUi'
```

然后在文件末尾续写：

```tsx
describe('B7 force-delete release results (§47)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const setupStoreWithAgents = async (agents: AgentRecord[], deleteReply: (agentId: string) => unknown) => {
    invokeMock.mockImplementation(async (cmd: string, args?: { agentId?: string }) => {
      if (cmd === 'agents_list') return { agents, agentSeq: agents.length, parseFailed: false }
      if (cmd === 'agents_delete') return deleteReply(args?.agentId ?? '')
      return {}
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    await act(async () => {
      await result.current.selectHome(home)
      await result.current.refreshAgents()
    })
    return { result, unmount }
  }

  it('释放失败的租约进全局通知列表（按被删 agent 追加、不覆盖），列出 exec 与 run，可逐条关闭', async () => {
    const a = agent({ id: 'agent-a', displayName: 'Agent 1', status: 'failed', attachment: 'none', lastExecId: 'exec-a' }, 'RUN-7')
    a.execs[0].execId = 'exec-a'
    const b = agent({ id: 'agent-b', displayName: 'Agent 2', status: 'failed', attachment: 'none', lastExecId: 'exec-b' }, 'RUN-8')
    b.execs[0].execId = 'exec-b'
    const { result, unmount } = await setupStoreWithAgents([a, b], agentId => ({
      deleted: agentId,
      leasesReleased: 0,
      leaseReleaseFailures: [`exec ${agentId === 'agent-a' ? 'exec-a' : 'exec-b'}: daemon down`],
    }))
    await act(async () => {
      await result.current.deleteAgent('agent-a', true)
    })
    await act(async () => {
      await result.current.deleteAgent('agent-b', true)
    })
    const notices = result.current.state.leaseNotices
    expect(notices.map(n => n.agentId)).toEqual(['agent-a', 'agent-b'])
    expect(notices[0].text).toContain('exec-a（RUN-7）')
    expect(notices[0].text).toContain('到 Runs 页')
    expect(notices[0].runIds).toEqual(['RUN-7'])
    act(() => {
      result.current.dismissLeaseNotice('agent-a')
    })
    expect(result.current.state.leaseNotices.map(n => n.agentId)).toEqual(['agent-b'])
    unmount()
  })

  it('store.deleteAgent 返回 agents_delete 的释放结果；全部释放成功时不产生通知', async () => {
    const reply = { deleted: 'agent-exec-1', leasesReleased: 1, leaseReleaseFailures: [] }
    const { result, unmount } = await setupStoreWithAgents([agent()], () => reply)
    let returned: unknown
    await act(async () => {
      returned = await result.current.deleteAgent('agent-exec-1', true)
    })
    expect(returned).toEqual(reply)
    expect(result.current.state.leaseNotices).toEqual([])
    unmount()
  })

  it('LeaseNoticeBanner：每条通知带「前往 Runs」与「知道了」', () => {
    const openRuns = vi.fn()
    const dismiss = vi.fn()
    render(
      <LeaseNoticeBanner
        notices={[
          { agentId: 'agent-a', homeId: 'h1', runIds: ['RUN-7'], text: '已删除 Agent 1，但 1 个租约释放失败、已被放弃：exec-a（RUN-7）。' },
          { agentId: 'agent-b', homeId: 'h1', runIds: ['RUN-8'], text: '已删除 Agent 2，但 1 个租约释放失败、已被放弃：exec-b（RUN-8）。' },
        ]}
        onDismiss={dismiss}
        onOpenRuns={openRuns}
      />,
    )
    expect(screen.getByText(/exec-a（RUN-7）/)).toBeTruthy()
    expect(screen.getByText(/exec-b（RUN-8）/)).toBeTruthy()
    fireEvent.click(screen.getAllByRole('button', { name: '前往 Runs' })[0])
    expect(openRuns).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getAllByRole('button', { name: '知道了' })[1])
    expect(dismiss).toHaveBeenCalledWith('agent-b')
  })
})

describe('B7 stop-all notice and focus (§39)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  const confirmStopAll = async () => {
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '确认' }))
    })
  }

  it('结果区域常驻挂载（role=status、aria-live=polite），只换文字不重新挂载', async () => {
    const stopAll = vi.fn().mockResolvedValue({ signaled: 1, failed: [] })
    renderPanel({ onStopAll: stopAll })
    const region = screen.getByRole('status')
    expect(region.getAttribute('aria-live')).toBe('polite')
    expect(region.textContent).toBe('')
    await confirmStopAll()
    expect(screen.getByRole('status')).toBe(region)
    expect(region.textContent).toContain('已向 1 个 agent 发出停止信号')
  })

  it('相同提示连续出现时计时器按 seq 重置，不提前清空（fe-stopall-toast）', async () => {
    vi.useFakeTimers()
    const stopAll = vi.fn().mockResolvedValue({ signaled: 1, failed: [] })
    renderPanel({ onStopAll: stopAll })
    await confirmStopAll()
    await act(async () => {
      vi.advanceTimersByTime(4000)
    })
    await confirmStopAll()
    await act(async () => {
      vi.advanceTimersByTime(2000) // 距第一次 6s、距第二次 2s
    })
    expect(screen.getByRole('status').textContent).toContain('已向 1 个 agent 发出停止信号')
    await act(async () => {
      vi.advanceTimersByTime(3100)
    })
    expect(screen.getByRole('status').textContent).toBe('')
  })

  it('含失败的结果进需手动关闭的 alert，不随 5 秒消失', async () => {
    vi.useFakeTimers()
    const stopAll = vi.fn().mockResolvedValue({ signaled: 1, failed: ['Agent 2 (exec-2): kill EPERM'] })
    renderPanel({ onStopAll: stopAll })
    await confirmStopAll()
    await act(async () => {
      vi.advanceTimersByTime(6000)
    })
    const alert = screen.getByText(/1 个停止失败：Agent 2 \(exec-2\): kill EPERM/)
    expect(alert.closest('[role="alert"]')).not.toBeNull()
    expect(screen.getByRole('status').textContent).toBe('')
    fireEvent.click(screen.getByRole('button', { name: '知道了' }))
    expect(screen.queryByText(/1 个停止失败/)).toBeNull()
  })

  it('第二步出现时焦点落在「取消」；Esc 取消并把焦点还给「停止全部」（fe-a11y-stopall）', () => {
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '取消' }))
    fireEvent.keyDown(document.activeElement as Element, { key: 'Escape' })
    const trigger = screen.getByRole('button', { name: '停止全部' })
    expect(document.activeElement).toBe(trigger)
  })

  it('确认并完成后焦点回到「停止全部」', async () => {
    renderPanel({ onStopAll: vi.fn().mockResolvedValue({ signaled: 1, failed: [] }) })
    await confirmStopAll()
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '停止全部' }))
  })

  it('3s 自动复位后焦点回到「停止全部」', async () => {
    vi.useFakeTimers()
    renderPanel({ onStopAll: vi.fn() })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    await act(async () => {
      vi.advanceTimersByTime(3100)
    })
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '停止全部' }))
  })
})

describe('B7 resume failure re-attach (§39)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue({})
  })

  it('续跑派发失败后，派发期间被推迟的 live agent 立即补接（fe-resume-fail-reattach）', async () => {
    let failStart!: (error: unknown) => void
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'exec_start') return new Promise((_, reject) => { failStart = reject })
      return Promise.resolve({})
    })
    const { useTreeStore } = await import('../src/store')
    const { renderHook } = await import('@testing-library/react')
    const { result, unmount } = renderHook(() => useTreeStore())
    const home = { homeId: 'h1', kind: 'workspace', path: '/tmp/ws/.omt' } as never
    let dispatch!: Promise<unknown>
    act(() => {
      dispatch = result.current
        .startDispatch({ home, nodeId: 'TICKET-1', harness: 'pi', unattended: true, agentId: 'agent-x' })
        .catch(() => undefined)
    })
    act(() => {
      result.current.attachTranscript('exec-live', 'h1')
    })
    expect(invokeMock).not.toHaveBeenCalledWith('exec_attach', expect.anything())
    await act(async () => {
      failStart(new Error('目标 agent 仍在运行，拒绝并发派发'))
      await dispatch
    })
    expect(invokeMock).toHaveBeenCalledWith(
      'exec_attach',
      expect.objectContaining({ execId: 'exec-live', homeId: 'h1' }),
    )
    unmount()
  })
})
```

```bash
cd apps/desktop && pnpm test -- exec-plane-p3 2>&1 | tail -15
```
Expected：B7 的 10 个用例 FAIL（`leaseNotices` / `dismissLeaseNotice` 不存在、`deleteAgent` 返回 undefined、`LeaseNoticeBanner` 未导出、结果区域在操作前不存在、第二次提示提前消失、含失败的结果 5 秒后消失、焦点不在「取消」/ Esc 无效 / 不回到「停止全部」、失败后没有补接）。

- [ ] **Step 3: 实现（omt-exec）**

`crates/omt-exec/src/shim.rs`：`fn run_bounded<T: Send + 'static>(` 改为 `pub(crate) fn run_bounded<T: Send + 'static>(`。

`crates/omt-exec/src/agents.rs`，`write_binding` 之后加：

```rust
/// 验收「强制删除」注：删除 agent 时移除指向它的绑定（nodeId → agentId 精确
/// 匹配）。不受 GC 冻结约束——冻结防的是「无法证明 agent 表有效时批量
/// 清空」，这里只删指向刚被删除的那个 agent 的项。返回移除数。
pub fn remove_bindings_to(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<usize, String> {
    let path = bindings_path(runtime_dir, home_id)?;
    if !path.is_file() {
        return Ok(0);
    }
    let mut removed = 0usize;
    update_json(&path, |bindings: &mut BTreeMap<String, String>| {
        let before = bindings.len();
        bindings.retain(|_, bound| bound != agent_id);
        removed = before - bindings.len();
        removed > 0
    })?;
    Ok(removed)
}
```

`delete_agent`（B5 版本）末尾 `Ok(())` 之前加：

```rust
    // 验收「强制删除」注：指向已删除 agent 的绑定一并移除（失败只记日志——
    // 删除本身已完成，悬空绑定仍可由用户的 bindings_gc 清理）
    if let Err(err) = remove_bindings_to(runtime_dir, home_id, agent_id) {
        eprintln!("删除 agent {agent_id}：移除绑定失败（{err}）");
    }
```

`crates/omt-exec/src/executor.rs`，`force_release_agent_leases` 替换为：

```rust
/// §47 force-release-no-bound：force 删除前尽力释放的总时限。desktop 侧同步
/// 等待——每个目标都要连接（拉起 daemon 最长 10s）加若干 30s 读超时，半卡死
/// daemon 下「删除中…」可能持续数分钟且没有反馈。
const FORCE_RELEASE_BUDGET: Duration = Duration::from_secs(30);

pub fn force_release_agent_leases(
    runtime_dir: &Path,
    connector: &crate::daemon_link::Connector,
    home_id: &str,
    agent_id: &str,
) -> Result<ForceReleaseOutcome, String> {
    force_release_agent_leases_within(runtime_dir, connector, home_id, agent_id, FORCE_RELEASE_BUDGET)
}

/// `force_release_agent_leases` 的内芯（预算可注入）。每个目标的释放在
/// `shim::run_bounded(剩余预算)` 里跑：超时按失败记（工作线程被放手，它
/// 若之后成功释放，只是没有 mark_reconciled——记录随删除消失，无害）；预算
/// 耗尽后剩余目标不再尝试，直接记为失败。
pub(crate) fn force_release_agent_leases_within(
    runtime_dir: &Path,
    connector: &crate::daemon_link::Connector,
    home_id: &str,
    agent_id: &str,
    budget: Duration,
) -> Result<ForceReleaseOutcome, String> {
    let record = crate::agents::get_agent(runtime_dir, home_id, agent_id)?
        .ok_or_else(|| format!("agent {agent_id} 不存在"))?;
    let deadline = Instant::now() + budget;
    let mut outcome = ForceReleaseOutcome::default();
    for exec in &record.execs {
        if !crate::agents::is_reconcile_target(exec) {
            continue;
        }
        let target = crate::agents::ReconcileTarget {
            home_id: home_id.to_string(),
            agent_id: agent_id.to_string(),
            exec_id: exec.exec_id.clone(),
            run_id: exec.run_id.clone().unwrap_or_default(),
            harness: record.harness.clone(),
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            outcome.failures.push(format!(
                "exec {}: 释放总时限（{}s）已到，未尝试",
                exec.exec_id,
                budget.as_secs()
            ));
            continue;
        }
        let release = {
            let runtime_dir = runtime_dir.to_path_buf();
            let connector = connector.clone();
            let home_id = home_id.to_string();
            let agent_id = agent_id.to_string();
            let exec_id = exec.exec_id.clone();
            let run_id = target.run_id.clone();
            move || -> Result<(), String> {
                let dir = crate::layout::agent_dir(&runtime_dir, &home_id, &agent_id)?;
                let spec_path =
                    crate::layout::exec_file(&dir, &exec_id, crate::layout::ExecFile::Spec);
                let spec = crate::spec::LaunchSpec::read(&spec_path)
                    .map_err(|err| format!("无 launch spec（{err}）——无法定位 home"))?;
                let rpc = connector(&spec.home_path, &home_id)?;
                release_items_for_exec(&*rpc, &home_id, &run_id, &exec_id)
            }
        };
        match crate::shim::run_bounded(release, remaining) {
            Ok(()) => {
                mark_reconciled(runtime_dir, &target);
                outcome.released += 1;
            }
            Err(err) => {
                eprintln!(
                    "强制删除：exec {} 租约释放失败（{err}）——该租约将被放弃",
                    exec.exec_id
                );
                outcome
                    .failures
                    .push(format!("exec {}: {err}", exec.exec_id));
            }
        }
    }
    Ok(outcome)
}
```

（函数上方原有的 F-EXEC-16 ① 长文档注释保留在 `force_release_agent_leases` 上。）

- [ ] **Step 4: 实现（P3-ACC-6 与 setup）**

`apps/desktop/src-tauri/Cargo.toml` 末尾（`[build-dependencies]` 之前）加：

```toml
# P3-ACC-6：退出 / 关窗确认框直调 NSAlert（「取消」绑定 Esc）。版本与 tauri /
# rfd 已拉进 Cargo.lock 的一致，不引入新版本。
[target.'cfg(target_os = "macos")'.dependencies]
dispatch2 = "0.3.1"
objc2 = "0.6.4"
objc2-app-kit = { version = "0.3.2", default-features = false, features = [
    "std",
    "NSAlert",
    "NSApplication",
    "NSButton",
    "NSControl",
    "NSResponder",
    "NSView",
] }
objc2-foundation = { version = "0.3.2", default-features = false, features = ["std", "NSString"] }
```

`apps/desktop/src-tauri/src/quit_dialog.rs`（测试模块之上）：

```rust
//! P3-ACC-6：退出 / 关窗的三按钮确认框。tauri-plugin-dialog 2.7.1 → rfd
//! 0.16.0 在无父窗口时走 `CFUserNotificationDisplayAlert`
//! （rfd `backend/macos/utils/user_alert.rs`），三个按钮都不绑定 Esc；有父窗口
//! 时走 NSAlert sheet，`addButtonWithTitle` 逐个加按钮、同样不设
//! keyEquivalent——NSAlert 只对标题恰为英文 "Cancel" 的按钮自动绑定 Esc。
//! macOS 上改为直接构造 NSAlert：按钮顺序 KEEP（首个 = Return）/ STOP /
//! CANCEL，并给「取消」显式设 keyEquivalent = Esc。其他平台保留插件路径。
//!
//! 线程：调用方都在主线程（RunEvent / WindowEvent 回调）。runModal 是嵌套
//! 事件循环——不在 tao 的事件回调里同步跑，而是投递到 GCD 主队列，下一轮
//! run loop 再弹（与插件「回调式」的语义一致：回调里再做隐藏 / 退出）。
//! 无单测缝（AppKit 胶水）——手动验收见 Task B7 Step 9。

/// 三按钮文案（review R2-6：首个按钮是 Return 默认键，放安全的「继续在
/// 后台运行」；「取消」= 不退出）。
pub const DIALOG_KEEP: &str = "继续在后台运行";
pub const DIALOG_STOP: &str = "全部停止后退出";
pub const DIALOG_CANCEL: &str = "取消";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitChoice {
    Keep,
    Stop,
    Cancel,
}

/// NSAlert 返回码 → 三选一。1000 / 1001 / 1002 = NSAlertFirst / Second /
/// ThirdButtonReturn（按添加顺序）；其余按安全方向归 Cancel。
pub fn choice_from_response(response: isize) -> QuitChoice {
    match response {
        1000 => QuitChoice::Keep,
        1001 => QuitChoice::Stop,
        _ => QuitChoice::Cancel,
    }
}

#[cfg(target_os = "macos")]
pub fn confirm_quit_dialog<F>(_app: &tauri::AppHandle, title: String, message: String, on_choice: F)
where
    F: FnOnce(QuitChoice) + Send + 'static,
{
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            // 不可达：主队列的任务必在主线程上执行；按安全方向不退出
            on_choice(QuitChoice::Cancel);
            return;
        };
        on_choice(run_alert(mtm, &title, &message));
    });
}

#[cfg(target_os = "macos")]
fn run_alert(mtm: objc2::MainThreadMarker, title: &str, message: &str) -> QuitChoice {
    use objc2_app_kit::{NSAlert, NSAlertStyle};
    use objc2_foundation::NSString;
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    let _keep = alert.addButtonWithTitle(&NSString::from_str(DIALOG_KEEP));
    let _stop = alert.addButtonWithTitle(&NSString::from_str(DIALOG_STOP));
    let cancel = alert.addButtonWithTitle(&NSString::from_str(DIALOG_CANCEL));
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    choice_from_response(alert.runModal())
}

#[cfg(not(target_os = "macos"))]
pub fn confirm_quit_dialog<F>(app: &tauri::AppHandle, title: String, message: String, on_choice: F)
where
    F: FnOnce(QuitChoice) + Send + 'static,
{
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};
    app.dialog()
        .message(message)
        .title(title)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            DIALOG_KEEP.into(),
            DIALOG_STOP.into(),
            DIALOG_CANCEL.into(),
        ))
        .show_with_result(move |result| {
            on_choice(match result {
                MessageDialogResult::Custom(label) if label == DIALOG_KEEP => QuitChoice::Keep,
                MessageDialogResult::Custom(label) if label == DIALOG_STOP => QuitChoice::Stop,
                MessageDialogResult::Yes => QuitChoice::Keep,
                MessageDialogResult::No => QuitChoice::Stop,
                _ => QuitChoice::Cancel,
            })
        });
}
```

`apps/desktop/src-tauri/src/lib.rs`：

1. 模块与 use：

```rust
mod agents;
mod daemon;
mod executor;
mod harness;
mod quit_dialog;
mod rpc_bridge;
mod state;

use quit_dialog::{QuitChoice, DIALOG_KEEP};
use state::SharedSession;
use std::path::Path;
use std::sync::atomic::Ordering;
use tauri::Manager;
```

删除 `use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};`、三个 `DIALOG_*` 常量（已移入 `quit_dialog`）与 `fn dialog_choice`。

2. `run()` 开头删除这两段（连同注释）：

```rust
    omt_exec::executor::startup_housekeeping(&daemon::runtime_dir());
    executor::release_reconcile_targets_in_background("启动对账");
```

在 `.manage(SharedSession::new())` 之后加：

```rust
        // §39 housekeeping-before-single-instance：执行面启动清扫（迁移 /
        // 启动对账 / 临时授权与 MCP 残留 / 删除残留）与 shim 路径的租约释放
        // 放在 app setup 里——single-instance 插件在 plugin setup 阶段就让
        // 第二实例 exit(0)，plugin setup 先于 app setup，于是只有存活的那个
        // 实例跑它们。仍早于任何 command 被调用；对账只改记录，不发信号。
        .setup(|_app| {
            omt_exec::executor::startup_housekeeping(&daemon::runtime_dir());
            executor::release_reconcile_targets_in_background("启动对账");
            Ok(())
        })
```

3. `on_window_event` 里从 `let handle = window.app_handle().clone();` 到 `.show_with_result(...)` 结束的整段替换为：

```rust
            let handle = window.app_handle().clone();
            let dialog_app = handle.clone();
            quit_dialog::confirm_quit_dialog(
                &dialog_app,
                "关闭 oh-my-ticket".to_string(),
                format!(
                    "还有 {running} 个 agent 在运行。选择「{DIALOG_KEEP}」后窗口会隐藏，\
                     它们继续执行到完成或超时；点击 dock 图标可重新打开本应用查看或停止。"
                ),
                move |choice| match choice {
                    QuitChoice::Keep => {
                        // 后台继续：隐藏保活的窗口，应用不退出——回到正常运行，
                        // 清 DIALOG_OPEN 放行后续弹框
                        if let Some(win) = handle.get_webview_window("main") {
                            let _ = win.hide();
                        }
                        DIALOG_OPEN.store(false, Ordering::SeqCst);
                    }
                    QuitChoice::Stop => {
                        // 退出路径（P3-a）：DIALOG_OPEN 保持置位直到进程退出
                        stop_all_execs(&runtime_dir);
                        QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                        handle.exit(0);
                    }
                    // 取消 / Esc：窗口保持原样（close 已被 prevent）——回到正常
                    // 运行，清 DIALOG_OPEN
                    QuitChoice::Cancel => DIALOG_OPEN.store(false, Ordering::SeqCst),
                },
            );
```

4. `RunEvent::ExitRequested` 里从 `let handle = app.clone();` 到 `.show_with_result(...)` 结束的整段替换为：

```rust
                    let handle = app.clone();
                    quit_dialog::confirm_quit_dialog(
                        app,
                        "退出 oh-my-ticket".to_string(),
                        format!(
                            "还有 {running} 个 agent 在运行。选择「{DIALOG_KEEP}」后，它们会一直执行到完成或超时；\
                             随时可以重新打开本应用查看或停止。"
                        ),
                        move |choice| {
                            match choice {
                                // KEEP / STOP 都是退出路径（P3-a）：DIALOG_OPEN
                                // 保持置位直到进程退出
                                QuitChoice::Keep => {}
                                QuitChoice::Stop => stop_all_execs(&runtime_dir),
                                // 取消 / Esc：不退出——只有回到正常运行的分支
                                // 清 DIALOG_OPEN（P3-a）
                                QuitChoice::Cancel => {
                                    DIALOG_OPEN.store(false, Ordering::SeqCst);
                                    return;
                                }
                            }
                            QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                            handle.exit(0);
                        },
                    );
```

`DIALOG_OPEN` 上方注释里「无单测缝（tauri 胶水）——手动验收：…」一句追加「；Esc = 取消（P3-ACC-6）」。

- [ ] **Step 5: 实现（前端）**

`apps/desktop/src/types.ts` 末尾加：

```ts
/** §47 release-results-hidden：强制删除时释放失败（被放弃）的租约——全局
 * 通知列表的一条，按被删 agent 键控，直到用户关闭。 */
export interface LeaseNotice {
  agentId: string
  homeId: string
  /** 被放弃租约所属的 run（文案里也写明，便于在 Runs 页定位） */
  runIds: string[]
  text: string
}
```

`apps/desktop/src/agentUi.tsx`：

1. import 改为：

```tsx
import { useEffect, useRef, useState } from 'react'
import type { AgentRecord, LeaseNotice } from './types'
```

2. `ConfirmStopAllButton` 整体替换为：

```tsx
export function ConfirmStopAllButton({
  running,
  claimed,
  busy,
  onConfirm,
}: {
  running: number
  claimed: number | null
  busy: boolean
  onConfirm: () => void
}) {
  const [arming, setArming] = useState(false)
  // §39 fe-a11y-stopall：第二步出现时焦点落在「取消」（安全默认——回车不会
  // 误触确认）；第二步结束（确认、取消、3s 自动复位）后焦点回到「停止全部」
  // （确认时要等「停止中…」结束、按钮重新出现）；Esc 取消。
  const cancelRef = useRef<HTMLButtonElement | null>(null)
  const triggerRef = useRef<HTMLButtonElement | null>(null)
  const restoreFocusRef = useRef(false)
  useEffect(() => {
    if (!arming) return
    const timer = setTimeout(() => setArming(false), CONFIRM_RESET_MS)
    return () => clearTimeout(timer)
  }, [arming])
  useEffect(() => {
    if (arming) {
      restoreFocusRef.current = true
      cancelRef.current?.focus()
    } else if (!busy && restoreFocusRef.current) {
      restoreFocusRef.current = false
      triggerRef.current?.focus()
    }
  }, [arming, busy])
  if (busy) {
    return (
      <button type="button" className="chip agents-stop-all" disabled>
        停止中…
      </button>
    )
  }
  if (!arming) {
    return (
      <button
        ref={triggerRef}
        type="button"
        className="chip agents-stop-all"
        title="只停止当前 workspace 中可控的 agent；不受控（orphan）的需单独强制终止"
        onClick={() => setArming(true)}
      >
        停止全部
      </button>
    )
  }
  return (
    <span
      className="confirm-stop agents-stop-all"
      onKeyDown={event => {
        if (event.key === 'Escape') {
          event.preventDefault()
          setArming(false)
        }
      }}
    >
      确认停止 {running} 个 agent{claimed !== null ? `，将释放 ${claimed} 个已认领项` : ''}
      <button
        type="button"
        className="chip active"
        onClick={() => {
          setArming(false)
          onConfirm()
        }}
      >
        确认
      </button>
      <button ref={cancelRef} type="button" className="chip" onClick={() => setArming(false)}>
        取消
      </button>
    </span>
  )
}
```

3. 文件末尾加：

```tsx
/**
 * §47 release-results-hidden：强制删除时被放弃的租约——应用级通知列表（任何
 * 页面都可见，直到逐条关闭）。每条带「前往 Runs」（切到 Runs 页手动取消
 * 对应 run）与「知道了」。
 */
export function LeaseNoticeBanner({
  notices,
  onDismiss,
  onOpenRuns,
}: {
  notices: LeaseNotice[]
  onDismiss: (agentId: string) => void
  onOpenRuns: () => void
}) {
  if (notices.length === 0) return null
  return (
    <div className="error-banner lease-notices" role="alert">
      {notices.map(notice => (
        <div className="lease-notice" key={notice.agentId}>
          <p>{notice.text}</p>
          <div className="chip-row">
            <button className="chip" onClick={onOpenRuns}>
              前往 Runs
            </button>
            <button className="chip" onClick={() => onDismiss(notice.agentId)}>
              知道了
            </button>
          </div>
        </div>
      ))}
    </div>
  )
}
```

`apps/desktop/src/store.ts`：

1. bridge import 加 `type AgentsDeleteResult,`；types import 加 `LeaseNotice`（与 B6 加的 `CorruptAgent`、`LegacyUnmigrated` 并列）。

2. `TreeState` 在 `corruptAgents: CorruptAgent[]`（B6）之后加：

```ts
  /** §47：强制删除时被放弃的租约通知（应用级，逐条关闭；换 home 不清） */
  leaseNotices: LeaseNotice[]
```

`INITIAL` 加 `leaseNotices: [],`（`selectHome` 的重置块**不**加——通知跨 home 保留到用户关闭）。

3. `claimedCountForExec` 之后加（导出供测试）：

```ts
/**
 * §47 release-results-hidden：被放弃租约的通知文案。Rust 侧明细形如
 * `exec <execId>: <err>`（force_release_agent_leases）；run id 从被删 agent
 * 的 execs 反查；形状不符的明细原样列出。agent 记录已不在列表时退回 id。
 */
export function leaseNoticeOf(
  agent: AgentRecord | undefined,
  agentId: string,
  homeId: string,
  failures: string[],
): LeaseNotice {
  const runIds: string[] = []
  const parts = failures.map(failure => {
    const execId = /^exec (\S+): /.exec(failure)?.[1]
    if (!execId) return failure
    const runId = agent?.execs.find(e => e.execId === execId)?.runId
    if (runId && !runIds.includes(runId)) runIds.push(runId)
    return runId ? `${execId}（${runId}）` : execId
  })
  const name = agent ? (agent.title ?? agent.displayName) : agentId
  return {
    agentId,
    homeId,
    runIds,
    text: `已删除 ${name}，但 ${failures.length} 个租约释放失败、已被放弃：${parts.join('；')}。对应 run item 可能一直显示运行中，请到 Runs 页手动取消该 run。`,
  }
}
```

4. `dispatchingRef` 声明之后加：

```ts
  // §39 fe-resume-fail-reattach：派发进行中被推迟的接回请求（execId → homeId），
  // 派发结束（成功或失败）后补接——此前失败路径不重拉列表，被跳过的 live
  // agent 要等下一次列表重拉才接上
  const deferredAttachRef = useRef<Map<string, string>>(new Map())
```

5. 把 `attachTranscript` 整个移到 `startDispatch` 之前，并改为：

```ts
  /** P-10：重启后接回仍在运行的 exec。afterSeq 取本窗口最后一个事件的 seq。 */
  const attachTranscript = useCallback(
    (execId: string, homeId: string) => {
      if (attachedRef.current.has(execId)) return
      // review R2-9：派发进行中（续跑的就绪握手最长 30s）先不接——新 exec 的
      // 跟随者会由 exec_start 建立。§39：记下来，派发结束后补接
      if (dispatchingRef.current > 0) {
        deferredAttachRef.current.set(execId, homeId)
        return
      }
      attachedRef.current.add(execId)
      const channel = new Channel<TranscriptEvent>()
      channel.onmessage = ev => ingestTranscriptEvent(ev)
      const existing = transcriptsRef.current.get(execId)
      const last = existing && existing.length > 0 ? existing[existing.length - 1] : undefined
      const afterSeq = last ? last.seq : null
      void execAttach({ homeId, execId, afterSeq, channel }).catch(error => {
        attachedRef.current.delete(execId)
        setState(s => ({ ...s, error: presentError(error) }))
      })
    },
    [ingestTranscriptEvent],
  )

  /** §39：所有派发结束后补接被推迟的 exec（新 exec 已在 attachedRef 里，自然跳过）。 */
  const flushDeferredAttach = useCallback(() => {
    if (dispatchingRef.current > 0) return
    const pending = [...deferredAttachRef.current]
    deferredAttachRef.current.clear()
    for (const [execId, homeId] of pending) attachTranscript(execId, homeId)
  }, [attachTranscript])
```

（webview 重载后 `attachedRef` 随页面重建而清空，会对仍在运行的 exec 再调一次 `exec_attach`——后端在 Step 7 保证同一 exec 只保留一个跟随者，前端无需改动。）

6. `startDispatch` 里从 `dispatchingRef.current += 1` 到 `attachedRef.current.add(outcome.execId)` 替换为：

```ts
      // review R2-9：派发进行中（就绪握手最长 30s）不计接回实时流——新 exec
      // 的跟随者由 exec_start 建立
      dispatchingRef.current += 1
      let outcome: ExecStartOutcome
      try {
        outcome = await execStart({
          homeId: options.home.homeId,
          homePath: options.home.path ?? '',
          nodeId: options.nodeId,
          harness: options.harness,
          unattended: options.unattended,
          model: options.model ?? null,
          resumeSession: options.resumeSession ?? null,
          agentId: options.agentId ?? null,
          channel,
        })
      } catch (error) {
        dispatchingRef.current -= 1
        flushDeferredAttach()
        throw error
      }
      dispatchingRef.current -= 1
      // P-10：startDispatch 的 Channel 即本会话的实时跟随者——记入防重复 attach
      attachedRef.current.add(outcome.execId)
      flushDeferredAttach()
```

依赖数组改为 `[ingestTranscriptEvent, scheduleAgentsRepull, flushDeferredAttach]`。（`execStart` 的入参以 A2 之后 `bridge.execStart` 的签名为准，这里只改包裹它的 try/catch。）

7. `deleteAgent` 改为返回结果并登记通知：

```ts
  const deleteAgent = useCallback(
    async (agentId: string, force?: boolean): Promise<AgentsDeleteResult> => {
      const home = activeHomeRef.current
      if (!home) throw new Error('无活动 workspace')
      const result = await agentsDelete(home.homeId, agentId, force ?? false)
      const failures = result?.leaseReleaseFailures ?? []
      setState(s => ({
        ...s,
        selectedAgentId: s.selectedAgentId === agentId ? null : s.selectedAgentId,
        unviewedFailedIds: s.unviewedFailedIds.filter(id => id !== agentId),
        // §47：被放弃的租约进应用级通知列表——按被删 agent 追加，不覆盖先前的
        // 通知（updater 在 refreshAgents 之前执行，s.agents 里还有这条记录）
        leaseNotices:
          failures.length > 0
            ? [
                ...s.leaseNotices.filter(n => n.agentId !== agentId),
                leaseNoticeOf(
                  s.agents.find(a => a.id === agentId),
                  agentId,
                  home.homeId,
                  failures,
                ),
              ]
            : s.leaseNotices,
      }))
      await refreshAgents()
      return result
    },
    [refreshAgents],
  )

  /** §47：关闭一条被放弃租约的通知。 */
  const dismissLeaseNotice = useCallback((agentId: string) => {
    setState(s => ({ ...s, leaseNotices: s.leaseNotices.filter(n => n.agentId !== agentId) }))
  }, [])
```

返回对象在 `deleteAgent,` 之后加 `dismissLeaseNotice,`。

`apps/desktop/src/App.tsx`：

1. import 加 `import { LeaseNoticeBanner } from './agentUi'`。

2. `{state.error && <p className="error-banner">{state.error}</p>}` 之后加（应用级，任何页面可见；「前往 Runs」与 `onOpenHarnessSettings={() => setView('settings')}` 同一写法）：

```tsx
      <LeaseNoticeBanner
        notices={state.leaseNotices}
        onDismiss={store.dismissLeaseNotice}
        onOpenRuns={() => setView('runs')}
      />
```

`apps/desktop/src/AgentsPanel.tsx`（删除流程不变——通知由 store 登记、App 呈现；`onDeleteAgent` 类型不变）：

1. `stopAllNotice` 的 state 与 effect 替换为：

```tsx
  /** review F14（P-10）/ §39：停止全部的进行中状态与结果。
   * - 成功 / 无事可做：写入常驻挂载的 `role="status" aria-live="polite"` 区域，
   *   只换文字（重新挂载的 live region 不会被读屏软件播报），5 秒后清空；
   *   计时器按 seq 重置——同一文案连续出现时不会沿用上一次的计时器提前消失。
   * - 含失败（调用失败 / 全部失败 / 部分失败）：进需手动关闭的 alert，不随 5 秒消失。 */
  const [stoppingAll, setStoppingAll] = useState(false)
  const [stopAllNotice, setStopAllNotice] = useState<{ text: string; seq: number } | null>(null)
  const [stopAllAlert, setStopAllAlert] = useState<string | null>(null)
  const noticeSeqRef = useRef(0)
  const showStopAllNotice = (text: string) => {
    noticeSeqRef.current += 1
    setStopAllNotice({ text, seq: noticeSeqRef.current })
  }
  const noticeSeq = stopAllNotice?.seq
  useEffect(() => {
    if (noticeSeq === undefined) return
    const timer = setTimeout(() => setStopAllNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [noticeSeq])
```

2. `handleStopAll` 整体替换为（文案与原来逐字相同，既有 `exec-plane.spec` 的文本断言不受影响）：

```tsx
  const handleStopAll = async () => {
    if (!onStopAll) return
    setStoppingAll(true)
    try {
      const outcome = await onStopAll()
      const orphanNote =
        groups.orphan.length > 0 ? `；${groups.orphan.length} 个不受控 agent 需单独强制终止` : ''
      // review R2-7 / F-EXEC-12 / §43 P3：全失败、无事可做、成功、部分失败分开
      // 提示，失败带明细；outcome 携带 signaled=0 && failed>0 时也按失败处理。
      // §39：含失败的结果进需手动关闭的 alert，其余进 5 秒的 status 区域。
      if (outcome === null) {
        setStopAllAlert('停止失败，请重试')
      } else if (outcome.failed.length > 0) {
        setStopAllAlert(
          outcome.signaled === 0
            ? `停止失败：${describeStopAllFailures(outcome.failed)}`
            : `已向 ${outcome.signaled} 个 agent 发出停止信号，${outcome.failed.length} 个停止失败：${describeStopAllFailures(outcome.failed)}${orphanNote}`,
        )
      } else {
        showStopAllNotice(
          outcome.signaled === 0
            ? '当前没有运行中的 agent'
            : `已向 ${outcome.signaled} 个 agent 发出停止信号${orphanNote}`,
        )
      }
    } finally {
      setStoppingAll(false)
    }
  }
```

3. 列表分支的

```tsx
        {stopAllNotice && <p className="agents-notice">{stopAllNotice}</p>}
```

替换为：

```tsx
        <p className="agents-notice" role="status" aria-live="polite">
          {stopAllNotice?.text ?? ''}
        </p>
        {stopAllAlert && (
          <div className="error-banner" role="alert">
            <p>{stopAllAlert}</p>
            <button className="chip" onClick={() => setStopAllAlert(null)}>
              知道了
            </button>
          </div>
        )}
```

`apps/desktop/src/styles.css` 末尾加：

```css
/* §39：常驻的停止全部结果区域——无文字时不占位（仍挂载，live region 有效） */
.agents-notice:empty {
  margin: 0;
  padding: 0;
}
/* §47：应用级的被放弃租约通知 */
.lease-notices .lease-notice + .lease-notice {
  margin-top: 8px;
}
```

- [ ] **Step 6: 跟随者——写失败测试（§39 APPEAR_TIMEOUT / webview 重载双跟随者）**

`crates/omt-exec/src/follow.rs` 的 `mod tests` 加：

```rust
    /// §39：Ready::Timeout 后文件迟迟不出现——exec 仍活跃就一直等，不因
    /// appear_timeout 放弃；文件出现后照常推事件。
    #[test]
    fn waits_past_appear_timeout_while_exec_is_active() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("late.jsonl");
        let (sink, seen) = collector();
        let handle = follow_jsonl_with(
            path.clone(),
            None,
            sink,
            Arc::new(|| true),
            None,
            FollowOptions {
                appear_timeout: Duration::from_millis(100),
                cancel: None,
            },
        );
        std::thread::sleep(Duration::from_millis(600));
        std::fs::write(
            &path,
            "{\"kind\":\"session_start\",\"seq\":0}\n{\"kind\":\"exit\",\"seq\":1}\n",
        )
        .unwrap();
        handle.join().unwrap();
        let kinds: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, vec!["session_start", "exit"]);
    }

    /// 反例（行为不变）：已不活跃且文件始终不出现——超过 appear_timeout 即结束。
    #[test]
    fn gives_up_on_missing_file_once_inactive_and_past_appear_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let (sink, seen) = collector();
        let started = std::time::Instant::now();
        follow_jsonl_with(
            tmp.path().join("never.jsonl"),
            None,
            sink,
            Arc::new(|| false),
            None,
            FollowOptions {
                appear_timeout: Duration::from_millis(100),
                cancel: None,
            },
        )
        .join()
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(seen.lock().unwrap().is_empty());
    }

    /// §39：取消标志置位后跟随者在下一次轮询退出，且不调 on_lost（它不是
    /// 「执行丢失」，只是被新的跟随者接管）。
    #[test]
    fn cancel_stops_the_follower_without_on_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"raw\",\"seq\":0}\n").unwrap();
        let (sink, seen) = collector();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&called);
        let hook: LostHook = Box::new(move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            None
        });
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = follow_jsonl_with(
            path,
            None,
            sink,
            Arc::new(|| true),
            Some(hook),
            FollowOptions {
                appear_timeout: APPEAR_TIMEOUT,
                cancel: Some(cancel.clone()),
            },
        );
        std::thread::sleep(Duration::from_millis(300));
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !handle.is_finished() {
            assert!(std::time::Instant::now() < deadline, "取消后 2s 内必须退出");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.join().unwrap();
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst), "取消不是执行丢失");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
```

`apps/desktop/src-tauri/src/executor.rs` 的 `mod tests` 加：

```rust
    /// §39：同一 exec 的第二个跟随者（webview 重载后的 exec_attach）接管，
    /// 第一个被取消；过期的 release 不得摘掉当前的登记。
    #[test]
    fn a_second_follower_for_the_same_exec_cancels_the_first() {
        use std::sync::atomic::Ordering;
        let first = claim_follower("exec-dup-test");
        let second = claim_follower("exec-dup-test");
        assert!(first.load(Ordering::SeqCst), "旧跟随者必须被取消");
        assert!(!second.load(Ordering::SeqCst));
        release_follower("exec-dup-test", &first); // 旧的结束：不得影响新的
        assert!(FOLLOWERS
            .lock()
            .unwrap()
            .get("exec-dup-test")
            .is_some_and(|current| Arc::ptr_eq(current, &second)));
        release_follower("exec-dup-test", &second);
        assert!(!FOLLOWERS.lock().unwrap().contains_key("exec-dup-test"));
    }
```

```bash
cargo test -p omt-exec follow:: 2>&1 | tail -3
cargo test -p oh-my-ticket-desktop a_second_follower 2>&1 | tail -3
```
Expected：两处都编译失败（`follow_jsonl_with` / `FollowOptions`、`claim_follower` / `release_follower` / `FOLLOWERS` 未定义）。

- [ ] **Step 7: 跟随者——实现**

`crates/omt-exec/src/follow.rs`：

1. `use` 改为：

```rust
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
```

2. `LostHook` 之后加：

```rust
/// 跟随选项（§39）。
/// - `appear_timeout`：文件尚不存在时，exec **已不活跃**之后最多再等多久；
///   exec 仍活跃（shim 在、或仍在拉起宽限期内）就一直等——此前 launcher 的
///   `Ready::Timeout`（30s）之后，跟随者的 30s 先到期就静默退出，shim 之后
///   写出的事件再也接不回来。
/// - `cancel`：置位后跟随者在下一次轮询（≤ POLL）退出，不调 `on_lost`——
///   桌面端每个 exec 只保留一个跟随者，新的接管时取消旧的。
pub struct FollowOptions {
    pub appear_timeout: Duration,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for FollowOptions {
    fn default() -> Self {
        Self {
            appear_timeout: APPEAR_TIMEOUT,
            cancel: None,
        }
    }
}
```

3. `follow_jsonl` 改为委托，原函数体移入 `follow_jsonl_with`：

```rust
pub fn follow_jsonl(
    path: PathBuf,
    after_seq: Option<u64>,
    sink: EventSink,
    active: Arc<dyn Fn() -> bool + Send + Sync>,
    on_lost: Option<LostHook>,
) -> std::thread::JoinHandle<()> {
    follow_jsonl_with(path, after_seq, sink, active, on_lost, FollowOptions::default())
}

pub fn follow_jsonl_with(
    path: PathBuf,
    after_seq: Option<u64>,
    sink: EventSink,
    active: Arc<dyn Fn() -> bool + Send + Sync>,
    on_lost: Option<LostHook>,
    opts: FollowOptions,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut offset: u64 = 0;
        let mut next_seq: u64 = after_seq.map_or(0, |after| after + 1);
        let mut pending: Vec<u8> = Vec::new();
        let mut idle_inactive = 0u32;
        let appear_deadline = std::time::Instant::now() + opts.appear_timeout;
        loop {
            // §39：被新的跟随者接管——直接退出（不是执行丢失，不调 on_lost）
            if opts
                .cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::SeqCst))
            {
                return;
            }
            let mut grew = false;
            match std::fs::File::open(&path) {
                Ok(mut file) => {
                    if file.seek(SeekFrom::Start(offset)).is_ok() {
                        let mut buf = Vec::new();
                        if let Ok(n) = file.read_to_end(&mut buf) {
                            if n > 0 {
                                offset += n as u64;
                                pending.extend_from_slice(&buf);
                                grew = true;
                            }
                        }
                    }
                }
                // §39：文件还没出现——exec 仍活跃就一直等；appear_timeout 只
                // 约束已不活跃的情形
                Err(_) if active() || std::time::Instant::now() < appear_deadline => {}
                Err(_) => return,
            }
```

（循环其余部分——逐行解析、`seq` 过滤、读到 exit 返回、不活跃且连续两次不增长时调 `on_lost`——原样保留。）

`apps/desktop/src-tauri/src/executor.rs`：

1. `use` 区加：

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

2. `follow` 之前加：

```rust
/// §39 webview 重载后的双跟随者：本进程内每个 exec 至多一个跟随者。重载后
/// 前端的 attachedRef 随页面重建而清空，会对同一 exec 再调 exec_attach——
/// 新跟随者接管（它的 Channel 属于新页面），旧跟随者经取消标志在下一次轮询
/// （≤200ms）退出，不再向已失效的 Channel 空推、空转。
static FOLLOWERS: std::sync::Mutex<std::collections::BTreeMap<String, Arc<AtomicBool>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn claim_follower(exec_id: &str) -> Arc<AtomicBool> {
    let token = Arc::new(AtomicBool::new(false));
    let previous = FOLLOWERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(exec_id.to_string(), token.clone());
    if let Some(previous) = previous {
        previous.store(true, Ordering::SeqCst);
    }
    token
}

/// 跟随者结束时摘掉登记——只摘自己那一条（已被接管的旧跟随者不动新登记）。
fn release_follower(exec_id: &str, token: &Arc<AtomicBool>) {
    let mut followers = FOLLOWERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if followers
        .get(exec_id)
        .is_some_and(|current| Arc::ptr_eq(current, token))
    {
        followers.remove(exec_id);
    }
}
```

3. `follow()` 末尾的

```rust
    omt_exec::follow::follow_jsonl(path, after_seq, sink, active, Some(on_lost));
    Ok(())
```

替换为：

```rust
    // §39：同一 exec 只保留一个跟随者（exec_start 与 exec_attach 共用本函数）
    let cancel = claim_follower(exec_id);
    let handle = omt_exec::follow::follow_jsonl_with(
        path,
        after_seq,
        sink,
        active,
        Some(on_lost),
        omt_exec::follow::FollowOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        },
    );
    let exec_key = exec_id.to_string();
    std::thread::spawn(move || {
        let _ = handle.join();
        release_follower(&exec_key, &cancel);
    });
    Ok(())
```

```bash
cargo test -p omt-exec follow:: 2>&1 | grep -E '^test result|FAILED'
cargo test -p oh-my-ticket-desktop a_second_follower 2>&1 | grep -E '^test result|FAILED'
```
Expected：follow 模块 7 个测试（既有 4 个 + 新 3 个）与桌面端新测试全部通过。

- [ ] **Step 8: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result|FAILED'
cargo tree -p oh-my-ticket-desktop -i objc2-app-kit 2>&1 | head -3
cd apps/desktop && pnpm test 2>&1 | grep -E 'Tests +[0-9]+|FAIL' && pnpm exec tsc --noEmit
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
```
Expected：Rust 全绿（含 `quit_dialog_maps_alert_responses_safely`、follow 的 3 个新测试与 `a_second_follower_for_the_same_exec_cancels_the_first`）；`cargo tree -i` 只出现 `objc2-app-kit v0.3.2` 一个版本；桌面 vitest 全部通过——`exec-plane-p3` 的 B7 段 10 个用例通过；`exec-plane.spec` 的 `does not start a second follower while a dispatch is in flight` 仍绿（成功路径先把新 exec 记入 `attachedRef` 再补接），其停止全部的文本断言仍绿（文案未变，只换了承载元素）；tsc 干净；fmt / clippy 干净。

- [ ] **Step 9: 手工验收（AppKit 胶水无单测）**

`cd apps/desktop && pnpm tauri:dev`，派发一个长任务（如 `sleep 120` 的 fake harness 或真实 omp），然后逐项：
1. ⌘Q → 确认框出现 → 按 **Esc**：框关闭，应用不退出，exec 不受影响（P3-ACC-6 原失败项）。
2. ⌘Q → 按 **Return**：等同「继续在后台运行」，应用退出，exec 继续。
3. 关窗（红按钮）→ **Esc**：窗口保持；→ 再关窗选「继续在后台运行」：窗口隐藏，dock 点击恢复。
4. 连按 ⌘Q：只出现一个框（DIALOG_OPEN 防重入不变）。
5. 首实例运行时再启动一次 `oh-my-ticket-desktop`：第二实例立即退出，终端日志里**没有**第二实例的「启动对账」「启动清扫」行（§39 housekeeping-before-single-instance）。
6. 执行中在 Agents 页按 ⌘R 重载 webview，选中该 agent：实时流接回且每个事件只出现一次（§39 双跟随者）。

结果回填 findings log「执行面子项目 1 验收」节第 13 项（B8 的记账步骤一并写入）。

- [ ] **Step 10: 提交**

```bash
git add apps/desktop/src-tauri/Cargo.toml Cargo.lock apps/desktop/src-tauri/src/quit_dialog.rs \
  apps/desktop/src-tauri/src/lib.rs apps/desktop/src-tauri/src/executor.rs crates/omt-exec \
  apps/desktop/src/types.ts apps/desktop/src/store.ts apps/desktop/src/App.tsx \
  apps/desktop/src/AgentsPanel.tsx apps/desktop/src/agentUi.tsx apps/desktop/src/styles.css \
  apps/desktop/tests/exec-plane-p3.spec.tsx
git commit -m "fix(desktop): Esc cancels the quit dialog; app-level lease notices; single follower per exec

- P3-ACC-6: quit/close confirmation is a native NSAlert on macOS with
  Esc bound to 取消 (rfd's CFUserNotification alert binds no Esc).
- §39: startup housekeeping runs in the setup hook, after single-instance
  has already exited a second instance.
- §47: force-delete lease release has a 30s overall budget; abandoned
  leases become app-level notices (one per deleted agent, appended) with
  exec/run ids and a button to the Runs tab.
- Deleting an agent removes the bindings that point to it.
- §39 frontend: the stop-all result lives in a persistent polite status
  region with a seq-reset timer, failures go to a dismissible alert,
  focus lands on 取消 / returns to 停止全部 / Esc cancels, and a failed
  resume re-attaches the live agents it deferred.
- §39 followers: a follower keeps waiting for a missing transcript while
  the exec is active, and each exec keeps a single follower in the
  desktop process (a re-attach after a webview reload takes over).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task B8: 工具链、测试与记账（P3-ACC-7 / §41 环境变量测试 / F-BENCH-8 / F-BENCH-12 / findings log）

**Files:**
- Create: `apps/desktop/scripts/copy-bytes.mjs`、`apps/desktop/tests/copy-sidecar.spec.ts`
- Modify: `apps/desktop/scripts/copy-sidecar.mjs`
- Modify: `crates/omt-exec/src/shim.rs`（测试改用 `execute_with`）、`crates/omt-exec/src/launch.rs`（`wait_ready` 带超时参数）、`crates/omt-exec/src/daemon_link.rs`（`connect_with`）
- Modify: `crates/omt-harness/src/sanitize.rs`（`sanitize_text`）、`crates/omt-harness/examples/harness-capture.rs`
- Modify: `docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md`

**Interfaces:**
- Produces: `copyBytes(source, dest)`（`apps/desktop/scripts/copy-bytes.mjs`）：`readFileSync` → 写同目录临时名 `<dest>.tmp-<pid>`（mode 0755）→ `chmodSync(0o755)` → `renameSync` 覆盖目标；失败删除临时文件。新 inode 不带源文件的扩展属性（`com.apple.provenance` 等）——`copyFileSync` 在 macOS 走 clonefile，会把 provenance 一起克隆过去，随后 tauri-build 的 `copy_binaries` 克隆该文件时被拒（EPERM）。
- Produces: `pub(crate) fn DaemonLink::connect_with(runtime_dir, kind, daemon_bin, home_path, expected_home_id, rpc_timeout: Duration)`；`connect` = `connect_with(..., rpc_timeout())`；私有 `DaemonLink::enroll` 增加 `timeout: Duration` 参数，A4 的 `served_homes` 改传 `rpc_timeout()`。`launch::wait_ready` 增加 `timeout: Duration` 参数，`start` 传 `ready_timeout()`。生产行为不变（环境变量覆盖仍然有效），但 omt-exec 的单测不再调用 `std::env::set_var` / `remove_var`。macOS 的 `setenv` 与其他线程的 `getenv` 并发是数据竞争，这是 §41 担心 CI 高负载下 flake 的来源。集成测试（`exec_launch.rs` / `exec_daemon_link.rs` 设置 `OMT_HOME`）里每个测试都已经持有文件级 `SERIAL` 锁，不在此改动范围。
- Produces: `pub fn omt_harness::sanitize::sanitize_text(text: &str) -> String`——逐行 `sanitize_line`，保留换行结构（含末尾换行）。`harness-capture` 的 `normalize` 先调用它，再做 HOME / 时间戳 / pid 归一（F-BENCH-8 设计 B：capture 先脱敏再归一）。已入库的语料不重新采集。

- [ ] **Step 1: 写失败测试（sidecar 复制）**

新建 `apps/desktop/tests/copy-sidecar.spec.ts`：

```ts
// @vitest-environment node
/**
 * P3-ACC-7：sidecar 暂存按字节流复制——目标不得携带源文件的扩展属性
 * （com.apple.provenance 会让 tauri-build 的 clonefile 复制 EPERM）、可执行
 * 位为 0755、不留临时文件，且覆盖已存在（带 xattr）的旧副本。
 * 用自定义 xattr `com.omt.test` 代替 provenance（后者用户进程写不了）。
 */
import { describe, expect, it } from 'vitest'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { copyBytes } from '../scripts/copy-bytes.mjs'

const xattrs = (path: string) => execFileSync('xattr', [path], { encoding: 'utf8' })

describe.skipIf(process.platform !== 'darwin')('copyBytes (P3-ACC-7)', () => {
  it('不携带扩展属性、0755、无临时文件残留，并覆盖旧副本', () => {
    const dir = mkdtempSync(join(tmpdir(), 'omt-copy-'))
    const source = join(dir, 'omt-daemon')
    writeFileSync(source, '#!/bin/sh\necho hi\n')
    execFileSync('xattr', ['-w', 'com.omt.test', 'carried', source])
    const dest = join(dir, 'omt-daemon-aarch64-apple-darwin')
    writeFileSync(dest, 'stale')
    execFileSync('xattr', ['-w', 'com.omt.test', 'stale', dest])

    copyBytes(source, dest)

    expect(readFileSync(dest, 'utf8')).toBe('#!/bin/sh\necho hi\n')
    expect(statSync(dest).mode & 0o777).toBe(0o755)
    expect(xattrs(dest)).not.toContain('com.omt.test')
    expect(readdirSync(dir).filter(name => name.includes('.tmp-'))).toEqual([])
  })
})
```

```bash
cd apps/desktop && pnpm test -- copy-sidecar 2>&1 | tail -8
```
Expected：FAIL——`../scripts/copy-bytes.mjs` 不存在。

- [ ] **Step 2: 实现 `copyBytes` 并接入 copy-sidecar**

新建 `apps/desktop/scripts/copy-bytes.mjs`：

```js
/**
 * P3-ACC-7：按字节流复制可执行文件。`copyFileSync` 在 macOS 走 clonefile，
 * 会把源文件的扩展属性（cargo 产物带 com.apple.provenance）一并克隆；sidecar
 * 变化触发 tauri-build 重跑时，它的 copy_binaries 再克隆这个带 provenance
 * 的文件会被系统拒绝（EPERM），且 target/debug/omt-daemon 已被它先删掉。
 * 读写字节流得到的新 inode 不带任何 xattr；先写同目录临时名再 rename，
 * 避免半截文件被别的构建步骤拿去用。
 */
import { chmodSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs'

export function copyBytes(source, dest) {
  const tmp = `${dest}.tmp-${process.pid}`
  try {
    writeFileSync(tmp, readFileSync(source), { mode: 0o755 })
    chmodSync(tmp, 0o755) // umask 可能削掉 writeFileSync 的 mode
    renameSync(tmp, dest)
  } catch (error) {
    rmSync(tmp, { force: true })
    throw error
  }
}
```

`apps/desktop/scripts/copy-sidecar.mjs`：

```js
import { existsSync, mkdirSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { copyBytes } from './copy-bytes.mjs'
```

暂存循环：

```js
for (const bin of sidecars) {
  const source = join(binDir, bin)
  const staged = join(stagingDir, `${bin}-${triple}`)
  // P3-ACC-7：字节流复制，不携带 com.apple.provenance（见 copy-bytes.mjs）
  copyBytes(source, staged)
  console.log(`sidecar staged: ${staged} (from ${profile})`)
}
```

dev 分支里：

```js
    if (devCopy !== source) {
      copyBytes(source, devCopy)
    }
```

```bash
cd apps/desktop && pnpm test -- copy-sidecar 2>&1 | tail -5
cargo build -p omt-runtime --bin omt-daemon --bin omt && cd apps/desktop && node scripts/copy-sidecar.mjs --dev && xattr src-tauri/binaries/omt-daemon-$(rustc --print host-tuple)
```
Expected：spec 通过；脚本打印两行 `sidecar staged:`；最后的 `xattr` 输出里没有 `com.apple.provenance`。

- [ ] **Step 3: omt-exec 单测去掉进程级环境变量（§41 test-env-var-parallel）**

`crates/omt-exec/src/daemon_link.rs`：

```rust
impl DaemonLink {
    fn enroll(runtime_dir: &Path, kind: &str, timeout: Duration) -> Result<Self, String> {
```

`enroll` 函数体里删掉 `let timeout = rpc_timeout();` 一行（其余用到 `timeout` 的地方不变）。`enroll` 的另一个调用方是 A4 Step 5 新增的 `served_homes`（同文件），同步改为：

```rust
    let link = DaemonLink::enroll(runtime_dir, "cli", rpc_timeout())?;
```

（`grep -n "DaemonLink::enroll\|Self::enroll" crates/omt-exec/src/daemon_link.rs` 确认三处调用——`served_homes` 一处、`connect_with` 两处——都带上超时参数。）`connect` 改为委托：

```rust
    pub fn connect(
        runtime_dir: &Path,
        kind: &str,
        daemon_bin: Option<&Path>,
        home_path: &Path,
        expected_home_id: Option<&str>,
    ) -> Result<Self, String> {
        Self::connect_with(
            runtime_dir,
            kind,
            daemon_bin,
            home_path,
            expected_home_id,
            rpc_timeout(),
        )
    }

    /// `connect` 的内芯（§41 test-env-var-parallel：socket 超时从参数注入，
    /// 单测不再 set_var `OMT_DAEMON_RPC_TIMEOUT_MS`）。
    pub(crate) fn connect_with(
        runtime_dir: &Path,
        kind: &str,
        daemon_bin: Option<&Path>,
        home_path: &Path,
        expected_home_id: Option<&str>,
        rpc_timeout: Duration,
    ) -> Result<Self, String> {
        if live_descriptor(runtime_dir).is_none() {
            let bin = daemon_bin.ok_or("daemon 不在线，且没有可拉起的 omt-daemon")?;
            spawn_daemon(bin, runtime_dir)?;
        }
        let mut link = Self::enroll(runtime_dir, kind, rpc_timeout)?;
        if link.home_id_for(home_path).is_none() {
            if expected_home_id.is_some() && !home_path.join("omt.db").is_file() {
                return Err(format!(
                    "{} 不是已初始化的 omt home，拒绝声明（shim 不新建 home）",
                    home_path.display()
                ));
            }
            // daemon 在线但没打开这个 home（idle 关闭 / 由别的面拉起）：声明后
            // 重新注册——declare 返回 requiresRehandshake，旧凭据看不到新 home
            link.call(
                "home/declare",
                serde_json::json!({ "path": home_path.display().to_string() }),
            )?;
            link = Self::enroll(runtime_dir, kind, rpc_timeout)?;
        }
        match (link.home_id_for(home_path), expected_home_id) {
            (None, _) => Err(format!(
                "home {} 声明后仍不在凭据范围内",
                home_path.display()
            )),
            (Some(actual), Some(expected)) if actual != expected => Err(format!(
                "home {} 的 id 是 {actual}，与期望的 {expected} 不一致",
                home_path.display()
            )),
            _ => Ok(link),
        }
    }
```

测试 `wedged_daemon_connection_times_out` 中把

```rust
        std::env::set_var("OMT_DAEMON_RPC_TIMEOUT_MS", "300");
        let started = Instant::now();
        let err = match DaemonLink::connect(&runtime, "cli", None, &home_path, Some("h_wedged")) {
            Ok(_) => panic!("wedged daemon 的 enroll 必须在界内报错"),
            Err(err) => err,
        };
        std::env::remove_var("OMT_DAEMON_RPC_TIMEOUT_MS");
```

替换为：

```rust
        let started = Instant::now();
        let err = match DaemonLink::connect_with(
            &runtime,
            "cli",
            None,
            &home_path,
            Some("h_wedged"),
            Duration::from_millis(300),
        ) {
            Ok(_) => panic!("wedged daemon 的 enroll 必须在界内报错"),
            Err(err) => err,
        };
```

`crates/omt-exec/src/launch.rs`：`wait_ready` 签名加参数，函数体首行改用它：

```rust
fn wait_ready(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    shim_log: &Path,
    timeout: Duration,
) -> Ready {
    let deadline = Instant::now() + timeout;
```

`start()` 的调用加 `ready_timeout()` 作为最后一个实参；测试 `wait_ready_treats_missing_shim_start_id_as_unknown_not_dead` 中把

```rust
        std::env::set_var("OMT_LAUNCH_READY_TIMEOUT_MS", "300");
        let ready = wait_ready(runtime, "h_t", &agent.id, "exec-nostart", &log);
        std::env::remove_var("OMT_LAUNCH_READY_TIMEOUT_MS");
```

替换为：

```rust
        let ready = wait_ready(
            runtime,
            "h_t",
            &agent.id,
            "exec-nostart",
            &log,
            Duration::from_millis(300),
        );
```

`crates/omt-exec/src/shim.rs` 的 `mod tests`：删除 `static LEASE_TIMEOUT_ENV_LOCK` 及其文档注释；`wedged_lease_rpc_completes_with_pending_marker`、`lease_timeout_revokes_temp_grant_immediately`、`slow_leaser_connect_is_bounded_and_revokes_grant` 三个测试里各删掉首行 `let env_guard = LEASE_TIMEOUT_ENV_LOCK.lock().unwrap();` 与末行 `drop(env_guard);`，并把各自的

```rust
        std::env::set_var("OMT_SHIM_LEASE_TIMEOUT_MS", "200");
```

删掉、把紧随其后的 `execute(` 调用与随后的 `std::env::remove_var("OMT_SHIM_LEASE_TIMEOUT_MS");` 替换为（`<leaser>` 分别是该测试里的 `wedged` / `wedged` / `slow`）：

```rust
        let stop = AtomicBool::new(false);
        let code = execute_with(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            <leaser>,
            idle_keepalive(),
            ShimOptions {
                stop: &stop,
                lease_timeout: Duration::from_millis(200),
            },
        );
```

（`wedged_lease_rpc_completes_with_pending_marker` 与 `slow_leaser_connect_is_bounded_and_revokes_grant` 在 `execute` 之前有 `let started = Instant::now();`，保留在 `let stop` 之前。）其余使用 `execute(...)` 的 shim 测试不变：它们的假 Leaser 立即返回，不依赖租约上限。

```bash
git grep -n "set_var\|remove_var" -- crates/omt-exec ; echo "(应为空)"
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
for i in 1 2 3; do cargo test -p omt-exec 2>&1 | grep -E '^test result: FAILED|panicked'; done; echo "(三遍均应为空)"
```
Expected：grep 为空；omt-exec 全绿，连跑三遍无失败（本步是测试缝替换，没有新增失败测试，验证以全绿 + grep 为空为准）。

- [ ] **Step 4: F-BENCH-8——capture 先脱敏**

`crates/omt-harness/src/sanitize.rs`，`sanitize_stderr_tail` 之前加：

```rust
/// F-BENCH-8（评测设计 B）：语料采集的整段文本脱敏——逐行过 sanitize_line，
/// 保留原有换行结构（含末尾换行）。help / models 输出可能回显 env 里的 key
/// （如 `--api-key <KEY>  (current: sk-…)`），harness-capture 先调用本函数，
/// 再做 HOME / 时间戳 / pid 归一。
pub fn sanitize_text(text: &str) -> String {
    let mut out = text.lines().map(sanitize_line).collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}
```

同文件 `mod tests` 加：

```rust
    /// F-BENCH-8：整段脱敏——含密钥的行被截断，普通行与空行原样，行数与
    /// 末尾换行保持不变。
    #[test]
    fn sanitize_text_masks_secret_lines_and_keeps_layout() {
        let input =
            "Usage: x [options]\n  --api-key <KEY>  (current: sk-live-abc123)\nAUTH_TOKEN=abc\n\nplain\n";
        let out = sanitize_text(input);
        assert!(!out.contains("abc123"), "{out}");
        assert!(!out.contains("AUTH_TOKEN=abc"), "{out}");
        assert!(out.starts_with("Usage: x [options]\n"), "{out}");
        assert!(out.ends_with("plain\n"), "{out}");
        assert_eq!(out.lines().count(), input.lines().count());
    }
```

```bash
cargo test -p omt-harness sanitize_text 2>&1 | tail -3
```
先跑一次（实现前）Expected：编译失败（`sanitize_text` 未定义）；加上实现后 Expected：`1 passed`。

`crates/omt-harness/examples/harness-capture.rs` 的 `normalize`：

```rust
/// 输出归一（二轮评审 B + F-BENCH-24）：先逐行脱敏（F-BENCH-8，设计 B——
/// help / models 可能回显 env 里的 key），再 HOME → __HOME__、ISO 时间戳 →
/// __TS__、mcp:<pid> → mcp:__PID__——跨机器 diff 稳定，且不泄真实时间 /
/// pid / 密钥（占位符与语料既有约定一致）。
fn normalize(text: &str) -> String {
    let mut s = omt_harness::sanitize::sanitize_text(text);
    if let Ok(home) = std::env::var("HOME") {
        s = s.replace(&home, "__HOME__");
    }
    let iso = regex::Regex::new(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?",
    )
    .unwrap();
    let mcp_pid = regex::Regex::new(r"mcp:\d+").unwrap();
    mcp_pid
        .replace_all(&iso.replace_all(&s, "__TS__"), "mcp:__PID__")
        .into_owned()
}
```

模块注释里「采样即归一（二轮评审 B）：HOME → `__HOME__`」一行改为「采样即脱敏 + 归一（二轮评审 B / F-BENCH-8）：先 `sanitize_text`，再 HOME → `__HOME__`」。

```bash
cargo build -p omt-harness --examples 2>&1 | tail -2
```
Expected：编译通过（example 需要真实 harness 才能运行，不在 CI 跑）。

- [ ] **Step 5: F-BENCH-12——只核实，不改代码**

```bash
grep -n "上两级\|上三级" crates/omt-harness/src/bench.rs
grep -n "\.\./\.\./fixtures" crates/omt-harness/src/bench.rs | head -2
grep -n "MCP 握手" crates/omt-harness/tests/harness_probe.rs ; echo "(应为空)"
```
Expected：`bench.rs:249` 的注释是「CARGO_MANIFEST_DIR = crates/omt-harness → 上两级是仓库根」，与紧随其后的 `../../fixtures/harness` 一致（Task 1 搬迁时已改；F-BENCH-12 里的「应为三级」说的是 `apps/desktop/src-tauri` 时期，那时是 `../../../fixtures`）；`harness_probe.rs` 已不提「MCP 握手」。F-BENCH-12 在 Step 6 记为 fixed。

- [ ] **Step 6: findings log 记账**

1. 取各 Task 的提交 sha：

```bash
for t in "P3-ACC-2" "launch-path P3s" "lease release and reconcile" "daemon link, keepalive" "storage and security" "degraded agent storage" "Esc cancels" "toolchain"; do
  printf '%s\t%s\n' "$t" "$(git log --format=%h -1 --fixed-strings --grep="$t")"
done
```

记下 B1..B7 的 sha；B8 本身的 sha 在提交后用 `git commit --amend` 回填，或在下一次评审节里补写。下文用 `<B1>` … `<B7>` 指代。

2. 在「总览（open 项在前）」表头之后插入下列行（新行在前）：

```markdown
| P3-ACC-1 | P3 | gated_auto | 执行面/验收 | `executor.rs` DISPATCH_PROMPT 第 4 条 | 派发 prompt 未要求 leaseToken，omp 首次 `run__report` 被拒 | fixed(fe84f27) | — |
| P3-ACC-2 | P3 | gated_auto | daemon/验收 | `omt-domain runs.rs::authorize_report` | 缺 token 的非管理员 report 报「token 不匹配」而非「缺少 leaseToken」 | fixed(<B1>)：规则 `lease-token-missing`，消息指向 run/claim 的 lease.token | — |
| P3-ACC-3 | P3 | gated_auto | 执行面/验收 | DISPATCH_PROMPT 第 7 条 | run completed 后模型多认领一次 | fixed(fe84f27) | — |
| P3-ACC-4 | P3 | gated_auto | 执行面/验收 | `executor.rs::dispatch_daemon_side` ① | 已完成 ticket 续跑报「无未完成可执行成员」，不说明原因 | fixed(<B2>) | — |
| P3-ACC-5 | P3 | gated_auto | 执行面/验收 | `executor.rs::spawn_inner` / `agents.rs::mark_exec_spawned` | agent 状态从不进入 running | fixed(<B2>) | — |
| P3-ACC-6 | P3 | gated_auto | 桌面/验收 | `lib.rs` 退出 / 关窗确认框 | Esc 无效（rfd 无父窗口走 CFUserNotification，不绑 Esc） | fixed(<B7>)：macOS 直调 NSAlert、「取消」绑 Esc；Esc 手工验收见第 13 项 | — |
| P3-ACC-7 | P3 | gated_auto | 工具链/验收 | `apps/desktop/scripts/copy-sidecar.mjs` | `copyFileSync` 带 provenance，tauri-build EPERM | fixed(<B8>)：字节流复制 + 临时名 rename | — |
| P3-ACC-8 | P3 | gated_auto | 执行面/验收 | `executor.rs::release_items_for_exec` | 对已取消 run 释放 → retry CONFLICT，日志误报失败 | fixed(<B3>) | — |
| F-ACC-12 | P2 | gated_auto | 执行面/验收 | `executor.rs::dispatch_daemon_side` ② | 停止后重新派发同一 ticket，item 被桌面端身份激活后卡死 | fixed(5ce2750) | — |
| F-EXEC-17 | P2 | gated_auto | 执行面/桌面 | `follow.rs` / `control.rs::settle_lost_exec` | app 运行期间 shim 崩溃不可见，租约持有到下次启动 | fixed(b2f8aca) | — |
| F-DAEMON-1 | P1 | gated_auto | daemon | `server.rs` / `events.rs` | events/resume 每次调用新增一个永不撤销的订阅与转发线程 | fixed(4e8c92e) | — |
```

3. 就地更新既有行的「状态」列：

| 行 | 新状态 |
|---|---|
| F-EXEC-1 | `fixed(e292202)；P3 降级永久 / 无恢复入口 / 横幅文案 → fixed(<B6>)` |
| F-EXEC-2 | `fixed(bbdc194)；P3 残口 fixed(501d898)；launcher SIGTERM 补偿分支补测试 fixed(<B2>)` |
| F-EXEC-10 | `fixed(cd332d1)；P3 副作用：临时授权残留 fixed(501d898)，item 卡 failed fixed(<B3>)` |
| F-EXEC-11 | `fixed(cd332d1)；kill_orphan 两步写 fixed(<B4>)` |
| F-EXEC-12 | `fixed(501d898)；§43 P3 fixed(c72830c)` |
| F-EXEC-16 | `fixed(c8abc6a)；§47 P3（释放结果展示、总时限）fixed(<B7>)` |
| F-BENCH-8 | `fixed(2b8e036 + <B8>)：capture 先 sanitize_text 再归一` |
| F-BENCH-12 | `fixed：bench.rs「上两级」在 Task 1 搬迁后已与 ../../fixtures 一致；probe 模块注释已不提 MCP 握手（<B8> 核实）` |
| F-U15-3 | `verified（2026-10-22 第二实例聚焦手工验收通过，见「执行面子项目 1 验收」追加项）` |
| F-U2-7 | `fixed：spawn_inner 写 session_header + 事件 jsonl（F-U12-4 / KD-E3），<B8> 核实` |
| F-U4a-1 | `fixed：parseFailed 粘性（has_unrecovered_corrupt + 旧版文件判定，F-EXEC-1），<B8> 核实` |
| F-U4a-7 | `fixed：agents::append_exec（U11），<B8> 核实` |
| F-U1-4 | `superseded(F-ACC-12)：omp 无头 + 放行 flag 已实测（-p / --approval-mode=yolo）` |
| F-U1-7 | `fixed(<B2>)：probe_version 同时读 stderr，stdout 首行为空时回落 stderr 首行` |

关闭前逐条核实：

```bash
grep -n '"kind": "session_header"' crates/omt-exec/src/executor.rs
grep -n "has_unrecovered_corrupt(&legacy)" crates/omt-exec/src/agents.rs
grep -n "pub fn append_exec" crates/omt-exec/src/agents.rs
grep -n '"--approval-mode=yolo"' crates/omt-harness/src/harness.rs
grep -n "fn probe_version_falls_back_to_stderr_when_stdout_is_empty" crates/omt-harness/src/harness.rs
```
Expected：五条各命中一行（对应关闭依据；最后一条是 B2 Step 8 加的 F-U1-7 测试）。

4. 「待办的跨阶段事项」里把这几项勾掉并注明依据：「U9 落地 bindings.json 时确认走 atomic_json…」（`write_binding` 走 `update_json`；粘性判定见 F-U4a-1）、「计划定 transcript `.jsonl` header/事件写入的归属（F-U2-7）」、「P0 桌面冒烟时执行 F-U15-3 的手工双开验证」、「U4 前关 F-U3-15/16…」（d53b191）。

5. 「执行面子项目 1 验收」表第 13 项结果列追加 B7 Step 9 的手工验收结论（Esc / Return / 关窗 Esc / 连按 ⌘Q / 第二实例无清扫日志 / 重载后不重复推事件）。

6. 文末追加一节：

```markdown
### §48. Part B · P3 清理（<B1>..<B8>）

按 Part B 分诊表落地：B1 daemon 缺 token 规则（P3-ACC-2）；B2 派发链与拉起（P3-ACC-4/5、§41 拉起前停止、§39 宽限期停止、§41 launcher 补偿分支测试、F-U1-7 版本探测回落 stderr）；B3 租约释放与对账（P3-ACC-8、§41 failed 残项、§39 快照回写、§39 对账器串行——进程内排队各跑一轮、跨进程跳过，§45/§47 假阴性按存活）；B4 daemon_link 与 shim（§39 陈旧 descriptor / daemon 僵尸 / 保活下限 / 授权泄漏，orphan 整组强杀 + 单事务）；B5 存储与安全（§39 tail 穿越与迁移中断回落、MCP 清扫锁内快照与 fsync，§41 删除四项 + 残留清扫）；B6 旧版 home 降级说明、损坏 agent 的解冻说明与两步「确认并归档」（§41）；B7 桌面 UI（P3-ACC-6 Esc、§47 应用级被放弃租约通知 + 前往 Runs 与总时限、删除时移除绑定、§39 前端三项、§39 清扫挪进 setup、§39 跟随者不提前放弃 / 每 exec 唯一）；B8 工具链与记账（P3-ACC-7、§41 测试环境变量、F-BENCH-8、F-BENCH-12 核实）。

**won't-fix**：§39 dock-quit-bypass（效果等同后台继续）、§45 run-on-main-thread-drop（事件循环退出后才可达；B7 后不再经过插件）、§47 force-double-release（已接受的取舍）。

**后续项**：daemon janitor（过期租约回收）；omp title 事件与 §17 三项（孤儿 transcript 采纳 / pid 复用 cmdline 校验 / 从 transcript 重建损坏 agents.json）——增强性质而非缺陷 / 窄崩溃路径，不在 P3 清理范围；桌面端派发的 node/execute 先于校验（CLI/MCP 发起已由 Q-2/A2 绕开 node/execute；桌面端派发仍先 node/execute，校验前置列入后续项）。

**待手工验收**：ACC-14；ACC-6 的 UI 确认；B7 Step 9（Esc / 第二实例无清扫日志 / 重载后不重复推事件）。

**验证**：fmt、clippy `-D warnings` 干净；`cargo test --workspace` 三遍全绿；`nm -a target/debug/omt-daemon | grep -c -E 'omt_exec|omt_harness'` = 0（KD-E9）；桌面 vitest 全绿、tsc 干净。
```

- [ ] **Step 7: 全量验证并提交**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
for i in 1 2 3; do cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked'; done; echo "(三遍均应为空)"
cargo build -p omt-runtime --bin omt-daemon && nm -a target/debug/omt-daemon | grep -c -E 'omt_exec|omt_harness'
cd apps/desktop && pnpm test && pnpm exec tsc --noEmit
```
Expected：fmt / clippy 干净；workspace 三遍无失败；`nm` 计数 `0`；桌面 vitest 全绿、tsc 干净。

```bash
git add apps/desktop/scripts apps/desktop/tests/copy-sidecar.spec.ts crates/omt-exec crates/omt-harness \
  docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md
git commit -m "chore: toolchain, test-seam and findings-log bookkeeping for Part B

- P3-ACC-7: copy-sidecar copies bytes (no com.apple.provenance) via a
  temp name + rename.
- §41: omt-exec unit tests inject timeouts instead of set_var.
- F-BENCH-8: harness-capture sanitizes before normalizing.
- Findings log: P3-ACC-1..8 / F-ACC-12 / F-EXEC-17 / F-DAEMON-1 rows,
  stale pre-§39 rows closed after verification, §48 summary.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

