---
title: Desktop Agents 派发 - 分阶段 Code Review Findings 跟踪日志
type: review-log
created: 2026-09-06
plan: docs/plans/2026-09-06-1100-feat-desktop-agents-dispatch-implementation-plan.md
design_review: docs/plans/2026-09-06-1530-review-desktop-agents-dispatch-plan-review.md
branch: feat/desktop-agents-dispatch
---

# Desktop Agents 派发 - Code Review Findings 跟踪日志

本文件是该 feature 分支**所有阶段** code review findings 的唯一落点。每个实施单元 / commit 评审后追加一节，不新开文件；已有 finding 的状态变化就地更新，便于跨阶段跟踪与协同。

## 使用约定

- **一节一次评审**：按**时间正序**追加在文末，方便顺读演进。节标题 = `## <序号>. <commit 短 sha> · <单元> · <一句话>`。
- **Finding ID**：`F-<单元>-<序号>`（如 `F-U15-1`），跨节稳定不重排；同一 finding 在后续评审中复现或解决时，引用原 ID 更新状态，不新开 ID。
- **状态**：`open` → `fixed(<sha>)` / `wontfix(<理由>)` / `deferred(<去处>)` / `superseded(<新 ID>)`。
- **级别**：P0 阻断 / P1 应修 / P2 顺手修 / P3 知情即可（与 ce-code-review 一致）。
- **类型**：`gated_auto`（有具体改法，可直接改）/ `manual`（需设计判断）/ `advisory`（仅记录，不改代码）/ `testing-gap` / `residual-risk`。
- **评审方式**必须写明：独立子代理 / 跨模型 / 单上下文内联——决定置信度如何解读。
- **总览表**（下方）每次评审后同步更新，是协同时的第一视图；细节看各节。

## 总览（open 项在前）

| ID | 级别 | 类型 | 单元 | 位置 | 摘要 | 状态 | 负责人 |
|---|---|---|---|---|---|---|---|
| F-U1b-1 | P3 | advisory | U1b | 计划 U1b Files / 排期 | `store.ts` 的 `harness_list` 封装放在 U1b，但 U6 弹窗选择器先于 U1b 需要它——依赖倒置 | fixed(43bf89f)（封装移入 U6 Files，U1b 只留退役/迁移/接线） | — |
| F-U1-8 | P3 | gated_auto | U1 | `harness.rs::upsert_config` | `CONFIG_LOCK` 一旦 poison，所有后续保存永久失败直至重启 | fixed(43bf89f)（写纪律收敛进 atomic_json，`STORE_LOCK` poison 容忍） | — |
| F-U1-9 | P3 | advisory | U1 | `harness.rs::save_configs` | 写入与 rename 之间崩溃留下的 `config.json.tmp-*` 无人清扫 | fixed(43bf89f)（update 内清扫 mtime>1h 的残留 tmp，测试覆盖） | — |
| F-U1-4 | P3 | testing-gap | U1 | `harness.rs::REGISTRY` omp | omp `--approval-mode write` 无 TTY 实测（计划 U1 必做项）未做，`unattended_flag` 暂记 `--yolo` | open（U6 前置门） | — |
| F-U1-7 | P3 | advisory | U1 | `harness.rs::probe_version` | stderr 被丢弃，向 stderr 打版本号的 CLI 会被判「待验证」 | open（U1b/U2 用真实 binary 实测后定） | — |
| F-U15-1 | P3 | advisory | U15 | `apps/desktop/src-tauri/src/lib.rs:19` | single-instance 为 best-effort 守卫，非正确性保证；需注释点明 U4a 文件锁才是 | fixed(43bf89f)（atomic_json flock 覆盖 harness config + agents.json 两条写路径；bindings.json 由 U9 按计划用同一 helper） | — |
| F-U15-3 | P3 | testing-gap | U15 | — | 「双开 → 第二实例退出并聚焦」无自动化或记录的手工验证 | open（手工验证排期 P0 链路打通后的桌面冒烟；`OMT_ALLOW_MULTI_INSTANCE` 已就绪，可支撑 dev 自动化双开脚本） | — |
| F-U1-1 | P1 | gated_auto | U1 | `harness.rs`（7 处） | `cargo fmt --check` 失败，CI `rust` job 会红 | fixed(716ce0e) | — |
| F-U1-2 | P2 | gated_auto | U1 | `harness.rs::harness_config_set` / `save_configs` | 无进程内锁：并发 `harness_config_set` 会丢更新，且 tmp 文件名仅含 pid，同进程并发写会互相截断 | fixed(716ce0e) | — |
| F-U1-3 | P3 | advisory | U1 | 计划 U1 | U1 前端半（HarnessSelect 移除、bag v2 迁移、store/tests）未落地，计划需记注拆分为 U1b | fixed(ba0e046)（计划已加 U1b 单元，排期 U7 前） | — |
| F-U1-5 | P3 | gated_auto | U1 | `harness.rs::harness_entry` | Manual 路径不存在时投影 `installed: true` + `pendingVerification`，语义矛盾 | fixed(716ce0e) | — |
| F-U1-6 | P3 | gated_auto | U1 | `harness.rs::harness_config_set` | `Manual{path}` 空串/纯空白未拒绝 | fixed(716ce0e) | — |
| F-U15-4 | P3 | gated_auto | U15 | `apps/desktop/src-tauri/src/lib.rs:30` | `OMT_ALLOW_MULTI_INSTANCE` 用 `is_some()` 判定，空值/`0` 也会关掉守卫，违背仓库 env「空即未设置」约定 | fixed(7be3f76) | — |
| F-U15-5 | P3 | advisory | U15 | `docs/dsh-plugin-dev-notes.md §1.4` | `OMT_ALLOW_MULTI_INSTANCE` 只在 lib.rs 注释与本日志可见，开发回路文档未记 | fixed(7be3f76) | — |
| F-U15-2 | P3 | manual | U15 | `apps/desktop/src-tauri/src/lib.rs:19` | dev 实例与打包版共用 single-instance socket，`tauri dev` 被静默吞掉 | fixed(c8d8946) | — |

