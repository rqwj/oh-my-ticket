---
title: 执行面下沉 · 子项目 1 实施计划（crate 拆分 + shim + 存储拆分 + 桌面端改走 crate）
type: plan
status: draft
date: 2026-10-20
design: docs/plans/2026-10-20-0802-feat-omt-exec-plane-orchestration-design.md（§3.1–§3.4、§5、§6、§7 子项目 1）
findings_log: docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md
---

# 执行面下沉 · 子项目 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把桌面端 Tauri 后端里的执行面（harness 注册/探测/启动参数、agent 存储、MCP 注入、子进程执行与收尾）拆成两个共享 crate，执行改由每 exec 一个分离的 `omt exec-shim` 进程承担。桌面端功能保持不变，但**关闭桌面端后执行继续**。

**Architecture:** `crates/omt-harness`（纯库：REGISTRY、探测、启动参数组装、流 adapters、评测分析器、atomic_json）与 `crates/omt-exec`（agent 存储、MCP 注入、执行器、launcher、shim、控制与对账、jsonl 尾随）。桌面端只保留 tauri command 包装：先在进程内做派发链的 daemon 侧步骤（①–⑥），再写 launch spec，用 `setsid` 拉起 `omt exec-shim`，等它报告就绪。shim 负责 MCP 注入 → spawn harness → 事件写入 `<agentDir>/<execId>.jsonl` → 收尾（含租约释放）→ 退出。前端经桌面端尾随 jsonl 拿到实时事件，事件形状与现在一致。

**Tech Stack:** Rust 2021（workspace crates）、Tauri 2、`omt-client`（JSON-RPC over UDS）、libc（flock / setsid / kill / proc_pidinfo）、React + vitest（桌面前端）。

## Global Constraints

- **daemon 零改动**（KD-E9）：不改 `crates/omt-runtime/src/{server,dispatch,auth,…}.rs` 的行为，不新增 daemon 方法；`omt-daemon` 二进制不得链接 `omt_exec`（Task 8 用 `nm` 验证）。
- **前端组件与既有 vitest 不变**是验收门（设计 §3.4）。允许新增可选 props、新增 bridge 函数和新测试文件，但不得修改既有测试的断言。
- **secrets 纪律**（旧设计 §3.1）：env 值绝不落盘，launch spec 里也不能有；argv 进事件或日志前一律经 `sanitize_args`。
- **单写者**：所有机器本地 JSON 状态文件只经 `omt_harness::atomic_json`（进程内锁 + companion flock + tmp/rename）读写。
- **平台**：v1 仅 macOS（与现状一致）；保留现有 `#[cfg(target_os = "linux")]` 分支，但不新增 Windows 支持。
- **版本**：新 crate 用 `version.workspace = true`、`edition.workspace = true`、`license.workspace = true`。
- **提交**：每个 Task 结束提交一次；提交信息末尾加 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`；不要提交 `.tmp-tauri-research/`。
- **验证基线**：每个 Task 收尾都跑 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`，桌面端相关 Task 加跑 `cd apps/desktop && pnpm test && pnpm exec tsc --noEmit`。不设线程上限，默认线程数下必须全绿。

## 决策记录（本计划新增，设计未覆盖或需要细化的点）

| ID | 决策 | 理由 |
|---|---|---|
| **P-1** | **shim 释放租约时临时自授 admin**：连接 daemon（kind `cli`，principal `cli:<shimPid>`）→ 把 principal 写入 `admin-grants.json` → 释放租约 → 删除该条目。desktop 的 `register_admin_grant` 改用同一个 flock 保护的 helper；临时条目额外登记在 `exec/temp-grants.json`，启动清扫会移除 pid 已死的残留 | 用户已确认（2026-10-20）。daemon 的无 token `run/report` 只认 admin，而 principal 形如 `<kind>:<pid>`；desktop 本来就是这样自授的，daemon 零改动 |
| **P-2** | **`atomic_json` 放在 `omt-harness`**（设计 §3.1 原列在 `omt-exec`） | `harness::upsert_config` 本身依赖它，而依赖方向是 `omt-exec → omt-harness`；放低一层避免循环依赖 |
| **P-3** | **派发链的 daemon 侧步骤（①–⑥）留在发起方进程**，shim 只做 ⑦ MCP 注入、写 prompt 文件、⑧ spawn 与收尾 | `node/execute` 是 adapter_only，shim 以 `cli` 身份调不了；发起方同步执行这些步骤，既保留现有的同步报错体验，也保留补偿表（cancel 新建 run）。子项目 2 再处理 CLI/MCP 发起时如何绕开 `node/execute`（设计 §3.5） |
| **P-4** | **launch spec 只记录重算启动参数所需的输入**（harness / model / resume / unattended / prompt / 路径），不存 env；shim 用 `resolve_launch` + `compose_launch_args` 重新计算 | secrets 不落新文件；env 只在 `harness/config.json`（用户本来就在那里配置） |
| **P-5** | **就绪握手**：launcher 拉起 shim 后轮询 agent.json，等 `exec.pid` 出现（harness 已 spawn）或 agent 进入 `Failed`；30s 超时则返回成功并附带警告 | 保留「spawn 失败 = exec_start 报错」的现有契约（例如 MCP 注入失败时弹窗直接报错） |
| **P-6** | **实时 transcript 只用轮询**（200ms），不引入文件事件依赖 | YAGNI；延迟 ≤200ms 对展示足够。设计 §3.4 的「文件事件 + 轮询兜底」简化为纯轮询 |
| **P-7** | **删除 agent 会连同它目录下的 transcript 一起删除** | 按 agent 拆目录后的自然语义；以前 transcript 留在 `transcripts/` 里成了无主文件 |
| **P-8** | **启动对账替代 demote**：shim 仍活 → `attachment=live`（可控）；shim 已死 → `Failed` + `SHIM_LOST_MARKER`，harness 进程仍活则 `attachment=orphan`；最近 60s 内注册、还没记下 shim pid 的 exec 跳过（可能另一个进程正在拉起） | KD-D7 被推翻：重启后的 exec 仍然受控 |
| **P-9** | **MCP 项目级注入（omp/pi）加跨进程 flock**；崩溃残留清扫跳过仍有存活 exec 的 workspace 和 launcher | 多个 shim 进程会共写同一个 `state.json` 的引用计数；桌面重启时清扫不能摘掉正在运行的 exec 的注入 |
| **P-10** | **前端新增两项**：Agents 面板的「停止全部」按钮（设计 §3.4 要求，替代「退出即终止」）；重启后选中仍在运行的 agent 时，用 `exec_attach` 重新接上实时流 | 都以**可选 props** 接入，既有测试不受影响 |

## 文件结构

```
crates/omt-harness/                     # 新建（Task 1）
  Cargo.toml
  src/lib.rs                            # 模块声明 + 兼容 re-export
  src/adapters.rs                       # ← apps/desktop/src-tauri/src/adapters.rs（git mv）
  src/atomic_json.rs                    # ← …/atomic_json.rs（git mv；Task 3 加 lock_exclusive/write_atomic）
  src/bench.rs                          # ← …/bench.rs（git mv；fixtures 路径改两级）
  src/harness.rs                        # ← …/harness.rs（git mv；tauri commands 与登录 helper 切回桌面）
  src/sanitize.rs                       # ← executor.rs 的 sanitize_line / sanitize_stderr_tail / sanitize_args
  tests/harness_probe.rs                # ← apps/desktop/src-tauri/tests/harness_probe.rs
  examples/harness-capture.rs           # ← apps/desktop/src-tauri/examples/harness-capture.rs

crates/omt-exec/                        # 新建（Task 2 起）
  Cargo.toml
  src/lib.rs
  src/agents.rs                         # ← …/agents.rs（存储半；Task 4 改为按 agent 拆分）
  src/mcp_injection.rs                  # ← …/mcp_injection.rs（Task 6 加 flock 与 live-aware 清扫）
  src/executor.rs                       # ← …/executor.rs（内核：ExecutionManager、收尾、对账、派发链）
  src/layout.rs                         # 新（Task 4）：存储路径
  src/migrate.rs                        # 新（Task 5）：旧 agents.json → 按 agent 拆分
  src/grants.rs                         # 新（Task 7）：admin-grants.json 加锁读写 + 临时授权
  src/daemon_link.rs                    # 新（Task 7）：shim 侧 daemon 连接（discover-or-spawn）
  src/spec.rs                           # 新（Task 8）：LaunchSpec
  src/shim.rs                           # 新（Task 8）：shim 主循环
  src/launch.rs                         # 新（Task 9）：launcher（派发链 + 拉起 shim + 就绪握手）
  src/control.rs                        # 新（Task 9）：查找 / 存活 / 停止 / 启动对账
  src/follow.rs                         # 新（Task 10）：jsonl 尾随

crates/omt-runtime/
  Cargo.toml                            # 依赖 omt-exec（Task 8）
  src/cli/mod.rs                        # 隐藏动词 exec-shim（Task 8）
  tests/exec_shim.rs                    # 新（Task 8/12）：真二进制端到端

apps/desktop/src-tauri/
  Cargo.toml                            # 依赖 omt-harness / omt-exec
  src/lib.rs                            # 去掉移走的 mod、ExitRequested kill_all；启动清扫
  src/harness.rs                        # 只剩 5 个 tauri command + 登录 helper
  src/agents.rs                         # 只剩 4 个 tauri command
  src/executor.rs                       # 只剩 exec_* tauri command + rpc 适配
  src/state.rs                          # 删除 ExecutionState（Task 10）
  src/daemon.rs                         # register_admin_grant 改走 omt_exec::grants（Task 7）

apps/desktop/src/
  bridge.ts / store.ts / AgentsPanel.tsx / App.tsx / types.ts   # Task 11
apps/desktop/tests/exec-plane.spec.tsx  # 新（Task 11）
```

**依赖方向**：`omt-harness ← omt-exec ← {omt-runtime 的 omt CLI, apps/desktop}`。`omt-exec` 还依赖 `omt-client`。`omt-harness` 只依赖 serde / serde_json / libc。

## 阶段与任务一览

| 阶段 | Task | 交付 | 每阶段结束时的可用状态 |
|---|---|---|---|
| A 纯搬迁 | 1 omt-harness · 2 omt-exec | 两个 crate 编译通过、测试随代码迁移 | 桌面端行为完全不变 |
| B 存储 | 3 atomic_json 修复 · 4 按 agent 拆分 · 5 迁移 · 6 MCP 跨进程锁 | 新布局 + 一次性迁移 | 桌面端行为不变（数据换位置） |
| C 执行面 | 7 grants + daemon_link · 8 spec + shim · 9 launcher + control | `omt exec-shim` 可独立运行 | 桌面端仍走旧的进程内路径 |
| D 接线 | 10 桌面端改走 launcher · 11 前端补两项 · 12 验收与文档 | 关闭桌面端后执行继续 | 子项目 1 完成 |

开工前记录基线测试数（后面每个搬迁 Task 都要对数）：

```bash
cargo test --workspace 2>&1 | grep -E '^test result' | awk '{s+=$4} END {print "passed:", s}'
cd apps/desktop && pnpm test 2>&1 | grep -E 'Tests +[0-9]+ passed'
```

---

### Task 1: 新建 `omt-harness` crate（纯搬迁）

**Files:**
- Create: `crates/omt-harness/Cargo.toml`, `crates/omt-harness/src/lib.rs`, `crates/omt-harness/src/sanitize.rs`
- Move: `apps/desktop/src-tauri/src/{adapters,atomic_json,bench,harness}.rs` → `crates/omt-harness/src/`
- Move: `apps/desktop/src-tauri/tests/harness_probe.rs` → `crates/omt-harness/tests/`；`apps/desktop/src-tauri/examples/harness-capture.rs` → `crates/omt-harness/examples/`
- Modify: `apps/desktop/src-tauri/Cargo.toml`, `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/src/executor.rs`（引用路径）
- Create（切回来的部分）: `apps/desktop/src-tauri/src/harness.rs`（只含 tauri command 与登录 helper）

**Interfaces:**
- Produces: `omt_harness::{adapters, atomic_json, bench, harness, sanitize}` 模块；`omt_harness::{descriptor_of, probe_version, which_binary, HarnessDescriptor, REGISTRY}` re-export；`omt_harness::sanitize::{sanitize_line, sanitize_stderr_tail, sanitize_args}`（均为 `pub fn`，签名不变）；`omt_harness::atomic_json::OpenOptionsUnix` 改为 `pub trait`。
- `harness.rs` 里凡被桌面端 command 引用的函数改为 `pub`：`probe_all`、`harness_entry`、`launch_preview_value`、`parse_models_output`、`capture_with_timeout`、`augmented_path`、`which_binary_in`（以及编译器报错指出的其他函数）。

- [ ] **Step 1: 记录桌面 crate 当前测试数**

```bash
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result'
```
记下 lib 测试数（当前 128）和 `harness_probe` 测试数（1 passed + 2 ignored），Step 9 对数用。

- [ ] **Step 2: 建 crate 骨架**

`crates/omt-harness/Cargo.toml`：

```toml
[package]
name = "omt-harness"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Harness registry, probing, launch composition, stream adapters and the benchmark analyzer — the pure-library half of the exec plane (design §3.1)."

[dependencies]
libc = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
regex = "1"
tempfile = { workspace = true }
```

`crates/omt-harness/src/lib.rs`：

```rust
//! omt-harness：执行面设计 §3.1 的纯库半——REGISTRY / 探测 / 启动参数 /
//! 流 adapters / 评测分析器 / 本地 JSON 状态文件的原子读写。不管进程生命
//! 周期（那是 omt-exec）。

pub mod adapters;
pub mod atomic_json;
pub mod bench;
pub mod harness;
pub mod sanitize;

/// F-BENCH-5：探针 / capture / 分析器的最小 pub 面——registry 驱动。
pub use harness::{descriptor_of, probe_version, which_binary, HarnessDescriptor, REGISTRY};
```

- [ ] **Step 3: git mv 四个模块与探针、capture**

```bash
mkdir -p crates/omt-harness/src crates/omt-harness/tests crates/omt-harness/examples
git mv apps/desktop/src-tauri/src/adapters.rs     crates/omt-harness/src/adapters.rs
git mv apps/desktop/src-tauri/src/atomic_json.rs  crates/omt-harness/src/atomic_json.rs
git mv apps/desktop/src-tauri/src/bench.rs        crates/omt-harness/src/bench.rs
git mv apps/desktop/src-tauri/src/harness.rs      crates/omt-harness/src/harness.rs
git mv apps/desktop/src-tauri/tests/harness_probe.rs     crates/omt-harness/tests/harness_probe.rs
git mv apps/desktop/src-tauri/examples/harness-capture.rs crates/omt-harness/examples/harness-capture.rs
```

- [ ] **Step 4: 把 tauri command 与登录 helper 切回桌面端**

在 `crates/omt-harness/src/harness.rs` 中**剪切**以下条目（连同各自的 doc 注释），粘贴进新建的 `apps/desktop/src-tauri/src/harness.rs`：
- `harness_launch_preview`、`harness_list_models`、`harness_open_login`、`harness_list`、`harness_config_set` 五个 `#[tauri::command(async)]` 函数；
- 登录 helper：`resolve_login_program`、`shell_quote`、`escape_applescript_string`、`terminal_login_script`；
- 这两个 helper 的测试：`terminal_login_script_escapes_hostile_paths`、`resolve_login_program_works_without_enabled_config`（放进桌面 `harness.rs` 底部新的 `#[cfg(test)] mod tests`）。

桌面端 `apps/desktop/src-tauri/src/harness.rs` 的文件头：

```rust
//! 设置页 Harnesses 数据面的 tauri command 包装。内核在 omt_harness::harness
//! （执行面设计 §3.1）；这里只负责 runtime dir 解析与登录终端拉起。

use std::path::{Path, PathBuf};

use omt_harness::harness::{
    descriptor_of, harness_entry, launch_preview_value, load_configs, parse_models_output,
    probe_all, resolve_launch, upsert_config, validate_config, HarnessUserConfig,
};

use crate::daemon;
```

`crates/omt-harness/src/harness.rs` 里删除 `use crate::daemon;`。粘贴进来的函数体若还引用了别的内核函数，就加进上面的 `use` 列表；内核里对应的函数若是私有或 `pub(crate)`，改成 `pub`。上面的 `use` 列表是预估，Step 8 编译后删掉未使用的条目，否则 `clippy -D warnings` 会失败。

- [ ] **Step 5: 把 sanitize 三个函数移进 `omt-harness`**

从 `apps/desktop/src-tauri/src/executor.rs` 剪切 `sanitize_line`、`sanitize_stderr_tail`、`sanitize_args` 及其测试（`sanitize_args_redacts_key_value_flags` 以及其他以 `sanitize_` 开头的测试），放进 `crates/omt-harness/src/sanitize.rs`：

```rust
//! §3.1 secrets 纪律：argv / stderr 进事件、日志、持久化之前的行级脱敏。
//! 执行器（omt-exec）与 launcher 共用。

// ← 这里粘贴原 executor.rs 的三个函数，把 `fn` 改为 `pub fn`

#[cfg(test)]
mod tests {
    use super::*;
    // ← 这里粘贴原 executor.rs 中以 sanitize_ 开头的测试
}
```

`executor.rs` 顶部加：

```rust
use omt_harness::sanitize::{sanitize_args, sanitize_stderr_tail};
```

- [ ] **Step 6: 修路径与引用**

```bash
# bench / 探针 / capture 的 fixtures 路径：src-tauri 下三级 → crates/omt-harness 下两级
sed -i '' 's#"\.\./\.\./\.\./fixtures#"../../fixtures#g' \
  crates/omt-harness/src/bench.rs crates/omt-harness/tests/harness_probe.rs crates/omt-harness/examples/harness-capture.rs
# 探针 / capture 的 crate 名
sed -i '' 's/oh_my_ticket_desktop_lib::/omt_harness::/g' \
  crates/omt-harness/tests/harness_probe.rs crates/omt-harness/examples/harness-capture.rs
# 桌面端剩余模块里对搬走模块的引用
sed -i '' -e 's/crate::adapters/omt_harness::adapters/g' \
          -e 's/crate::atomic_json/omt_harness::atomic_json/g' \
          -e 's/use crate::harness;/use omt_harness::harness;/g' \
          -e 's/crate::harness::/omt_harness::harness::/g' \
  apps/desktop/src-tauri/src/executor.rs apps/desktop/src-tauri/src/agents.rs \
  apps/desktop/src-tauri/src/mcp_injection.rs
grep -rn 'fixtures' crates/omt-harness | grep -v '\.\./\.\./fixtures' ; echo "(应为空)"
```

`bench.rs` 的 `fixture()` 函数注释（「上两级是仓库根」）同步改为「`crates/omt-harness` 上两级是仓库根」。`atomic_json.rs` 的 `pub(crate) trait OpenOptionsUnix` 改为 `pub trait OpenOptionsUnix`。

- [ ] **Step 7: 改桌面端 Cargo 与 lib.rs**

`apps/desktop/src-tauri/Cargo.toml` 的 `[dependencies]` 加：

```toml
omt-harness = { path = "../../../crates/omt-harness" }
```

`[dev-dependencies]` 删掉 `regex = "1"`（它的使用方已全部移走；若 `cargo build --tests` 报缺，再加回来）。

`apps/desktop/src-tauri/src/lib.rs`：删掉 `mod adapters;`、`mod atomic_json;`、`mod bench;`，以及 F-BENCH-5 那行 `pub use harness::{…};` 和它上面的注释；`mod harness;` 保留（现在是 command 文件）。

- [ ] **Step 8: 编译并修可见性**

```bash
cargo build -p omt-harness -p oh-my-ticket-desktop --all-targets 2>&1 | grep -E '^error' -A6 | head -60
```

报 `private` / `pub(crate)` 的条目改为 `pub`；报 `unresolved import` 的补 `use omt_harness::…`。反复执行直到无错误。

- [ ] **Step 9: 对数并跑测试**

```bash
cargo test -p omt-harness 2>&1 | grep -E '^test result'
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result'
```

Expected：`omt-harness` lib 测试数 + 桌面 lib 测试数 = Step 1 的 128；`omt-harness --test harness_probe` 为 1 passed、2 ignored；全部 0 failed。

```bash
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check
cd apps/desktop && pnpm test && pnpm exec tsc --noEmit
```

- [ ] **Step 10: 提交**

```bash
git add -A crates/omt-harness apps/desktop/src-tauri Cargo.lock
git commit -m "refactor: extract omt-harness crate (registry, adapters, bench, atomic_json, sanitize)

Pure move per exec-plane design §3.1; desktop keeps only the tauri
command wrappers and the login-terminal helpers. Test count unchanged.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: 新建 `omt-exec` crate（纯搬迁 + 去掉 tauri / SharedSession 依赖）

**Files:**
- Create: `crates/omt-exec/Cargo.toml`, `crates/omt-exec/src/lib.rs`
- Move: `apps/desktop/src-tauri/src/{agents,mcp_injection,executor}.rs` → `crates/omt-exec/src/`
- Create（切回来的部分）: `apps/desktop/src-tauri/src/agents.rs`、`apps/desktop/src-tauri/src/executor.rs`（只含 command）
- Modify: `apps/desktop/src-tauri/src/{lib,state,rpc_bridge}.rs`, `apps/desktop/src-tauri/Cargo.toml`

**Interfaces:**
- Consumes: Task 1 的 `omt_harness::{adapters, atomic_json, harness, sanitize}`。
- Produces（后续 Task 依赖的名称）：
  - `omt_exec::executor::RpcFn = Arc<dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>`
  - `omt_exec::executor::LeaseRelease`（`pub type`，签名不变：`Arc<dyn Fn(&str, &str, &str) + Send + Sync>`）
  - `ExecutionManager::new() -> Self`（**不再**做 demote / sweep）、`ExecutionManager::set_rpc(&self, rpc: RpcFn)`、`ExecutionManager::set_lease_release(&self, f: LeaseRelease)`、`ExecutionManager::with_max_duration(max: Duration) -> Self`
  - `omt_exec::executor::startup_housekeeping(runtime_dir: &Path)`（原 `ExecutionManager::new` 里的 demote + sweep）
  - `omt_exec::executor::exec_start_impl(rpc: &RpcFn, manager: &ExecutionManager, omt_path: &Path, runtime_dir: &Path, home_id, home_path, node_id, provider_id, unattended, model, resume_session, target_agent_id, sink) -> Result<ExecStartOutcome, String>`
  - `omt_exec::executor::reconcile_interrupted_execs(rpc: &RpcFn, runtime_dir: &Path) -> usize`
  - `omt_exec::executor::release_items_for_exec(rpc: &dyn Fn(&str, Value) -> Result<Value, String>, home_id, run_id, exec_id) -> Result<(), String>`（`pub`；run/get 失败时返回 Err，其余同原逻辑）
  - `omt_exec::executor::{transcript_tail_from, kill_orphan, process_start_identity, workspace_root_of, wait_for}` 改为 `pub`；常量 `STARTUP_WINDOW`、`DEFAULT_MAX_DURATION`、`KILL_GRACE` 改为 `pub const`
  - `omt_exec::mcp_injection::prepare(omt_path: &Path, runtime_dir, workspace_root, provider_id, exec_id, persistent)`（即原 `prepare_with`，改名并改为 `pub`；原基于 `daemon::omt_sidecar_path` 的 `prepare` 删除）
  - `omt_exec::agents::{agents_list_value, bindings_get_value, bindings_gc_value}`（原 `agents_list_in` / `bindings_get_in` / `bindings_gc_in`，改为 `pub`）；`now_epoch_ms`、`validate_home_id` 改为 `pub`

- [ ] **Step 1: 建 crate 骨架**

`crates/omt-exec/Cargo.toml`：

```toml
[package]
name = "omt-exec"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Exec plane: agent storage, MCP injection, executor, launcher and the per-exec shim (design §3.1–§3.3). Never linked into omt-daemon."

