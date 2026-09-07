---
artifact_contract: "ce-handoff/v1"
created_at: "2026-09-07T12:30:00Z"
title: "Desktop Agents 派发 — 后端链路完成，UI 波次待开工"
summary: "U1-U4a 后端全部落地（63 测试全绿），下一 session 按 U5→U6→U9→U1b→U7→U8 做 UI 层，含文件靶点、接口契约与评审教训"
keywords: ["desktop-agents", "harness-executor", "dispatch-chain", "UI-wave", "oh-my-ticket"]
cwd: "/Users/robertq/Documents/development/ts/dsh-plugins/oh-my-ticket"
resume_focus: "UI 波次（U5→U6→U9→U1b→U7→U8）— 后端链路已完成并评审，前端消费方待实现"
repository: "oh-my-ticket"
branch: "feat/desktop-agents-dispatch"
head: "64fcdf9"
---

# Desktop Agents 派发 — 后端链路完成，UI 波次待开工

## 目标

omt desktop 的 harness 执行器已完成后端全链（U1–U4a + U15 部分），本次 session 要做 **UI 层**——把后端链路暴露给用户。计划文档与评审记录见下方权威引用。

## 当前状态

### 分支与提交

- **分支**: `feat/desktop-agents-dispatch`（基于 `main` @ `10cb0cd`）
- **HEAD**: `64fcdf9` — U4 dispatch chain + 终态租约释放 + 补偿表
- **未推送**: 所有 12 个 commit 都在本地分支上，未 push
- **工作区**: 干净（CLAUDE.md 是用户的 WIP，不入提交）

### 已完成的实施单元

| 单元 | 内容 | 提交 |
|------|------|------|
| U15（部分） | tauri-plugin-single-instance + F-U2-12/13/14/15/16/18 全关闭 | 9b104c5, c8d8946, 7be3f76, 0cb958d |
| U1（后端层） | harness.rs Provider Contract + config store + 探测超时修复 | 3cf13d9 |
| U4a | agents.rs Agent 最小存储 + 共用原子写 helper（atomic_json.rs） | 43bf89f |
| U2 | executor.rs ExecutionManager（spawn/kill/Channel/终态分类/起动窗） | 64fcdf9（合入 U4） |
| U3 | mcp_injection.rs（opencode env / omp refcount）+ omt 第二 sidecar 打包 | 556c1d2 |
| U4 | dispatch chain（锁内预检 → ensure-run → per-step commandId → 补偿表 → 终态租约释放）+ prompt 模板 + exec_start 命令 | 64fcdf9 |

U4 之后又经过一轮 ce-code-review（7 路独立 persona），4 条 actionable 全部修复（`95fb1e0` + `0cb958d`）。

### 测试与质量门

- desktop `cargo test --lib`: **63 passed**（executor 10 / harness 12 / agents 8 / atomic_json 6 / daemon 2 / lib.rs 1 / mcp_injection 9）
- runtime `cargo test --lib`: **20 passed**（含 enroll_identity 2 / auth.rs 嵌套 actor 2）
- `cargo fmt --all -- --check`: 通过
- `cargo clippy --all-targets -D warnings`: 通过（desktop + runtime 两 crate）
- `cargo test --workspace`: 全绿

## 权威文档（新 session 必读，按优先级）

1. **实施计划**: `docs/plans/2026-09-06-1100-feat-desktop-agents-dispatch-implementation-plan.md` — 所有 U 单元的 Files/Approach/Test scenarios，§3.3 链序 + 补偿表，§3.6 工程纪律
2. **设计文档**: `docs/plans/2026-09-05-1245-feat-desktop-harness-executor-design.md` — KD-D1~D8、adapter 表、§5.4.1 session 连续性
3. **深度评审**: `docs/plans/2026-09-06-1530-review-desktop-agents-dispatch-plan-review.md` — §6 决策记录（含 2 个阻断级修复：interrupt-retry 链、`--actor` 身份）
4. **Findings 日志**: `docs/plans/2026-09-06-1245-review-desktop-agents-dispatch-findings-log.md` — §1–§13 全部评审轮次记录；§13 是最新（63 测试状态）

## UI 波次：各单元的文件靶点与接口契约