## 待办的跨阶段事项（由 findings 派生）

- [ ] U4a 落地时回看 F-U15-1：确认文件锁覆盖 agents.json / bindings.json / harness config 三条写路径后，将 F-U15-1 置 `fixed`。
- [ ] P0 桌面冒烟时执行 F-U15-3 的手工双开验证并回填记录（可顺手用 `OMT_ALLOW_MULTI_INSTANCE` 写 dev 自动化脚本）。
- [x] ~~下次触碰 lib.rs 时顺手关 F-U15-4；下次改 dev-notes 时顺手关 F-U15-5~~ 已于 7be3f76 关闭。

---

## 1. `9b104c5` · U15（single-instance 部分）· 引入 tauri-plugin-single-instance

- **日期**：2026-09-06
- **范围**：`Cargo.toml` +1、`lib.rs` +9、`Cargo.lock` +411（`tauri-plugin-single-instance` 2.4.4 及 zbus 依赖树，Linux 侧 cfg 依赖，macOS 不编译）
- **意图**：计划 §3.6.9 / P0 排序首项——第二实例退出并聚焦首实例，为 agents/bindings 存储写入防双开
- **评审方式**：单上下文内联（correctness + 项目标准 + 计划符合性）；读取插件 2.4.4 `platform_impl/macos.rs` 源码核验运行时行为；`cargo check -p oh-my-ticket-desktop` 通过。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready to merge**。代码正确、注册位置正确、与计划一致；三条 P3 不阻塞。

### 核验通过

| 检查 | 结果 |
|---|---|
| 注册顺序 | 首位注册；插件 `setup` 在 `Builder::run()` 构建期执行，早于窗口创建、daemon discover-or-spawn、admin-grant 自注册——第二实例不留任何副作用 |
| 窗口 label `"main"` | 与 `tauri.conf.json` 一致 |
| `show → unminimize → set_focus` | 顺序正确；`WebviewWindow` 方法线程安全，可在插件 tokio 监听线程回调 |
| 崩溃残留 | 主实例 `kill -9` 留 stale socket → 下次启动 `ConnectionRefused` → 插件清理并接管，自愈 |
| capabilities | 插件无 command，无需改 `capabilities/default.json` |
| 计划符合性 | §3.6.9 ✔、P0 排序首项 ✔；U15 其余（events_subscribe 泄漏、omt_call async）按计划留 P1，未混入 |
| 项目标准 | `CLAUDE.md` 无桌面 Rust 专项约束；中文注释风格与 `rpc_bridge.rs` 一致 |