[dependencies]
omt-client = { path = "../omt-client" }
omt-harness = { path = "../omt-harness" }
libc = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

`crates/omt-exec/src/lib.rs`：

```rust
//! omt-exec：执行面（设计 §3.1–§3.3）。agent 存储、MCP 注入、执行器内核；
//! 后续 Task 加入 launcher / shim / 控制与对账。daemon 代码路径不引用本 crate。

pub mod agents;
pub mod executor;
pub mod mcp_injection;
```

- [ ] **Step 2: git mv**

```bash
mkdir -p crates/omt-exec/src
git mv apps/desktop/src-tauri/src/agents.rs        crates/omt-exec/src/agents.rs
git mv apps/desktop/src-tauri/src/mcp_injection.rs crates/omt-exec/src/mcp_injection.rs
git mv apps/desktop/src-tauri/src/executor.rs      crates/omt-exec/src/executor.rs
```

- [ ] **Step 3: 切回 agents 的 4 个 command**

从 `crates/omt-exec/src/agents.rs` 剪切 `agents_delete`、`agents_list`、`bindings_get`、`bindings_gc` 四个 `#[tauri::command(async)]` 函数，删掉 `use crate::daemon;`。把 `agents_list_in` / `bindings_get_in` / `bindings_gc_in` 改名为 `agents_list_value` / `bindings_get_value` / `bindings_gc_value` 并改为 `pub fn`：

```bash
sed -i '' -e 's/\bagents_list_in\b/agents_list_value/g' \
          -e 's/\bbindings_get_in\b/bindings_get_value/g' \
          -e 's/\bbindings_gc_in\b/bindings_gc_value/g' crates/omt-exec/src/agents.rs
```

新建 `apps/desktop/src-tauri/src/agents.rs`：

```rust
//! Agents / bindings 面板的 tauri command 包装；存储内核在 omt_exec::agents。

use crate::daemon;

#[tauri::command(async)]
pub fn agents_list(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::agents_list_value(&daemon::runtime_dir(), &home_id)
}

#[tauri::command(async)]
pub fn agents_delete(home_id: String, agent_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::delete_agent(&daemon::runtime_dir(), &home_id, &agent_id)?;
    Ok(serde_json::json!({ "deleted": agent_id }))
}

#[tauri::command(async)]
pub fn bindings_get(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::bindings_get_value(&daemon::runtime_dir(), &home_id)
}

#[tauri::command(async)]
pub fn bindings_gc(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::bindings_gc_value(&daemon::runtime_dir(), &home_id)
}
```

- [ ] **Step 4: mcp_injection 去掉 daemon 依赖**

在 `crates/omt-exec/src/mcp_injection.rs` 中：
1. 删除 `use crate::daemon;` 和原 `pub fn prepare(...)`（内部调 `daemon::omt_sidecar_path()` 的那个）；
2. 把 `pub(crate) fn prepare_with(` 改为 `pub fn prepare(`，doc 注释改为：

```rust
/// 注入准备：sidecar 预检 + 按 provider 分派。任何失败 = 派发失败（绝不
/// spawn 无 MCP 的 harness）。`omt_path` 是 MCP 桥二进制（桌面端 sidecar
/// 或 shim 自身所在目录的 `omt`）；`persistent` 供 U7 设置项透传。
```

3. 全仓替换调用名：

```bash
grep -rln 'prepare_with' crates/omt-exec | xargs sed -i '' 's/mcp_injection::prepare_with/mcp_injection::prepare/g; s/\bprepare_with(/prepare(/g'
```

4. `use omt_harness::atomic_json::OpenOptionsUnix;` 保持（Task 1 已改为 `pub trait`）。

- [ ] **Step 5: executor 去掉 tauri / SharedSession / daemon 依赖**

在 `crates/omt-exec/src/executor.rs` 中：

1. 删除 `use crate::state::SharedSession;`、`use crate::daemon;`，`use crate::harness` 改为 `use omt_harness::harness;`。
2. 剪切 `exec_stop`、`exec_kill_orphan`、`exec_start`、`exec_transcript_tail` 四个 `#[tauri::command(async)]` 函数，以及 `daemon_rpc`、`release_lease_on_terminal`，移到桌面端（Step 6）。
3. 在 `type LeaseRelease` 旁新增并改为 pub：

```rust
/// daemon RPC 通道（桌面端由 SharedSession 适配；shim 由 daemon_link 适配）。
pub type RpcFn =
    Arc<dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;
/// 终态租约释放回调（home_id, run_id, exec_id）。
pub type LeaseRelease = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
```

4. 用下面三段替换 `ExecutionManager::new` 与 `set_shared_session`：

```rust
impl Default for ExecutionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionManager {
    /// 纯构造（不碰磁盘）。启动清扫见 [`startup_housekeeping`]。
    pub fn new() -> Self {
        Self::with_kill_grace(STARTUP_WINDOW, DEFAULT_MAX_DURATION, KILL_GRACE)
    }

    /// shim 用：每 exec 一个 manager，maxDuration 来自 launch spec。
    pub fn with_max_duration(max_duration: Duration) -> Self {
        Self::with_kill_grace(STARTUP_WINDOW, max_duration, KILL_GRACE)
    }

    /// 注入 daemon RPC（终态租约释放的通道）。
    pub fn set_rpc(&self, rpc: RpcFn) {
        self.set_lease_release(Arc::new(move |home_id, run_id, exec_id| {
            let _ = release_items_for_exec(&*rpc, home_id, run_id, exec_id);
        }));
    }

    /// 直接注入租约释放回调（shim 用：按需连接 + 临时授权，见 daemon_link）。
    pub fn set_lease_release(&self, release: LeaseRelease) {
        *self
            .lease_release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(release);
    }
```

（`with_limits`、`with_kill_grace` 等其余方法保持原样，接在后面。）

5. 在文件内（`impl ExecutionManager` 之外）新增：

```rust
/// 桌面端启动时的一次性清扫（原 ExecutionManager::new 的副作用）：demote
/// 上次运行遗留的 Live 记录 + omp/pi 注入的崩溃残留。Task 10 改为启动对账。
pub fn startup_housekeeping(runtime_dir: &Path) {
    match agents::demote_live_agents(runtime_dir) {
        Ok(n) if n > 0 => eprintln!("启动 demote：{n} 个 Live agent 已降级（app 重启）"),
        Err(err) => eprintln!("启动 demote 失败：{err}"),
        _ => {}
    }
    match crate::mcp_injection::sweep_crash_residue(runtime_dir) {
        Ok(n) if n > 0 => eprintln!("启动清扫：{n} 个 workspace 的 omp MCP 注入残留已恢复"),
        Err(err) => eprintln!("omp 注入残留清扫失败：{err}"),
        _ => {}
    }
}
```

6. `exec_start_impl` 改为下面的签名与开头（函数体其余部分不变，`dispatch_chain` 多传一个 `omt_path`）：

```rust
#[allow(clippy::too_many_arguments)]
pub fn exec_start_impl(
    rpc: &RpcFn,
    manager: &ExecutionManager,
    omt_path: &Path,
    runtime_dir: &Path,
    home_id: &str,
    home_path: &Path,
    node_id: &str,
    provider_id: &str,
    unattended: bool,
    model: Option<&str>,
    resume_session: Option<&str>,
    target_agent_id: Option<&str>,
    sink: EventSink,
) -> Result<ExecStartOutcome, String> {
    if let Some(model) = model {
        harness::validate_model_id(model)?;
    }
    if let Some(session) = resume_session {
        harness::validate_model_id(session)?;
    }
    manager.set_rpc(rpc.clone());
    let _guard = DISPATCH_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let seam = |method: &str, params: serde_json::Value| rpc(method, params);
    dispatch_chain(
        &seam, manager, omt_path, runtime_dir, home_id, home_path, node_id, provider_id,
        unattended, model, resume_session, target_agent_id, sink,
    )
}
```

`dispatch_chain` 的参数表在 `manager: &ExecutionManager,` 之后插入 `omt_path: &Path,`；步骤 ⑦ 改为：

```rust
    let mcp = match crate::mcp_injection::prepare(omt_path, runtime_dir, &cwd, provider_id, &exec_id, false) {
```

dispatch 相关测试里调用 `dispatch_chain(` 的地方，在 manager 实参之后补一个 `&fake_omt` 或测试里已有的 omt 路径变量；如果测试原来依赖 sidecar 缺失的报错，就传 `Path::new("/nonexistent/omt")`。

7. `reconcile_interrupted_execs` 改为：

```rust
/// 启动对账的生产入口（rpc 由调用方适配）。
pub fn reconcile_interrupted_execs(rpc: &RpcFn, runtime_dir: &Path) -> usize {
    let seam = |method: &str, params: serde_json::Value| rpc(method, params);
    let handled = reconcile_interrupted_execs_with(&seam, runtime_dir);
    if handled > 0 {
        eprintln!("U10 对账：释放了 {handled} 个中断 exec 的 daemon 租约");
    }
    handled
}
```

8. `release_items_for_exec` 改为 `pub fn … -> Result<(), String>`：run/get 的 `Err(_) => return,` 改为 `Err(err) => return Err(err),`，函数末尾返回 `Ok(())`。其余调用方（`reconcile_interrupted_execs_with`）写成 `let _ = release_items_for_exec(...)`。
9. 把 Interfaces 里列出的函数与常量改为 `pub`。
10. 测试里 `crate::agents::…`、`crate::mcp_injection::…` 的路径保持不变（同 crate）；对 `crate::atomic_json` 的引用此前已在 Task 1 改为 `omt_harness::atomic_json`。测试若调用了 `ExecutionManager::with_limits`（仍为 `#[cfg(test)]`），保持不变。

- [ ] **Step 6: 桌面端 executor command 文件**

新建 `apps/desktop/src-tauri/src/executor.rs`：

```rust
//! exec_* tauri command 包装。执行面内核在 omt_exec（设计 §3.1）；这里
//! 负责把 SharedSession 适配成 RpcFn、把 Tauri Channel 适配成 EventSink。

use std::path::Path;
use std::sync::Arc;

use omt_exec::executor::{EventSink, ExecStartOutcome, RpcFn};

use crate::daemon;
use crate::state::SharedSession;

/// SharedSession → RpcFn（clone 共享同一个内部 mutex，等待线程据此释放租约）。
pub fn rpc_of(shared: &SharedSession) -> RpcFn {
    let shared = shared.clone();
    Arc::new(move |method: &str, params: serde_json::Value| {
        let mut guard = shared
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = guard.as_mut().ok_or("daemon session not established")?;
        session.call(method, params)
    })
}

/// U6 (R-B)：exec_start（§3.4 统一入口，内含 §3.3 链）。
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub fn exec_start(
    shared: tauri::State<'_, SharedSession>,
    state: tauri::State<'_, crate::state::ExecutionState>,
    home_id: String,
    home_path: String,
    node_id: String,
    harness: String,
    unattended: bool,
    model: Option<String>,
    resume_session: Option<String>,
    agent_id: Option<String>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<ExecStartOutcome, String> {
    let sink: EventSink = Arc::new(move |value| {
        let _ = channel.send(value);
    });
    let omt_path = daemon::omt_sidecar_path()?;
    omt_exec::executor::exec_start_impl(
        &rpc_of(&shared),
        &state.inner().0,
        &omt_path,
        &daemon::runtime_dir(),
        &home_id,
        Path::new(&home_path),
        &node_id,
        &harness,
        unattended,
        model.as_deref(),
        resume_session.as_deref(),
        agent_id.as_deref(),
        sink,
    )
}

/// 幂等终止（§3.4）：对已终态/不存在的 execId 直接成功。
#[tauri::command(async)]
pub fn exec_stop(
    state: tauri::State<'_, crate::state::ExecutionState>,
    exec_id: String,
) -> Result<(), String> {
    state.0.exec_stop(&exec_id)
}

/// 历史 transcript 尾部回载（F-PI-3 / F-U12-4）。
#[tauri::command(async)]
pub fn exec_transcript_tail(
    home_id: String,
    exec_id: String,
    max_lines: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    Ok(omt_exec::executor::transcript_tail_from(
        &daemon::runtime_dir(),
        &home_id,
        &exec_id,
        max_lines,
    ))
}

/// Orphan 强杀。Renderer 只能提交持久化记录身份，不能提交 pid。
#[tauri::command(async)]
pub fn exec_kill_orphan(home_id: String, agent_id: String, exec_id: String) -> Result<(), String> {
    omt_exec::executor::kill_orphan(&daemon::runtime_dir(), &home_id, &agent_id, &exec_id)
}
```

- [ ] **Step 7: state / rpc_bridge / lib.rs / Cargo**

`apps/desktop/src-tauri/src/state.rs` 的 `ExecutionState`：

```rust
/// U2: 所有执行会话的持有者（照 SharedSession 模式 manage）。构造时执行
/// 启动清扫（F-U4a-2 demote + MCP 残留 sweep）。
pub struct ExecutionState(pub omt_exec::executor::ExecutionManager);

impl ExecutionState {
    pub fn new() -> Self {
        omt_exec::executor::startup_housekeeping(&daemon::runtime_dir());
        Self(omt_exec::executor::ExecutionManager::new())
    }
}
```

`apps/desktop/src-tauri/src/rpc_bridge.rs` 中 U10 对账线程：

```rust
                let reconciler = crate::state::SharedSession(shared.0.clone());
                std::thread::spawn(move || {
                    omt_exec::executor::reconcile_interrupted_execs(
                        &crate::executor::rpc_of(&reconciler),
                        &daemon::runtime_dir(),
                    );
                });
```

`apps/desktop/src-tauri/src/lib.rs`：删除 `mod mcp_injection;` 及其上方的 `#[allow(dead_code)]` 注释；`mod executor;` 上方的 `#[allow(dead_code)]` 与注释删除。`ExitRequested` 分支里的 `use crate::executor::KillReason;` 改为 `use omt_exec::executor::KillReason;`。

`apps/desktop/src-tauri/Cargo.toml` 的 `[dependencies]` 加 `omt-exec = { path = "../../../crates/omt-exec" }`；`sha2` 若已无使用方就删掉（`cargo build` 报 unused 时删）。

- [ ] **Step 8: 编译、对数、全量测试**

```bash
cargo build --workspace --all-targets 2>&1 | grep -E '^error' -A6 | head -60
cargo test -p omt-exec 2>&1 | grep -E '^test result'
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result'
cargo test -p omt-harness 2>&1 | grep -E '^test result'
```

Expected：三者 lib 测试数之和 = 128（Task 1 Step 1 的基线），0 failed。然后跑 Global Constraints 里的验证基线。

- [ ] **Step 9: 提交**

```bash
git add -A crates/omt-exec apps/desktop/src-tauri Cargo.lock
git commit -m "refactor: extract omt-exec crate (agents storage, MCP injection, executor core)

Pure move per exec-plane design §3.1. The kernel no longer depends on
tauri or SharedSession: daemon RPC is an injected RpcFn and the MCP
bridge path an explicit omt_path. Desktop keeps thin command wrappers.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `atomic_json` 前置修复（F-U4a-3 / F-U4a-4）与共享锁 helper

**Files:**
- Modify: `crates/omt-harness/src/atomic_json.rs`
- Test: 同文件 `mod tests`

**Interfaces:**
- Produces:
  - `pub fn lock_exclusive(path: &Path, timeout: Duration) -> Result<std::fs::File, String>`：在 `<path>.lock` 上取 flock，返回持锁句柄（drop 即释放）。Task 5 / 6 / 7 使用。
  - `pub fn write_atomic(path: &Path, text: &str) -> Result<(), String>`：0600 tmp + `sync_all` + rename + 父目录 fsync。Task 7 / 8 使用。
  - `pub const LOCK_ACQUIRE_TIMEOUT: Duration`（10s，原私有常量改为 pub）。

- [ ] **Step 1: 写失败测试**

在 `atomic_json.rs` 的 `mod tests` 末尾加：

```rust
    /// F-U4a-3：损坏降级的 rename 必须在 flock 内做「复核」——若锁内重读
    /// 发现文件已被另一写入方修好，不得把好文件挪成 .corrupt。
    #[test]
    fn corrupt_recovery_rechecks_under_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        // 模拟：快路径读到坏内容后、取锁前，另一进程已写入好内容
        let (value, failed) =
            recover_corrupt_under_lock::<std::collections::BTreeMap<String, i32>>(&path).unwrap();
        assert!(!failed);
        assert_eq!(value.get("a"), Some(&1));
        let evidence = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(!evidence, "好文件不得被挪成 .corrupt");
    }

    #[test]
    fn lock_exclusive_blocks_second_holder_until_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        let _held = lock_exclusive(&path, Duration::from_secs(1)).unwrap();
        let err = lock_exclusive(&path, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("flock"), "{err}");
    }

    #[test]
    fn write_atomic_writes_0600_without_leftover_tmp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        write_atomic(&path, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }
```

注意：同一进程内对同一文件的两次 `flock` 会打开两个不同的 fd；macOS 上 flock 按打开文件描述（open file description）计，第二个 fd 拿不到锁，测试成立。

- [ ] **Step 2: 运行确认失败**

```bash
cargo test -p omt-harness atomic_json 2>&1 | tail -5
```
Expected：编译失败，提示 `recover_corrupt_under_lock` / `lock_exclusive` / `write_atomic` 未定义。

- [ ] **Step 3: 实现**

1. `const LOCK_ACQUIRE_TIMEOUT` 改为 `pub const LOCK_ACQUIRE_TIMEOUT`。
2. 新增 `lock_exclusive`（放在 `flock_exclusive` 之后）：

```rust
/// 在 `<path>.lock`（稳定 inode）上取跨进程排他锁；返回的句柄 drop 即释放。
/// 给 atomic_json 之外、但需要与它同一把锁语义的写入方使用（grants、
/// MCP 项目注入、迁移）。
pub fn lock_exclusive(path: &Path, timeout: Duration) -> Result<std::fs::File, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create_dir_all: {e}"))?;
    }
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode_if_unix(0o600)
        .open(lock_file_of(path))
        .map_err(|e| format!("open lock file: {e}"))?;
    flock_exclusive(&handle, timeout)?;
    Ok(handle)
}
```

3. `write_tmp_and_rename` 改为（F-U4a-4）：

```rust
fn write_tmp_and_rename(path: &Path, text: &str) -> Result<(), String> {
    let tmp = unique_tmp_path(path);
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode_if_unix(0o600)
            .open(&tmp)
            .map_err(|e| format!("open tmp: {e}"))?;
        file.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        // F-U4a-4：rename 前落盘，断电不会留下 0 字节文件
        file.sync_all().map_err(|e| format!("fsync tmp: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    // 目录项落盘（best-effort：部分文件系统不支持对目录 fsync）
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// 0600 原子写（tmp + fsync + rename）。调用方自行负责互斥。
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create_dir_all: {e}"))?;
    }
    write_tmp_and_rename(path, text)
}
```

4. `read_json` 改为快路径 + 锁内复核（F-U4a-3）：

```rust
/// 读取：返回 (值, parse_failed)。文件不存在 → 默认值。
/// F-U4a-3：健康文件走无 flock 的快路径；解析失败时取跨进程 flock 后
/// 在锁内重读复核，仍然损坏才改名留证——不会把另一写入方刚写好的文件
/// 挪成 .corrupt。
pub fn read_json<T>(path: &Path) -> Result<(T, bool), String>
where
    T: Default + serde::Serialize + serde::de::DeserializeOwned,
{
    let _store_guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok((T::default(), false));
    };
    if let Ok(value) = serde_json::from_str::<T>(&text) {
        return Ok((value, false));
    }
    recover_corrupt_under_lock::<T>(path)
}

/// 锁内复核 + 降级（调用方已持 STORE_LOCK）。
fn recover_corrupt_under_lock<T>(path: &Path) -> Result<(T, bool), String>
where
    T: Default + serde::Serialize + serde::de::DeserializeOwned,
{
    let _flock = lock_exclusive(path, LOCK_ACQUIRE_TIMEOUT)?;
    read_json_locked::<T>(path)
}
```

5. `update_json` 中自行打开 lock 文件、flock 的那段，替换为 `let _flock = lock_exclusive(path, LOCK_ACQUIRE_TIMEOUT)?;`（保留其前面的 `create_dir_all` 与 `sweep_stale_tmp`）。

- [ ] **Step 4: 运行测试**

```bash
cargo test -p omt-harness atomic_json 2>&1 | grep -E '^test result|FAILED'
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
```

- [ ] **Step 5: 提交**

```bash
git add crates/omt-harness/src/atomic_json.rs
git commit -m "fix: atomic_json recheck corrupt under flock, fsync before rename (F-U4a-3/4)

Prerequisite for multi-process writers (exec-plane design §3.3). Also
exposes lock_exclusive / write_atomic for grants, MCP injection and
migration.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: 存储按 agent 拆分（KD-E2）

**Files:**
- Create: `crates/omt-exec/src/layout.rs`
- Modify: `crates/omt-exec/src/agents.rs`（存储函数全部改为新布局）、`crates/omt-exec/src/executor.rs`（transcript 路径、`SpawnRequest.transcript_dir`、tail、对账回填）、`crates/omt-exec/src/lib.rs`
- Modify: `crates/omt-harness/src/harness.rs`（`prompt_file_path` 签名）
- Test: `agents.rs` / `executor.rs` / `layout.rs` 的 `mod tests`

**Interfaces:**
- Consumes: Task 3 的 `lock_exclusive`。
- Produces:
  - `omt_exec::layout::{validate_segment, home_dir, agent_dir, agent_file, seq_file, bindings_file, exec_file, ExecFile, new_agent_id, home_ids, agent_ids}`（签名见下方代码）
  - `omt_exec::agents::list_agents(runtime_dir, home_id) -> Result<(Vec<AgentRecord>, bool), String>`（按首个 exec 的 startedAt 排序；bool = parseFailed，粘性）
  - `omt_exec::agents::agent_seq(runtime_dir, home_id) -> Result<u64, String>`
  - `omt_exec::agents::find_exec_in_home(runtime_dir, home_id, exec_id) -> Result<Option<(AgentRecord, ExecRecord)>, String>`
  - `register_agent` / `append_exec` / `get_agent` / `update_agent_status` / `update_exec` / `delete_agent` / `write_binding` / `demote_live_agents` / `list_reconcile_targets` / `agents_list_value` / `bindings_get_value` / `bindings_gc_value` **签名不变**
  - `ReconcileTarget` 不变（已含 `agent_id`）
  - `SpawnRequest` 新增字段 `pub transcript_dir: PathBuf`（jsonl / raw.log 写在这里）
  - `omt_harness::harness::prompt_file_path(dir: &Path, exec_id: &str, channel: PromptChannel) -> Option<PathBuf>`
  - 测试 helper（`#[cfg(test)] pub(crate)`）：`omt_exec::agents::snapshot(runtime_dir, home_id) -> AgentsFile`