### U5. TranscriptView 与右侧 Agents 面板

**Goal**: Agents tab 显示 agent 列表 + 选中 agent 的动态 message 流。

**Files**:
- 新建 `apps/desktop/src/TranscriptView.tsx`（虚拟滚动 + raw 50ms 合并刷新 + 自动跟随/上滚 pill）
- 新建 `apps/desktop/src/AgentsPanel.tsx`（列表 + 选中态）
- 修改 `apps/desktop/src/App.tsx`（center-pane 内层 tab: 详情 | Agents）
- 修改 `apps/desktop/src/store.ts`（executions/agents 切片 + Channel 归并）
- 修改 `apps/desktop/src/styles.css`
- 复用 `apps/desktop/src/MarkdownText.tsx`

**Rust 侧接口**（已实现，U5 消费）：
- `agents_list(home_id)` command → `{agents: [...], agentSeq, parseFailed}` — agents.rs 已导出
- TranscriptEvent 经 Tauri Channel 推送：`{execId, seq, ts, kind: "session_start"|"raw"|"exit", ...}` — executor.rs `spawn_reader` + `finish_terminal` 产生
- `exec_stop(exec_id)` command — executor.rs 已导出，幂等

**Pattern**: 所有活跃 exec 各保尾窗（内存上界 = agent 数 × 2000 条）；display_name = title ?? `Agent ${seq}`（P0 进程内单调）；按当前 home 过滤；条目含状态/当前节点/run item 统计；零 claim 退出标注；空态提示 + 「配置 Harness」入口。

### U6. 执行弹窗（ExecutionDialog）

**Goal**: ticket 详情运行按钮 → 弹窗选择 harness → 触发 exec_start → 派发全链。

**Files**:
- 新建 `apps/desktop/src/ExecutionDialog.tsx`（Modal 照 WorkspaceSwitcher overlay 模式）
- 修改 `apps/desktop/src/App.tsx`（全局门控）、`apps/desktop/src/DetailPanel.tsx`
- 修改 `apps/desktop/src/store.ts`（F-U1b-1：`harness_list`/`harness_config_set` 封装 + `HarnessEntry` 类型**在此落地**）

**Rust 侧接口**（已实现）：
- `harness_list()` → `{harnesses: [{id, label, installed, detectedPath, detectedVersion, enabled, binarySource, manualPath, state, unattendedFlag}]}` — harness.rs 已导出
- `exec_start` 需要在 lib.rs 注册（尚未注册！当前只有 exec_stop）——这是 U6 的第一步

**注意**: `exec_start` 需要同时注入 `State<SharedSession>` 和 `State<ExecutionState>`。当前 executor.rs 的 `exec_start_impl` 已实现全链（预检 → ensure-run → 补偿表 → prompt → spawn）但尚未作为 tauri command 暴露。

### U9. 绑定模型

**Goal**: 树上渲染绑定标记；弹窗预选绑定 Agent。

**Files**:
- 修改 `apps/desktop/src-tauri/src/agents.rs`（追加 bindings.json 读写——U4a 已建立 agents.rs）
- 修改 `apps/desktop/src/TreePanel.tsx`（NodeRow badge）
- 修改 `apps/desktop/src/store.ts`（bindingsByHome + 继承解析纯函数）

**Approach**: 显式绑定只写派发节点，读侧沿祖先链取最近。GC 仅在 agents.json 解析成功后执行。

### U1b. Harness 前端接线

**Goal**: 设置页 Harnesses tab + bag v2 迁移 + HarnessSelect 移除。

**Files**:
- 修改 `apps/desktop/src/store.ts`（bag v2 迁移）、`apps/desktop/src/types.ts`
- 修改 `apps/desktop/src/WorkspaceSwitcher.tsx`、`apps/desktop/src/SettingsPanel.tsx`
- 移除 `apps/desktop/src/HarnessSelect.tsx`
- 调整 `apps/desktop/tests/store.spec.tsx`、`apps/desktop/tests/settings-install.spec.tsx`

### U7. 设置页 Harnesses tab

**Goal**: R-A 完整管理面（Enabled / CLI&Args / Env）。