### Findings

#### F-U15-1 · P3 · advisory · `lib.rs:19` · single-instance 是 best-effort，不是正确性守卫

插件 macOS 实现：连接 `/tmp/com_ohmyticket_desktop_si.sock` 失败且错误**不是** `NotFound`/`ConnectionRefused` 时（如同机另一 OS 用户先启动占用同名 socket → `PermissionDenied`），**静默按普通启动继续且不监听**，两实例并存。commit message「the guard must land before any dispatch chain ships」把它当成了正确性门。

- **建议**：不改逻辑。计划 §3.6.9 已写「存储写路径同时保留文件锁」——U4a 的文件锁才是正确性守卫。在 `lib.rs` 注释补一句「best-effort；跨进程写安全由 U4a 文件锁保证」，防止后人省掉文件锁。
- **状态**：fixed(43bf89f) —— U4a 落地 `atomic_json`（companion lockfile flock + 进程内锁），覆盖 harness config 与 agents.json 两条写路径；bindings.json（U9）按计划用同一 helper。single-instance 保持 best-effort 定位不变。

#### F-U15-2 · P3 · manual · `lib.rs:19` · dev 与打包版共用 socket

socket 名仅由 `identifier`（`com.ohmyticket.desktop`）派生。`pnpm tauri:dev` 与正在运行的 `.app` 互判为「第二实例」：dev 实例 `exit(0)`、只把打包版聚焦过来，表现为「tauri dev 没反应」；两个 worktree 各起 dev 实例亦不可。

- **建议**（二选一）：(a) `docs/dsh-plugin-dev-notes.md` 记一条踩坑；(b) **推荐**——debug 构建下读 env `OMT_ALLOW_MULTI_INSTANCE=1` 跳过插件注册：
  ```rust
  let builder = tauri::Builder::default();
  let builder = if cfg!(debug_assertions) && std::env::var_os("OMT_ALLOW_MULTI_INSTANCE").is_some() {
      builder
  } else {
      builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| { /* … */ }))
  };
  ```
- **状态**：fixed(c8d8946) —— 按 (b) 落地（debug + env 跳过注册；release 不受 env 影响）。

#### F-U15-3 · P3 · testing-gap · U15 验收场景无验证记录

计划 U15 测试场景「双开 app → 第二实例退出并聚焦首实例」既无自动化也无记录的手工验证。Tauri builder 层难以单测，可接受，但它是计划验收项。

- **建议**：至少在本日志或 PR 描述记录一次手工验证（第二实例 exit 0 + 首实例前置）；若要自动化：dev-only 脚本启动二进制两次，断言第二个 2s 内退出且第一个仍存活。
- **状态**：open。手工验证未执行（不预造记录），排期：P0 后端链路打通后的桌面冒烟一并做；`OMT_ALLOW_MULTI_INSTANCE`（F-U15-2）落地后可在 dev 构建上写自动化双开脚本。

### Coverage 缺口

- Linux/Windows 分支（v1 macOS-only）
- `Cargo.lock` 新增 crate 的供应链审计
- 运行产物：`/tmp/compound-engineering-501/ce-code-review/20260906-124145-da5b7689/`

---

## 2. `c8d8946` · U15 · 修复 F-U15-1/2（best-effort 注释 + debug 多实例开关）