- [ ] **Step 1: 写 layout 与它的测试**

`crates/omt-exec/src/layout.rs`：

```rust
//! 执行面存储布局（设计 §3.3 / KD-E2）：按 agent 拆分。
//!
//! ```text
//! <runtime>/agents/<homeId>/seq.json                    agent 序号（AgentsMeta）
//! <runtime>/agents/<homeId>/bindings.json               nodeId → agentId（位置不变）
//! <runtime>/agents/<homeId>/<agentId>/agent.json        AgentRecord + execs[]
//! <runtime>/agents/<homeId>/<agentId>/<execId>.jsonl     规范化事件（transcript 权威，KD-E3）
//! <runtime>/agents/<homeId>/<agentId>/<execId>.raw.log   harness 原始 stdout/stderr
//! <runtime>/agents/<homeId>/<agentId>/<execId>.prompt.md FileAttach 渠道的 prompt
//! <runtime>/agents/<homeId>/<agentId>/<execId>.spec.json launch spec（Task 8）
//! <runtime>/agents/<homeId>/<agentId>/<execId>.shim.log  shim 自身 stdout/stderr
//! ```
//! agent.json 的跨进程互斥沿用 atomic_json 的 companion `agent.json.lock`；
//! 「同一 agent 同时只允许一个活跃 exec」在 `append_exec` 的锁内校验。

use std::path::{Path, PathBuf};

/// 路径段校验：拒绝空串、分隔符、`.` 开头与 `..`（防穿越）。
pub fn validate_segment(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || value.contains("..")
    {
        return Err(format!("invalid {what}: {value:?}"));
    }
    Ok(())
}

pub fn home_dir(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    validate_segment("home id", home_id)?;
    Ok(runtime_dir.join("agents").join(home_id))
}

pub fn agent_dir(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<PathBuf, String> {
    validate_segment("agent id", agent_id)?;
    Ok(home_dir(runtime_dir, home_id)?.join(agent_id))
}

pub fn agent_file(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<PathBuf, String> {
    Ok(agent_dir(runtime_dir, home_id, agent_id)?.join("agent.json"))
}

pub fn seq_file(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    Ok(home_dir(runtime_dir, home_id)?.join("seq.json"))
}

pub fn bindings_file(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    Ok(home_dir(runtime_dir, home_id)?.join("bindings.json"))
}

/// 每次执行的伴生文件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecFile {
    Jsonl,
    RawLog,
    Prompt,
    Spec,
    ShimLog,
}

impl ExecFile {
    fn suffix(self) -> &'static str {
        match self {
            ExecFile::Jsonl => "jsonl",
            ExecFile::RawLog => "raw.log",
            ExecFile::Prompt => "prompt.md",
            ExecFile::Spec => "spec.json",
            ExecFile::ShimLog => "shim.log",
        }
    }
}

pub fn exec_file(agent_dir: &Path, exec_id: &str, kind: ExecFile) -> PathBuf {
    agent_dir.join(format!("{exec_id}.{}", kind.suffix()))
}

/// 新 agent 的 id（与旧实现 `agent-<execId>` 同形）。
pub fn new_agent_id(exec_id: &str) -> String {
    format!("agent-{exec_id}")
}

fn subdirs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// `agents/` 下的 home id（目录名，排序）。
pub fn home_ids(runtime_dir: &Path) -> Vec<String> {
    subdirs(&runtime_dir.join("agents"))
}