**Files**: 新建 `apps/desktop/src/HarnessManagerTab.tsx`；修改 `apps/desktop/src/SettingsPanel.tsx`

### U8. App 布局调整

**Goal**: R-D（树仅 Tickets tab 显示）。

**Files**: 修改 `apps/desktop/src/App.tsx`

## 关键设计决策（已定，勿重开）

1. **统一 run 化**：派发 = ensure run + harness MCP 自 claim/report；不做轻量直派。
2. **存储 = runtime dir 本地 + 中性命名**：`agents/<home-id>/`、`harness/`、`transcripts/<home-id>/`；无 desktop 前缀。
3. **不内置 harness**：v1 仅 opencode + omp；全部依赖自动检测 + 用户手动指定。
4. **Agents 历史跨重启保留**：agents.json 落盘 + 启动 demote。
5. **嵌套 actor 命名空间**：`mcp:<pid>/exec:<execId>`；desktop 匹配用 `ends_with("/exec:<execId>")`（不是精确相等！）。
6. **终止归因偏好 SIGTERM**：classify 的 stop_reason 偏好仅对 `sig == SIGTERM` 生效，外部 SIGKILL/SEGV 保持 External。

## 工程纪律（§3.6 全文见计划文档，最常踩的）

1. **async command**：所有新 tauri command 必须 `#[tauri::command(async)]`
2. **cwd 剥 `.omt` 尾**：home.path 是 `.omt` 目录本身，spawn cwd 必须剥到 workspace 根
3. **存储写纪律**：进程内锁 + tmp+rename 原子替换；损坏 → `.corrupt-<ts>` + GC 冻结
4. **测试 mock**：tauri-api mock 工厂必须导出 `Channel`（不止 `invoke`/`listen`）
5. **F-U2-13 回归测试**：classify 测试必须传 `kill_reason=None`（不能传 Some——那会掩蔽参数化重构回归）
6. **exec_start 的 release 契约**：spawn 失败后调用方不要再 release（spawn 内部已兜底释放）

## 构建与验证命令

```bash
# Rust 测试（desktop）
cargo test -p oh-my-ticket-desktop --lib

# Rust 全 workspace
cargo test --workspace

# 格式 + lint
cargo fmt --all -- --check
cargo clippy -p oh-my-ticket-desktop -p omt-runtime --all-targets -- -D warnings

# sidecar staging（构建 omt 第二 sidecar 后）
cd apps/desktop && node scripts/copy-sidecar.mjs --dev

# 前端类型检查 + 测试
pnpm typecheck
pnpm test
```

## 已知未关闭项（不阻塞 UI 开发）

| ID | 内容 | 归属 |
|---|------|------|
| F-U2-17 | 测试模块拆分（executor.rs ~600 行测试） | 设计/人工 |
| F-U2-6 | 事件 envelope 统一 seq/ts | U5 前 |
| F-U2-18 已修但 F-U3-9 行翻转的实际编辑需复核 | findings-log :85/:99 | 本 handoff 的读者确认 |
| F-U15-3 | 双开手工冒烟 | P0 桌面冒烟 |

## 失败教训（不要重蹈）

1. **自评不能替代独立评审**：§9 自评说"无新增 finding"——独立评审找出 2 个回归 + 4 个漏检 P1。每轮修复后必须跑独立评审。
2. **日志虚假关闭**：首轮标记 F-U2-15/18 为 fixed 时未执行声称的编辑——独立评审全部命中。修复后必须确认代码与日志声明一致。
3. **透传忘改**：stop_requested 删除后 `stop_reason()` 采样/收割窗口的竞态被独立评审命中——删除字段后必须重新审计所有消费方。
4. **`to_lowercase()` → `to_ascii_lowercase()`**：多字节字符的字节偏移错位是真实的脱敏绕过向量。

## 已安装的相关 skill

- `ce-work` — 实施计划中的单元
- `ce-code-review` — 每个实施单元后的代码评审
- `ce-simplify-code` — 阶段性简化（≥30 行变更时）

```text
/ce-handoff resume /Users/robertq/Documents/development/ts/dsh-plugins/oh-my-ticket/docs/plans/2026-09-07-desktop-agents-ui-wave-handoff.md
```