- **日期**：2026-09-06
- **范围**：`lib.rs` +23/−10；本日志首次入库（含 F-U15-1/2/3 状态更新）
- **意图**：F-U15-2 按建议 (b) 落地——debug 构建 `OMT_ALLOW_MULTI_INSTANCE` 跳过 single-instance 注册；F-U15-1 补注释点明 best-effort 与 U4a 文件锁；F-U15-3 明确排期、不预造验证记录
- **评审方式**：单上下文内联；`cargo check -p oh-my-ticket-desktop` 通过。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready to merge**。两项修复均与 §1 建议一致，逻辑正确；新增两条 P3（一条有具体改法）。

### 核验通过

| 检查 | 结果 |
|---|---|
| release 不受 env 影响 | `enable = !(cfg!(debug_assertions) && env)`：release 下 `cfg!` 恒 false → 守卫恒开 ✔ |
| 未注册时的插件构造 | `init()` 只构造 `TauriPlugin`，socket 操作全在 `setup` 中；disabled 分支构造后即丢弃，无副作用 ✔ |
| 插件仍首位注册 | enabled 分支 `builder.plugin(single_instance)` 先于 shell/dialog ✔ |
| F-U15-1 注释 | 明确写了「best-effort」「静默放行」场景与「不要省掉文件锁」——符合建议，状态保持 open 待 U4a 复核 ✔ |
| F-U15-3 处理 | 未伪造手工验证记录，改为排期 P0 冒烟——正确做法 ✔ |
| 日志约定 | 总览按 open 在前重排、状态就地更新、ID 稳定 ✔ |

### Findings

#### F-U15-4 · P3 · gated_auto · `lib.rs:30` · env 判定用 `is_some()`，空值/`0` 也关守卫

`std::env::var_os("OMT_ALLOW_MULTI_INSTANCE").is_some()`：`OMT_ALLOW_MULTI_INSTANCE=` 与 `=0` 都会关闭守卫。仓库 env 约定（`docs/runtime/config.md`「环境变量为空字符串或纯空白时视为未设置」，`paths.rs::resolve` 同样 `trim().is_empty()` 判定）是空即未设置；commit message 也写的是 `=1`。

- **建议**（可直接改）：
  ```rust
  let allow_multi = std::env::var("OMT_ALLOW_MULTI_INSTANCE")
      .map(|v| { let v = v.trim(); !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") })
      .unwrap_or(false);
  let enable_single_instance = !(cfg!(debug_assertions) && allow_multi);
  ```
- **状态**：fixed(7be3f76) —— 按建议落地为 `parse_allow_multi_instance`（纯函数 + 单测覆盖 unset/空/空白/`0`/`false`/大小写/有效值）。

#### F-U15-5 · P3 · advisory · `docs/dsh-plugin-dev-notes.md §1.4` · 开发回路文档未记新 env

`OMT_ALLOW_MULTI_INSTANCE` 目前只在 `lib.rs` 注释和本日志出现。`dsh-plugin-dev-notes.md §1.4 开发回路` 已有「多实例开发必须隔离 `DSH_HOME`」条目，是该 env 的自然归宿——多 worktree 并行开发 desktop 时两者要一起设。

- **建议**：§1.4 追加一行：「desktop 多实例/多 worktree 开发：debug 构建设 `OMT_ALLOW_MULTI_INSTANCE=1` 跳过 single-instance 守卫；release 不受影响。」
- **状态**：fixed(7be3f76) —— 已追加（并注明与 `DSH_HOME` 隔离配合使用）。

### Coverage 缺口

- 同 §1：Linux/Windows 分支、供应链审计。
- 备注：计划 frontmatter 的 `findings_log:` 反向链接行仍在工作区未提交（不影响本 commit 评审）。

---

## 3. `7be3f76` · U15 · 修复 F-U15-4/5（env 解析纯函数 + dev-notes）

- **日期**：2026-09-06
- **范围**：`lib.rs` +31/−2（`parse_allow_multi_instance` + 单测）、`dsh-plugin-dev-notes.md` +1、本日志 §2 入库 + F-U15-4/5 状态更新
- **意图**：F-U15-4 env 判定改为仓库约定（空/空白/`0`/`false` = 未设置）；F-U15-5 在开发回路文档记录 `OMT_ALLOW_MULTI_INSTANCE`
- **评审方式**：单上下文内联；`cargo test -p oh-my-ticket-desktop --lib allow_multi` → 1 passed。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready to merge**。无新 finding。