/// 某 home 下的 agent 目录名（排序；不保证每个都有可读的 agent.json）。
pub fn agent_ids(runtime_dir: &Path, home_id: &str) -> Result<Vec<String>, String> {
    Ok(subdirs(&home_dir(runtime_dir, home_id)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_reject_traversal() {
        let rt = Path::new("/tmp/rt");
        assert!(home_dir(rt, "../x").is_err());
        assert!(home_dir(rt, "a/b").is_err());
        assert!(agent_dir(rt, "h_ok", ".hidden").is_err());
        assert!(agent_dir(rt, "h_ok", "agent-exec-1-0").is_ok());
    }

    #[test]
    fn exec_files_share_the_agent_dir() {
        let dir = Path::new("/tmp/rt/agents/h/agent-exec-1");
        assert_eq!(
            exec_file(dir, "exec-1", ExecFile::Jsonl),
            dir.join("exec-1.jsonl")
        );
        assert_eq!(
            exec_file(dir, "exec-1", ExecFile::RawLog),
            dir.join("exec-1.raw.log")
        );
    }

    #[test]
    fn listings_skip_hidden_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agents").join("h_a");
        std::fs::create_dir_all(home.join("agent-2")).unwrap();
        std::fs::create_dir_all(home.join("agent-1")).unwrap();
        std::fs::create_dir_all(home.join(".trash")).unwrap();
        std::fs::write(home.join("seq.json"), "{}").unwrap();
        assert_eq!(home_ids(tmp.path()), vec!["h_a".to_string()]);
        assert_eq!(
            agent_ids(tmp.path(), "h_a").unwrap(),
            vec!["agent-1".to_string(), "agent-2".to_string()]
        );
    }
}
```

`lib.rs` 加 `pub mod layout;`。

```bash
cargo test -p omt-exec layout 2>&1 | grep -E '^test result'
```
Expected：3 passed。

- [ ] **Step 2: 写存储层的失败测试（新布局契约）**

在 `crates/omt-exec/src/agents.rs` 的 `mod tests` 末尾加：

```rust
    #[test]
    fn register_writes_one_dir_per_agent_and_seq_is_monotonic() {
        let (_d, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let b = register_agent(&runtime, &home, "omp", new_exec("exec-b")).unwrap();
        assert_eq!(a.display_name, "Agent 1");
        assert_eq!(b.display_name, "Agent 2");
        assert!(crate::layout::agent_file(&runtime, &home, &a.id).unwrap().is_file());
        assert!(crate::layout::agent_file(&runtime, &home, &b.id).unwrap().is_file());
        assert!(!runtime.join("agents").join(&home).join("agents.json").exists());
        assert_eq!(agent_seq(&runtime, &home).unwrap(), 2);
        let (agents, failed) = list_agents(&runtime, &home).unwrap();
        assert!(!failed);
        assert_eq!(
            agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec![a.id.as_str(), b.id.as_str()]
        );
    }

    #[test]
    fn one_corrupt_agent_file_marks_list_degraded_but_keeps_others() {
        let (_d, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let b = register_agent(&runtime, &home, "pi", new_exec("exec-b")).unwrap();
        std::fs::write(crate::layout::agent_file(&runtime, &home, &a.id).unwrap(), "{oops").unwrap();
        let (agents, failed) = list_agents(&runtime, &home).unwrap();
        assert!(failed, "损坏应置 parseFailed");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, b.id);
        // 粘性：第二次读（损坏文件已被改名留证）仍为降级
        assert!(list_agents(&runtime, &home).unwrap().1);
    }

    #[test]
    fn updates_to_missing_agent_do_not_create_dirs() {
        let (_d, runtime, home) = temp_runtime();
        let changed = update_agent_status(
            &runtime, &home, "agent-ghost", AgentRecordStatus::Failed, AgentAttachment::None,
        )
        .unwrap();
        assert!(!changed);
        assert!(!crate::layout::agent_dir(&runtime, &home, "agent-ghost").unwrap().exists());
    }

    #[test]
    fn delete_removes_the_agent_dir_with_transcripts() {
        let (_d, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        update_agent_status(&runtime, &home, &a.id, AgentRecordStatus::Exited, AgentAttachment::None).unwrap();
        let dir = crate::layout::agent_dir(&runtime, &home, &a.id).unwrap();
        std::fs::write(dir.join("exec-a.jsonl"), "{}\n").unwrap();
        delete_agent(&runtime, &home, &a.id).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn find_exec_in_home_locates_by_exec_id() {
        let (_d, runtime, home) = temp_runtime();
        let a = register_agent(&runtime, &home, "pi", new_exec("exec-a")).unwrap();
        let (agent, exec) = find_exec_in_home(&runtime, &home, "exec-a").unwrap().unwrap();
        assert_eq!(agent.id, a.id);
        assert_eq!(exec.exec_id, "exec-a");
        assert!(find_exec_in_home(&runtime, &home, "exec-zz").unwrap().is_none());
    }

    fn new_exec(exec_id: &str) -> NewExec {
        NewExec {
            exec_id: exec_id.into(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
            model: None,
        }
    }
```

（`temp_runtime()` 是该文件已有的测试 helper，返回 `(TempDir, PathBuf, String)`；如果已有同名的 `new_exec` helper，就删掉这里新加的这一份。）

```bash
cargo test -p omt-exec agents:: 2>&1 | tail -5
```
Expected：编译失败，提示 `list_agents` / `agent_seq` / `find_exec_in_home` 未定义。

- [ ] **Step 3: 用新布局重写存储函数**

在 `crates/omt-exec/src/agents.rs` 中：

1. 删除 `agents_path`；`bindings_path` 改为委托 `crate::layout::bindings_file`；`validate_home_id` 改为委托 `crate::layout::validate_segment("home id", …)`：

```rust
pub fn validate_home_id(home_id: &str) -> Result<(), String> {
    crate::layout::validate_segment("home id", home_id)
}

pub fn bindings_path(runtime_dir: &Path, home_id: &str) -> Result<PathBuf, String> {
    crate::layout::bindings_file(runtime_dir, home_id)
}
```

2. 新增读侧函数（放在 `now_epoch_ms` 之后）：

```rust
use crate::layout;

fn first_started(agent: &AgentRecord) -> &str {
    agent.execs.first().map(|e| e.started_at.as_str()).unwrap_or("")
}

/// 某 home 的全部 agent（按首个 exec 的 startedAt 排序，epoch ms 字符串
/// 等长，字典序 = 时间序）。返回值第二项 = parseFailed（粘性：任一
/// agent.json 解析失败或留有未恢复的 `.corrupt-*` 证据）。
pub fn list_agents(runtime_dir: &Path, home_id: &str) -> Result<(Vec<AgentRecord>, bool), String> {
    let mut agents = Vec::new();
    let mut parse_failed = false;
    for agent_id in layout::agent_ids(runtime_dir, home_id)? {
        let path = layout::agent_file(runtime_dir, home_id, &agent_id)?;
        let (record, failed) = read_json::<AgentRecord>(&path)?;
        parse_failed |= failed || has_unrecovered_corrupt(&path);
        if !record.id.is_empty() {
            agents.push(record);
        }
    }
    agents.sort_by(|a, b| first_started(a).cmp(first_started(b)).then(a.id.cmp(&b.id)));
    Ok((agents, parse_failed))
}

/// 持久单调的 agent 序号（`Agent N` 的 N）。
pub fn agent_seq(runtime_dir: &Path, home_id: &str) -> Result<u64, String> {
    Ok(read_json::<AgentsMeta>(&layout::seq_file(runtime_dir, home_id)?)?.0.agent_seq)
}

/// 在某 home 内按 exec id 找 (agent, exec)。
pub fn find_exec_in_home(
    runtime_dir: &Path,
    home_id: &str,
    exec_id: &str,
) -> Result<Option<(AgentRecord, ExecRecord)>, String> {
    let (agents, _) = list_agents(runtime_dir, home_id)?;
    Ok(agents.into_iter().find_map(|agent| {
        let exec = agent.execs.iter().find(|e| e.exec_id == exec_id).cloned()?;
        Some((agent, exec))
    }))
}

/// 测试 helper：把新布局拼回旧的 AgentsFile 形状（旧测试的断言面）。
#[cfg(test)]
pub(crate) fn snapshot(runtime_dir: &Path, home_id: &str) -> AgentsFile {
    AgentsFile {
        meta: AgentsMeta {
            agent_seq: agent_seq(runtime_dir, home_id).unwrap_or(0),
        },
        agents: list_agents(runtime_dir, home_id).map(|(a, _)| a).unwrap_or_default(),
    }
}
```

3. `register_agent`：

```rust
pub fn register_agent(
    runtime_dir: &Path,
    home_id: &str,
    harness: &str,
    new_exec: NewExec,
) -> Result<AgentRecord, String> {
    let (meta, _) = update_json(&layout::seq_file(runtime_dir, home_id)?, |m: &mut AgentsMeta| {
        m.agent_seq += 1;
        true
    })?;
    let seq = meta.agent_seq;
    let agent_id = layout::new_agent_id(&new_exec.exec_id);
    let path = layout::agent_file(runtime_dir, home_id, &agent_id)?;
    let (record, outcome) = update_json(&path, |rec: &mut AgentRecord| {
        if !rec.id.is_empty() {
            return false; // exec id 全局唯一，正常不会撞
        }
        *rec = AgentRecord {
            id: agent_id.clone(),
            display_name: format!("Agent {seq}"),
            title: None,
            harness: harness.to_string(),
            status: AgentRecordStatus::Starting,
            attachment: AgentAttachment::Live,
            last_exec_id: Some(new_exec.exec_id.clone()),
            last_session_id: None,
            execs: vec![ExecRecord {
                exec_id: new_exec.exec_id.clone(),
                node_id: new_exec.node_id.clone(),
                run_id: new_exec.run_id.clone(),
                started_at: now_epoch_ms(),
                binary_path: new_exec.binary_path.clone(),
                binary_args: new_exec.binary_args.clone(),
                model: new_exec.model.clone(),
                ..Default::default()
            }],
        };
        true
    })?;
    if !outcome.changed {
        return Err(format!("register_agent: agent {agent_id} 已存在"));
    }
    Ok(record)
}
```

4. `append_exec`：把 `agents_path` + `file.agents.iter_mut().find(...)` 改为直接操作单个 `AgentRecord`：

```rust
pub fn append_exec(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    harness: &str,
    new_exec: NewExec,
) -> Result<AgentRecord, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.is_file() {
        return Err(format!("append_exec: agent {agent_id} 不存在"));
    }
    let (record, outcome) = update_json(&path, |agent: &mut AgentRecord| {
        if agent.id != agent_id || agent.harness != harness {
            return false;
        }
        // 同一 agent 同时只允许一个活跃 exec（设计 §3.3），锁内校验
        if matches!(
            agent.status,
            AgentRecordStatus::Starting | AgentRecordStatus::Running
        ) {
            return false;
        }
        agent.execs.push(ExecRecord {
            exec_id: new_exec.exec_id.clone(),
            node_id: new_exec.node_id.clone(),
            run_id: new_exec.run_id.clone(),
            started_at: now_epoch_ms(),
            binary_path: new_exec.binary_path.clone(),
            binary_args: new_exec.binary_args.clone(),
            model: new_exec.model.clone(),
            ..Default::default()
        });
        agent.status = AgentRecordStatus::Starting;
        agent.attachment = AgentAttachment::Live;
        agent.last_exec_id = Some(new_exec.exec_id.clone());
        true
    })?;
    if !outcome.changed {
        return Err(format!(
            "append_exec: agent {agent_id} 不可追加（harness 不一致/运行中）"
        ));
    }
    Ok(record)
}
```

5. `get_agent`：

```rust
pub fn get_agent(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
) -> Result<Option<AgentRecord>, String> {
    let path = layout::agent_file(runtime_dir, home_id, agent_id)?;
    if !path.exists() && !has_unrecovered_corrupt(&path) {
        return Ok(None);
    }
    let (record, parse_failed) = read_json::<AgentRecord>(&path)?;
    if parse_failed || has_unrecovered_corrupt(&path) {
        return Err(format!("{} 已损坏，拒绝解析进程身份", path.display()));
    }
    Ok((!record.id.is_empty()).then_some(record))
}
```

6. `update_agent_status` / `update_exec`：路径改为 `layout::agent_file(...)`；函数开头加 `if !path.is_file() { return Ok(false); }`；闭包参数改为 `|agent: &mut AgentRecord|`，删掉 `file.agents.iter_mut().find(...)` 那一层，改为 `if agent.id != agent_id { return false; }`。`update_exec` 的 `exec` 查找与 `last_session_id` 镜像逻辑保持不变。

7. `delete_agent`：

```rust
pub fn delete_agent(runtime_dir: &Path, home_id: &str, agent_id: &str) -> Result<(), String> {
    let agent = get_agent(runtime_dir, home_id, agent_id)
        .map_err(|e| format!("删除失败：{e}"))?
        .ok_or_else(|| format!("agent {agent_id} 不存在"))?;
    if matches!(
        agent.status,
        AgentRecordStatus::Starting | AgentRecordStatus::Running
    ) {
        return Err(format!("agent {agent_id} 仍在运行——请先停止再删除"));
    }
    if agent.attachment == AgentAttachment::Orphan {
        return Err(format!(
            "agent {agent_id} 处于不受控态（pid 存活）——请先强制终止再删除"
        ));
    }
    // P-7：按 agent 拆目录后，transcript 随 agent 一起删除
    let dir = layout::agent_dir(runtime_dir, home_id, agent_id)?;
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败：{e}"))
}
```

8. `demote_live_agents` 与 `list_reconcile_targets`：外层循环改为

```rust
    for home_id in layout::home_ids(runtime_dir) {
        for agent_id in layout::agent_ids(runtime_dir, &home_id)? {
            let path = layout::agent_file(runtime_dir, &home_id, &agent_id)?;
            // demote：update_json(&path, |agent: &mut AgentRecord| { …原单 agent 逻辑… })
            // reconcile targets：let (agent, _) = read_json::<AgentRecord>(&path)?; …原单 agent 逻辑…
        }
    }
```

两者对单个 agent 的处理逻辑与原来一致；`demote_live_agents` 里 `if agent.id.is_empty() { return false; }` 防止空记录被写出来。

9. `agents_list_value`：

```rust
pub fn agents_list_value(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let (agents, parse_failed) = list_agents(runtime_dir, home_id)?;
    Ok(serde_json::json!({
        "agents": agents,
        "agentSeq": agent_seq(runtime_dir, home_id)?,
        "parseFailed": parse_failed,
    }))
}
```

10. `bindings_get_value`：

```rust
pub fn bindings_get_value(runtime_dir: &Path, home_id: &str) -> Result<serde_json::Value, String> {
    let bindings = read_bindings(runtime_dir, home_id)?;
    let b_path = bindings_path(runtime_dir, home_id)?;
    let (_, agents_parse_failed) = list_agents(runtime_dir, home_id)?;
    Ok(serde_json::json!({
        "bindings": bindings,
        "parseFailed": has_unrecovered_corrupt(&b_path) || agents_parse_failed,
    }))
}
```

11. `bindings_gc_value`：从「`let agents_path = …`」到「`let live_ids = …`」这段替换为：

```rust
    // 缺失判定：从未注册过 agent 的 home 没有 seq.json（与旧的「agents.json
    // 缺失」等价）——此时 agent 表无法证明有效，冻结。
    let agents_missing = !layout::seq_file(runtime_dir, home_id)?.exists();
    let (agents, agents_parse_failed) = list_agents(runtime_dir, home_id)?;
    if agents_missing || agents_parse_failed {
        return Err(
            "agent 表无法证明有效（缺失 / 解析失败 / .corrupt-* 留证未恢复）——绑定清理已冻结"
                .to_string(),
        );
    }
    let live_ids: std::collections::HashSet<String> =
        agents.iter().map(|a| a.id.clone()).collect();
```

- [ ] **Step 4: executor 的路径改走 layout**

在 `crates/omt-exec/src/executor.rs` 中：

1. `SpawnRequest` 加字段：

```rust
    /// transcript 目录（= agent 目录，设计 §3.3）：jsonl / raw.log 写在这里。
    pub transcript_dir: PathBuf,
```

2. `spawn_inner` 中 `raw_log` 的计算改为：

```rust
        let raw_log = crate::layout::exec_file(
            &req.transcript_dir,
            &req.exec_id,
            crate::layout::ExecFile::RawLog,
        );
        std::fs::create_dir_all(&req.transcript_dir)
            .map_err(|e| format!("create transcript dir: {e}"))?;
        let events_log = crate::layout::exec_file(
            &req.transcript_dir,
            &req.exec_id,
            crate::layout::ExecFile::Jsonl,
        );
```

（删掉原来的 `if let Some(dir) = raw_log.parent()` 和 `let events_log = raw_log.with_file_name(...)` 两段。）

3. `dispatch_chain`：在 `let exec_id = …` 之后、`prompt_file_path` 之前计算 agent 目录（新 agent 的 id 可以提前确定）：

```rust
    let agent_id_for_paths = match target_agent_id {
        Some(target) => target.to_string(),
        None => crate::layout::new_agent_id(&exec_id),
    };
    let agent_dir = crate::layout::agent_dir(runtime_dir, home_id, &agent_id_for_paths)?;
    let prompt_file = harness::prompt_file_path(&agent_dir, &exec_id, plan.prompt_channel);
```

prompt 写盘那段的 `"create transcripts dir"` 文案改为 `"create agent dir"`。`SpawnRequest { … }` 加 `transcript_dir: agent_dir.clone(),`。

4. `omt-harness` 的 `prompt_file_path`：

```rust
pub fn prompt_file_path(dir: &Path, exec_id: &str, channel: PromptChannel) -> Option<PathBuf> {
    (channel == PromptChannel::FileAttach).then(|| dir.join(format!("{exec_id}.prompt.md")))
}
```

同步修改它在 `harness.rs` 测试中的调用（例如 `compose_launch_args_file_attach_avoids_long_positional`）：第一个参数改为一个临时目录路径，删掉 home_id 参数。

5. `transcript_tail_from`：开头改为按 exec 定位 agent 目录，找不到时回落旧的 `transcripts/<home>/`（迁移前遗留的数据）：

```rust
    let dir = match crate::agents::find_exec_in_home(runtime_dir, home_id, exec_id) {
        Ok(Some((agent, _))) => match crate::layout::agent_dir(runtime_dir, home_id, &agent.id) {
            Ok(dir) => dir,
            Err(_) => return Vec::new(),
        },
        _ => runtime_dir.join("transcripts").join(home_id),
    };
```

（之后的 `dir.join(format!("{exec_id}.jsonl"))` 与 raw.log 回落逻辑不变。）

6. `session_id_from_raw_log` 改为接收 `agent_id`，路径用 `layout::exec_file(&layout::agent_dir(runtime_dir, home_id, agent_id).ok()?, exec_id, ExecFile::RawLog)`；`reconcile_interrupted_execs_with` 里的调用补上 `&t.agent_id`。

- [ ] **Step 5: 迁移旧测试的断言面**

旧测试直接读 `agents.json`。按以下规则替换（只改读取方式，不改断言内容）：

| 旧写法 | 新写法 |
|---|---|
| `read_json::<AgentsFile>(&agents_path(&runtime, X).unwrap()).unwrap()` | `(crate::agents::snapshot(&runtime, X), false)` |
| `crate::atomic_json::read_json::<crate::agents::AgentsFile>(&runtime.join("agents").join("h_test").join("agents.json"))` 及等价的 `omt_harness::atomic_json::…` | `Ok::<_, String>((crate::agents::snapshot(&runtime, "h_test"), false))` |
| executor 测试里的 `fn read_agents_file(runtime: &Path) -> crate::agents::AgentsFile` 的函数体 | `crate::agents::snapshot(runtime, "h_test")` |
| 轮询 `agents_file` 路径直到满足条件的测试（`kill_all` 相关，约原 3007 行） | 轮询 `crate::agents::snapshot(&runtime, "h_test")` |
| `runtime.join("transcripts").join("h_test").join(format!("{}.jsonl", id))` | `crate::layout::exec_file(&crate::layout::agent_dir(&runtime, "h_test", &agent_id).unwrap(), &id, crate::layout::ExecFile::Jsonl)`（`agent_id` 取 spawn 时传入的 `SpawnRequest.agent_id`，或 `outcome.agent_id`） |
| 直接构造 `SpawnRequest { … }` 的测试 | 补 `transcript_dir: crate::layout::agent_dir(&runtime, "h_test", &agent.id).unwrap(),`（无注册 agent 的测试用 `runtime.join("agents").join("h_test").join("agent-test")`） |
| `agents_path(...)` 的穿越校验测试 | 改为对 `crate::layout::home_dir(...)` 断言 |

依赖「整个 agents.json 损坏」的 F-U4a-1 粘性测试，改为损坏**某一个** agent 的 `agent.json`（写 `"{oops"`），断言 `agents_list_value(...)["parseFailed"] == true`、`bindings_gc_value` 返回冻结错误、`bindings_get_value(...)["parseFailed"] == true`。旧测试名保持不变，断言语义也不变（降级 → 冻结）。

```bash
grep -rn 'agents\.json\|agents_path\|"transcripts"' crates/omt-exec/src | grep -v '^.*//' 
```
Expected：只剩 `transcript_tail_from` 的旧路径回落，以及 `demote` / 对账注释中的字样。

- [ ] **Step 6: 跑测试**

```bash
cargo test -p omt-exec 2>&1 | grep -E '^test result|FAILED|panicked'
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
cd apps/desktop && pnpm test
```

Expected：omt-exec 全绿（比 Task 2 多 Step 1/2 新增的 8 个测试）；桌面 vitest 131 不变。

- [ ] **Step 7: 提交**

```bash
git add -A crates/omt-exec crates/omt-harness
git commit -m "feat: per-agent storage layout (KD-E2)

agents/<home>/<agentId>/agent.json plus each exec's jsonl/raw.log/prompt
in the same dir; seq.json keeps Agent N monotonic. Storage function
signatures are unchanged; deleting an agent now removes its transcripts.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: 旧存储一次性迁移（幂等、可中断重入）

**Files:**
- Create: `crates/omt-exec/src/migrate.rs`
- Modify: `crates/omt-exec/src/lib.rs`、`crates/omt-exec/src/executor.rs`（`startup_housekeeping` 首行调用迁移）

**Interfaces:**
- Consumes: `layout::*`、`agents::{AgentsFile, AgentsMeta, AgentRecord}`、`atomic_json::{read_json, update_json, lock_exclusive, LOCK_ACQUIRE_TIMEOUT}`。
- Produces: `omt_exec::migrate::migrate_legacy(runtime_dir: &Path) -> Result<MigrationReport, String>`；`pub struct MigrationReport { pub homes: usize, pub agents: usize, pub files_moved: usize, pub skipped_corrupt: Vec<String> }`。

- [ ] **Step 1: 写失败测试**

`crates/omt-exec/src/migrate.rs`（先只放测试与空壳，确认失败）：

```rust
//! 旧存储迁移（设计 §3.3）：`agents/<home>/agents.json` + `transcripts/<home>/`
//! → 按 agent 拆分。幂等、可中断重入；全局 flock 防止两个进程同时迁移。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentRecord, AgentRecordStatus, AgentsFile, AgentsMeta, ExecRecord};

    fn legacy_home(runtime: &std::path::Path) {
        let file = AgentsFile {
            meta: AgentsMeta { agent_seq: 7 },
            agents: vec![AgentRecord {
                id: "agent-exec-1".into(),
                display_name: "Agent 7".into(),
                harness: "pi".into(),
                status: AgentRecordStatus::Exited,
                last_exec_id: Some("exec-1".into()),
                execs: vec![ExecRecord {
                    exec_id: "exec-1".into(),
                    started_at: "1760000000000".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let home = runtime.join("agents").join("h_a");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("agents.json"), serde_json::to_string(&file).unwrap()).unwrap();
        let tr = runtime.join("transcripts").join("h_a");
        std::fs::create_dir_all(&tr).unwrap();
        std::fs::write(tr.join("exec-1.jsonl"), "{\"kind\":\"session_header\"}\n").unwrap();
        std::fs::write(tr.join("exec-1.raw.log"), "[stdout] hi\n").unwrap();
    }

    #[test]
    fn migrates_records_and_transcripts_then_marks_legacy_file() {
        let tmp = tempfile::tempdir().unwrap();
        legacy_home(tmp.path());
        let report = migrate_legacy(tmp.path()).unwrap();
        assert_eq!((report.homes, report.agents, report.files_moved), (1, 1, 2));
        let dir = crate::layout::agent_dir(tmp.path(), "h_a", "agent-exec-1").unwrap();
        assert!(dir.join("agent.json").is_file());
        assert!(dir.join("exec-1.jsonl").is_file());
        assert!(dir.join("exec-1.raw.log").is_file());
        assert_eq!(crate::agents::agent_seq(tmp.path(), "h_a").unwrap(), 7);
        let home = tmp.path().join("agents").join("h_a");
        assert!(!home.join("agents.json").exists());
        assert!(home.join("agents.json.migrated").is_file());
        let (agents, failed) = crate::agents::list_agents(tmp.path(), "h_a").unwrap();
        assert!(!failed);
        assert_eq!(agents[0].display_name, "Agent 7");
    }

    #[test]
    fn rerun_is_a_noop_and_interrupted_run_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        legacy_home(tmp.path());
        // 模拟中断：agent.json 已写、transcript 尚未移动、旧文件仍在
        let dir = crate::layout::agent_dir(tmp.path(), "h_a", "agent-exec-1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.json"), "{\"id\":\"agent-exec-1\",\"harness\":\"pi\"}").unwrap();
        let first = migrate_legacy(tmp.path()).unwrap();
        assert_eq!(first.files_moved, 2, "断点续做应补完移动");
        // 已写的 agent.json 不被覆盖
        let (rec, _) =
            omt_harness::atomic_json::read_json::<AgentRecord>(&dir.join("agent.json")).unwrap();
        assert_eq!(rec.display_name, "");
        let second = migrate_legacy(tmp.path()).unwrap();
        assert_eq!((second.homes, second.files_moved), (0, 0));
    }

    #[test]
    fn corrupt_legacy_file_is_left_alone_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agents").join("h_bad");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("agents.json"), "{oops").unwrap();
        let report = migrate_legacy(tmp.path()).unwrap();
        assert_eq!(report.skipped_corrupt, vec!["h_bad".to_string()]);
        assert!(home.join("agents.json").is_file(), "损坏文件留原地等人工处理");
    }

    #[test]
    fn no_legacy_data_is_cheap_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(migrate_legacy(tmp.path()).unwrap(), MigrationReport::default());
    }
}
```

`lib.rs` 加 `pub mod migrate;`。

```bash
cargo test -p omt-exec migrate 2>&1 | tail -3
```
Expected：编译失败（`migrate_legacy` / `MigrationReport` 未定义）。

- [ ] **Step 2: 实现**

在 `migrate.rs` 的测试模块之前加：

```rust
use std::path::Path;

use omt_harness::atomic_json::{lock_exclusive, update_json, LOCK_ACQUIRE_TIMEOUT};

use crate::agents::{AgentRecord, AgentsFile, AgentsMeta};
use crate::layout::{self, ExecFile};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub homes: usize,
    pub agents: usize,
    pub files_moved: usize,
    /// 旧 agents.json 无法解析的 home（原地保留，待人工处理）。
    pub skipped_corrupt: Vec<String>,
}

const LEGACY_FILE: &str = "agents.json";
const MIGRATED_FILE: &str = "agents.json.migrated";

/// 迁移入口：任一面启动时调用。无旧数据时只做几次 stat。
pub fn migrate_legacy(runtime_dir: &Path) -> Result<MigrationReport, String> {
    let mut report = MigrationReport::default();
    let pending: Vec<String> = layout::home_ids(runtime_dir)
        .into_iter()
        .filter(|h| runtime_dir.join("agents").join(h).join(LEGACY_FILE).is_file())
        .collect();
    if pending.is_empty() {
        return Ok(report);
    }
    let _guard = lock_exclusive(
        &runtime_dir.join("agents").join(".migrate"),
        LOCK_ACQUIRE_TIMEOUT,
    )?;
    for home_id in pending {
        migrate_home(runtime_dir, &home_id, &mut report)?;
    }
    Ok(report)
}

fn migrate_home(runtime_dir: &Path, home_id: &str, report: &mut MigrationReport) -> Result<(), String> {
    let home = layout::home_dir(runtime_dir, home_id)?;
    let legacy = home.join(LEGACY_FILE);
    if !legacy.is_file() {
        return Ok(()); // 另一个进程刚迁完
    }
    // 不经 read_json：损坏时不改名留证，原地保留给人工
    let text = std::fs::read_to_string(&legacy).map_err(|e| format!("read legacy: {e}"))?;
    let Ok(file) = serde_json::from_str::<AgentsFile>(&text) else {
        report.skipped_corrupt.push(home_id.to_string());
        return Ok(());
    };
    update_json(&layout::seq_file(runtime_dir, home_id)?, |m: &mut AgentsMeta| {
        if m.agent_seq >= file.meta.agent_seq {
            return false;
        }
        m.agent_seq = file.meta.agent_seq;
        true
    })?;
    let transcripts = runtime_dir.join("transcripts").join(home_id);
    for agent in &file.agents {
        let dir = layout::agent_dir(runtime_dir, home_id, &agent.id)?;
        update_json(&dir.join("agent.json"), |rec: &mut AgentRecord| {
            if !rec.id.is_empty() {
                return false; // 已迁过（断点续做），不覆盖
            }
            *rec = agent.clone();
            true
        })?;
        for exec in &agent.execs {
            for (legacy_suffix, kind) in [
                ("jsonl", ExecFile::Jsonl),
                ("raw.log", ExecFile::RawLog),
                ("prompt.md", ExecFile::Prompt),
            ] {
                let src = transcripts.join(format!("{}.{legacy_suffix}", exec.exec_id));
                let dst = layout::exec_file(&dir, &exec.exec_id, kind);
                if src.is_file() && !dst.exists() {
                    std::fs::rename(&src, &dst).map_err(|e| format!("move {}: {e}", src.display()))?;
                    report.files_moved += 1;
                }
            }
        }
        report.agents += 1;
    }
    std::fs::rename(&legacy, home.join(MIGRATED_FILE)).map_err(|e| format!("mark migrated: {e}"))?;
    let _ = std::fs::remove_file(home.join("agents.json.lock"));
    report.homes += 1;
    Ok(())
}
```

注意 `rerun_is_a_noop_and_interrupted_run_resumes` 的第一次调用：`report.agents` 也会计 1，测试只断言 `files_moved`。

在 `executor::startup_housekeeping` 的第一行加：

```rust
    match crate::migrate::migrate_legacy(runtime_dir) {
        Ok(r) if r.homes > 0 || !r.skipped_corrupt.is_empty() => eprintln!("存储迁移：{r:?}"),
        Err(err) => eprintln!("存储迁移失败：{err}（下次启动重试）"),
        _ => {}
    }
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p omt-exec migrate 2>&1 | grep -E '^test result'
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
```
Expected：migrate 4 passed。

- [ ] **Step 4: 用真实旧数据做一次手工演练（不改动真实目录）**

```bash
SRC="${OMT_RUNTIME_DIR:-$HOME/.omt/run}"
TMP=$(mktemp -d) && cp -R "$SRC/agents" "$SRC/transcripts" "$TMP/" 2>/dev/null
cat > /tmp/omt-migrate-drill.rs <<'EOF'
fn main() {
    let dir = std::env::args().nth(1).expect("runtime dir");
    println!("{:?}", omt_exec::migrate::migrate_legacy(std::path::Path::new(&dir)));
}
EOF
mkdir -p crates/omt-exec/examples && cp /tmp/omt-migrate-drill.rs crates/omt-exec/examples/migrate-drill.rs
cargo run -q -p omt-exec --example migrate-drill -- "$TMP"
find "$TMP/agents" -maxdepth 3 | head -20
rm crates/omt-exec/examples/migrate-drill.rs && rmdir crates/omt-exec/examples 2>/dev/null; rm -rf "$TMP"
```
Expected：报告里的 homes / agents 数与真实数据一致，`skipped_corrupt` 为空；每个 agent 目录下有 `agent.json` 和对应的 jsonl / raw.log。演练用的 example 不提交。

- [ ] **Step 5: 提交**

```bash
git add -A crates/omt-exec
git commit -m "feat: idempotent migration from agents.json to per-agent dirs

Runs at startup under a global flock; resumes after interruption,
never overwrites an already-migrated agent.json, leaves corrupt legacy
files in place and reports them.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: MCP 项目注入的跨进程锁 + 清扫跳过存活 exec（P-9）

**Files:**
- Modify: `crates/omt-exec/src/mcp_injection.rs`、`crates/omt-exec/src/executor.rs`（`startup_housekeeping` 的调用）
- Test: `mcp_injection.rs` 的 `mod tests`

**Interfaces:**
- Consumes: `omt_harness::atomic_json::{lock_exclusive, LOCK_ACQUIRE_TIMEOUT}`。
- Produces: `pub fn sweep_crash_residue(runtime_dir: &Path, is_exec_live: &dyn Fn(&str) -> bool) -> Result<usize, String>`（新增参数；launcher id 即 exec id，`persistent` 除外）。

- [ ] **Step 1: 写失败测试**

在 `mcp_injection.rs` 的 `mod tests` 加（复用该文件已有的 `rig()` 与 `launcher_count()`）：

```rust
    /// P-9：桌面端重启时 shim 可能仍在运行——清扫不得摘掉存活 exec 的注入。
    #[test]
    fn sweep_skips_workspaces_with_live_execs() {
        let rig = rig();
        let inj = prepare(&rig.fake_omt, &rig.runtime, &rig.workspace, "omp", "exec-live", false).unwrap();
        let mcp = rig.workspace.join(".omp").join("mcp.json");
        assert!(std::fs::read_to_string(&mcp).unwrap().contains("omt"));
        let swept = sweep_crash_residue(&rig.runtime, &|id| id == "exec-live").unwrap();
        assert_eq!(swept, 0);
        assert!(std::fs::read_to_string(&mcp).unwrap().contains("omt"), "存活 exec 的注入必须保留");
        assert_eq!(launcher_count(&rig.runtime), 1, "存活 exec 的 launcher 必须保留");
        // exec 结束后同一清扫照常恢复
        let swept = sweep_crash_residue(&rig.runtime, &|_| false).unwrap();
        assert_eq!(swept, 1);
        drop(inj);
    }

    /// P-9：跨进程锁——持锁期间另一个 prepare 必须等待而不是并发改写 state。
    #[test]
    fn project_ops_take_the_cross_process_lock() {
        let rig = rig();
        let held = project_flock(&rig.runtime, OMP_SPEC).unwrap();
        let runtime = rig.runtime.clone();
        let workspace = rig.workspace.clone();
        let omt = rig.fake_omt.clone();
        let started = std::time::Instant::now();
        let t = std::thread::spawn(move || {
            prepare(&omt, &runtime, &workspace, "omp", "exec-2", false).map(|_| started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(400));
        drop(held);
        let waited = t.join().unwrap().unwrap();
        assert!(waited >= std::time::Duration::from_millis(350), "{waited:?}");
    }
```

（若该文件已有的 omp 测试检查的注入文件不是 `.omp/mcp.json`，按 `OMP_SPEC.rel` 改这里的路径。）

```bash
cargo test -p omt-exec mcp_injection 2>&1 | tail -3
```
Expected：编译失败（`sweep_crash_residue` 参数个数不对、`project_flock` 未定义）。

- [ ] **Step 2: 实现**

1. 新增：

```rust
/// P-9：项目级注入的跨进程锁（多个 shim 进程共写同一 state.json 的引用
/// 计数）。与进程内 PROJECT_LOCK 叠加使用：先拿进程内锁，再拿 flock。
fn project_flock(runtime_dir: &Path, spec: ProjectSpec) -> Result<std::fs::File, String> {
    omt_harness::atomic_json::lock_exclusive(
        &spec.injection_root(runtime_dir).join(".project"),
        omt_harness::atomic_json::LOCK_ACQUIRE_TIMEOUT,
    )
}
```

2. `prepare_project_with` 与 `release_project` 里每一处 `let _guard = PROJECT_LOCK.lock()…;` 之后紧跟一行：

```rust
    let _flock = project_flock(runtime_dir, spec)?;
```

（若某处函数返回值不是 `Result<_, String>`，用 `match` 记日志后返回；`grep -n 'PROJECT_LOCK' crates/omt-exec/src/mcp_injection.rs` 列出全部位置，测试模块里的除外。）

3. `sweep_crash_residue` / `sweep_spec_crash_residue` 加参数 `is_exec_live: &dyn Fn(&str) -> bool`，并在 workspace 循环里 `let state = omp_state(...)` 之后加：

```rust
        let has_live = state["launchers"]
            .as_array()
            .map(|ids| ids.iter().filter_map(|v| v.as_str()).any(is_exec_live))
            .unwrap_or(false);
        if has_live {
            continue; // 该 workspace 仍被存活 exec 使用，由其 shim 收尾时释放
        }
```

孤儿 launcher 的清扫循环改为：

```rust
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
```

清扫循环内 `PROJECT_LOCK` 之后同样加 `let _flock = project_flock(runtime_dir, spec)?;`。

4. `executor::startup_housekeeping` 的调用改为 `crate::mcp_injection::sweep_crash_residue(runtime_dir, &|_| false)`（Task 10 换成真实的存活判定）。既有测试里对 `sweep_crash_residue(&rt)` 的调用一律改为 `sweep_crash_residue(&rt, &|_| false)`。

- [ ] **Step 3: 跑测试并提交**

```bash
cargo test -p omt-exec mcp_injection 2>&1 | grep -E '^test result|FAILED'
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
git add crates/omt-exec
git commit -m "fix: cross-process flock for project MCP injection; sweep spares live execs

Multiple shim processes share one workspace's state.json refcount, and a
desktop restart must not strip the injection of an exec that is still
running (exec-plane plan P-9).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: admin 授权 helper（P-1）与 shim 侧 daemon 连接

**Files:**
- Create: `crates/omt-exec/src/grants.rs`、`crates/omt-exec/src/daemon_link.rs`
- Modify: `crates/omt-exec/src/lib.rs`、`apps/desktop/src-tauri/src/daemon.rs`（`register_admin_grant` 改为委托）
- Create: `crates/omt-runtime/tests/exec_daemon_link.rs`（真实 daemon 集成测试）
- Modify: `crates/omt-runtime/Cargo.toml`（`[dev-dependencies]` 加 `omt-exec`）

**Interfaces:**
- Consumes: `lock_exclusive`、`write_atomic`、`omt_client::{read_descriptor, pid_live, endpoint_live, wait_for_descriptor, Client, EnrollOptions}`、`executor::{RpcFn, process_start_identity}`。
- Produces:
  - `omt_exec::grants::{register_replacing_prefix, add, remove, principals, sweep_dead_temp_grants}` 与 `TempGrant`（`TempGrant::add(runtime_dir, principal) -> Result<TempGrant, String>`，drop 时移除）
  - `omt_exec::daemon_link::{DaemonLink, Connector, admin_connector}`：
    - `pub type Connector = Arc<dyn Fn() -> Result<RpcFn, String> + Send + Sync>;`
    - `DaemonLink::connect(runtime_dir: &Path, kind: &str, daemon_bin: Option<&Path>) -> Result<DaemonLink, String>`
    - `DaemonLink::principal(&self) -> &str`、`DaemonLink::call(&mut self, method, params) -> Result<Value, String>`
    - `admin_connector(runtime_dir: PathBuf, daemon_bin: Option<PathBuf>) -> Connector`（连接 + 临时授权，返回的 RpcFn 被 drop 时授权随之撤销）

- [ ] **Step 1: 写 grants 的失败测试**

`crates/omt-exec/src/grants.rs` 先写测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_prefix_keeps_other_principals() {
        let tmp = tempfile::tempdir().unwrap();
        add(tmp.path(), "human:alice").unwrap();
        register_replacing_prefix(tmp.path(), "desktop:", "desktop:100").unwrap();
        register_replacing_prefix(tmp.path(), "desktop:", "desktop:200").unwrap();
        assert_eq!(
            principals(tmp.path()),
            vec!["human:alice".to_string(), "desktop:200".to_string()]
        );
    }

    #[test]
    fn temp_grant_is_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let _g = TempGrant::add(tmp.path(), "cli:4242").unwrap();
            assert!(principals(tmp.path()).contains(&"cli:4242".to_string()));
        }
        assert!(!principals(tmp.path()).contains(&"cli:4242".to_string()));
    }

    #[test]
    fn sweep_removes_temp_grants_of_dead_pids_only() {
        let tmp = tempfile::tempdir().unwrap();
        let me = format!("cli:{}", std::process::id());
        // 模拟崩溃遗留：登记了但没有 drop（forget 掉守卫）
        std::mem::forget(TempGrant::add(tmp.path(), &me).unwrap());
        std::mem::forget(TempGrant::add(tmp.path(), "cli:999999").unwrap());
        add(tmp.path(), "cli:999998").unwrap(); // 非临时条目：不归清扫管
        let removed = sweep_dead_temp_grants(tmp.path()).unwrap();
        assert_eq!(removed, 1);
        let now = principals(tmp.path());
        assert!(now.contains(&me));
        assert!(!now.contains(&"cli:999999".to_string()));
        assert!(now.contains(&"cli:999998".to_string()));
    }

    #[test]
    fn concurrent_writers_do_not_lose_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || add(&dir, &format!("p:{i}")).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(principals(&dir).len(), 8);
    }
}
```

`lib.rs` 加 `pub mod grants;`。

```bash
cargo test -p omt-exec grants 2>&1 | tail -3
```
Expected：编译失败。

- [ ] **Step 2: 实现 grants**

在测试模块之前加：

```rust
//! `<runtime>/admin-grants.json` 的加锁读写（P-1）。daemon 每次判定 admin
//! 时都重新读这个文件（auth.rs::admin_principals），所以增删立即生效。
//!
//! 写入方：desktop（单条 `desktop:<pid>` 替换语义）与 exec-shim（释放租约期间
//! 临时登记 `cli:<pid>`，用完即删）。两者经同一把 flock 串行，不会互相覆盖。
//! 临时条目另记在 `exec/temp-grants.json`，启动时清扫 pid 已死的残留。

use std::path::{Path, PathBuf};

use omt_harness::atomic_json::{
    lock_exclusive, read_json, update_json, write_atomic, LOCK_ACQUIRE_TIMEOUT,
};

const GRANTS_FILE: &str = "admin-grants.json";

fn grants_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(GRANTS_FILE)
}

fn temp_registry(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("exec").join("temp-grants.json")
}

fn read_principals(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("principalIds").and_then(|ids| ids.as_array()).map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// 锁内 read-modify-write；文件形状与 daemon 读取端一致。
fn update_principals(runtime_dir: &Path, f: impl FnOnce(&mut Vec<String>)) -> Result<(), String> {
    let path = grants_path(runtime_dir);
    let _lock = lock_exclusive(&path, LOCK_ACQUIRE_TIMEOUT)?;
    let mut ids = read_principals(&path);
    f(&mut ids);
    let body = serde_json::json!({ "schemaVersion": 1, "principalIds": ids }).to_string();
    write_atomic(&path, &body)
}

/// 当前 principal 列表（测试与诊断用）。
pub fn principals(runtime_dir: &Path) -> Vec<String> {
    read_principals(&grants_path(runtime_dir))
}

/// desktop 语义：删除所有 `prefix` 开头的条目，再加入 `principal`。
pub fn register_replacing_prefix(runtime_dir: &Path, prefix: &str, principal: &str) -> Result<(), String> {
    update_principals(runtime_dir, |ids| {
        ids.retain(|id| !id.starts_with(prefix));
        ids.push(principal.to_string());
    })
}

pub fn add(runtime_dir: &Path, principal: &str) -> Result<(), String> {
    update_principals(runtime_dir, |ids| {
        if !ids.iter().any(|id| id == principal) {
            ids.push(principal.to_string());
        }
    })
}

pub fn remove(runtime_dir: &Path, principal: &str) -> Result<(), String> {
    update_principals(runtime_dir, |ids| ids.retain(|id| id != principal))
}

/// 临时授权守卫：创建时登记（grants + 临时登记表），drop 时撤销。
pub struct TempGrant {
    runtime_dir: PathBuf,
    principal: String,
}

impl TempGrant {
    pub fn add(runtime_dir: &Path, principal: &str) -> Result<Self, String> {
        update_json(&temp_registry(runtime_dir), |list: &mut Vec<String>| {
            if list.iter().any(|p| p == principal) {
                return false;
            }
            list.push(principal.to_string());
            true
        })?;
        add(runtime_dir, principal)?;
        Ok(Self {
            runtime_dir: runtime_dir.to_path_buf(),
            principal: principal.to_string(),
        })
    }
}

impl Drop for TempGrant {
    fn drop(&mut self) {
        let _ = remove(&self.runtime_dir, &self.principal);
        let _ = update_json(&temp_registry(&self.runtime_dir), |list: &mut Vec<String>| {
            let before = list.len();
            list.retain(|p| p != &self.principal);
            list.len() != before
        });
    }
}

fn pid_of(principal: &str) -> Option<i64> {
    principal.rsplit(':').next()?.parse().ok()
}

/// 清扫崩溃遗留的临时授权：只处理临时登记表里、pid 已不存在的条目。
pub fn sweep_dead_temp_grants(runtime_dir: &Path) -> Result<usize, String> {
    let (list, _) = read_json::<Vec<String>>(&temp_registry(runtime_dir))?;
    let dead: Vec<String> = list
        .into_iter()
        .filter(|p| pid_of(p).map(|pid| !omt_client::pid_live(pid)).unwrap_or(true))
        .collect();
    for principal in &dead {
        remove(runtime_dir, principal)?;
    }
    update_json(&temp_registry(runtime_dir), |list: &mut Vec<String>| {
        let before = list.len();
        list.retain(|p| !dead.contains(p));
        list.len() != before
    })?;
    Ok(dead.len())
}
```

（`omt_client::pid_live` 的签名是 `fn(i64) -> bool`。）

- [ ] **Step 3: desktop 改为委托**

`apps/desktop/src-tauri/src/daemon.rs` 的 `register_admin_grant` 函数体替换为：

```rust
pub fn register_admin_grant(dir: &Path, principal_id: &str) -> Result<(), String> {
    // P-1：与 exec-shim 的临时授权共用同一把 flock，避免并发覆盖
    omt_exec::grants::register_replacing_prefix(dir, "desktop:", principal_id)
}
```

（doc 注释保留，追加一句「写入经 omt_exec::grants 加锁」。）

```bash
cargo test -p omt-exec grants 2>&1 | grep -E '^test result'
cargo test -p oh-my-ticket-desktop 2>&1 | grep -E '^test result'
```
Expected：grants 4 passed；桌面测试全绿。

- [ ] **Step 4: 实现 daemon_link**

`crates/omt-exec/src/daemon_link.rs`：

```rust
//! shim 侧的 daemon 连接（设计 §3.2「daemon 连接」）：discover-or-spawn →
//! enroll（kind 由调用方给，shim 用 "cli"）。不复用存储的凭据，principal
//! 恒为 `<kind>:<本进程 pid>`，P-1 的临时授权据此登记。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::executor::RpcFn;
use crate::grants::TempGrant;

pub type Connector = Arc<dyn Fn() -> Result<RpcFn, String> + Send + Sync>;

pub struct DaemonLink {
    client: omt_client::Client,
    principal: String,
}

fn live_descriptor(runtime_dir: &Path) -> Option<omt_client::Descriptor> {
    let d = omt_client::read_descriptor(runtime_dir)?;
    (omt_client::pid_live(d.pid) && omt_client::endpoint_live(&d.endpoint)).then_some(d)
}

fn spawn_daemon(bin: &Path, runtime_dir: &Path) -> Result<omt_client::Descriptor, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(bin);
    command
        .arg("--runtime-dir")
        .arg(runtime_dir)
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
    command.spawn().map_err(|e| format!("spawn omt-daemon: {e}"))?;
    omt_client::wait_for_descriptor(runtime_dir, Duration::from_secs(10))
        .ok_or_else(|| "omt-daemon 10s 内未产出 descriptor".to_string())
}

impl DaemonLink {
    pub fn connect(runtime_dir: &Path, kind: &str, daemon_bin: Option<&Path>) -> Result<Self, String> {
        let descriptor = match live_descriptor(runtime_dir) {
            Some(d) => d,
            None => {
                let bin = daemon_bin.ok_or("daemon 不在线，且没有可拉起的 omt-daemon")?;
                spawn_daemon(bin, runtime_dir)?
            }
        };
        let options = omt_client::EnrollOptions {
            kind: kind.into(),
            name: Some(format!("omt-exec-{kind}")),
            actor_namespace: None,
            operations: None,
            credential_path: None,
        };
        let enrollment = omt_client::Client::connect_and_enroll(&descriptor, &options)
            .map_err(|p| format!("enroll: {} ({})", p.message, p.code))?;
        let principal = enrollment.handshake["credential"]["principalId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if principal.is_empty() {
            return Err("handshake 缺少 credential.principalId".into());
        }
        Ok(Self {
            client: enrollment.client,
            principal,
        })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        self.client
            .call(method, params)
            .map_err(|p| format!("{} ({})", p.message, p.code))
    }
}

/// P-1：连接 + 临时 admin 授权。返回的 RpcFn 持有连接与授权守卫；RpcFn
/// 被 drop（shim 释放完租约）时授权随之撤销。
pub fn admin_connector(runtime_dir: PathBuf, daemon_bin: Option<PathBuf>) -> Connector {
    Arc::new(move || {
        let link = DaemonLink::connect(&runtime_dir, "cli", daemon_bin.as_deref())?;
        let grant = TempGrant::add(&runtime_dir, link.principal())?;
        let shared = Arc::new(Mutex::new((link, grant)));
        let rpc: RpcFn = Arc::new(move |method: &str, params: serde_json::Value| {
            let mut guard = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.0.call(method, params)
        });
        Ok(rpc)
    })
}
```

`lib.rs` 加 `pub mod daemon_link;`。

- [ ] **Step 5: 真实 daemon 集成测试**

`crates/omt-runtime/Cargo.toml` 的 `[dev-dependencies]` 加 `omt-exec = { path = "../omt-exec" }`。

`crates/omt-runtime/tests/exec_daemon_link.rs`：

```rust
//! P-1 端到端：shim 侧连接以 cli 身份 enroll；未授权时 admin 路径被拒，
//! 临时授权期间放行，授权撤销后再次被拒。daemon 零改动的前提下验证。

use std::path::PathBuf;

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_omt-daemon"))
}

