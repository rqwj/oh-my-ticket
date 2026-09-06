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
| F-U15-1 | P3 | advisory | U15 | `apps/desktop/src-tauri/src/lib.rs:19` | single-instance 为 best-effort 守卫，非正确性保证；需注释点明 U4a 文件锁才是 | open（注释已补；`fixed` 待 U4a 文件锁覆盖三条写路径后复核） | — |
| F-U15-3 | P3 | testing-gap | U15 | — | 「双开 → 第二实例退出并聚焦」无自动化或记录的手工验证 | open（手工验证排期 P0 链路打通后的桌面冒烟；`OMT_ALLOW_MULTI_INSTANCE` 已就绪，可支撑 dev 自动化双开脚本） | — |
| F-U15-2 | P3 | manual | U15 | `apps/desktop/src-tauri/src/lib.rs:19` | dev 实例与打包版共用 single-instance socket，`tauri dev` 被静默吞掉 | fixed | — |

## 待办的跨阶段事项（由 findings 派生）

- [ ] U4a 落地时回看 F-U15-1：确认文件锁覆盖 agents.json / bindings.json / harness config 三条写路径后，将 F-U15-1 置 `fixed`。
- [ ] 首次多 worktree 并行开发前处理 F-U15-2。

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
- **状态**：open → U4a 落地后复核。2026-09-06 注释已补（best-effort 定位 + 「不要省掉文件锁」警示）。

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
- **状态**：fixed —— 2026-09-06 按 (b) 落地（debug + env 跳过注册；release 不受 env 影响）。

#### F-U15-3 · P3 · testing-gap · U15 验收场景无验证记录

计划 U15 测试场景「双开 app → 第二实例退出并聚焦首实例」既无自动化也无记录的手工验证。Tauri builder 层难以单测，可接受，但它是计划验收项。

- **建议**：至少在本日志或 PR 描述记录一次手工验证（第二实例 exit 0 + 首实例前置）；若要自动化：dev-only 脚本启动二进制两次，断言第二个 2s 内退出且第一个仍存活。
- **状态**：open。手工验证未执行（不预造记录），排期：P0 后端链路打通后的桌面冒烟一并做；`OMT_ALLOW_MULTI_INSTANCE`（F-U15-2）落地后可在 dev 构建上写自动化双开脚本。

### Coverage 缺口

- Linux/Windows 分支（v1 macOS-only）
- `Cargo.lock` 新增 crate 的供应链审计
- 运行产物：`/tmp/compound-engineering-501/ce-code-review/20260906-124145-da5b7689/`