### 核验通过

| 检查 | 结果 |
|---|---|
| 解析语义 | `trim` 后空 / `0` / `false`（忽略大小写）→ off，其余非空 → on；与 `docs/runtime/config.md` 约定及 §2 建议一致 ✔ |
| 非 UTF-8 值 | `std::env::var(...).ok()` → `None` → 守卫保持开启（保守方向）✔ |
| release 不变式 | 仍为 `!(cfg!(debug_assertions) && allow_multi)`，release 恒开 ✔ |
| 测试 | 纯函数抽出后可单测；用例覆盖 unset/空/空白/`0`/`false`/`False`/`1`/`true`/带空白值，实际运行通过 ✔ |
| dev-notes | §1.4 追加一行，并点明与 `DSH_HOME` 隔离配合使用——比建议多了一层关联说明 ✔ |
| 日志维护 | 作者已就地更新 F-U15-4/5 详情状态；本次评审补 sha（`fixed(7be3f76)`）并关闭已过期的待办第三条 |

### Findings

无。

### 当前 U15 收口状态

| ID | 状态 |
|---|---|
| F-U15-1 | open，待 U4a 文件锁落地后复核 |
| F-U15-2 | fixed(c8d8946) |
| F-U15-3 | open，排期 P0 桌面冒烟 |
| F-U15-4 | fixed(7be3f76) |
| F-U15-5 | fixed(7be3f76) |

U15 single-instance 子项可视为完成；剩余两条 open 都有明确触发点，不阻塞下一单元（U1）。

---

## 4. `3cf13d9` · U1（后端半）· Provider Contract registry + 探测 + config store

- **日期**：2026-09-06
- **范围**：新建 `harness.rs` +608（registry / `HarnessUserConfig` / `BinarySource` / config store / `harness_list` / `harness_config_set` / 探测原语 + 8 个单测）；`rpc_bridge.rs` −63（探测原语上移）；`lib.rs` +3
- **意图**：计划 U1 的 contract 层——类型表、机器本地配置（0600、tmp+rename、`.corrupt-<ts>`）、`Detected|Manual` binary 来源、探测超时修复；前端半（HarnessSelect 移除、bag v2 迁移）按「后端先行」推迟
- **评审方式**：单上下文内联（correctness / reliability / 项目标准 / 计划符合性）；`cargo test -p oh-my-ticket-desktop --lib` → 12 passed；`cargo clippy --all-targets -D warnings` 通过；**`cargo fmt --all -- --check` 失败（exit 1，7 处）**。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready with fixes**——逻辑正确、测试覆盖到位，但 F-U1-1 会让 CI 红，F-U1-2 违背 §3.6.6 写纪律；两者都是几分钟的改动。

### 核验通过

| 检查 | 结果 |
|---|---|
| probe 超时（F1 修复） | `spawn` + 读线程 + `try_wait` 轮询 + 超时 kill；hanging 用例实测 1s 超时返回 ✔ |
| `BinarySource` 序列化 | `{"kind":"detected"}` / `{"kind":"manual","path"}`，`#[serde(default)]` 前向兼容，缺省字段回退默认 ✔ |
| 0600 + tmp/rename | 落盘权限实测 0o600，无 tmp 残留 ✔ |
| 损坏降级 | `config.json.corrupt-<ms>` 留证后空配置 ✔ |
| 保留键 | `OMT_RUNTIME_DIR` / `OMT_HOME` / `PATH` 拒绝，空键与含 `=` 拒绝 ✔ |
| Manual 校验 | 存在 + 可执行位，错误含完整路径 ✔ |
| `which_binary` | 要求可执行位；GUI PATH 增广保留 ✔ |
| async command | 两个 command 均 `#[tauri::command(async)]`（§3.4）✔ |
| clippy | `-D warnings` 通过 ✔ |
| 与 U15 衔接 | `augmented_path` 上移后 `dsh_plugin_install` 仍复用 ✔ |

### Findings