#[test]
fn temp_grant_toggles_admin_authority() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).unwrap();

    let mut link = omt_exec::daemon_link::DaemonLink::connect(&runtime, "cli", Some(&daemon_bin()))
        .expect("discover-or-spawn + enroll");
    assert!(link.principal().starts_with("cli:"), "{}", link.principal());

    // 未授权：admin-only 的 home/reindex（human_administrative）应被拒
    let denied = link.call("home/reindex", serde_json::json!({ "homeId": "h_none" }));
    assert!(denied.is_err(), "{denied:?}");

    {
        let _g = omt_exec::grants::TempGrant::add(&runtime, link.principal()).unwrap();
        let err = link
            .call("home/reindex", serde_json::json!({ "homeId": "h_none" }))
            .unwrap_err();
        // 授权后不再是授权错误（homeId 不存在会报 NOT_FOUND 之类的业务错误）
        assert!(!err.contains("admin-required"), "{err}");
    }
    let again = link
        .call("home/reindex", serde_json::json!({ "homeId": "h_none" }))
        .unwrap_err();
    assert!(again.contains("admin"), "撤销后应回到被拒：{again}");

    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_omt"))
        .args(["--runtime-dir", runtime.to_str().unwrap(), "daemon-stop"])
        .status();
}
```

先确认拒绝时的错误文本：

```bash
cargo test -p omt-runtime --test exec_daemon_link -- --nocapture 2>&1 | tail -20
```

若 `home/reindex` 未授权时报的不是含 `admin` 的错误（例如 `FORBIDDEN`），把两处断言改为匹配实际的错误码；若 `home/reindex` 对 cli 有「离线独占」的特殊放行（`dispatch.rs:1488` 附近），改用另一个只认 admin 的路径——无 leaseToken 的 `run/report` 打到不存在的 run 上，未授权时报 admin 相关错误，授权后报 NOT_FOUND。

- [ ] **Step 6: 提交**

```bash
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
git add -A crates/omt-exec crates/omt-runtime apps/desktop/src-tauri
git commit -m "feat: locked admin-grants helper and shim-side daemon link (plan P-1)

Desktop and exec-shim now write admin-grants.json under one flock. The
shim enrolls as kind cli and holds a temporary grant only while it
releases leases; leftovers from crashed shims are swept by pid.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: launch spec 与 `omt exec-shim`

**Files:**
- Create: `crates/omt-exec/src/spec.rs`、`crates/omt-exec/src/shim.rs`
- Modify: `crates/omt-exec/src/lib.rs`、`crates/omt-exec/src/agents.rs`（`ExecRecord` 新字段、`SHIM_LOST_MARKER`、`list_reconcile_targets` 识别 `lease_release_pending`）、`crates/omt-exec/src/executor.rs`（对账置 `reconciled` 时清 `lease_release_pending`）
- Modify: `crates/omt-runtime/Cargo.toml`（`[dependencies]` 加 `omt-exec`）、`crates/omt-runtime/src/cli/mod.rs`（隐藏动词）
- Create: `crates/omt-runtime/tests/exec_shim.rs`
- Modify: `apps/desktop/src/types.ts`（`ExecRecord` 新字段）

**Interfaces:**
- Consumes: Task 4 的 `layout`、Task 7 的 `daemon_link::{Connector, admin_connector}`、`executor::{ExecutionManager::with_max_duration, set_lease_release, SpawnRequest, release_items_for_exec, workspace_root_of}`、`mcp_injection::{prepare, release}`、`omt_harness::harness::{resolve_launch, compose_launch_args, prompt_file_path}`。
- Produces:
  - `omt_exec::spec::{LaunchSpec, SPEC_SCHEMA_VERSION}`；`LaunchSpec::write(&self, path) -> Result<(), String>`、`LaunchSpec::read(path) -> Result<LaunchSpec, String>`
  - `omt_exec::shim::run(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str, connector: Connector) -> i32`
  - `ExecRecord` 新字段：`shim_pid: Option<u32>`、`shim_start_id: Option<String>`、`lease_release_pending: bool`（均 `#[serde(default)]`）
  - CLI：`omt [--runtime-dir <d>] exec-shim <homeId> <agentId> <execId>`（不进 USAGE）

- [ ] **Step 1: ExecRecord 新字段**

`crates/omt-exec/src/agents.rs` 的 `ExecRecord` 末尾加：

```rust
    /// exec-shim 进程 pid 与内核起始标识（设计 §3.2：停止 / 存活检查先校验
    /// 起始标识，防 pid 复用）。由 launcher 在拉起 shim 后写入。
    #[serde(default)]
    pub shim_pid: Option<u32>,
    #[serde(default)]
    pub shim_start_id: Option<String>,
    /// shim 收尾时租约释放失败（daemon 不可达等）——留给下次对账重试（设计 §5）。
    #[serde(default)]
    pub lease_release_pending: bool,
```

`apps/desktop/src/types.ts` 的 `ExecRecord` 末尾加：

```ts
  /** exec-shim 进程身份（Rust 侧写入，仅展示/诊断；停止授权不信任 renderer） */
  shimPid?: number | null
  shimStartId?: string | null
  /** shim 收尾时租约释放失败、待对账重试 */
  leaseReleasePending?: boolean
```

`list_reconcile_targets` 的筛选条件改为：

```rust
                if !exec.reconciled
                    && exec.run_id.is_some()
                    && (exec.lease_release_pending
                        || matches!(
                            exec.failure_detail.as_deref(),
                            Some(RESTART_MARKER) | Some(SHIM_LOST_MARKER)
                        ))
```

并在 `RESTART_MARKER` 旁定义：

```rust
/// shim 在 exec 结束前消失（崩溃 / 被 KILL）的对账签名（P-8）。
pub const SHIM_LOST_MARKER: &str = "exec shim exited before finishing";
```

`reconcile_interrupted_execs_with` 里置 `exec.reconciled = true` 的闭包同时写 `exec.lease_release_pending = false;`。

- [ ] **Step 2: spec 与它的测试**

`crates/omt-exec/src/spec.rs`：

```rust
//! launch spec（P-4）：launcher 写、shim 读。只放重算启动参数所需的输入，
//! 不放 env 值（secrets 纪律）；shim 用 resolve_launch + compose_launch_args
//! 重新计算 argv 与 env。文件位置：`<agentDir>/<execId>.spec.json`（0600）。

use std::path::{Path, PathBuf};

pub const SPEC_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    pub schema_version: u32,
    pub home_id: String,
    /// `.omt` 目录（cwd = 其父目录，见 executor::workspace_root_of）
    pub home_path: PathBuf,
    pub agent_id: String,
    pub exec_id: String,
    pub harness: String,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub unattended: bool,
    pub model: Option<String>,
    pub resume_session: Option<String>,
    /// 渲染后的派发 prompt（模板 + run_id，非敏感）
    pub prompt: String,
    pub max_duration_secs: u64,
    /// MCP 桥二进制（omt）
    pub omt_path: PathBuf,
}

impl LaunchSpec {
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        omt_harness::atomic_json::write_atomic(path, &text)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read spec {}: {e}", path.display()))?;
        let spec: Self = serde_json::from_str(&text).map_err(|e| format!("parse spec: {e}"))?;
        if spec.schema_version != SPEC_SCHEMA_VERSION {
            return Err(format!(
                "spec schemaVersion {} 不受支持（期望 {SPEC_SCHEMA_VERSION}）",
                spec.schema_version
            ));
        }
        Ok(spec)
    }
}

#[cfg(test)]
pub(crate) fn sample(home_path: &Path, omt_path: &Path) -> LaunchSpec {
    LaunchSpec {
        schema_version: SPEC_SCHEMA_VERSION,
        home_id: "h_test".into(),
        home_path: home_path.to_path_buf(),
        agent_id: "agent-exec-s".into(),
        exec_id: "exec-s".into(),
        harness: "pi".into(),
        node_id: None,
        run_id: None,
        unattended: true,
        model: None,
        resume_session: None,
        prompt: "do it".into(),
        max_duration_secs: 60,
        omt_path: omt_path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_no_env_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("exec-s.spec.json");
        let spec = sample(&tmp.path().join(".omt"), Path::new("/bin/omt"));
        spec.write(&path).unwrap();
        assert_eq!(LaunchSpec::read(&path).unwrap(), spec);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("\"env"), "spec 不得含 env：{raw}");
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.json");
        let mut spec = sample(tmp.path(), Path::new("/bin/omt"));
        spec.schema_version = 99;
        spec.write(&path).unwrap();
        assert!(LaunchSpec::read(&path).unwrap_err().contains("schemaVersion"));
    }
}
```

`lib.rs` 加 `pub mod spec;`。

```bash
cargo test -p omt-exec spec 2>&1 | grep -E '^test result'
```
Expected：2 passed。

- [ ] **Step 3: shim 的失败测试（进程内，假 harness + 假 daemon）**

`crates/omt-exec/src/shim.rs` 先放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// 准备：harness 配置（Manual 指向假脚本）、home、已注册的 agent 与 spec。
    fn rig(harness_body: &str, run_id: Option<&str>) -> (tempfile::TempDir, PathBuf, crate::spec::LaunchSpec) {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run");
        let home_path = tmp.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let fake = write_script(tmp.path(), "fake-pi", harness_body);
        let fake_omt = write_script(tmp.path(), "omt", "echo omt");
        omt_harness::harness::upsert_config(
            &runtime,
            "pi",
            omt_harness::harness::HarnessUserConfig {
                enabled: true,
                binary: omt_harness::harness::BinarySource::Manual {
                    path: fake.display().to_string(),
                },
                extra_args: vec![],
                env: Default::default(),
            },
        )
        .unwrap();
        let agent = crate::agents::register_agent(
            &runtime,
            "h_test",
            "pi",
            crate::agents::NewExec {
                exec_id: "exec-s".into(),
                node_id: None,
                run_id: run_id.map(str::to_string),
                binary_path: None,
                binary_args: vec![],
                model: None,
            },
        )
        .unwrap();
        let mut spec = crate::spec::sample(&home_path, &fake_omt);
        spec.agent_id = agent.id.clone();
        spec.run_id = run_id.map(str::to_string);
        let dir = crate::layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
        spec.write(&crate::layout::exec_file(&dir, "exec-s", crate::layout::ExecFile::Spec))
            .unwrap();
        (tmp, runtime, spec)
    }

    fn recording_connector(calls: Arc<Mutex<Vec<String>>>) -> Connector {
        Arc::new(move || {
            let calls = calls.clone();
            let rpc: RpcFn = Arc::new(move |method: &str, _params: serde_json::Value| {
                calls.lock().unwrap().push(method.to_string());
                Ok(serde_json::json!({ "items": [] }))
            });
            Ok(rpc)
        })
    }

    #[test]
    fn shim_runs_harness_to_completion_and_archives() {
        let (_t, runtime, spec) = rig("echo '{\"type\":\"agent_end\"}'\nexit 0", Some("RUN-1"));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let code = run(&runtime, "h_test", &spec.agent_id, "exec-s", recording_connector(calls.clone()));
        assert_eq!(code, 0);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id).unwrap().unwrap();
        assert_eq!(agent.status, crate::agents::AgentRecordStatus::Exited);
        assert_eq!(agent.execs[0].exit_code, Some(0));
        assert!(agent.execs[0].pid.is_some());
        let dir = crate::layout::agent_dir(&runtime, "h_test", &spec.agent_id).unwrap();
        let jsonl = std::fs::read_to_string(crate::layout::exec_file(&dir, "exec-s", crate::layout::ExecFile::Jsonl)).unwrap();
        assert!(jsonl.lines().last().unwrap().contains("\"exit\""));
        assert_eq!(calls.lock().unwrap().first().map(String::as_str), Some("run/get"), "收尾应查询租约");
    }

    #[test]
    fn shim_marks_failed_when_spec_missing() {
        let (_t, runtime, spec) = rig("exit 0", None);
        let dir = crate::layout::agent_dir(&runtime, "h_test", &spec.agent_id).unwrap();
        std::fs::remove_file(crate::layout::exec_file(&dir, "exec-s", crate::layout::ExecFile::Spec)).unwrap();
        let code = run(&runtime, "h_test", &spec.agent_id, "exec-s", recording_connector(Default::default()));
        assert_eq!(code, 1);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id).unwrap().unwrap();
        assert_eq!(agent.status, crate::agents::AgentRecordStatus::Failed);
        assert!(agent.execs[0].failure_detail.as_deref().unwrap_or("").contains("spec"));
    }

    #[test]
    fn unreachable_daemon_leaves_lease_release_pending() {
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        let failing: Connector = Arc::new(|| Err("daemon down".to_string()));
        assert_eq!(run(&runtime, "h_test", &spec.agent_id, "exec-s", failing), 0);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id).unwrap().unwrap();
        assert!(agent.execs[0].lease_release_pending);
    }
}
```

`lib.rs` 加 `pub mod shim;`。

```bash
cargo test -p omt-exec shim:: 2>&1 | tail -3
```
Expected：编译失败（`run` / `Connector` 未定义）。

- [ ] **Step 4: 实现 shim**

在 `shim.rs` 的测试模块之前加：

```rust
//! `omt exec-shim <homeId> <agentId> <execId>`（设计 §3.2 / KD-E1）：每 exec
//! 一个分离进程。读 spec → MCP 注入 → 写 prompt 文件 → spawn harness（事件
//! 写入 jsonl）→ 等终态 → 收尾（含租约释放）→ 退出。发起方此后可以退出。
//!
//! 停止：SIGTERM / SIGINT 置标志，主循环转成 `exec_stop`（TERM → 宽限 → KILL
//! 升级沿用执行器原逻辑）。信号处理函数在 exec 时被内核重置，harness 不受影响。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::agents::{self, AgentAttachment, AgentRecordStatus};
use crate::daemon_link::Connector;
use crate::executor::{ExecutionManager, RpcFn, SpawnRequest};
use crate::layout::{self, ExecFile};
use crate::spec::LaunchSpec;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_sig: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_stop_handler() {
    // SAFETY: 处理函数只做一次原子写，满足 async-signal-safe
    unsafe {
        libc::signal(libc::SIGTERM, on_stop_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop_signal as libc::sighandler_t);
    }
}

/// shim 主体；返回进程退出码（0 = 走完收尾，1 = 启动阶段失败）。
pub fn run(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str, connector: Connector) -> i32 {
    install_stop_handler();
    match run_inner(runtime_dir, home_id, agent_id, exec_id, connector) {
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

fn run_inner(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    connector: Connector,
) -> Result<(), String> {
    let agent_dir = layout::agent_dir(runtime_dir, home_id, agent_id)?;
    let spec = LaunchSpec::read(&layout::exec_file(&agent_dir, exec_id, ExecFile::Spec))?;
    if spec.home_id != home_id || spec.agent_id != agent_id || spec.exec_id != exec_id {
        return Err("spec 与命令行参数不一致".into());
    }
    let plan = omt_harness::harness::resolve_launch(runtime_dir, &spec.harness)?;
    let prompt_file =
        omt_harness::harness::prompt_file_path(&agent_dir, exec_id, plan.prompt_channel);
    let args = omt_harness::harness::compose_launch_args(
        &plan,
        spec.unattended,
        &spec.prompt,
        prompt_file.as_deref(),
        spec.model.as_deref(),
        spec.resume_session.as_deref(),
    );
    let env_additions = plan.sanitized_env();
    let cwd = crate::executor::workspace_root_of(&spec.home_path);
    let mcp = crate::mcp_injection::prepare(&spec.omt_path, runtime_dir, &cwd, &spec.harness, exec_id, false)?;
    if let Some(path) = &prompt_file {
        if let Err(e) = std::fs::write(path, &spec.prompt) {
            let _ = crate::mcp_injection::release(runtime_dir, &mcp);
            return Err(format!("write prompt file: {e}"));
        }
    }

    let manager = ExecutionManager::with_max_duration(Duration::from_secs(spec.max_duration_secs));
    manager.set_lease_release(lease_releaser(runtime_dir.to_path_buf(), agent_id.to_string(), connector));
    let request = SpawnRequest {
        exec_id: exec_id.to_string(),
        agent_id: agent_id.to_string(),
        home_id: home_id.to_string(),
        home_path: spec.home_path.clone(),
        harness: spec.harness.clone(),
        node_id: spec.node_id.clone(),
        run_id: spec.run_id.clone(),
        program: plan.program.clone(),
        args,
        env_additions,
        mcp,
        transcript_dir: agent_dir.clone(),
    };
    // 事件已由执行器写入 jsonl（KD-E3）；进程内 sink 无人消费
    let session = manager.spawn(runtime_dir, request, Arc::new(|_| {}))?;
    loop {
        if session.terminal().is_some() {
            return Ok(());
        }
        if STOP_REQUESTED.swap(false, Ordering::SeqCst) {
            manager.exec_stop(exec_id)?;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 终态租约释放：按需连接 daemon（长时间执行期间 daemon 可能已 idle 退出）。
/// 失败只记待对账标记，不影响收尾的其余步骤。
fn lease_releaser(runtime_dir: PathBuf, agent_id: String, connector: Connector) -> crate::executor::LeaseRelease {
    Arc::new(move |home_id: &str, run_id: &str, exec_id: &str| {
        let outcome = connector()
            .and_then(|rpc: RpcFn| crate::executor::release_items_for_exec(&*rpc, home_id, run_id, exec_id));
        if let Err(err) = outcome {
            eprintln!("exec-shim {exec_id}: 租约释放失败（{err}），留待对账");
            let _ = agents::update_exec(&runtime_dir, home_id, &agent_id, exec_id, |exec| {
                exec.lease_release_pending = true;
            });
        }
    })
}
```

（`release_items_for_exec` 在 Task 2 已改为返回 `Result<(), String>`。这里的 `connector()` 返回的 `RpcFn` 在闭包结束时 drop，临时授权随之撤销。）

- [ ] **Step 5: 跑 shim 单测**

```bash
cargo test -p omt-exec shim:: 2>&1 | grep -E '^test result|FAILED|panicked'
```
Expected：3 passed。

- [ ] **Step 6: CLI 隐藏动词**

`crates/omt-runtime/Cargo.toml` 的 `[dependencies]` 加 `omt-exec = { path = "../omt-exec" }`（Task 7 加的 dev-dependency 可删除，改由普通依赖覆盖）。

`crates/omt-runtime/src/cli/mod.rs` 的 `dispatch_args` 中，在 `"mcp" => …` 分支之后加：

```rust
        // 设计 §3.2 / KD-E1：每 exec 一个分离 shim（launcher 拉起，不进 USAGE）。
        // 参数用位置参数——全局 `--home` 会被 split_globals 吞掉。
        "exec-shim" => {
            let args = rest.make_contiguous();
            let [home_id, agent_id, exec_id] = args else {
                return Err(usage_err("exec-shim <homeId> <agentId> <execId>"));
            };
            let runtime_dir = crate::paths::resolve(split.globals.runtime_dir.as_deref());
            let daemon_bin = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("omt-daemon")))
                .filter(|path| path.is_file());
            let connector = omt_exec::daemon_link::admin_connector(runtime_dir.clone(), daemon_bin);
            std::process::exit(omt_exec::shim::run(
                &runtime_dir, home_id, agent_id, exec_id, connector,
            ));
        }
```

（`let [a, b, c] = args else` 是对 `&mut [String]` 的切片模式，三个绑定都是 `&mut String`，可自动解引用为 `&str`。若编译器对可变借用报错，改为 `match &*args { [h, a, e] => …, _ => return Err(…) }`。）

- [ ] **Step 7: 真二进制端到端测试**

`crates/omt-runtime/tests/exec_shim.rs`：

```rust
//! omt exec-shim 端到端（设计 §6「shim 生命周期」）：真二进制、假 harness。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn omt() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_omt"))
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Rig {
    _tmp: tempfile::TempDir,
    runtime: PathBuf,
    agent_id: String,
}

fn rig(harness_body: &str) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    let home_path = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&home_path).unwrap();
    let fake = write_script(tmp.path(), "fake-pi", harness_body);
    omt_harness::harness::upsert_config(
        &runtime,
        "pi",
        omt_harness::harness::HarnessUserConfig {
            enabled: true,
            binary: omt_harness::harness::BinarySource::Manual { path: fake.display().to_string() },
            extra_args: vec![],
            env: Default::default(),
        },
    )
    .unwrap();
    let agent = omt_exec::agents::register_agent(
        &runtime,
        "h_test",
        "pi",
        omt_exec::agents::NewExec {
            exec_id: "exec-e2e".into(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
            model: None,
        },
    )
    .unwrap();
    let spec = omt_exec::spec::LaunchSpec {
        schema_version: omt_exec::spec::SPEC_SCHEMA_VERSION,
        home_id: "h_test".into(),
        home_path,
        agent_id: agent.id.clone(),
        exec_id: "exec-e2e".into(),
        harness: "pi".into(),
        node_id: None,
        run_id: None,
        unattended: true,
        model: None,
        resume_session: None,
        prompt: "go".into(),
        max_duration_secs: 60,
        omt_path: omt(),
    };
    let dir = omt_exec::layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
    spec.write(&omt_exec::layout::exec_file(&dir, "exec-e2e", omt_exec::layout::ExecFile::Spec)).unwrap();
    Rig { _tmp: tmp, runtime, agent_id: agent.id }
}

fn shim(rig: &Rig) -> Command {
    let mut cmd = Command::new(omt());
    cmd.args(["--runtime-dir", rig.runtime.to_str().unwrap(), "exec-shim", "h_test", &rig.agent_id, "exec-e2e"]);
    cmd
}

fn agent(rig: &Rig) -> omt_exec::agents::AgentRecord {
    omt_exec::agents::get_agent(&rig.runtime, "h_test", &rig.agent_id).unwrap().unwrap()
}

#[test]
fn shim_binary_runs_exec_to_exit() {
    let rig = rig("echo hello\nexit 0");
    let status = shim(&rig).status().unwrap();
    assert!(status.success(), "{status:?}");
    let a = agent(&rig);
    assert_eq!(a.status, omt_exec::agents::AgentRecordStatus::Exited);
    assert_eq!(a.execs[0].exit_code, Some(0));
}

#[test]
fn sigterm_to_shim_kills_harness_as_user_stop() {
    let rig = rig("trap '' TERM\necho READY\nsleep 30");
    let mut child = shim(&rig).spawn().unwrap();
    // 等 harness 起来（exec.pid 落盘）
    let deadline = Instant::now() + Duration::from_secs(30);
    while agent(&rig).execs[0].pid.is_none() {
        assert!(Instant::now() < deadline, "harness 未在 30s 内拉起");
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let started = Instant::now();
    let status = child.wait().unwrap();
    assert!(status.success());
    // harness 忽略 TERM → 宽限后 KILL 升级（KILL_GRACE = 3s）
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    let a = agent(&rig);
    assert_eq!(a.status, omt_exec::agents::AgentRecordStatus::Killed);
    assert_eq!(a.execs[0].kill_reason.as_deref(), Some("user"));
}
```

`crates/omt-runtime/Cargo.toml` 的 `[dev-dependencies]` 加 `omt-harness = { path = "../omt-harness" }`；`libc` 已是普通依赖。

```bash
cargo test -p omt-runtime --test exec_shim 2>&1 | grep -E '^test result|FAILED|panicked'
```
Expected：2 passed。若 `kill_reason` 的实际取值不是 `"user"`，以 `KillReason::User.key()` 的返回值为准修改断言。

- [ ] **Step 8: 确认 daemon 二进制不链接 omt_exec**

```bash
cargo build -p omt-runtime --bins
nm -a target/debug/omt-daemon | grep -c 'omt_exec' ; echo "↑ 必须为 0"
nm -a target/debug/omt        | grep -c 'omt_exec' ; echo "↑ 应大于 0"
```

若 `omt-daemon` 非 0：说明 `src/main.rs` 经由某个模块引用到了 cli，需要把 `exec-shim` 分支的调用挪进只由 `cli_main.rs` 使用的函数里，再验证一遍。

- [ ] **Step 9: 提交**

```bash
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
cd apps/desktop && pnpm exec tsc --noEmit && cd ../..
git add -A crates apps/desktop/src/types.ts Cargo.lock
git commit -m "feat: launch spec and omt exec-shim (design §3.2, KD-E1)

The shim reads its spec, injects MCP, spawns the harness, writes events
to the agent-dir jsonl, finishes the exec (lease release via a temporary
admin grant) and exits; SIGTERM maps to a user stop. omt-daemon does not
link omt_exec.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: launcher 与控制面（拉起 / 就绪握手 / 停止 / 启动对账）

**Files:**
- Create: `crates/omt-exec/src/launch.rs`、`crates/omt-exec/src/control.rs`
- Modify: `crates/omt-exec/src/lib.rs`、`crates/omt-exec/src/executor.rs`（`DISPATCH_PROMPT`、`collect_members`、`compensate_new_run` 改为 `pub(crate)`，供 launch 复用）
- Create: `crates/omt-runtime/tests/exec_launch.rs`

**Interfaces:**
- Consumes: Task 4 的存储 API、Task 8 的 `LaunchSpec` 与 shim、`executor::process_start_identity`。
- Produces:
  - `omt_exec::launch::LaunchRequest<'a>`（字段见代码）、`omt_exec::launch::start(rpc: &dyn Fn(&str, Value) -> Result<Value, String>, req: &LaunchRequest) -> Result<ExecStartOutcome, String>`
  - `omt_exec::launch::spawn_shim(omt_bin, runtime_dir, home_id, agent_id, exec_id, log_path) -> Result<u32, String>`
  - `omt_exec::control::{find_exec, identity_alive, shim_alive, exec_active, stop_exec, stop_all, reconcile_on_startup, exec_id_live}`（签名见代码）

- [ ] **Step 1: 写 control 的失败测试**

`crates/omt-exec/src/control.rs` 先放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{register_agent, update_exec, NewExec};

    fn reg(runtime: &Path, exec_id: &str) -> String {
        register_agent(runtime, "h_t", "pi", NewExec {
            exec_id: exec_id.into(), node_id: None, run_id: Some("RUN-1".into()),
            binary_path: None, binary_args: vec![], model: None,
        })
        .unwrap()
        .id
    }

    fn backdate(runtime: &Path, agent_id: &str, exec_id: &str) {
        update_exec(runtime, "h_t", agent_id, exec_id, |e| e.started_at = "1000000000000".into()).unwrap();
    }

    #[test]
    fn startup_reconcile_marks_dead_shim_failed_and_keeps_live_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        // 存活 shim：用本测试进程自己的 pid 与起始标识冒充
        let live = reg(rt, "exec-live");
        let me = std::process::id();
        let my_start = crate::executor::process_start_identity(me).unwrap();
        update_exec(rt, "h_t", &live, "exec-live", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some(my_start.clone());
        })
        .unwrap();
        // 已死 shim：起始标识对不上
        let dead = reg(rt, "exec-dead");
        update_exec(rt, "h_t", &dead, "exec-dead", |e| {
            e.shim_pid = Some(me);
            e.shim_start_id = Some("macos:0:0".into());
        })
        .unwrap();
        backdate(rt, &dead, "exec-dead");

        assert_eq!(reconcile_on_startup(rt), 1);
        let a = crate::agents::get_agent(rt, "h_t", &live).unwrap().unwrap();
        assert_eq!(a.status, AgentRecordStatus::Starting);
        assert_eq!(a.attachment, AgentAttachment::Live);
        let d = crate::agents::get_agent(rt, "h_t", &dead).unwrap().unwrap();
        assert_eq!(d.status, AgentRecordStatus::Failed);
        assert_eq!(d.execs[0].failure_detail.as_deref(), Some(crate::agents::SHIM_LOST_MARKER));
        // 进入 U10 对账目标（释放租约）
        let targets = crate::agents::list_reconcile_targets(rt).unwrap();
        assert_eq!(targets.iter().map(|t| t.exec_id.as_str()).collect::<Vec<_>>(), vec!["exec-dead"]);
    }

    #[test]
    fn startup_reconcile_spares_fresh_launch_without_shim_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = reg(tmp.path(), "exec-fresh");
        assert_eq!(reconcile_on_startup(tmp.path()), 0);
        let a = crate::agents::get_agent(tmp.path(), "h_t", &fresh).unwrap().unwrap();
        assert_eq!(a.status, AgentRecordStatus::Starting, "可能有别的进程正在拉起，跳过");
    }

    #[test]
    fn stop_exec_is_idempotent_for_unknown_and_finished() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(stop_exec(tmp.path(), "exec-nope").is_ok());
        let id = reg(tmp.path(), "exec-done");
        crate::agents::update_agent_status(tmp.path(), "h_t", &id, AgentRecordStatus::Exited, AgentAttachment::None).unwrap();
        assert!(stop_exec(tmp.path(), "exec-done").is_ok());
    }

    #[test]
    fn stop_exec_signals_a_live_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let id = reg(tmp.path(), "exec-sig");
        let mut child = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let start = crate::executor::process_start_identity(pid).unwrap();
        update_exec(tmp.path(), "h_t", &id, "exec-sig", |e| {
            e.shim_pid = Some(pid);
            e.shim_start_id = Some(start.clone());
        })
        .unwrap();
        stop_exec(tmp.path(), "exec-sig").unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }
}
```

`lib.rs` 加 `pub mod control;`。

```bash
cargo test -p omt-exec control:: 2>&1 | tail -3
```
Expected：编译失败。

- [ ] **Step 2: 实现 control**

在测试模块之前加：

```rust
//! 执行面控制（设计 §3.2「控制」）：定位 exec、存活判定（pid + 内核起始
//! 标识，防复用）、停止（SIGTERM → shim）、启动对账（P-8，替代 demote）。

use std::path::Path;
use std::time::Duration;

use crate::agents::{self, AgentAttachment, AgentRecord, AgentRecordStatus, ExecRecord};
use crate::layout;

/// 还没记下 shim pid 的 exec 在注册后多久内视为「可能正在拉起」。
const LAUNCH_GRACE: Duration = Duration::from_secs(60);

pub struct ExecLocation {
    pub home_id: String,
    pub agent: AgentRecord,
    pub exec: ExecRecord,
}

/// 全部 home 中按 exec id 定位。
pub fn find_exec(runtime_dir: &Path, exec_id: &str) -> Result<Option<ExecLocation>, String> {
    for home_id in layout::home_ids(runtime_dir) {
        if let Some((agent, exec)) = agents::find_exec_in_home(runtime_dir, &home_id, exec_id)? {
            return Ok(Some(ExecLocation { home_id, agent, exec }));
        }
    }
    Ok(None)
}

/// pid 与内核起始标识都对得上才算同一个活进程。
pub fn identity_alive(pid: Option<u32>, start_id: Option<&str>) -> bool {
    match (pid, start_id) {
        (Some(pid), Some(expected)) => crate::executor::process_start_identity(pid)
            .map(|actual| actual == expected)
            .unwrap_or(false),
        _ => false,
    }
}

pub fn shim_alive(exec: &ExecRecord) -> bool {
    identity_alive(exec.shim_pid, exec.shim_start_id.as_deref())
}

fn is_active(agent: &AgentRecord) -> bool {
    matches!(agent.status, AgentRecordStatus::Starting | AgentRecordStatus::Running)
}

fn latest_exec(agent: &AgentRecord) -> Option<&ExecRecord> {
    let last = agent.last_exec_id.as_deref()?;
    agent.execs.iter().find(|e| e.exec_id == last)
}

fn within_launch_grace(exec: &ExecRecord) -> bool {
    let now = agents::now_epoch_ms().parse::<u128>().unwrap_or(0);
    let started = exec.started_at.parse::<u128>().unwrap_or(0);
    exec.shim_pid.is_none() && now.saturating_sub(started) < LAUNCH_GRACE.as_millis()
}

/// 尾随方用：exec 是否仍可能产出事件。
pub fn exec_active(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str) -> bool {
    match agents::get_agent(runtime_dir, home_id, agent_id) {
        Ok(Some(agent)) if is_active(&agent) => match latest_exec(&agent) {
            Some(exec) if exec.exec_id == exec_id => shim_alive(exec) || within_launch_grace(exec),
            _ => false,
        },
        _ => false,
    }
}

/// MCP 残留清扫用：launcher id（= exec id）对应的 exec 是否仍在运行。
pub fn exec_id_live(runtime_dir: &Path, exec_id: &str) -> bool {
    matches!(find_exec(runtime_dir, exec_id), Ok(Some(loc))
        if is_active(&loc.agent) && (shim_alive(&loc.exec) || within_launch_grace(&loc.exec)))
}

/// 停止（幂等）：未知 / 已终态 → Ok；shim 存活 → SIGTERM；shim 已死 → 对账收尾。
pub fn stop_exec(runtime_dir: &Path, exec_id: &str) -> Result<(), String> {
    let Some(loc) = find_exec(runtime_dir, exec_id)? else {
        return Ok(());
    };
    if !is_active(&loc.agent) || loc.agent.last_exec_id.as_deref() != Some(exec_id) {
        return Ok(());
    }
    if shim_alive(&loc.exec) {
        let pid = i32::try_from(loc.exec.shim_pid.unwrap_or(0)).map_err(|_| "shim pid 越界".to_string())?;
        // SAFETY: 起始标识刚校验过，pid 指向的仍是我们的 shim
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        if rc != 0 {
            return Err(format!("kill({pid}, SIGTERM): {}", std::io::Error::last_os_error()));
        }
        return Ok(());
    }
    reconcile_agent(runtime_dir, &loc.home_id, &loc.agent);
    Ok(())
}

/// 停止某 home 的全部活跃 exec，返回发出停止的数量（P-10「停止全部」）。
pub fn stop_all(runtime_dir: &Path, home_id: &str) -> Result<usize, String> {
    let (agents, _) = agents::list_agents(runtime_dir, home_id)?;
    let mut count = 0;
    for agent in agents.iter().filter(|a| is_active(a)) {
        if let Some(exec) = latest_exec(agent) {
            stop_exec(runtime_dir, &exec.exec_id)?;
            count += 1;
        }
    }
    Ok(count)
}

/// 单个活跃 agent 的对账（P-8）。返回是否改成了终态。
fn reconcile_agent(runtime_dir: &Path, home_id: &str, agent: &AgentRecord) -> bool {
    let Some(exec) = latest_exec(agent) else {
        return false;
    };
    if shim_alive(exec) {
        let _ = agents::update_agent_status(runtime_dir, home_id, &agent.id, agent.status, AgentAttachment::Live);
        return false;
    }
    if within_launch_grace(exec) {
        return false;
    }
    let harness_alive = identity_alive(exec.pid, exec.process_start_id.as_deref());
    let _ = agents::update_exec(runtime_dir, home_id, &agent.id, &exec.exec_id, |e| {
        if e.failure_detail.is_none() {
            e.failure_detail = Some(agents::SHIM_LOST_MARKER.to_string());
        }
    });
    let attachment = if harness_alive { AgentAttachment::Orphan } else { AgentAttachment::None };
    let _ = agents::update_agent_status(runtime_dir, home_id, &agent.id, AgentRecordStatus::Failed, attachment);
    true
}

/// 启动对账：替代 demote。返回改为终态的 agent 数。
pub fn reconcile_on_startup(runtime_dir: &Path) -> usize {
    let mut changed = 0;
    for home_id in layout::home_ids(runtime_dir) {
        let Ok((agents, _)) = agents::list_agents(runtime_dir, &home_id) else {
            continue;
        };
        for agent in agents.iter().filter(|a| is_active(a)) {
            if reconcile_agent(runtime_dir, &home_id, agent) {
                changed += 1;
            }
        }
    }
    changed
}
```

注意 `SHIM_LOST_MARKER` 在 Task 8 定义在 `agents.rs`；若当时放在了 executor，这里改引用路径。`update_agent_status` 只在状态或 attachment 变化时落盘，对存活 shim 重复调用无副作用。

```bash
cargo test -p omt-exec control:: 2>&1 | grep -E '^test result|FAILED'
```
Expected：4 passed。

- [ ] **Step 3: 实现 launch**

`crates/omt-exec/src/launch.rs`：

```rust
//! launcher（设计 §3.2「启动」+ 计划 P-3 / P-5）：发起方进程内执行派发链的
//! daemon 侧步骤（①–⑥，含补偿表），写 spec，以 setsid 拉起 `omt exec-shim`，
//! 等 shim 报告就绪后返回。发起方此后可以退出，执行继续。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use omt_harness::atomic_json::OpenOptionsUnix;
use omt_harness::harness;
use omt_harness::sanitize::sanitize_args;

use crate::agents::{self, AgentAttachment, AgentRecordStatus, NewExec};
use crate::executor::{compensate_new_run, collect_members, ExecStartOutcome, DISPATCH_PROMPT};
use crate::layout::{self, ExecFile};
use crate::spec::{LaunchSpec, SPEC_SCHEMA_VERSION};

/// 进程内串行化派发（与旧 DISPATCH_LOCK 同语义；跨进程由子项目 2 处理）。
static LAUNCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const READY_TIMEOUT: Duration = Duration::from_secs(30);

type Rpc<'a> = &'a dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String>;

pub struct LaunchRequest<'a> {
    pub runtime_dir: &'a Path,
    /// `omt` 二进制：既是 shim，也是注入给 harness 的 MCP 桥
    pub omt_bin: &'a Path,
    pub home_id: &'a str,
    /// `.omt` 目录
    pub home_path: &'a Path,
    pub node_id: &'a str,
    pub harness: &'a str,
    pub unattended: bool,
    pub model: Option<&'a str>,
    pub resume_session: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
    pub max_duration: Duration,
}

enum Ready {
    Spawned,
    Failed(String),
    Timeout,
}