#### F-U1-1 · P1 · gated_auto · `harness.rs:160,184,227,272,280,376,545` · rustfmt 不通过 → CI 红

`ci.yml` 的 `rust` job 第一步是 `cargo fmt --all -- --check`；本 commit 有 7 处格式差异（`Manual { path }` 展开、长 `format!` 换行等）。

- **建议**：`cargo fmt --all` 后 amend/追加 commit；建议在本地加 pre-commit 或 `pnpm` 脚本跑 `cargo fmt --check`。
- **状态**：fixed(716ce0e) —— `cargo fmt --all` 应用，`--check` 全仓通过（漂移仅在 harness.rs，无历史文件噪声）。

#### F-U1-2 · P2 · gated_auto · `harness_config_set` / `save_configs` · read-merge-write 无进程内锁

计划 §3.6.6 明确「统一 read-merge-write under **进程内锁** + tmp+rename」。现实现：`harness_config_set` 是 async command，设置页快速连续保存两个 provider（或两个窗口/多次点击）会并发进入 `load → insert → save`，后写者覆盖前写者的 key（丢更新）。更糟的是 tmp 文件名只含 pid（`config.json.tmp-<pid>`），**同进程并发写共用同一 tmp 路径**，`truncate(true)` 会截断对方正在写的内容，rename 后可能得到半截 JSON → 下次启动走 `.corrupt` 降级、用户配置丢失。

- **建议**：
  ```rust
  static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
  // harness_config_set 内：
  let _guard = CONFIG_LOCK.lock().map_err(|_| "config lock poisoned")?;
  let mut configs = load_configs(&runtime_dir); … save_configs(…)?;
  ```
  同时 tmp 名加唯一后缀（如 `tmp-<pid>-<COUNTER.fetch_add>`），使锁失效时也不会互相截断。**跨进程**文件锁（fs4 flock，F-U15-1 的关闭条件）建议在 U4a 与 agents/bindings 一并做成共用 helper，不在本单元重复造。加一个并发测试：两线程各写不同 key，断言两者都在。
- **状态**：fixed(716ce0e) —— `CONFIG_LOCK` 进程内锁 + `upsert_config` 锁内 read-merge-write + tmp 名 `tmp-<pid>-<seq>`；并发测试 `concurrent_upsert_does_not_lose_updates`（8 线程 × 5 写，8 key 全在）。跨进程 flock 留 U4a。

#### F-U1-3 · P3 · advisory · 计划 U1 · 前端半未落地，计划未记注

commit message 明示前端消费者（HarnessSelect 移除、bag v2 迁移、store.ts、settings-install.spec 调整）推迟到 UI wave，但计划 U1 的 Files / Test scenarios（「旧 bag 含 opencode/dsh 条目 → 迁移」）仍把它们算在 U1 内。现状：`harness_detect` 仍硬编码 dsh/opencode 被 `HarnessSelect` 调用，`harness_list` 尚无前端消费者。

- **建议**：计划 §五 把 U1 拆成 U1（后端 contract，本 commit）与 U1b（前端接线 + bag v2 迁移 + HarnessSelect 移除），U1b 排在 U7 之前；避免 P0 验收时漏掉迁移用例。
- **状态**：fixed(716ce0e) —— 计划 §五 已加 U1b 单元（Dependencies: U1、U6；排期 UI 波次、U7 之前），分期行已更新。

#### F-U1-4 · P3 · testing-gap · `REGISTRY` omp `unattended_flag` · 计划 U1 实测项未做

计划 U1 Approach 列为**实测项**：omp `--approval-mode write` 在无 TTY 下的实际行为（阻塞 / deny / 放行），结论回填 descriptor `permission_model`，决定 U6 权限确认文案。代码注释承认「实测结论回填前按官方文档记 `--yolo`」。

- **建议**：在 U6 开工前完成实测并回填；本 finding 作为 U6 的前置门。实测脚本：`echo 'run: touch /tmp/x' | omp -p --approval-mode write --mode json < /dev/null`，观察是否退出/挂起/写文件。
- **状态**：open（U6 前置）