#[allow(clippy::too_many_lines)]
pub fn start(rpc: Rpc<'_>, req: &LaunchRequest<'_>) -> Result<ExecStartOutcome, String> {
    if let Some(model) = req.model {
        harness::validate_model_id(model)?;
    }
    if let Some(session) = req.resume_session {
        harness::validate_model_id(session)?;
    }
    let _guard = LAUNCH_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (runtime_dir, home_id, node_id) = (req.runtime_dir, req.home_id, req.node_id);
    let dispatch_id = format!("d-{}-{}", agents::now_epoch_ms(), omt_harness::atomic_json::unique_suffix());

    // ① 预检
    let node_resp = rpc("node/get", serde_json::json!({"homeId": home_id, "nodeId": node_id}))?;
    let members = collect_members(rpc, home_id, node_id)?;
    if members.is_empty() {
        return Err("无未完成可执行成员".into());
    }
    let active_runs: Vec<String> = node_resp["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["runId"].as_str().map(str::to_owned))
        .collect();

    // ② node/execute（发起方是 desktop，adapter_only 可用；P-3）
    rpc("node/execute", serde_json::json!({
        "homeId": home_id, "nodeId": node_id, "commandId": format!("{dispatch_id}:execute"),
    }))?;

    // ③ ensure-run
    let (run_id, run_was_newly_created) = if let Some(existing) = active_runs.first() {
        (existing.clone(), false)
    } else {
        let resp = rpc("run/create", serde_json::json!({
            "homeId": home_id, "title": format!("dispatch {node_id}"),
            "nodeIds": members, "commandId": format!("{dispatch_id}:create"),
        }))?;
        let created = resp["run"]["runId"]
            .as_str()
            .ok_or("run/create 响应缺少 run.runId（daemon 契约漂移）")?
            .to_string();
        (created, true)
    };

    // ④ run/control start（仅 pending）
    let run_status = rpc("run/get", serde_json::json!({"homeId": home_id, "runId": run_id}))?["run"]["status"]
        .as_str()
        .unwrap_or("pending")
        .to_string();
    match run_status.as_str() {
        "pending" => {
            rpc("run/control", serde_json::json!({
                "homeId": home_id, "runId": run_id, "action": "start",
                "commandId": format!("{dispatch_id}:start"),
            }))?;
        }
        "running" => {}
        "paused" | "interrupted" => {
            return Err(format!("run {run_id} 是 {run_status}，需在弹窗中选择 resume/retry 或取消"));
        }
        other => return Err(format!("run {run_id} 状态 {other}，无法派发")),
    }

    // 启动参数（记录用；shim 会用同一输入重算，P-4）
    let plan = harness::resolve_launch(runtime_dir, req.harness)?;
    let prompt = DISPATCH_PROMPT
        .replace("{run_id}", &run_id)
        .replace("{home_id}", home_id)
        .replace("{tool_hint}", harness::descriptor_of(req.harness).map(|d| d.tool_hint).unwrap_or(""));
    let exec_id = format!("exec-{}", omt_harness::atomic_json::unique_suffix());
    let agent_id = match req.target_agent_id {
        Some(target) => target.to_string(),
        None => layout::new_agent_id(&exec_id),
    };
    let agent_dir = layout::agent_dir(runtime_dir, home_id, &agent_id)?;
    let prompt_file = harness::prompt_file_path(&agent_dir, &exec_id, plan.prompt_channel);
    let args = harness::compose_launch_args(
        &plan, req.unattended, &prompt, prompt_file.as_deref(), req.model, req.resume_session,
    );

    // ⑤ 注册 / 续接 agent
    if let Some(target) = req.target_agent_id {
        match agents::get_agent(runtime_dir, home_id, target)? {
            Some(agent) if agent.harness != req.harness => {
                return Err(format!("目标 agent 属于 {}，不能以 {} 续跑", agent.harness, req.harness));
            }
            Some(agent) if matches!(agent.status, AgentRecordStatus::Starting | AgentRecordStatus::Running) => {
                return Err(format!("目标 agent {target} 仍在运行，拒绝并发派发"));
            }
            Some(_) => {}
            None => return Err(format!("目标 agent {target} 不存在（可能已被清理）")),
        }
    }
    let new_exec = NewExec {
        exec_id: exec_id.clone(),
        node_id: Some(node_id.to_string()),
        run_id: Some(run_id.clone()),
        binary_path: Some(plan.program.display().to_string()),
        binary_args: sanitize_args(&args),
        model: req.model.map(str::to_string),
    };
    let registered = match req.target_agent_id {
        Some(target) => agents::append_exec(runtime_dir, home_id, target, req.harness, new_exec),
        None => agents::register_agent(runtime_dir, home_id, req.harness, new_exec),
    };
    let agent = match registered {
        Ok(agent) => agent,
        Err(err) => {
            compensate_new_run(rpc, home_id, &run_id, run_was_newly_created);
            return Err(err);
        }
    };

    // ⑥ 绑定（失败只警告）
    let mut warnings: Vec<String> = agents::write_binding(runtime_dir, home_id, node_id, &agent.id)
        .err()
        .map(|e| format!("绑定写入失败：{e}"))
        .into_iter()
        .collect();

    let fail_and_compensate = |err: String| -> String {
        let _ = agents::update_exec(runtime_dir, home_id, &agent.id, &exec_id, |e| {
            if e.failure_detail.is_none() {
                e.failure_detail = Some(err.clone());
            }
        });
        let _ = agents::update_agent_status(runtime_dir, home_id, &agent.id, AgentRecordStatus::Failed, AgentAttachment::None);
        compensate_new_run(rpc, home_id, &run_id, run_was_newly_created);
        err
    };

    // ⑦ spec 落盘
    let spec = LaunchSpec {
        schema_version: SPEC_SCHEMA_VERSION,
        home_id: home_id.to_string(),
        home_path: req.home_path.to_path_buf(),
        agent_id: agent.id.clone(),
        exec_id: exec_id.clone(),
        harness: req.harness.to_string(),
        node_id: Some(node_id.to_string()),
        run_id: Some(run_id.clone()),
        unattended: req.unattended,
        model: req.model.map(str::to_string),
        resume_session: req.resume_session.map(str::to_string),
        prompt,
        max_duration_secs: req.max_duration.as_secs(),
        omt_path: req.omt_bin.to_path_buf(),
    };
    if let Err(err) = spec.write(&layout::exec_file(&agent_dir, &exec_id, ExecFile::Spec)) {
        return Err(fail_and_compensate(err));
    }

    // ⑧ 拉起 shim 并记录其身份
    let shim_log = layout::exec_file(&agent_dir, &exec_id, ExecFile::ShimLog);
    let shim_pid = match spawn_shim(req.omt_bin, runtime_dir, home_id, &agent.id, &exec_id, &shim_log) {
        Ok(pid) => pid,
        Err(err) => return Err(fail_and_compensate(err)),
    };
    let shim_start_id = crate::executor::process_start_identity(shim_pid).ok();
    let _ = agents::update_exec(runtime_dir, home_id, &agent.id, &exec_id, |e| {
        e.shim_pid = Some(shim_pid);
        e.shim_start_id.clone_from(&shim_start_id);
    });

    // ⑨ 就绪握手（P-5）
    match wait_ready(runtime_dir, home_id, &agent.id, &exec_id, &shim_log) {
        Ready::Spawned => {}
        Ready::Failed(detail) => {
            compensate_new_run(rpc, home_id, &run_id, run_was_newly_created);
            return Err(detail);
        }
        Ready::Timeout => warnings.push(format!(
            "exec-shim {}s 内未拉起 harness，执行仍在进行——稍后在 Agents 面板查看",
            READY_TIMEOUT.as_secs()
        )),
    }

    Ok(ExecStartOutcome { exec_id, agent_id: agent.id, run_id, warnings })
}

fn wait_ready(runtime_dir: &Path, home_id: &str, agent_id: &str, exec_id: &str, shim_log: &Path) -> Ready {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Ok(Some(agent)) = agents::get_agent(runtime_dir, home_id, agent_id) {
            if let Some(exec) = agent.execs.iter().find(|e| e.exec_id == exec_id) {
                if exec.pid.is_some() {
                    return Ready::Spawned;
                }
                if agent.status == AgentRecordStatus::Failed {
                    return Ready::Failed(exec.failure_detail.clone().unwrap_or_else(|| "exec-shim 启动失败".into()));
                }
                if exec.shim_pid.is_some() && !crate::control::shim_alive(exec) {
                    return Ready::Failed(format!("exec-shim 意外退出，详见 {}", shim_log.display()));
                }
            }
        }
        if Instant::now() >= deadline {
            return Ready::Timeout;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 以新会话（setsid）拉起 shim；stdout/stderr 写 shim.log。发起方存活时由
/// 收割线程 wait，避免僵尸；发起方先退出则由 init/launchd 收割。
pub fn spawn_shim(
    omt_bin: &Path,
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    log_path: &Path,
) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode_if_unix(0o600)
        .open(log_path)
        .map_err(|e| format!("open shim log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("dup shim log: {e}"))?;
    let mut cmd = Command::new(omt_bin);
    cmd.arg("--runtime-dir")
        .arg(runtime_dir)
        .args(["exec-shim", home_id, agent_id, exec_id])
        .env("OMT_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // SAFETY: setsid 只在 fork 与 exec 之间调用
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn exec-shim: {e}"))?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}
```

`executor.rs` 中 `DISPATCH_PROMPT`、`collect_members`、`compensate_new_run` 改为 `pub(crate)`；`collect_members` 与 `compensate_new_run` 的第一个参数类型 `DaemonRpc<'_>` 与这里的 `Rpc<'_>` 是同一个 `&dyn Fn(...)`，无需转换。`lib.rs` 加 `pub mod launch;`。

- [ ] **Step 4: launcher 端到端测试（真 omt 二进制 + 假 daemon RPC）**

`crates/omt-runtime/tests/exec_launch.rs`：

```rust
//! launcher 端到端：假 daemon RPC（脚本化响应）+ 真 omt exec-shim + 假 harness。
//! 覆盖：就绪握手成功、MCP 注入失败时同步报错并补偿、发起方不等执行结束。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct FakeDaemon {
    calls: Mutex<Vec<String>>,
}

impl FakeDaemon {
    fn rpc(&self, method: &str, _params: serde_json::Value) -> Result<serde_json::Value, String> {
        self.calls.lock().unwrap().push(method.to_string());
        Ok(match method {
            "node/get" => serde_json::json!({"node": {"type": "ticket", "status": "todo"}, "children": [], "runs": []}),
            "run/create" => serde_json::json!({"run": {"runId": "RUN-1"}}),
            "run/get" => serde_json::json!({"run": {"status": "pending"}, "items": []}),
            _ => serde_json::json!({}),
        })
    }
}

fn setup(harness_body: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    let home_path = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&home_path).unwrap();
    let fake = write_script(tmp.path(), "fake-opencode", harness_body);
    omt_harness::harness::upsert_config(
        &runtime,
        "opencode",
        omt_harness::harness::HarnessUserConfig {
            enabled: true,
            binary: omt_harness::harness::BinarySource::Manual { path: fake.display().to_string() },
            extra_args: vec![],
            env: Default::default(),
        },
    )
    .unwrap();
    (tmp, runtime, home_path)
}

fn request<'a>(runtime: &'a Path, home_path: &'a Path, omt_bin: &'a Path) -> omt_exec::launch::LaunchRequest<'a> {
    omt_exec::launch::LaunchRequest {
        runtime_dir: runtime,
        omt_bin,
        home_id: "h_test",
        home_path,
        node_id: "TICKET-0001",
        harness: "opencode",
        unattended: true,
        model: None,
        resume_session: None,
        target_agent_id: None,
        max_duration: Duration::from_secs(60),
    }
}

#[test]
fn launch_returns_once_harness_is_spawned_and_exec_finishes_later() {
    // harness 跑 5s：launcher 在 pid 落盘后就返回，此刻 exec 必然尚未结束。
    // 不断言耗时——macOS 首次 exec 新写入脚本可能停顿数秒（F-BENCH-22）。
    let (_t, runtime, home_path) = setup("sleep 5\necho done\nexit 0");
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    let fake = FakeDaemon { calls: Mutex::new(vec![]) };
    let outcome = omt_exec::launch::start(
        &|m: &str, p: serde_json::Value| fake.rpc(m, p),
        &request(&runtime, &home_path, &omt),
    )
    .unwrap();
    assert_eq!(outcome.run_id, "RUN-1");
    let at_return = omt_exec::agents::get_agent(&runtime, "h_test", &outcome.agent_id).unwrap().unwrap();
    assert_eq!(
        at_return.status,
        omt_exec::agents::AgentRecordStatus::Starting,
        "launcher 不应等执行结束"
    );
    assert!(at_return.execs[0].pid.is_some(), "就绪 = harness 已 spawn");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let agent = omt_exec::agents::get_agent(&runtime, "h_test", &outcome.agent_id).unwrap().unwrap();
        if agent.status == omt_exec::agents::AgentRecordStatus::Exited {
            break;
        }
        assert!(Instant::now() < deadline, "exec 未在 40s 内结束：{agent:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
    // 收尾时 shim 会为释放 RUN-1 的租约拉起临时 runtime 里的真 daemon
    //（假 run 不存在 → lease_release_pending）；测试结束前停掉它
    let _ = std::process::Command::new(&omt)
        .args(["--runtime-dir", runtime.to_str().unwrap(), "daemon-stop"])
        .status();
}

#[test]
fn missing_mcp_bridge_fails_synchronously_and_cancels_new_run() {
    let (_t, runtime, home_path) = setup("exit 0");
    // omt_bin 用真 omt 作 shim，但 spec 里的 MCP 桥路径不存在 → shim 启动阶段失败
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    let fake = FakeDaemon { calls: Mutex::new(vec![]) };
    let mut req = request(&runtime, &home_path, &omt);
    let bogus = PathBuf::from("/nonexistent/omt");
    req.omt_bin = &bogus;
    let err = omt_exec::launch::start(&|m: &str, p: serde_json::Value| fake.rpc(m, p), &req).unwrap_err();
    assert!(err.contains("exec-shim") || err.contains("omt"), "{err}");
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.last().map(String::as_str), Some("run/control"), "应 cancel 新建 run：{calls:?}");
}
```

第二个测试里 `omt_bin` 不存在，`spawn_shim` 直接失败，走 `fail_and_compensate`。（「shim 启动后在 MCP 注入阶段失败」的路径由 Task 8 的 `shim_marks_failed_when_spec_missing` 与 `wait_ready` 的 `Failed` 分支共同覆盖。）

```bash
cargo test -p omt-runtime --test exec_launch 2>&1 | grep -E '^test result|FAILED|panicked'
```
Expected：2 passed。若 opencode 的 `compose_launch_args` 需要 `prompt_channel` 以外的配置项，按 `resolve_launch` 的报错补齐 `HarnessUserConfig`。

- [ ] **Step 5: 提交**

```bash
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
git add -A crates
git commit -m "feat: launcher with ready handshake and exec control plane

launch::start runs dispatch steps ①–⑥ in the caller, writes the spec,
spawns a setsid'd exec-shim and returns once the harness is up (sync
errors and run compensation preserved). control provides identity-
checked stop/stop-all and startup reconcile replacing demote (P-8).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: 桌面端改走 launcher（关闭桌面端后执行继续）

**Files:**
- Create: `crates/omt-exec/src/follow.rs`
- Modify: `crates/omt-exec/src/lib.rs`、`crates/omt-exec/src/executor.rs`（`startup_housekeeping` 改为启动对账 + grants 清扫 + live-aware 清扫）
- Modify: `apps/desktop/src-tauri/src/{executor,lib,state,rpc_bridge}.rs`

**Interfaces:**
- Consumes: `launch::{start, LaunchRequest}`、`control::{stop_exec, stop_all, exec_active, exec_id_live, reconcile_on_startup}`、`grants::sweep_dead_temp_grants`。
- Produces:
  - `omt_exec::follow::follow_jsonl(path: PathBuf, after_seq: Option<u64>, sink: EventSink, active: Arc<dyn Fn() -> bool + Send + Sync>) -> std::thread::JoinHandle<()>`
  - tauri commands：`exec_start`（签名不变）、`exec_stop`（签名不变）、`exec_transcript_tail`（不变）、`exec_kill_orphan`（不变）、新增 `exec_stop_all(home_id: String) -> Result<usize, String>`、`exec_attach(home_id: String, exec_id: String, after_seq: Option<u64>, channel: Channel<Value>) -> Result<(), String>`

- [ ] **Step 1: follow 的失败测试**

`crates/omt-exec/src/follow.rs` 先放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    fn collector() -> (EventSink, Arc<Mutex<Vec<serde_json::Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        (Arc::new(move |v| s.lock().unwrap().push(v)), seen)
    }

    #[test]
    fn streams_lines_written_after_start_and_stops_at_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"session_header\"}\n{\"kind\":\"session_start\",\"seq\":0}\n").unwrap();
        let (sink, seen) = collector();
        let handle = follow_jsonl(path.clone(), None, sink, Arc::new(|| true));
        std::thread::sleep(Duration::from_millis(300));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        // 半行先写、再补全：不得把半行当事件
        f.write_all(b"{\"kind\":\"raw\",\"seq\":1,").unwrap();
        f.flush().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        f.write_all(b"\"text\":\"hi\"}\n{\"kind\":\"exit\",\"seq\":2}\n").unwrap();
        handle.join().unwrap();
        let kinds: Vec<String> = seen.lock().unwrap().iter().map(|v| v["kind"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, vec!["session_start", "raw", "exit"]);
    }

    #[test]
    fn skips_seq_up_to_after_seq() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"raw\",\"seq\":0}\n{\"kind\":\"raw\",\"seq\":1}\n{\"kind\":\"exit\",\"seq\":2}\n").unwrap();
        let (sink, seen) = collector();
        follow_jsonl(path, Some(0), sink, Arc::new(|| true)).join().unwrap();
        let seqs: Vec<u64> = seen.lock().unwrap().iter().map(|v| v["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn gives_up_when_exec_inactive_and_file_stops_growing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"raw\",\"seq\":0}\n").unwrap();
        let (sink, seen) = collector();
        let started = std::time::Instant::now();
        follow_jsonl(path, None, sink, Arc::new(|| false)).join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
```

`lib.rs` 加 `pub mod follow;`。

```bash
cargo test -p omt-exec follow 2>&1 | tail -3
```
Expected：编译失败。

- [ ] **Step 2: 实现 follow**

在测试模块之前加：

```rust
//! jsonl 尾随（设计 §3.4 / KD-E3，P-6 纯轮询）：从文件读出新增的完整行，
//! 按原样推给 sink（事件形状与旧 Channel 推送一致），读到 exit 即结束；
//! exec 已不活跃且文件连续两次不增长时也结束（shim 崩溃的兜底）。

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::executor::EventSink;

const POLL: Duration = Duration::from_millis(200);
/// 文件尚不存在时最多等多久（shim 还没写出 header）。
const APPEAR_TIMEOUT: Duration = Duration::from_secs(30);

pub fn follow_jsonl(
    path: PathBuf,
    after_seq: Option<u64>,
    sink: EventSink,
    active: Arc<dyn Fn() -> bool + Send + Sync>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut offset: u64 = 0;
        let mut pending: Vec<u8> = Vec::new();
        let mut idle_inactive = 0u32;
        let appear_deadline = std::time::Instant::now() + APPEAR_TIMEOUT;
        loop {
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
                Err(_) if std::time::Instant::now() < appear_deadline => {}
                Err(_) => return,
            }
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let Ok(event) = serde_json::from_slice::<serde_json::Value>(&line) else {
                    continue;
                };
                if event["kind"] == "session_header" {
                    continue;
                }
                if let (Some(after), Some(seq)) = (after_seq, event["seq"].as_u64()) {
                    if seq <= after {
                        continue;
                    }
                }
                let is_exit = event["kind"] == "exit";
                sink(event);
                if is_exit {
                    return;
                }
            }
            if grew {
                idle_inactive = 0;
            } else if !active() {
                idle_inactive += 1;
                if idle_inactive >= 2 {
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    })
}
```

```bash
cargo test -p omt-exec follow 2>&1 | grep -E '^test result'
```
Expected：3 passed。

- [ ] **Step 3: 启动清扫改为对账语义**

`crates/omt-exec/src/executor.rs` 的 `startup_housekeeping` 整体替换为：

```rust
/// 任一面启动时的一次性清扫（P-8 / P-9 / P-1）：迁移旧存储 → 启动对账
/// （shim 存活的保持受控，已死的标 Failed）→ 清扫临时授权残留 → 清扫
/// MCP 注入残留（跳过仍在运行的 exec）。
pub fn startup_housekeeping(runtime_dir: &Path) {
    match crate::migrate::migrate_legacy(runtime_dir) {
        Ok(r) if r.homes > 0 || !r.skipped_corrupt.is_empty() => eprintln!("存储迁移：{r:?}"),
        Err(err) => eprintln!("存储迁移失败：{err}（下次启动重试）"),
        _ => {}
    }
    let lost = crate::control::reconcile_on_startup(runtime_dir);
    if lost > 0 {
        eprintln!("启动对账：{lost} 个 exec 的 shim 已不在，标记为失败");
    }
    if let Err(err) = crate::grants::sweep_dead_temp_grants(runtime_dir) {
        eprintln!("临时授权清扫失败：{err}");
    }
    let live = |id: &str| crate::control::exec_id_live(runtime_dir, id);
    match crate::mcp_injection::sweep_crash_residue(runtime_dir, &live) {
        Ok(n) if n > 0 => eprintln!("启动清扫：{n} 个 workspace 的 MCP 注入残留已恢复"),
        Err(err) => eprintln!("MCP 注入残留清扫失败：{err}"),
        _ => {}
    }
}
```

`agents::demote_live_agents` 已无调用方：删除它及其测试（`cargo build` 会报 dead_code）。`RESTART_MARKER` 保留，仅用于识别旧记录（`list_reconcile_targets`）。

- [ ] **Step 4: 桌面端 command 改写**

`apps/desktop/src-tauri/src/executor.rs` 中，`exec_start` 与 `exec_stop` 整体替换，并新增 `exec_stop_all`、`exec_attach` 与私有 helper `follow`：

```rust
use omt_exec::layout::{self, ExecFile};

/// 尾随 `<agentDir>/<execId>.jsonl` 推送前端（KD-E3：取代进程内 Channel 泵）。
fn follow(home_id: &str, agent_id: &str, exec_id: &str, after_seq: Option<u64>, sink: EventSink) -> Result<(), String> {
    let runtime_dir = daemon::runtime_dir();
    let dir = layout::agent_dir(&runtime_dir, home_id, agent_id)?;
    let path = layout::exec_file(&dir, exec_id, ExecFile::Jsonl);
    let (h, a, e) = (home_id.to_string(), agent_id.to_string(), exec_id.to_string());
    let active = Arc::new(move || omt_exec::control::exec_active(&runtime_dir, &h, &a, &e));
    omt_exec::follow::follow_jsonl(path, after_seq, sink, active);
    Ok(())
}

fn channel_sink(channel: tauri::ipc::Channel<serde_json::Value>) -> EventSink {
    Arc::new(move |value| {
        let _ = channel.send(value);
    })
}

/// U6 (R-B)：exec_start。派发链 ①–⑥ 在本进程，执行在分离的 exec-shim 里；
/// 前端 Channel 由 jsonl 尾随喂入（事件形状不变）。
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub fn exec_start(
    shared: tauri::State<'_, SharedSession>,
    home_id: String,
    home_path: String,
    node_id: String,
    harness: String,
    unattended: bool,
    model: Option<String>,
    resume_session: Option<String>,
    agent_id: Option<String>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<ExecStartOutcome, String> {
    let rpc = rpc_of(&shared);
    let omt_bin = daemon::omt_sidecar_path()?;
    let runtime_dir = daemon::runtime_dir();
    let outcome = omt_exec::launch::start(
        &|method: &str, params: serde_json::Value| rpc(method, params),
        &omt_exec::launch::LaunchRequest {
            runtime_dir: &runtime_dir,
            omt_bin: &omt_bin,
            home_id: &home_id,
            home_path: Path::new(&home_path),
            node_id: &node_id,
            harness: &harness,
            unattended,
            model: model.as_deref(),
            resume_session: resume_session.as_deref(),
            target_agent_id: agent_id.as_deref(),
            max_duration: omt_exec::executor::DEFAULT_MAX_DURATION,
        },
    )?;
    follow(&home_id, &outcome.agent_id, &outcome.exec_id, None, channel_sink(channel))?;
    Ok(outcome)
}

/// 幂等终止：SIGTERM 发给该 exec 的 shim（身份校验后）。
#[tauri::command(async)]
pub fn exec_stop(exec_id: String) -> Result<(), String> {
    omt_exec::control::stop_exec(&daemon::runtime_dir(), &exec_id)
}

/// P-10：停止当前 home 的全部运行中 exec（替代旧的「桌面退出即终止」）。
#[tauri::command(async)]
pub fn exec_stop_all(home_id: String) -> Result<usize, String> {
    omt_exec::control::stop_all(&daemon::runtime_dir(), &home_id)
}

/// P-10：重新接上仍在运行的 exec 的实时流（桌面重启后 shim 仍在跑）。
/// `after_seq` = 前端已有的最后一个 seq（无则 None，从头推）。
#[tauri::command(async)]
pub fn exec_attach(
    home_id: String,
    exec_id: String,
    after_seq: Option<u64>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    let (agent, _) = omt_exec::agents::find_exec_in_home(&daemon::runtime_dir(), &home_id, &exec_id)?
        .ok_or_else(|| format!("exec {exec_id} 不存在"))?;
    follow(&home_id, &agent.id, &exec_id, after_seq, channel_sink(channel))
}
```

删除文件中对 `crate::state::ExecutionState` 与 `omt_exec::executor::exec_start_impl` 的引用。

- [ ] **Step 5: lib.rs / state.rs / rpc_bridge.rs**

`apps/desktop/src-tauri/src/state.rs`：删除 `ExecutionState` 结构体、它的两个 `impl` 以及文件头里关于它的注释。

`apps/desktop/src-tauri/src/lib.rs`：
1. `use state::{ExecutionState, SharedSession};` 改为 `use state::SharedSession;`；
2. 删除 `.manage(ExecutionState::new())`，在 `let builder = tauri::Builder::default();` 之前加：

```rust
    // 执行面启动清扫（迁移 / 启动对账 / 临时授权与 MCP 残留）——放在任何
    // command 可被调用之前；对账只改记录，不发信号。
    omt_exec::executor::startup_housekeeping(&daemon::runtime_dir());
```

3. `invoke_handler` 列表在 `executor::exec_kill_orphan,` 后加 `executor::exec_stop_all,` 与 `executor::exec_attach,`；
4. `.run(|app, event| { … })` 中整个 `ExitRequested` 分支删除，改为 `.run(|_app, _event| {})`。在 `run()` 函数上方的模块注释里加一句：「设计 §3.4：桌面退出不再终止执行（KD-D7 已被 KD-E1 修订），停止改由 Agents 面板『停止全部』。」

`rpc_bridge.rs` 的 U10 对账保持 Task 2 的写法（它负责释放 `SHIM_LOST_MARKER` / `lease_release_pending` 的租约）。

- [ ] **Step 6: 编译并跑全量**

```bash
cargo build --workspace --all-targets 2>&1 | grep -E '^(error|warning)' -A5 | head -40
cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; echo "(应为空)"
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check
cd apps/desktop && pnpm test && pnpm exec tsc --noEmit
```

Expected：Rust 全绿。`omt-exec` 里原来驱动 `ExecutionManager` 的进程内测试仍然有效（shim 复用的就是它）；只为 `exec_start_impl` / `dispatch_chain` 写的测试，若其覆盖面已被 `launch.rs` 与 `tests/exec_launch.rs` 取代，就连同 `exec_start_impl` 与 `dispatch_chain` 一起删除，并在提交信息里列出删除的测试名。桌面 vitest 131 全绿、无改动。

- [ ] **Step 7: 手工冒烟（真 harness）**

```bash
cd apps/desktop && pnpm tauri:dev
```
1. 派发一个 ticket 给 pi：弹窗无报错，Agents 面板实时显示 transcript；
2. 派发后**退出桌面端**（⌘Q），在终端执行 `ps -ef | grep 'exec-shim' | grep -v grep`：shim 仍在；
3. 等执行结束后重开桌面端：该 agent 为 `exited`，transcript 完整（历史回载），run 进度正确；
4. 再派发一个长任务，执行中退出并立刻重开：该 agent 显示「运行中」并可停止；点停止后显示 `killed`。

把结果记进 Task 12 的验收清单。

- [ ] **Step 8: 提交**

```bash
git add -A crates/omt-exec apps/desktop/src-tauri
git commit -m "feat: desktop dispatches through exec-shim; quitting no longer kills execs

exec_start launches via omt_exec::launch and feeds the Channel by
tailing the agent-dir jsonl; exec_stop signals the shim. New
exec_stop_all / exec_attach commands. Startup runs migration and
shim-aware reconcile instead of demote; the ExitRequested kill_all hook
is removed (design §3.4, KD-D7 superseded).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: 前端补齐「停止全部」与重启后接回实时流（P-10）

**Files:**
- Modify: `apps/desktop/src/bridge.ts`、`apps/desktop/src/store.ts`、`apps/desktop/src/AgentsPanel.tsx`、`apps/desktop/src/App.tsx`
- Create: `apps/desktop/tests/exec-plane.spec.tsx`

**Interfaces:**
- Consumes: Task 10 的 `exec_stop_all`、`exec_attach` command。
- Produces:
  - `bridge.ts`：`execStopAll(homeId: string): Promise<number>`、`execAttach(options: { homeId: string; execId: string; afterSeq: number | null; channel: Channel<TranscriptEvent> }): Promise<void>`
  - `store.ts` 返回对象新增：`attachTranscript(execId: string, homeId: string): void`、`stopAll(): Promise<void>`
  - `AgentsPanel` 新增**可选** props：`attachTranscript?: (execId: string, homeId: string) => void`、`onStopAll?: () => void`

- [ ] **Step 1: 写失败测试**

`apps/desktop/tests/exec-plane.spec.tsx`：

```tsx
/**
 * 执行面子项目 1（P-10）：Agents 面板的「停止全部」与重启后接回实时流。
 * 只测新增的可选 props；既有 agents.spec 不改。
 */
import { describe, expect, it, vi, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { AgentRecord } from '../src/types'

afterEach(cleanup)

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: ((m: unknown) => void) | null = null
  },
}))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }))
vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn().mockResolvedValue(false),
  requestPermission: vi.fn(),
  sendNotification: vi.fn(),
}))

import { AgentsPanel } from '../src/AgentsPanel'

function agent(over: Partial<AgentRecord> = {}): AgentRecord {
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
        runId: null,
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
      fetchRun={vi.fn().mockResolvedValue({ run: {}, items: [] })}
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

describe('exec plane P-10', () => {
  it('re-attaches a live agent that has no events in this session', () => {
    const attach = vi.fn()
    const history = vi.fn()
    renderPanel({ attachTranscript: attach, loadHistoryTail: history })
    expect(attach).toHaveBeenCalledWith('exec-1', 'h_test')
    expect(history).not.toHaveBeenCalled()
  })

  it('does not attach a finished agent (history tail path instead)', () => {
    const attach = vi.fn()
    const history = vi.fn()
    renderPanel({
      agents: [agent({ status: 'exited', attachment: 'none' })],
      attachTranscript: attach,
      loadHistoryTail: history,
    })
    expect(attach).not.toHaveBeenCalled()
    expect(history).toHaveBeenCalledWith('exec-1', 'h_test')
  })

  it('shows 停止全部 only when something is running and wires the callback', () => {
    const stopAll = vi.fn()
    renderPanel({ onStopAll: stopAll })
    fireEvent.click(screen.getByRole('button', { name: '停止全部' }))
    expect(stopAll).toHaveBeenCalledTimes(1)
    cleanup()
    renderPanel({ agents: [agent({ status: 'exited', attachment: 'none' })], onStopAll: stopAll })
    expect(screen.queryByRole('button', { name: '停止全部' })).toBeNull()
  })

  it('renders without the new props (existing callers unaffected)', () => {
    renderPanel()
    expect(screen.queryByRole('button', { name: '停止全部' })).toBeNull()
  })
})
```

（若 `agents.spec.tsx` 里 mock 的通知插件路径或 `fetchRun` 的返回形状与这里不同，按它改，保证新文件与既有 mock 边界一致。）

```bash
cd apps/desktop && pnpm vitest run tests/exec-plane.spec.tsx 2>&1 | tail -15
```
Expected：前 3 个用例失败（`attachTranscript` 未被调用；找不到「停止全部」按钮），第 4 个通过。

- [ ] **Step 2: 实现 bridge**

`apps/desktop/src/bridge.ts` 在 `execKillOrphan` 之后加：

```ts
/** P-10：停止当前 home 的全部运行中 exec（设计 §3.4：替代「桌面退出即终止」）。
 * 返回发出停止信号的数量。 */
export async function execStopAll(homeId: string): Promise<number> {
  return invoke<number>('exec_stop_all', { homeId })
}

/** P-10：重新接上仍在运行的 exec 的实时 transcript（桌面重启后 shim 仍在跑）。
 * `afterSeq` = 已有的最后一个事件 seq（无则 null，从头推）。 */
export async function execAttach(options: {
  homeId: string
  execId: string
  afterSeq: number | null
  channel: Channel<TranscriptEvent>
}): Promise<void> {
  return invoke<void>('exec_attach', {
    homeId: options.homeId,
    execId: options.execId,
    afterSeq: options.afterSeq,
    channel: options.channel,
  })
}
```

- [ ] **Step 3: 实现 store**

`apps/desktop/src/store.ts`：
1. 从 `./bridge` 的 import 列表加 `execAttach`、`execStopAll`；
2. 在 `transcriptsRef` 声明旁加：

```ts
  // P-10：本会话已接上实时流的 execId（startDispatch 的 Channel 也算）——防重复 attach
  const attachedRef = useRef<Set<string>>(new Set())
```

3. `startDispatch` 里 `const outcome = await execStart({...})` 之后加 `attachedRef.current.add(outcome.execId)`；
4. 在 `stopAgent` 之后加：

```ts
  /** P-10：重启后接回仍在运行的 exec。afterSeq 取本窗口最后一个事件的 seq。 */
  const attachTranscript = useCallback(
    (execId: string, homeId: string) => {
      if (attachedRef.current.has(execId)) return
      attachedRef.current.add(execId)
      const channel = new Channel<TranscriptEvent>()
      channel.onmessage = ev => ingestTranscriptEvent(ev)
      const existing = transcriptsRef.current.get(execId)
      const last = existing && existing.length > 0 ? existing[existing.length - 1] : undefined
      const afterSeq = (last as { seq?: number } | undefined)?.seq ?? null
      void execAttach({ homeId, execId, afterSeq, channel }).catch(error => {
        attachedRef.current.delete(execId)
        setState(s => ({ ...s, error: presentError(error) }))
      })
    },
    [ingestTranscriptEvent],
  )

  /** P-10：停止当前 home 的全部运行中 exec。 */
  const stopAll = useCallback(async () => {
    const home = activeHomeRef.current
    if (!home) return
    try {
      await execStopAll(home.homeId)
      scheduleAgentsRepull()
    } catch (error) {
      setState(s => ({ ...s, error: presentError(error) }))
    }
  }, [scheduleAgentsRepull])
```

5. store 返回对象（`loadHistoryTail,` 所在的那个对象字面量）加 `attachTranscript,` 与 `stopAll,`。

- [ ] **Step 4: 实现 AgentsPanel 与 App 接线**

`apps/desktop/src/AgentsPanel.tsx`：
1. `Props` 在 `loadHistoryTail` 之后加：

```ts
  /** P-10：选中仍在运行、但本会话无事件的 agent 时接回实时流（桌面重启后） */
  attachTranscript?: (execId: string, homeId: string) => void
  /** P-10：停止全部运行中 exec（替代桌面退出即终止） */
  onStopAll?: () => void
```

2. 解构参数加 `attachTranscript,` 与 `onStopAll,`；
3. F-PI-3 的 effect 中 `if (live) return` 改为：

```ts
    if (live) {
      // P-10：本会话没有事件 = 重启后的存活 exec，接回实时流
      if (eventsOf(exec.execId).length === 0) attachTranscript?.(exec.execId, homeId)
      return
    }
```

4. 主布局 `agents-list` 里 `{gcBar}` 之后加：

```tsx
        {onStopAll && groups.active.length > 0 && (
          <button type="button" className="agents-stop-all" onClick={onStopAll}>
            停止全部
          </button>
        )}
```

`apps/desktop/src/App.tsx` 的 `<AgentsPanel …>` 在 `loadHistoryTail={store.loadHistoryTail}` 之后加：

```tsx
                    attachTranscript={store.attachTranscript}
                    onStopAll={() => void store.stopAll()}
```

`apps/desktop/src/styles.css` 末尾加：

```css
.agents-stop-all {
  margin: 4px 8px;
  align-self: flex-start;
}
```

- [ ] **Step 5: 跑测试**

```bash
cd apps/desktop && pnpm vitest run tests/exec-plane.spec.tsx && pnpm test && pnpm exec tsc --noEmit && pnpm build
```
Expected：新文件 4 passed；全量 131 + 4 = 135 passed；既有测试文件无改动（`git diff --stat apps/desktop/tests` 只有新文件）。

- [ ] **Step 6: 提交**

```bash
git add apps/desktop/src apps/desktop/tests/exec-plane.spec.tsx
git commit -m "feat(desktop): stop-all button and live re-attach after restart (plan P-10)

Both wired as optional AgentsPanel props, so existing callers and tests
are untouched.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: 验收、实测项与文档

**Files:**
- Create: `crates/omt-runtime/tests/exec_survives_launcher.rs`
- Modify: `CLAUDE.md`（Desktop execution plane 一节、crate 列表）、`docs/plans/2026-10-20-0802-feat-omt-exec-plane-orchestration-design.md`（status 与 §8）、`docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md`（F-U4a-3/4/5、F-U2-11 状态）

- [ ] **Step 1: 「发起方退出后执行继续」的自动化测试**

`crates/omt-runtime/tests/exec_survives_launcher.rs`：

```rust
//! 设计 §6「发起方退出后执行继续」：launcher 所在进程被 KILL 后，shim 仍
//! 把 exec 跑完并收尾。用一个子进程充当发起方（调用 omt exec-shim 后立即被杀）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn exec_completes_after_launcher_is_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    let home_path = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&home_path).unwrap();
    let fake = write_script(tmp.path(), "fake-pi", "sleep 2\necho done\nexit 0");
    omt_harness::harness::upsert_config(
        &runtime,
        "pi",
        omt_harness::harness::HarnessUserConfig {
            enabled: true,
            binary: omt_harness::harness::BinarySource::Manual { path: fake.display().to_string() },
            extra_args: vec![],
            env: Default::default(),
        },
    )
    .unwrap();
    let agent = omt_exec::agents::register_agent(
        &runtime,
        "h_test",
        "pi",
        omt_exec::agents::NewExec {
            exec_id: "exec-surv".into(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
            model: None,
        },
    )
    .unwrap();
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    let dir = omt_exec::layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
    omt_exec::spec::LaunchSpec {
        schema_version: omt_exec::spec::SPEC_SCHEMA_VERSION,
        home_id: "h_test".into(),
        home_path,
        agent_id: agent.id.clone(),
        exec_id: "exec-surv".into(),
        harness: "pi".into(),
        node_id: None,
        run_id: None,
        unattended: true,
        model: None,
        resume_session: None,
        prompt: "go".into(),
        max_duration_secs: 60,
        omt_path: omt.clone(),
    }
    .write(&omt_exec::layout::exec_file(&dir, "exec-surv", omt_exec::layout::ExecFile::Spec))
    .unwrap();

    // 发起方：一个 sh，后台拉起 shim 后挂住，随即被 KILL（只杀发起方单个 pid，
    // 与桌面端 ⌘Q 的语义一致）。macOS 无 setsid 命令；生产路径的 setsid 由
    // launch::spawn_shim 在 pre_exec 里做，Task 9 已覆盖
    let launcher_script = format!(
        "{omt} --runtime-dir {rt} exec-shim h_test {agent} exec-surv </dev/null >/dev/null 2>&1 &\nsleep 30",
        omt = omt.display(),
        rt = runtime.display(),
        agent = agent.id,
    );
    let mut launcher = std::process::Command::new("/bin/sh").arg("-c").arg(&launcher_script).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    launcher.kill().unwrap();
    let _ = launcher.wait();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let a = omt_exec::agents::get_agent(&runtime, "h_test", &agent.id).unwrap().unwrap();
        if a.status == omt_exec::agents::AgentRecordStatus::Exited {
            assert_eq!(a.execs[0].exit_code, Some(0));
            break;
        }
        assert!(Instant::now() < deadline, "发起方被杀后 exec 未完成：{a:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
```

```bash
cargo test -p omt-runtime --test exec_survives_launcher 2>&1 | grep -E '^test result|FAILED'
```
Expected：1 passed。

- [ ] **Step 2: 全量验证（三遍，默认线程数）**

```bash
for i in 1 2 3; do cargo test --workspace 2>&1 | grep -E '^test result: FAILED|panicked' ; done ; echo "(三遍都应为空)"
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check
cargo build -p omt-runtime --bins && nm -a target/debug/omt-daemon | grep -c omt_exec   # 必须为 0
cd apps/desktop && pnpm test && pnpm exec tsc --noEmit && pnpm build
```

- [ ] **Step 3: 手工验收清单（设计 §6 桌面端一节）**

在 `pnpm tauri:dev` 下逐项执行，并把结果（通过 / 失败 + 现象）写进 findings log 的新小节「执行面子项目 1 验收」：

| # | 场景 | 期望 |
|---|---|---|
| 1 | 派发 pi / omp / opencode 各一次 | 实时 transcript 与改造前一致；结束后 run 进度正确 |
| 2 | 执行中点「停止」 | 3s 内 `killed`，run item 回到 pending（租约已释放） |
| 3 | 空闲 agent「续跑」 | 同一 agent 追加 exec，新 jsonl 在同一目录 |
| 4 | 进入信任门（awaiting_confirmation）后在桌面端裁决 | 与改造前一致 |
| 5 | 执行中 ⌘Q 退出，等执行结束再打开 | agent `exited`，transcript 完整，run 进度正确 |
| 6 | 执行中 ⌘Q 退出后立刻打开 | agent「运行中」，实时流接回（P-10），可停止 |
| 7 | 执行中 `kill -9 <shim pid>`，再打开桌面端 | agent `failed`（shim lost），租约被对账释放；harness 若仍活则显示为不受控、可强制终止 |
| 8 | 「停止全部」 | 所有运行中 exec 进入 `killed` |
| 9 | 旧版本留下的 `agents.json` 数据 | 首次启动迁移，Agents 面板列表与历史 transcript 完整 |

- [ ] **Step 4: 实测项 §8「resume 后的 usage 语义」**

对 pi 与 omp 各做一次「派发 → 结束 → 续跑」，比较两个 exec 的 `usageTokens` 与各自 jsonl 里 usage 事件的 tokens 序列：续跑 exec 的首条 usage 若从小值开始，说明逐消息计、与前一 exec 独立（agent 级用量 = 各 exec 之和）；若首条就接近前一 exec 的末值，说明延续了会话累计。把结论写进设计文档 §8 对应条目（改为「已实测：…」）。

- [ ] **Step 5: 文档**

1. `CLAUDE.md`：
   - 「What this is」的 crate 列表补 `omt-contracts`（此前遗漏）、`omt-harness`、`omt-exec`；
   - 「Desktop execution plane」一节改为：执行面内核在 `crates/omt-harness`（注册 / 探测 / 启动参数 / adapters / 分析器 / atomic_json）与 `crates/omt-exec`（存储、MCP 注入、执行器、launcher、shim、控制、尾随）；每个 exec 由分离的 `omt exec-shim` 进程执行，桌面端只做 tauri command 包装与 jsonl 尾随；**桌面退出不再终止执行**；
   - 「Runtime / daemon plane」补一句：`omt-daemon` 不链接 `omt-exec`（KD-E9）。
2. 设计文档 front matter 的 `status: draft` 改为 `status: sub-project-1-done`，§7 子项目 1 一行的验收列追加「已完成（<最后一个提交的 hash>）」。
3. findings log：总览表里 F-U4a-3、F-U4a-4 标 `fixed(<Task 3 提交>)`；F-U4a-5 标 `resolved-by-design（按 agent 拆分，Task 4）`；F-U2-11 标 `superseded（F-BENCH-25 并行探测）`。

- [ ] **Step 6: 提交**

```bash
git add -A crates/omt-runtime/tests CLAUDE.md docs/plans
git commit -m "test+docs: exec survives launcher kill; sub-project 1 acceptance

Adds the launcher-killed end-to-end test, records the manual acceptance
matrix and the resume-usage measurement, and updates CLAUDE.md, the
design status and the findings log.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Self-Review 记录

**设计覆盖（§3.1–§3.4、§5、§6、§7 子项目 1）：**
- §3.1 两个 crate、依赖方向、daemon 不链接 → Task 1、2、8 Step 8；`atomic_json` 位置的偏差见 P-2。
- §3.2 launcher（Task 9）、shim 主循环（Task 8）、停止 / 状态 / 对账 / `kill_orphan` 降级（Task 9 control；`kill_orphan` 保持原 Orphan 语义，P-8 的对账负责把「shim 死、harness 活」标成 Orphan）、daemon 连接 kind cli（Task 7）。
- §3.3 存储布局（Task 4）、session 复用与单活跃 exec（Task 4 `append_exec` 锁内校验）、序号（seq.json）、迁移（Task 5）、前置修复 F-U4a-3/4（Task 3）。目录锁沿用 `agent.json.lock`，未另设 `.lock`。
- §3.4 桌面端改走 crate、尾随推送、行为变化与「停止全部」（Task 10、11）。
- §5 错误处理：shim 崩溃 → 对账（Task 9）；harness 卡死 → maxDuration（沿用执行器）；daemon 不可达 → 启动阶段 launcher 同步报错，收尾阶段 `lease_release_pending`（Task 8）；agent-busy（Task 4 / 9）；迁移中断（Task 5）。
- §6 测试：shim 生命周期（Task 8 Step 7、Task 12 Step 1）、pid 复用防护（Task 9 `identity_alive` 测试）、存储并发与迁移（Task 4 / 5 / 7）、桌面 vitest（Task 11）。§6 的「CLI / MCP」「claude-code adapter」属子项目 2 / 3，不在本计划内。
- §8 实测项中属于子项目 1 的「resume 后 usage 语义」→ Task 12 Step 4。

**已知的有意简化**：P-6 纯轮询；跨进程的派发互斥（`LAUNCH_LOCK` 仅进程内）留给子项目 2；`kill_orphan` 仍只做单 pid SIGKILL。