#### F-U1-5 · P3 · gated_auto · `harness_entry` · Manual 缺失路径投影 `installed: true`

`installed: detected_path.is_some()` 对 Manual 恒为 true（path 总是 Some），与 `state: pendingVerification` 并存时 UI 会同时显示「已安装」和「待验证」。

- **建议**：`installed = match binary { Detected => path.is_some(), Manual{path} => Path::new(path).is_file() }`；对应测试 `pending["installed"] == false`。
- **状态**：fixed(716ce0e) —— 按建议实现，`harness_entry_projects_state_from_binary_source` 断言 pending 投影 `installed == false`。

#### F-U1-6 · P3 · gated_auto · `harness_config_set` · `Manual{path}` 空值未拒绝

`validate_env` 校验了 env，但 `BinarySource::Manual { path: "" }` 可保存，之后 `resolve_program` 才报「路径不存在：」（空）。

- **建议**：保存时 `path.trim().is_empty()` → `Err("手动指定的 binary 路径不能为空")`；不要求存在（允许「待验证」）。
- **状态**：fixed(716ce0e) —— 新增 `validate_config`（保存时校验空路径，存在性仍留给 exec_start/resolve_program），测试覆盖空串与「合法但不存在」两态。

#### F-U1-7 · P3 · advisory · `probe_version` · 丢弃 stderr

`stderr(Stdio::null())`——若某 harness 把 `--version` 打到 stderr（或 stdout 为空、stderr 有 banner），探针返回 None，Manual 路径会被判「待验证」而实际可用。v1 两家（opencode/omp）实测 stdout 有输出即可关闭；否则改为 stdout 优先、stderr 兜底。

- **建议**：U1b 或 U2 集成时用真实 binary 各跑一次 `harness_list` 确认；若有 stderr 情况再改。
- **状态**：open（待实测）

### 下一单元（U2）需注意的点（非本 commit finding）

- `extra_args` 未做任何校验，spawn 时应放在 desktop 注入的 `unattended_flag` / MCP args **之前**，让 desktop 的显式 flag 后置生效（CLI 通常后者覆盖前者）；或在 U7 设置页对 `--approval-mode`/`--mcp-config` 类键做警告。
- `resolve_program` 的 `Detected` 分支每次 exec_start 重探——与 §3.1 一致；U2 注意把探测结果写进 transcript header 的 binary 路径字段。

### Coverage 缺口

- 与 §1 同；此外 `harness_list` 的真实 opencode/omp 探测未在评审环境跑（测试仅断言自洽）。

---

## 5. `716ce0e` + `ba0e046` · U1 · 修复 F-U1-1/2/5/6 + 计划拆出 U1b

- **日期**：2026-09-06
- **范围**：`716ce0e` `harness.rs` +113/−19（fmt、`CONFIG_LOCK` + `upsert_config`、`unique_tmp_path`、`validate_config`、`installed` 语义、2 个新测试）；`ba0e046` 计划 +17（U1b 单元、P0 排序）、日志 §4 入库 + 状态更新
- **意图**：关闭 §4 的 P1/P2 与三条 P3；把 U1 前端半在计划中显式拆为 U1b
- **评审方式**：单上下文内联；`cargo fmt --all -- --check` exit 0；`cargo clippy --all-targets -D warnings` 通过；`cargo test -p oh-my-ticket-desktop --lib` → 14 passed。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready to merge**。四项代码修复均按建议落地且有测试；新增三条 P3（两条代码、一条计划），不阻塞。

### 核验通过

| 项 | 结果 |
|---|---|
| F-U1-1 fmt | 全仓 `--check` exit 0；漂移仅在 harness.rs，无历史文件噪声 ✔ |
| F-U1-2 锁 | `static CONFIG_LOCK: Mutex<()>`；`upsert_config` 锁内 load→insert→save；tmp 名 `tmp-<pid>-<seq>`（AtomicU64）；并发测试 8 线程 × 5 写、8 key 全在 ✔。锁外的二次 `load_configs` 仅用于投影，无正确性影响 ✔ |
| F-U1-5 | `installed` 对 Manual 改 `Path::is_file()`；pending 用例断言 `installed == false` ✔ |
| F-U1-6 | `validate_config` 保存时拒绝空/空白 Manual 路径，不要求存在（保留「待验证」态）；测试覆盖两态 ✔ |
| F-U1-3 → 计划 | U1b 单元含 Files/Approach/Test scenarios/排期；P0 排序行更新为 `… U6 → U9 → U1b → U7 → U8` ✔（依赖顺序问题见 F-U1b-1） |
| 日志簿记 | §4 入库、状态就地更新 ✔；F-U1-3 的 sha 应为 `ba0e046`（计划改动在该 commit），本次已改正 |

### Findings

#### F-U1b-1 · P3 · advisory · 计划 U1b · `harness_list` store 封装与 U6 依赖倒置

U1b Files 把「`store.ts` 的 `harness_list`/`harness_config_set` 封装」放在 U1b，而 U1b 排在 U6 之后；但 U6 弹窗的「选择器 = 已启用 harness ∪ 空闲 Agent」「默认 = workspace 默认 harness（读取时校验 enabled）」必须消费 `harness_list`。U6 开工时要么临时自造封装（重复），要么被迫先做 U1b 的一半。

- **建议**：把 `store.ts` 的 `harness_list`/`harness_config_set` 封装（含类型 `HarnessEntry`）移入 U6 Files；U1b 只保留 HarnessSelect 退役、bag v2 迁移、SettingsPanel/WorkspaceSwitcher 接线与测试调整。或者反过来把 U1b 提到 U6 之前——但 U1b 的 SettingsPanel 接线又想等 U7，故前者更顺。
- **状态**：fixed —— 2026-09-06 计划已改（U1b Files 只留退役/迁移/接线；`store.ts` 的 `harness_list`/`harness_config_set` 封装与 `HarnessEntry` 类型移入 U6 Files）。
#### F-U1-8 · P3 · gated_auto · `upsert_config` · 锁 poison 即永久失败

`CONFIG_LOCK.lock().map_err(|_| "config lock poisoned")`：任何一次持锁期间 panic（如 `serde_json::to_string_pretty` 极端情况、未来 helper 演进）都会让后续所有保存返回错误直到 app 重启。锁保护的是 `()`，没有需要保护的不变量，poison 无意义。

- **建议**：`let _guard = CONFIG_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);`
- **状态**：fixed(43bf89f) —— 写纪律收敛进 `atomic_json`：`STORE_LOCK` 以 `PoisonError::into_inner` 容忍 poison；`upsert_config` 改为该 helper 的薄封装。

#### F-U1-9 · P3 · advisory · `save_configs` · tmp 残留无清扫

写入 tmp 与 rename 之间崩溃会留下 `harness/config.json.tmp-<pid>-<seq>`；文件 0600 且很小，无功能影响，但会累积。

- **建议**：U4a 的共用原子写 helper 在 `load` 时顺手清扫同目录 `*.tmp-*`（pid 已死或 mtime 超过 1h）。
- **状态**：fixed(43bf89f) —— `update_json` 每次顺手清扫同目录 mtime>1h 的 `*.json.tmp-*`；测试 `tmp_files_are_unique_and_swept` 覆盖「阈值 0 清除 / 阈值 1h 不误删新鲜 tmp」。

### 当前 U1 收口状态

| ID | 状态 |
|---|---|
| F-U1-1/2/5/6 | fixed(716ce0e) |
| F-U1-3 | fixed(ba0e046) |
| F-U1-4 | open，U6 前置门（omp 审批模式实测） |
| F-U1-7 | open，U1b/U2 用真实 binary 实测后定 |
| F-U1-8/9 | open，U4a 共用 helper 时顺手 |
| F-U1b-1 | open，计划动作 |

U1 后端半可视为完成，**不阻塞 U4a 开工**；U4a 是三条「顺手」项（F-U15-1、F-U1-8、F-U1-9）的汇合点，建议 U4a 的共用原子写 helper 一次把「进程内锁 + flock + poison 容忍 + tmp 清扫」做齐，然后把 harness config 切过去。
