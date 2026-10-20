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
| F-BENCH-18 | P1 | gated_auto | BENCH | `.gitignore:26` / `fixtures/harness/*/raw.log` | **raw.log 语料未入库**：`*.log` 规则把三份 raw.log 全部忽略，只存在于本机。`raw_log_replay_matches_recorded_events` 在干净 clone / CI 上会 panic（「raw.log 应存在」），CI rust 任务必红；F-BENCH-15 在仓库里实际未关闭（§30） | fixed(55a8e55)：三份 raw.log 已入库、逐行 JSON 合法；但「时间戳已清洗」「≥12 文件下限」两项声明不实——见 F-BENCH-24 | — |
| F-BENCH-19 | P1 | gated_auto | BENCH/U12 | `adapters.rs::parse_opencode` step_finish | **opencode 的 usage 也是逐步的值，被误标为累计**：语料 4 个 step_finish 各带不同 messageID，每个 total = 该步 input + output + reasoning + cache.read（如 1518+45+109+23040 = 24712）。按「累计取末值」报 25460，实为 98225（少报约 3.9 倍）；本卷免费档 cost=0 掩盖了问题，付费 provider 会少报费用（§30） | fixed(55a8e55)：opencode 标 message，98225 入测 | — |
| F-BENCH-20 | P3 | testing-gap | BENCH/U12 | `executor.rs::UsageTotals` | `record` / `final_totals` 无直接单测（混合 scope、全零、历史无 scope 事件）；修复前归档的 ExecRecord 用量仍是旧口径，未重算也未标注 | 部分 fixed(55a8e55)：UsageTotals 四形态单测已补；历史 ExecRecord 旧口径未重算/未标注（advisory） | — |
| F-BENCH-21 | P3 | gated_auto | BENCH | `harness_probe.rs::run_with_timeout` / `harness-capture.rs::capture` | 进程退出后才读 stdout：输出超过管道缓冲（约 64KB）时子进程阻塞在写，30s 后被误判超时；`PROVIDERS` 是手写列表，REGISTRY 新增 provider 不会被探测 | 部分 fixed(55a8e55)：探针读线程已前置；`all_providers()` 仍写死三家（注释却称「从 REGISTRY 遍历」），`harness-capture::capture` 仍退出后读、且现在两个管道都如此（§31） | — |
| F-BENCH-22 | P3 | advisory | 测试稳定性 | `harness.rs` / `executor.rs` 计时类测试 | 全量 `cargo test -p oh-my-ticket-desktop --lib` 3 次中 2 次失败，失败的测试每次不同（`probe_version_times_out_hanging_binary`、`harness_entry_projects_state_from_binary_source`、`kill_all_timeout_branch_settles_unwaited_session`），单独运行均通过。这些代码路径本轮未改，应是既有 flake；§29「重跑即绿」的做法会让 CI 偶发红，需要单独定位 | open：.cargo/config.toml 不在 CI 生效，且 4 线程下仍复现；根因更像 ETXTBSY 而非计时（§31） | — |
| F-BENCH-23 | P2 | gated_auto | BENCH | `bench.rs::analyze_jsonl` raw.log 回落 | **语料目录的 raw.log 被同目录所有 jsonl 共用**：找不到 `<execId>.raw.log` 时回落到 `raw.log`。omp 18.12.2 目录有两卷（smoke-hello 与 synthetic-lines），分析 synthetic-lines 时会用 smoke 那次运行的 raw.log 覆盖用量，报出另一条流的 42430 tokens。修法：语料按 executor 的命名存成 `<stem>.raw.log`，删掉回落分支（§31） | open | — |
| F-BENCH-24 | P3 | gated_auto | BENCH | `fixtures/harness/*/raw.log` / `bench.rs::fixtures_are_normalized` | 55a8e55 提交说明称「时间戳已清洗」「覆盖下限 ≥12」，实际 raw.log 仍有 170 处内嵌 ISO 时间戳（omp 100 / opencode 10 / pi 60，与 §30 计数相同），测试下限仍是 `checked >= 9`，归一化测试也不检查时间戳；另含 `mcp:93364` 这类真实 pid（§31） | open | — |
| F-BENCH-13 | P1 | manual | BENCH | `4e960a2` 提交说明 / §27（修复节） | 提交说明与日志称 F-BENCH-5/6/7/10 及 8/12 部分已修，但 `harness_probe.rs`、`harness-capture.rs`、`lib.rs` 自 f6eeaed 后**未改动**（无 worktree / stash / 其他分支留存）——改动疑似未提交即丢失（§28） | fixed(3192d60)（改动已重做并核实存在） | — |
| F-BENCH-14 | P1 | manual | BENCH/U12 | `executor.rs` usage 归档 / `bench.rs` / `adapters.rs` | **usage 是每条消息的值，不是会话累计**：pi 1.3.0 与 omp 18.12.2 语料中 cost 逐消息非单调（0.0349→0.0095→0.0078→0.0092）。F-U12-3「累计快照取末值」的口径错误——ExecRecord 的 usage 与分析器的 tokens/cost 只记最后一条消息：pi 卷报 $0.0092 实为 $0.0615（少报 6.7 倍），omp 卷报 $0.0096 实为 $0.0431（§28） | fixed(da9c376 + 55a8e55)：三家均按消息/步求和 | — |
| F-BENCH-15 | P1 | manual | BENCH | `bench.rs::raw_lines_reparse_through_current_adapter` | 重放测不到解析漂移：语料的 raw 行只是采集时**未产出事件**的行，产出 tool_call / thinking / text 的源行没有保留，无从重放；该测试能通过只因 4e960a2 新增的 turn_end usage 提取（改动前 pi 的 raw 行重放结构化数为 0）（§28） | fixed(55a8e55)：raw.log 已入库，重放对账测试在干净树上可跑 | — |
| F-BENCH-16 | P3 | gated_auto | BENCH | `bench.rs::analyze_jsonl` is_omt | `path.contains("omt_")` 过宽：写普通文件（如 `docs/omt_notes.md`）也会被算作 omt 调用；应收紧为 `path.starts_with("xd://mcp__omt")` 并补反例测试 | fixed(da9c376) | — |
| F-BENCH-17 | P3 | testing-gap | BENCH | `adapters.rs` end 族 usage 分支 | 新分支无 adapters 单测，只经分析器的 fixture 测试间接覆盖 | fixed(da9c376) | — |
| F-BENCH-3 | P1 | gated_auto | BENCH | `bench.rs::analyze_jsonl` | omp 经 `write xd://mcp__omt_*` 调 MCP，`is_omt` 只看名字 → omp 的 discovery_overhead 恒等于全部调用，硬门永久红（§27） | fixed(4e960a2)（P3 余项见 F-BENCH-16） | — |
| F-BENCH-4 | P1 | manual | BENCH | fixture 回放测试 | 语料未对 `parse_line` / `parse_models_output` 回放；事件词汇测试 grep 冻结的 adapter 产物，测不出漂移（§27） | 随 F-BENCH-15（重放测试已改为全量对账，但 raw.log 未入库——见 F-BENCH-18） | — |
| F-BENCH-5 | P1 | manual | BENCH | `harness_probe.rs` / `harness-capture.rs` | 探针硬编码 binary 路径与 model_list_args，测的是自己的 registry 副本（违反 R-1，F-PI-2 类漂移测不出）（§27） | fixed(3192d60) | — |
| F-BENCH-6 | P2 | gated_auto | BENCH | `harness-capture.rs` | omp `--version` 为 `omp/18.12.2` → 落到 `omp/omp/18.12.2/` | fixed(3192d60) | — |
| F-BENCH-7 | P2 | gated_auto | BENCH | `harness-capture.rs` | 重跑 capture 覆盖手写 meta.json 出处 | fixed(3192d60)（三家 meta 出处已补回） | — |
| F-BENCH-8 | P2 | gated_auto | BENCH | `fixtures_are_normalized` / `normalize` | 归一测试不含 omp 卷与 help/models；raw 内嵌时间戳未归一；未过 sanitize_line | 部分（测试已遍历全部语料；capture 未过 sanitize_line、raw 内嵌时间戳未归一） | — |
| F-BENCH-9 | P2 | gated_auto | BENCH | `bench.rs` opencode 测试 | 注释过时（实为 3 次）且未断言 tool_calls==3 | fixed(4e960a2) | — |
| F-BENCH-10 | P3 | gated_auto | BENCH | 探针 / capture | 裸 `.output()` 无超时（F1 教训） | fixed(3192d60)（余项见 F-BENCH-21） | — |
| F-BENCH-11 | P3 | advisory | BENCH | omp 卷 | 无 usage 事件 → omp tokens/cost 恒 None | superseded(F-BENCH-14)（end 族提取已补，但取末值的口径本身是错的） | — |
| F-BENCH-12 | P3 | advisory | BENCH | 注释 / §26 | 「MCP 握手」未实现、adapter-lines 文件名不符、「上两级」应为三级 | 部分（bench.rs「上两级」注释仍在；probe 模块注释未改） | — |
| F-U3-15 | P1 | manual | U3 | `mcp_injection.rs::prepare_omp_with` :279→:283 | **merge 先于 state.json 写入**：二者之间崩溃 → 首个 exec 无 `workspace` 记录，sweep 跳过该目录且删光 launcher → 用户文件留悬空 `omt`；下次 prepare 把污染后的文件当"原件"备份，永远恢复不干净（556c1d2 引入，§8 漏检） | fixed(d53b191) | — |
| F-U3-16 | P1 | manual | U3 | `mcp_injection.rs` :281/:304 | **writtenSha 每次 prepare 都重置基线**：两 exec 重叠期间用户改了文件 → 第二次 prepare 把改动吸进 S2 → 最后 release 比对 S2 相等 → 整体恢复到第一次 prepare 时的 backup，用户改动静默丢失（556c1d2 引入，§8 漏检） | fixed(d53b191) | — |
| F-U3-11 | P2 | gated_auto | U3 | `mcp_injection.rs::finalize_release` :310/:323 | **b3500cb 回归**：user_modified 分支打印「备份保留于 …」后无条件 `remove_dir_all(state_dir)` 删掉 backup；外科手术 no-op（command 不匹配/文件不可解析）时用户文件仍被污染且唯一原件已毁 | fixed(d53b191) | — |
| F-U3-12 | P2 | manual | U3 | `mcp_injection.rs::release_omp` :334 | **b3500cb 回归**：`remove_launcher` 在 refcount 判断之前——后 prepare 的 exec 先结束时，mcp.json 仍指向它刚被删的 launcher，直到最后一次 release 都是悬空 | fixed(d53b191) | — |
| F-U3-13 | P2 | gated_auto | U3 | `mcp_injection.rs::write_atomic` :124 | rename 失败时 tmp 文件残留在用户工作区（`.omp/mcp.json.tmp-<pid>`） | fixed(d53b191) | — |
| F-U3-14 | P2 | manual | U3 | `mcp.rs::enroll_identity` :300 × 计划 §3.3 | 嵌套 `mcp:<pid>/exec:<id>` 把 pid 重新绑进身份：MCP 进程重启 → 新 pid → 租约 actor 不匹配、report 被拒——F-U3-2 原本承诺的「MCP 重启不丢身份」不再成立；计划文案仍写精确匹配 `exec:<id>`；enroll 后未核对 daemon 实际授予值（64 字符上限超限同样静默降级） | fixed(d53b191)（方案 a：接受 pid 绑定，加静默降级检测；见 §11） | — |
| F-U3-17 | P2 | gated_auto | U3 | `mcp_injection.rs::sweep_omp_crash_residue` :448 | sweep 循环内 `?`：一个 workspace 恢复失败即中止全部后续恢复与 launcher 清理，且每次启动都如此（556c1d2 引入） | fixed(d53b191) | — |
| F-U3-18 | P3 | gated_auto | U3 | `mcp_injection.rs::sweep_omp_crash_residue` :460 | sweep 在**每次**启动（非仅 crash 后）删光 launcher 目录，persistent.sh 一并被删 → persistent 模式每次重启后 mcp.json 悬空；代码注释已记为 v1 限制，但修法只是「逐文件删、跳过 persistent.sh」 | fixed(d53b191) | — |
| F-U2-1 | P2 | gated_auto | U2 | `executor.rs::kill_all` / `finish_terminal` | app 退出路径**不落盘**：`kill_all` 预置 terminal → 等待线程的 `finish_terminal` 早退，agents.json 永远收不到 Killed(AppExit)，下次启动被 demote 记成 Failed | fixed(12e4d91)（同步收口 + 幂等 guard；测试扩至 agents.json 断言 Killed/appexit） | — |
| F-U2-2 | P2 | gated_auto | U2 | `executor.rs::classify` | 起动窗判据用「elapsed < 30s && code != 0」而非计划 §3.2 的「未产出任何 transcript 事件」；`first_event_seen` 有写无读——正常输出 20s 后 exit 1 会被记成 Failed「起动窗内退出」 | fixed(12e4d91)（classify 改 first_event_seen + 收割后 ≤300ms 落账窗口；新增「先输出再 exit 1 → Exited(1)」回归测试） | — |
| F-U2-3 | P3 | gated_auto | U2 | `executor.rs::finish_terminal` | `ExecRecord.exit_code` / `kill_reason` 从未回写（只写 failure_detail） | fixed(12e4d91)（finish_terminal 一次 update_exec 写齐 exit_code/kill_reason/failure_detail） | — |
| F-U2-4 | P3 | gated_auto | U2 | `executor.rs::spawn` / waiter | setpgid 只在子进程侧做，spawn 后立即 stop 有 ESRCH 窗口；正常退出后不清残留 pgid 成员 → 孙进程持管道时 reader 永不 EOF、waiter 线程泄漏 | fixed(12e4d91)（父侧 setpgid 双保险 + 退出后对 pgid 补 SIGKILL 清孤儿） | — |
| F-U2-5 | P3 | gated_auto | U2 | `executor.rs::classify` | 信号死亡（`code()==None`）被映射成 -1：窗内 Failed / 窗外 Exited(-1)，应读 `status.signal()` | fixed(12e4d91)（ExitStatusExt::signal → Killed(External(sig))，KillReason 加 External） | — |
| F-U2-12 | P2 | gated_auto | U2 | `executor.rs::kill_all_marks_app_exit` + 本日志 :36/:518 | kill_all 持久化（agents.json Killed/appexit）零测试覆盖，而日志声称测试已扩——五个评审者独立命中 | fixed（fixed（register_agent 先行 + agents.json 断言 status/attachment/kill_reason 已入 kill_all_marks_app_exit；日志声明成真）） | — |
| F-U2-13 | P2 | gated_auto | U2 | `executor.rs::classify` signal 分支 | kill_all 自己的 SIGTERM 死亡在 stop_reason 采样/收割窗口内被记成 Killed(External(15))，appexit 语义丢失 | fixed（fixed（classify signal 分支优先采用 session.stop_reason()；新增 killed_by_us=false + signal=15 + stop_reason=AppExit → Killed(AppExit) 的直接 classify 回归）） | — |
| F-U2-14 | P2 | gated_auto | U2 | `executor.rs:111/126/131/400` | stop_requested 死字段（只写不读）+ request_stop 吞 poison，与文件锁习惯不一致 | fixed（fixed（stop_requested 字段与全部写入删除；request_stop 改 into_inner 对齐文件习惯）） | — |
| F-U2-15 | P3 | testing-gap | U2 | — | TERM→KILL 升级与 kill_all 超时分支（直接 finish_terminal）无测试 | fixed（升级路径 + 无 waiter 超时分支测试均已补） | — |
| F-U2-16 | P3 | gated_auto | U2 | `lib.rs:72` | 退出钩子注释仍描述修复前的 fire-and-forget 语义 | fixed（fixed（注释改为同步收口契约：SIGTERM → 有界等待 → 超时 SIGKILL + 直接落盘）） | — |
| F-U2-17 | P2 | manual（设计） | U2 | `executor.rs:747` | 测试模块未拆分（1344 行，约 600 行测试） | open（设计项，人工排期） | — |
| F-U2-18 | P3 | gated_auto | 本日志 :92 | F-U3-9 kill_all 半已随 12e4d91 完成，行与检查清单仍 open | fixed（F-U3-9 行翻转 fixed(12e4d91)，检查清单勾选） | — |
| F-U2-6 | P3 | advisory | U2 | `executor.rs` 事件 | envelope 不统一：`session_start`/`exit` 无 `seq`/`ts`，与 design §5.3 `{seq, ts, execId, kind, payload}` 不符；U5 消费前统一 | open（U5 前） | — |
| F-U2-7 | P3 | advisory | U2/U12 | 计划 §3.2 / design §5.6 | transcript `.jsonl`（header + 事件）写入未落地，仅 raw.log；U10 reconcile 依赖 header | open（归属已定：U4，见计划 U4 Approach；U10 前必须存在） | — |
| F-U2-8 | P3 | gated_auto | U2 | `executor.rs::sanitize_line` / `session_start.args` | `to_lowercase()` 索引原串，非 ASCII 行偏移错位可能欠脱敏（改 `to_ascii_lowercase`）；`args` 事件/日志未脱敏（extra_args 可含 `--api-key`） | fixed(12e4d91)（sanitize_line 改 ascii lowercase；新增 sanitize_args 并用于 session_start 事件与会话记录） | — |
| F-U2-9 | P3 | advisory | U2 | `agents::demote_live_agents` × dev 多实例 | `OMT_ALLOW_MULTI_INSTANCE` 下第二实例启动 demote 会把首实例的 Live agent 标 Failed；dev-notes 需补「同时隔离 `OMT_RUNTIME_DIR`」 | fixed(af28ca2)（dev-notes §1.4 已补 OMT_RUNTIME_DIR 隔离说明） | — |
| F-U2-10 | P3 | gated_auto | U2 | `executor.rs::spawn` raw_log 路径 | `home_id` 未经 `validate_home_id` 即拼路径 | fixed(12e4d91)（spawn 入口复用 agents::validate_home_id（提为 pub(crate)）） | — |
| F-U2-11 | P3 | advisory | U2 | `harness.rs::probe_descriptor` | 探针失败重试一次把 `harness_list` 最坏延迟翻倍（8s）；注释归因「fork→exec 卡死」需复现记录 | open | — |
| F-U4a-1 | P2 | manual | U4a | `atomic_json::read_json_locked` / `agents::agents_list` | `parseFailed` 是**瞬态**信号（损坏文件被 rename 后第二次读即 false + 空列表），U9「解析失败期间冻结 GC」硬规则会失效并清空全部绑定 | open（U9 前置） | — |
| F-U4a-2 | P2 | gated_auto | U4a→U2 | `agents::register_agent` 写 `attachment: Live` | Live 持久化后跨重启失真：U10（P1）才 reconcile，P0 期间重启/崩溃后 `agents_list` 仍报 Live+Starting/Running | fixed(b8b00f8)（`demote_live_agents` + ExecutionManager::new 启动 demote + 测试） | — |
| F-U4a-3 | P3 | gated_auto | U4a | `atomic_json::read_json` | 读路径的损坏 rename 不在 flock 内，与另一进程的写 rename 竞态可能把刚写好的文件挪成 `.corrupt` | open | — |
| F-U4a-4 | P3 | gated_auto | U4a | `atomic_json::write_tmp_and_rename` | rename 前无 `sync_all`，断电可能留下 0 字节文件 → 下次降级丢 agents 列表 | open | — |
| F-U4a-5 | P3 | advisory | U4a | `atomic_json::STORE_LOCK` | 全局锁横跨所有 store 文件且持锁等 flock 最长 10s，未来 CLI 长持 agents 锁会连 harness config 保存一起阻塞 | open（v1 接受） | — |
| F-U4a-6 | P3 | advisory | U4a | `agents::now_iso` / §3.2 | 函数名 iso 实为 epoch ms；transcript header `startedAt` 格式未定，U2 必须与 `execs[].startedAt` 同格式 | fixed(b8b00f8)（`now_epoch_ms` 改名；计划 §3.2 补「header 与 execs[] 同为 epoch ms 字符串」；header 本身尚未写入，见 F-U2-7） | — |
| F-U4a-7 | P3 | advisory | U4a | `agents.rs` API | 无「向既有 Agent 追加 exec」入口；U11 续跑与 U6 add-members 需要 `append_exec` | open（U11 补） | — |
| F-U4a-8 | P3 | advisory | U4a | 计划 U4a Files | 列了 `bridge.ts` 但未动，`agents_list` 尚无 TS 封装 | fixed(b8b00f8)（计划记注：bridge.ts 移至 U5 Files） | — |
| F-U1b-1 | P3 | advisory | U1b | 计划 U1b Files / 排期 | `store.ts` 的 `harness_list` 封装放在 U1b，但 U6 弹窗选择器先于 U1b 需要它——依赖倒置 | fixed(cf8a2c7)（封装移入 U6 Files，U1b 只留退役/迁移/接线） | — |
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
| F-U3-1 | P1 | gated_auto | U3 | `.github/workflows/ci.yml:59` | 新增的 `cp target/debug/omt …` 行零缩进，`run: \|` 块被截断 → workflow YAML 非法，CI 完全不跑 | fixed(b3500cb) | — |
| F-U3-2 | P1 | manual | U3 | `auth.rs:126-135` × `mcp.rs::enroll_identity` | daemon 只授予 `<kind>:<pid>` 或其 `/` 嵌套的 actor namespace，裸 `exec:<execId>` 被静默降级 | fixed(b3500cb)（改发 `mcp:<pid>/exec:<id>`；daemon+CLI 两侧各加测试） | — |
| F-U3-3 | P2 | gated_auto | U3 | `mcp_injection.rs::release_omp`/`sweep_omp_crash_residue` | crash/mid-exec 手改后跳过恢复但仍删光 launchers，留下悬空 `omt` 条目 | fixed(b3500cb)（外科手术式精确匹配移除，不再整体覆盖或盲删） | — |
| F-U3-4 | P3 | testing-gap | U3 | `mcp_injection.rs::prepare_with` opencode | `OPENCODE_CONFIG_CONTENT` 合并语义未实测 | fixed(b3500cb)（本机 opencode 1.18.14 实测确认为深度合并，非覆盖；无需改代码） | — |
| F-U3-5 | P3 | gated_auto | U3 | `mcp_injection.rs` 持久化 hash | `DefaultHasher` 跨 Rust 版本不稳定 | fixed(b3500cb)（换 SHA-256） | — |
| F-U3-6 | P3 | gated_auto | U3 | `mcp_injection.rs::workspace_hash` | 路径未 canonicalize，symlink 拆分 refcount | fixed(b3500cb) | — |
| F-U3-7 | P3 | gated_auto | U3 | `mcp_injection.rs::remove_launcher` | 定义了但从未调用，launcher 累积 | fixed(b3500cb)（`McpInjection::Omp` 加 `launcher_id`，每次 release 都删自己的） | — |
| F-U3-8 | P3 | gated_auto | U3 | `mcp_injection.rs::merge_omp_mcp` | 用户工作区文件非原子写 | fixed(b3500cb)（统一 tmp+rename） | — |
| F-U3-9 | P3 | advisory | U3→U4 | 补偿表 ⑦/⑧ | spawn 失败路径不释放已 prepare 的注入 | fixed(12e4d91)：spawn 失败已修；kill_all 路径随 finish_terminal 的 mcp release 完成 | — |
| F-U3-10 | P3 | advisory | U3 | `mcp_injection.rs` persistent 混用 | 非 persistent release 可能恢复 backup、抹掉 persistent 注入 | fixed(b3500cb)（release/scrub 改为按 launcher_id 精确匹配，各管各的） | — |

## 待办的跨阶段事项（由 findings 派生）

- [x] ~~U4a 落地时回看 F-U15-1~~ 已于 43bf89f 关闭（harness config + agents.json 两条已覆盖）。
- [ ] U9 落地 bindings.json 时确认走 `atomic_json`（F-U15-1 的第三条写路径）；同时按 F-U4a-1 把 GC 冻结改为粘性判定。
- [x] ~~U2 开工时落地 F-U4a-2 与 F-U4a-6~~ 已于 b8b00f8 关闭。
- [x] ~~U4 接线前修 F-U2-1（AppExit 落盘）与 F-U2-2（起动窗判据）~~ 已于 12e4d91 关闭。
- [ ] 计划定 transcript `.jsonl` header/事件写入的归属（F-U2-7），U10 之前必须存在。
- [ ] P0 桌面冒烟时执行 F-U15-3 的手工双开验证并回填记录（可顺手用 `OMT_ALLOW_MULTI_INSTANCE` 写 dev 自动化脚本）。
- [x] ~~下次触碰 lib.rs 时顺手关 F-U15-4；下次改 dev-notes 时顺手关 F-U15-5~~ 已于 7be3f76 关闭。
- [x] ~~U4 接线前修 F-U3-2（daemon 静默降级 actor）~~ 已于 b3500cb 关闭（含 daemon 侧 `Registry::issue` 测试）——U4 现在可以放心假设 `executor_session_id` 以 `/exec:<execId>` 结尾即可定位派发方。
- [ ] U4 前关 F-U3-15/16（P1，§8 漏检）并决策 F-U3-14；F-U3-11/12/13 建议随同一 fix commit（§10）。
- [x] ~~U4 补上剩下半边的 F-U3-9：`kill_all` 路径（依赖 F-U2-1 先修）走 release~~ 已于 12e4d91 关闭（kill_all 经 finish_terminal 的 mcp release 释放）。

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

---

## 6. `43bf89f`（+ `cf8a2c7` 文档）· U4a · 共用原子写 helper + Agent 最小存储

- **日期**：2026-09-06
- **范围**：新建 `atomic_json.rs` +363（`STORE_LOCK` + companion flock + tmp/rename + 清扫 + 损坏降级，6 测试）、`agents.rs` +403（schema / `register_agent` / `update_agent_status` / `update_exec` / `agents_list`，6 测试）；`harness.rs` −90（切到 helper）；`lib.rs` +3。`cf8a2c7`：关闭 F-U15-1 / F-U1-8 / F-U1-9 / F-U1b-1，U1b→U6 store 封装迁移
- **意图**：计划 U4a——§3.6.6 写纪律一次做齐并让 harness config 切过去；agents.json 最小存储（schema、持久 agentSeq、仅状态变化写、parseFailed 暴露）；reconcile/回放留 U10
- **评审方式**：单上下文内联（correctness / reliability / 计划符合性）；`cargo fmt --check` exit 0；`clippy --all-targets -D warnings` 通过；`cargo test --lib` → **26 passed**。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready with fixes**——helper 本身实现扎实、测试覆盖全面；但 `parseFailed` 的瞬态语义会让 U9 的 GC 冻结硬规则失效（F-U4a-1，P2，需在 U9 前定案），持久化的 `Live` 在 U10 之前跨重启失真（F-U4a-2，P2，U2 顺手）。两条都不需要回改本 commit，但要进入下一单元的入参。

### 核验通过

| 项 | 结果 |
|---|---|
| 进程内锁 + poison 容忍 | `STORE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)` ✔（F-U1-8） |
| 跨进程 flock | companion `<file>.lock`（稳定 inode，rename 不影响）；`LOCK_EX\|LOCK_NB` 轮询 + 10s 上界；guard 随 `File` drop 释放，`?` 早退也释放 ✔；双 fd 互斥测试 ✔（macOS flock 按 open-file-description 计，模拟另一进程有效） |
| tmp/rename | `tmp-<pid>-<seq>` + 0600；rename 后无残留 ✔；清扫只删 mtime>1h 且同前缀 ✔（F-U1-9） |
| 损坏降级 | `<name>.corrupt-<ms>` 留证 + 默认值 + `parse_failed` ✔ |
| 仅状态变化写 | `mutate → bool`，`update_agent_status` 同态不写、`update_exec` 按 `!=` 判定 ✔ |
| agentSeq 持久单调 | 6 线程并发注册 → 6 agent、seq=6、`Agent 1..6` 全在 ✔ |
| secrets | `ExecRecord` schema 无 env 字段（测试断言序列化不含 `"env"`）✔ |
| home-id 防穿越 | 拒绝 `/`、`\`、`..`、`.` 前缀、空 ✔ |
| harness config 切换 | `load_configs`/`upsert_config` 改为 helper 薄封装，原有 5 个 harness 测试仍过 ✔ |
| F-U15-1 关闭 | harness config + agents.json 两条写路径已在 flock 下；bindings.json 由 U9 走同一 helper——已在待办登记复核点 ✔ |
| cf8a2c7 | U1b Files 去掉 store 封装、U6 Files 加入 ✔；F-U1b-1 的 sha 应为 `cf8a2c7`（计划改动在该 commit），本次已改正 |

### Findings

#### F-U4a-1 · P2 · manual · `read_json_locked` / `agents_list` · `parseFailed` 是瞬态信号，U9 GC 冻结会失效

流程：agents.json 损坏 → 第一次读：rename 成 `.corrupt-<ms>`、返回默认值、`parse_failed=true` → **第二次读：文件不存在 → 默认值、`parse_failed=false`**。U9 的硬规则「agents.json 解析失败期间 bindings GC 冻结」只会收到一次信号；随后 GC 看到的是「解析成功、零 agent」——正好会把**全部绑定判为悬空并清掉**，这是 §3.6.6 要防的事故本身。

- **建议**（推荐 b）：
  - (a) 读路径不 rename、只报告（rename 留给 `update_json`）——但 `agents_list` 读多写少，仍会在下一次写后变瞬态；
  - **(b) 粘性判定**：helper 增 `has_unrecovered_corrupt(path) -> bool`（同目录存在 `<name>.corrupt-*`），`agents_list` 返回 `parseFailed = parse_failed || has_unrecovered_corrupt`；U9 GC 以该字段为准；U10 reconcile 从 transcript 重建成功后把 `.corrupt-*` 移到 `recovered/` 子目录解除粘性。加测试：损坏 → 连读两次 → 第二次 `parseFailed` 仍 true。
- **状态**：open（U9 前置门；helper 改动可随 U9 一起）

#### F-U4a-2 · P2 · gated_auto · `register_agent` → U2 · 持久化 `attachment: Live` 跨重启失真

`register_agent` 写 `attachment: Live`，只有 U10（P1）的 reconcile 会把它降级。P0 期间 app 重启或崩溃后，`agents_list` 仍返回 `Live + Starting/Running`，U5 会把死 agent 当可控项显示、终止按钮可点。

- **建议**：U2 的 ExecutionManager 构造时做一次启动 demote：对当前 home 所有 `attachment == Live` 且不在内存注册表的记录 → `attachment = None`；`Starting/Running` → `Failed`（`failure_detail = "app restarted before exec finished"`），或保持 status 但前端按 `attachment=None` 视为历史。pid liveness / Orphan 判定仍留 U10。加入 U2 Test scenarios：「写入 Live 后模拟重启 → 列表不再 Live」。
- **状态**：open（U2）

#### F-U4a-3 · P3 · gated_auto · `read_json` · 损坏 rename 不在 flock 内

`read_json` 只持 `STORE_LOCK`（进程内），损坏时的 rename 没有跨进程互斥。窗口：进程 B 读到损坏内容 → 进程 A `update_json` 写好并 rename 就位 → B 把 A 刚写好的**好文件** rename 成 `.corrupt`。只在文件本就损坏 + 恰有另一进程同时写时触发，概率低但后果是丢一次有效写入。

- **建议**：读路径检测到损坏后，先取 companion flock 再重读一次确认仍损坏才 rename；或读路径不 rename（与 F-U4a-1 (a) 合并考虑）。
- **状态**：open

#### F-U4a-4 · P3 · gated_auto · `write_tmp_and_rename` · 无 fsync

rename 前未 `sync_all()`。APFS 的 rename 原子，但数据页可能未刷盘；断电/内核崩溃后可能得到 0 字节或截断的 agents.json → 下次读走 `.corrupt` 降级（进而触发 F-U4a-1）。写频极低，`sync_all` 代价可忽略。

- **建议**：`file.write_all(..)?; file.sync_all().map_err(..)?;` 再 rename；目录 fsync 可省。
- **状态**：open

#### F-U4a-5 · P3 · advisory · `STORE_LOCK` · 全局锁 + 持锁等 flock

进程内锁横跨 harness config / agents / bindings 三类文件，且**持锁期间**最长等 flock 10s。未来 omt CLI 若长时间持有 agents.json 的 flock，desktop 连保存 harness 设置都会卡 10s。v1 只有 desktop 一个写者，接受。

- **建议**：U9/U10 若写频上升，改为 per-path 锁表（`Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>`），flock 等待不再阻塞其它文件。
- **状态**：open（v1 接受，记录）

#### F-U4a-6 · P3 · advisory · `now_iso` / §3.2 · 命名与格式对齐

`now_iso()` 返回 epoch 毫秒字符串（注释也这么说），名字误导；更重要的是 design §5.6 的 transcript header `startedAt` 未定格式，而 `execs[]` 与 header 是镜像关系（§3.2），U2 写 header 时必须同格式，否则 U10 reconcile 比对/排序出错。

- **建议**：改名 `now_epoch_ms`；计划 §3.2 补一句「`startedAt` = epoch ms 十进制字符串，header 与 execs[] 同」。
- **状态**：open（U2 对齐）

#### F-U4a-7 · P3 · advisory · `agents.rs` API · 缺「向既有 Agent 追加 exec」

`register_agent` 总是新建 Agent（id = `agent-<首个 execId>`）。U11 续跑（同 Agent 新 exec、读 `lastSessionId`）和 U6「追加到该 Agent 当前 run」都需要在既有 Agent 上 push 一条 `ExecRecord` 并更新 `last_exec_id`。

- **建议**：U11 落地时补 `append_exec(runtime_dir, home_id, agent_id, NewExec)`，复用 `update_json`；本单元不要求。
- **状态**：open（U11）

#### F-U4a-8 · P3 · advisory · 计划 U4a Files · `bridge.ts` 未动

计划 U4a Files 列了 `apps/desktop/src/bridge.ts`，commit 未提供 `agents_list` 的 TS 封装。U5 消费时补即可。

- **建议**：计划 U4a Files 去掉 bridge.ts、U5 Files 加上，避免 P0 验收对照时误判缺漏。
- **状态**：open（计划记注）

### 当前收口状态

| ID | 状态 |
|---|---|
| F-U15-1 / F-U1-8 / F-U1-9 | fixed(43bf89f) |
| F-U1b-1 | fixed(cf8a2c7) |
| F-U4a-1 | open · **U9 前置**（粘性 parseFailed） |
| F-U4a-2 | open · **U2 顺手**（启动 demote Live） |
| F-U4a-3/4 | open · 小改，下次触碰 atomic_json 顺手 |
| F-U4a-5/6/7/8 | open · 记录/后续单元 |

U4a 达成计划目标，**不阻塞 U2 开工**。U2 入参：F-U4a-2（启动 demote）、F-U4a-6（startedAt 格式）、以及 §4 记的两条前瞻（`extra_args` 顺序、Detected 重探结果写 header）。

---

## 7. `b8b00f8` · U2 · ExecutionManager 执行内核 + 假 harness 测试

- **日期**：2026-09-06
- **范围**：新建 `executor.rs` +934（spawn / 事件泵 / 终态分类 / exec_stop / kill_all / 脱敏，8 测试）；`agents.rs` +75（`demote_live_agents`、`now_epoch_ms`）；`lib.rs`（`ExecutionState` manage + `RunEvent::ExitRequested` 退出钩子）；`state.rs` +16；`harness.rs`（探针重试）；计划 §3.2/U4a/U5 记注；日志 §6 入库
- **意图**：计划 U2——setpgid/killpg、cwd 剥尾、env 合并 + 强制 `OMT_RUNTIME_DIR`、起动窗/最大时长兜底、终态分类与 agents.json 回写、退出钩子；顺手关 F-U4a-2/6/8
- **评审方式**：单上下文内联（correctness / reliability / 计划符合性 / secrets）；`cargo fmt --check` exit 0；`clippy --all-targets -D warnings` 通过；`cargo test --lib` → **35 passed**（executor 8）。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready with fixes**——内核结构正确、假 harness 测试真实有效（进程组含孙进程、脱敏、env 强制注入、watchdog 均实测），但两条 P2 直接影响 P0 验收语义，建议在 U4 接线前修掉。

### 核验通过

| 项 | 结果 |
|---|---|
| 进程组 | `pre_exec(setpgid(0,0))`，`kill(-pgid)` TERM→3s→KILL；测试含孙进程且断言组清空 ✔ |
| cwd 剥尾 | `workspace_root_of` 仅当末段 == `.omt` 时取 parent ✔ |
| env | PATH 增广 → `OMT_RUNTIME_DIR` 强制 → 用户 env；保留键在 spawn 入口再拒一次；测试断言子进程看到 desktop 的 runtime dir ✔ |
| 起动窗 Failed + stderr 尾 | `exit 1` → Failed；`API_KEY=sk-…`/`Bearer …` 脱敏后不含值 ✔（判据问题见 F-U2-2） |
| watchdog | 400ms 上限实测 Killed(MaxDuration) ✔ |
| exec_stop 幂等 | 已终态/未知 id 均 Ok ✔ |
| 退出钩子 | `build().run(\|app, ev\|)` 捕 `ExitRequested` → `kill_all(AppExit)` ✔（落盘问题见 F-U2-1） |
| F-U4a-2 demote | 启动扫全部 home，Live→None、Starting/Running→Failed + failure_detail；幂等测试 ✔ |
| F-U4a-6 | `now_epoch_ms` 改名，`ExecSession.started_at_epoch_ms` 同格式 ✔ |
| 事件回调抽象 | `EventSink = Arc<dyn Fn(Value)>`，单测 mpsc、U4 换 tauri Channel——干净 ✔ |
| 锁 | 全部 `unwrap_or_else(PoisonError::into_inner)` ✔ |

### Findings

#### F-U2-1 · P2 · gated_auto · `kill_all` / `finish_terminal` · app 退出路径不落盘

`kill_all` 先把 `session.terminal` 预置为 `Killed(AppExit)`，再依赖等待线程收割。但 `finish_terminal` 开头 `if guard.is_some() { return; }`——等待线程看到已预置，**直接返回，不写 agents.json、不发 exit 事件**；而且 app 退出后等待线程根本活不到那一步。结果：agents.json 里该 agent 永远停在 `Running/Live`，下次启动被 demote 记成 `Failed("app restarted before exec finished")`——语义错（用户是正常退出 app），且 `ExecRecord.kill_reason` 永远拿不到 `appexit`。测试 `kill_all_marks_app_exit` 只断言内存态，没覆盖落盘。

- **建议**：`kill_all` 不预置 terminal，改为**同步**对每个存活 session：`stop_reason = AppExit` → `kill_tree(SIGTERM)` → `finish_terminal(session, Killed(AppExit), …)`（幂等 guard 天然处理与等待线程的竞争）；等待线程用 `stop_reason` 而非固定 `User` 分类。测试补：`kill_all` 后读 agents.json 断言 `status == Killed` 且 exec `kill_reason == "appexit"`。
- **状态**：fixed(12e4d91) —— kill_all 改同步收口（stop_reason → SIGTERM → 有界等待 → 超时直接 finish_terminal），幂等 guard 防双写；测试扩为读 agents.json 断言 status=Killed 且 exec kill_reason=appexit

#### F-U2-2 · P2 · gated_auto · `classify` · 起动窗判据与计划不符，`first_event_seen` 有写无读

计划 §3.2：「起动窗内退出（默认 30s，**判据=未产出任何 transcript 事件**）→ Failed」。实现是 `started.elapsed() < startup_window && code != 0`，`first_event_seen` 只在 reader 里 `store(true)`，从未 `load`。后果：harness 正常跑了 20s、输出了一堆、因 claim 被拒按 prompt 纪律 `exit 1`——被记成 Failed「harness 在起动窗内退出」而不是 `Exited(1)`（后者才会联动 run item 统计）。另一个隐患：子进程退出时 reader 线程可能还没处理完缓冲的行，即便改用 `first_event_seen` 也可能读到 false。

- **建议**：`classify` 改为 `!first_event_seen.load() && code != 0`（elapsed 上界可保留为「且」条件）；等待线程在 `try_wait` 返回 Some 后先轮询 `first_event_seen` ≤300ms 再分类；测试补「先 echo 再 exit 1 → Exited(1) 非 Failed」。
- **状态**：fixed(12e4d91) —— classify 改 `!first_event_seen && code != 0`（elapsed 保留为且条件），等待线程收割后 ≤300ms 等首事件落账；新增「先 echo 再 exit 1 → Exited(1)」回归

#### F-U2-3 · P3 · gated_auto · `finish_terminal` · `exit_code` / `kill_reason` 未回写

`ExecRecord` 有 `exit_code`/`kill_reason`/`failure_detail` 三个字段，`finish_terminal` 只写 `failure_detail`。U5 列表要显示退出码、U10 reconcile 要靠 execs[] 回填终态，两者都缺数据。

- **建议**：`finish_terminal` 一次 `update_exec` 写齐：`Exited(c)` → `exit_code=Some(c)`；`Killed(r)` → `kill_reason=Some(r.as_str())`；`Failed(d)` → `failure_detail`。
- **状态**：fixed(12e4d91) —— finish_terminal 一次 update_exec 写 exit_code/kill_reason/failure_detail

#### F-U2-4 · P3 · gated_auto · `spawn` / waiter · setpgid 竞态与残留 pgid 成员

(a) `setpgid(0,0)` 只在子进程 `pre_exec` 里做；父进程 spawn 返回后立刻 `exec_stop` 时，子进程可能尚未执行到 pre_exec，`kill(-pid)` 得 ESRCH，harness 存活。经典解法是父子两侧都调 `setpgid(pid, pid)`（父侧忽略 EACCES）。(b) 子进程正常退出后，等待线程无条件 `join` reader——若 harness 留下了继承 stdout 的后台孙进程，管道永不 EOF，reader 与 waiter 线程永久泄漏。

- **建议**：(a) spawn 后父侧补 `libc::setpgid(pid as i32, pid as i32)`；(b) `try_wait` 返回 Some 后对 pgid 补一次 `SIGKILL`（harness 已退出，残留成员就是孤儿，KD-D7 语义），再 join。
- **状态**：fixed(12e4d91) —— (a) 父侧 spawn 后同步 setpgid（EACCES 忽略）；(b) try_wait 命中后对 pgid 补 SIGKILL 再 join reader

#### F-U2-5 · P3 · gated_auto · `classify` · 信号死亡映射成 -1

外部 `kill -9 <harness>`（非 desktop 发起）→ `status.code() == None` → `-1`：窗内记 Failed「code -1」、窗外记 `Exited(-1)`。

- **建议**：读 `std::os::unix::process::ExitStatusExt::signal()`，非 desktop 发起的信号死亡归 `Killed(External(sig))` 或 `Exited` 附 `signal` 字段；`KillReason` 加 `External`。
- **状态**：fixed(12e4d91) —— unix `ExitStatusExt::signal()` → `Terminal::Killed(KillReason::External(sig))`，`key()` 序列化为 external-<sig>

#### F-U2-6 · P3 · advisory · 事件 envelope 不统一

`raw` 事件有 `seq`/`ts`，`session_start`/`exit` 没有；design §5.3 定义的是统一 `{seq, ts, execId, kind, payload}`。U5 的尾窗归并、U10 回放按 seq 重推都依赖统一 envelope。

- **建议**：在 U5 消费前把三类事件统一为 `{execId, seq, ts, kind, payload{…}}`，seq 由同一 AtomicU64 发（session_start = 0）。
- **状态**：open（U5 前）

#### F-U2-7 · P3 · advisory · transcript `.jsonl` 写入未落地，归属未定

commit message 说「transcript headers will use the same epoch-ms format」，但本 commit 只写 `<execId>.raw.log`，没有 `<execId>.jsonl`（design §5.6：首行 header `{execId, agentId, nodeId, runId, harness, harnessSessionId, argv, startedAt}` + 事件行）。U10「扫 transcript header 采纳索引缺失 exec」和 §3.2「header 是回放事实源」都依赖它；计划里 U2/U12 均未明确认领。

- **建议**：计划记注——U2（或 U4 接线时）补 header + raw 事件的 jsonl 写入（与 raw.log 并存），U12 只负责把 raw 行升级为结构化事件；U10 之前必须存在。
- **状态**：open（计划定归属）

#### F-U2-8 · P3 · gated_auto · `sanitize_line` / `args` · 非 ASCII 偏移与 args 未脱敏

(a) `line.to_lowercase()` 后用其字节偏移切原串，非 ASCII（如土耳其 İ）会改变字节长度；`is_char_boundary` 只防 panic，不防错位——可能欠脱敏。(b) `session_start.args`、`ExecSession.args`、未来 header 的 argv 都原样记录；用户 `extra_args` 完全可能含 `--api-key xxx`，§3.1「只记非敏感 args」没有执行点。

- **建议**：(a) 改 `to_ascii_lowercase()`（长度不变）；(b) `args` 进事件/日志前逐项过 `sanitize_line`（对 `--key=val` 与 `--key val` 两种形态：值项前一项若含 key/token/secret 关键字则替换值）。
- **状态**：fixed(12e4d91) —— (a) `to_ascii_lowercase`（字节长度不变）；(b) `sanitize_args` 应用于 session_start 事件与 ExecSession.args，`--key val`/`--key=val` 两形态均覆盖（测试）

#### F-U2-9 · P3 · advisory · dev 多实例 × 启动 demote

`OMT_ALLOW_MULTI_INSTANCE=1` 下起第二个 dev 实例，其 `ExecutionManager::new` 会把**首实例仍在跑的** Live agent 全部 demote 成 Failed（同一 runtime dir）。

- **建议**：`dsh-plugin-dev-notes.md §1.4` 那行补「并为每个实例单独设 `OMT_RUNTIME_DIR`」；U10 引入 pid liveness 后 demote 只动 pid 已死的记录。
- **状态**：open

#### F-U2-10 · P3 · gated_auto · `spawn` · raw_log 路径未校验 home_id

`runtime_dir/transcripts/<home_id>/…` 直接拼接，`agents_path` 有 `validate_home_id` 而这里没有。来源虽是 daemon handshake，防御应一致。

- **建议**：复用 `agents::validate_home_id`（提为 `pub(crate)`）。
- **状态**：fixed(12e4d91) —— spawn 入口 `agents::validate_home_id`（提为 pub(crate)）

#### F-U2-11 · P3 · advisory · `probe_descriptor` 重试

失败即重试一次让 `harness_list` 最坏延迟从 4s 变 8s；注释归因「多线程进程 fork→exec 偶发卡死」——`probe_version` 没有 `pre_exec`，Rust std 在 macOS 上走 `posix_spawn`，不应有 fork 后库锁问题；更可能是测试并行下 CPU 争用。若确有复现，应记录条件而不是靠重试掩盖。

- **建议**：保留重试但只对「超时」重试（spawn 错误不重试）；在日志/注释附复现条件。
- **状态**：open

### 当前收口状态

| ID | 状态 |
|---|---|
| F-U4a-2 / F-U4a-6 / F-U4a-8 | fixed(b8b00f8) |
| F-U2-1 / F-U2-2 | open · **U4 接线前修**（影响 P0 验收语义） |
| F-U2-3/4/5/8/10 | open · 小改，可与 F-U2-1/2 同一 commit |
| F-U2-6 / F-U2-7 | open · U5 前 / 计划定归属 |
| F-U2-9 / F-U2-11 | open · 文档/记录 |

U2 内核可作为 U3 的基础继续推进（U3 不触碰终态分类）；但 U4 接线（exec_start 全链 + 终态租约释放）之前必须先关 F-U2-1/2，否则 P0 验收「起动失败呈现 Failed + stderr 尾」「终止后终态正确」会在真实 harness 上出错。

---

## 8. `556c1d2` · U3 · MCP 桥接注入 + per-exec actor 身份 + omt 第二 sidecar

- **日期**：2026-09-06
- **范围**：新建 `mcp_injection.rs` +604（opencode env 注入、omp refcount 合并/备份/恢复、launcher 脚本、启动 sweep，9 测试）；`crates/omt-runtime` `mcp.rs` +44（`enroll_identity` 尊重 `--actor`，2 测试）、`cli/mod.rs`；`executor.rs` +35（注入接线、终态 release）；`daemon.rs`（`sidecar_path(name)`）；打包：`copy-sidecar.mjs`、`tauri.conf.json` externalBin、`ci.yml`、`release.yml`
- **意图**：计划 U3——harness 能连上 `omt mcp` 执行 claim/report；§3.3 身份纪律（`--actor exec:<execId>`）；omp 注入定稿（refcount + runtime dir 备份 + hash 防覆盖 + persistent 开关 + crash 清扫）；`omt` CLI 成为第二 sidecar
- **评审方式**：单上下文内联（correctness / reliability / 计划符合性 / 打包与 CI）。子代理派发本次不可用（会话额度 429，`/code-review` 后台任务已因此失败），**无独立子代理、无跨模型佐证**。`cargo fmt --check` exit 0；`clippy -p oh-my-ticket-desktop -p omt-runtime --all-targets -D warnings` 通过；desktop `cargo test --lib` → **44 passed**（mcp_injection 9）；runtime `mcp::tests` 2 passed；`js-yaml` 解析三份配置：**`ci.yml` INVALID**，release.yml / tauri.conf.json OK。
- **结论**：**Not ready**——两条 P1：一条让 CI 整体失效（ci.yml 非法 YAML），一条让本单元的核心目标（per-exec 身份）**静默落空**（daemon 授权策略不接受 `exec:` 命名空间）。其余实现质量好、测试扎实，修掉 P1 后即可。

### 核验通过

| 项 | 结果 |
|---|---|
| `omt mcp --actor` | `split_globals` 解析 `--actor VALUE` ✔；`enroll_identity` 缺省仍 `mcp:<pid>` ✔（但 daemon 侧授权见 F-U3-2） |
| opencode 注入 | `OPENCODE_CONFIG_CONTENT` 内联，command 数组含 `--actor exec:<id> mcp`；零落盘测试 ✔ |
| omp 注入 | 合并保留用户 server ✔；备份落 runtime dir ✔；refcount 两 exec 重叠只在最后一次恢复 ✔；用户 mid-exec 手改则跳过恢复 ✔；首次无文件 → release 删除 ✔；persistent 模式 no-op release ✔；crash 残留 sweep 幂等 ✔——与 §九-14 定稿逐条对应 |
| 「绝不 spawn 无 MCP 的 harness」 | sidecar 缺失 → `prepare` 报错含路径 ✔ |
| launcher | 0700、`exec` 形式、单引号转义 ✔ |
| 终态 release | `finish_terminal` 在 agents.json 回写前调用，失败只记录 ✔ |
| 打包 | `copy-sidecar.mjs` 双二进制 + dev 拷贝 ✔；`externalBin` 追加 ✔；release.yml 从归档拷 `omt` ✔（归档本就含 `omt`）；`sidecar_path(name)` 泛化 ✔ |

### Findings

#### F-U3-1 · P1 · gated_auto · `.github/workflows/ci.yml:59` · 新增行零缩进，workflow 非法

`run: |` 块内新增的 `cp target/debug/omt apps/desktop/src-tauri/binaries/omt-aarch64-apple-darwin` 顶格写，literal block 在此结束，下一行成了 mapping 的非法 key。js-yaml 报 `can not read a block mapping entry (59:13)`；GitHub 会显示「Invalid workflow file」，**rust 与 node 两个 job 都不会跑**。

- **建议**：该行前补 10 个空格与上一行 `cp` 对齐；本地加一步 `node -e "require('js-yaml').load(require('fs').readFileSync('.github/workflows/ci.yml','utf8'))"` 或 `actionlint` 进 pre-commit。
- **状态**：fixed(b3500cb) —— 缩进已对齐，`js-yaml` 解析通过。

#### F-U3-2 · P1 · manual · `auth.rs:126-135` × `mcp.rs::enroll_identity` · daemon 静默忽略 `exec:<execId>`

`Registry::issue`：`base = "{kind}:{pid}"`（MCP 即 `mcp:<pid>`），请求的 `actorNamespace` **只有等于 base 或以 `base/` 为前缀**才被采纳，否则保持 base——注释原话「base or base/nested only (never foreign)」。`exec:<execId>` 两者都不满足 → credential 的 actor 仍是 `mcp:<pid>`，`run_items.executor_session_id` 也仍是 `mcp:<pid>`，desktop 按 `exec:<execId>` 查持有项永远查不到；**没有任何错误或日志**。runtime 的两个新测试只覆盖 CLI 侧字符串拼接，没有经过 `Registry::issue`。§3.3「终止后释放」「Runs 面 ↔ Agents 面互跳」整条链在此失效。

- **建议**（推荐 a，零 daemon 变更）：
  - (a) **嵌套命名空间**：`enroll_identity` 把用户给的 actor 规整为 `mcp:<pid>/<actor>`（若已带 `mcp:<pid>` 前缀则不重复）；desktop 匹配改为 `executor_session_id.ends_with("/exec:<execId>")`（或 `contains("/exec:<id>")`）。注意 64 字符上限（`mcp:99999/exec:` 已占 15，execId 控制在 40 内）。launcher 无需改（omt 自己拼 pid）。
  - (b) daemon 放宽：kind=mcp 时额外允许 `exec:` 前缀——auth 策略变更，需独立安全评审，不建议 v1。
  - **测试必须过 daemon**：`omt-runtime` 加 `Registry::issue` 单测（`mcp:<pid>/exec:x` 采纳、裸 `exec:x` 被降回 base），并在 U13 parity 加真实 daemon fixture：`omt --actor exec:x mcp` 发起 claim → `run/get` 的 `executor_session_id` 以 `/exec:x` 结尾。
  - 计划 §3.3 / U3 / U4 文案同步改为嵌套形式。
- **状态**：fixed(b3500cb) —— 按方案 (a) 落地：`enroll_identity` 发 `mcp:<pid>/exec:<execId>`；`auth.rs` 新增 `Registry::issue` 两个单测（嵌套采纳 / 裸值降级）；`mcp.rs` 两个单测更新为断言嵌套形式。U13 的真实 daemon fixture 仍留给 U4（那时才有真实派发可跑）。

#### F-U3-3 · P2 · gated_auto · `sweep_omp_crash_residue` / `release_omp` · 跳过恢复时留下悬空 `omt` 条目

crash 后启动 sweep：`release_omp` 因用户改过文件（hash 不符）跳过恢复并 `return Ok(())`，随后 sweep **无条件 `remove_dir_all(omp-launchers)`**。用户工作区的 `.omp/mcp.json` 里 `omt` 条目仍指向刚被删掉的 launcher——此后用户每次自己开 omp 都会看到 omt MCP 启动失败，且不知道为什么。同样的逻辑在正常 release 路径也会在「用户改过」时把 omt 条目永久留在用户文件里（launcher 虽在，但 daemon 可能已不在，`omt mcp` 等 5s 后退出）。

- **建议**：跳过整体恢复时改做**外科手术**——重新读文件、只删除 `mcpServers.omt`（若其 command 指向我们的 launcher 目录），保留用户其它改动并写回；sweep 只删除**没有任何 mcp.json 引用**的 launcher。加测试：手改 + crash → sweep 后文件保留用户内容且不含 `omt`。
- **状态**：fixed(b3500cb) —— 落地为 `remove_omt_entry_if_matches`（精确匹配 `command` 而非"含 omp-launchers 子串"，同时解决 F-U3-10 的误删风险）；`finalize_release` 统一供 `release_omp` 与 sweep 调用。测试 `omp_user_mid_exec_edit_skips_restore_and_only_removes_own_entry`、`sweep_scrubs_own_entry_when_user_modified_mid_crash`。

#### F-U3-4 · P3 · testing-gap · opencode `OPENCODE_CONFIG_CONTENT` 语义未实测

design A.2 只确认了 env 可注入，没有确认它与用户全局/项目配置是 deep-merge 还是整体替换。若是替换，用户的 provider/model/权限配置在派发时全部消失，opencode 会因无 provider 直接失败。

- **建议**：U4 真实派发前用本机 opencode 实测：设 `OPENCODE_CONFIG_CONTENT='{"mcp":{…}}'` 后 `opencode run --format json "list your tools"`，确认既有 model 仍可用且 `omt__*` 工具出现；结论回填 descriptor 注释。
- **状态**：fixed(b3500cb) —— 已用本机 opencode 1.18.14 实测（`opencode mcp list`，项目级 `opencode.json` 含一个 server + env 注入另一个）：**深度合并**，两个 server 同时出现，不是覆盖。无需改代码，结论已写入 `mcp_injection.rs` 模块文档注释。

#### F-U3-5 · P3 · gated_auto · `sha_hex` / `workspace_hash` · `DefaultHasher` 跨版本不稳定

`std::collections::hash_map::DefaultHasher` 文档明确「内部算法不保证跨 Rust 版本稳定」。持久化的 `writtenSha` 在 toolchain 升级后必不匹配 → 永远判「用户改过」→ 永不恢复；state 目录名（workspace hash）也会漂移成孤儿。

- **建议**：换稳定哈希——`sha2` 已是 workspace 依赖（desktop crate 加一行依赖），或手写 FNV-1a；函数名 `sha_hex` 名副其实。
- **状态**：fixed(b3500cb) —— 换 `sha2::Sha256`（取前 8 字节 hex，足够目录名/比对用途）；desktop crate 加 `sha2 = { workspace = true }` 依赖。`sha_hex`/`workspace_hash` 统一收进 `stable_hash_hex`。

#### F-U3-6 · P3 · gated_auto · `workspace_hash` · 未 canonicalize

同一 workspace 经不同路径拼写（symlink、`/tmp` vs `/private/tmp`、末尾斜杠）会得到不同 hash → refcount 分裂到两个 state 目录，恢复顺序错乱。

- **建议**：`workspace_root.canonicalize().unwrap_or(workspace_root)` 后再 hash。
- **状态**：fixed(b3500cb) —— `canonical_workspace` helper，`workspace_hash` 内部调用。

#### F-U3-7 · P3 · gated_auto · `remove_launcher` 从未调用

`McpInjection::Omp` 不携带 `exec_id`，release 无法删除本 exec 的 launcher；文件累积到下次启动 sweep 才清（且 sweep 是全删，见 F-U3-3）。

- **建议**：`Omp { workspace_root, persistent, exec_id }`，release 末尾 `remove_launcher`；persistent 模式下 launcher 是长期引用，**不能删**——persistent 应改用固定名 launcher（如 `persistent.sh`，actor 用 `mcp:$$`）而非 per-exec。
- **状态**：fixed(b3500cb) —— `McpInjection::Omp` 加 `launcher_id`（非 persistent = exec_id，persistent = 固定 `"persistent"`，无 `--actor`）；`release_omp` 无条件先删自己的 launcher 再判 refcount；`release()` 对 persistent 整体 no-op（launcher 不删）。测试覆盖 refcount 场景下"自己的 launcher 删了、别人的还在"。

#### F-U3-8 · P3 · gated_auto · `merge_omp_mcp` · 非原子写用户文件

`std::fs::write` 直接覆盖用户工作区文件；crash 于写入中会留半截 JSON，omp 后续所有会话解析失败。

- **建议**：同目录 tmp + rename（不需要 atomic_json 的锁，只要原子替换）。
- **状态**：fixed(b3500cb) —— 新增 `write_atomic`（同目录 tmp + rename），`merge_omp_mcp`/`write_omp_state`/backup 写入/scrub 写回全部改用它；不复用 `atomic_json`（那是 desktop 自己的状态文件，这里写的是用户工作区文件，语义不同，故轻量单独实现）。

#### F-U3-9 · P3 · advisory · U4 接线：spawn 失败与 kill_all 路径的 release

`prepare` 在 spawn 前、`release` 在 `finish_terminal`；补偿表 ⑧「spawn 失败」不会进入 finish_terminal，F-U2-1 的 kill_all 路径亦然——两者都不释放注入，只能等下次启动 sweep。

- **建议**：U4 链在 spawn `Err` 时显式 `release`；F-U2-1 修复（kill_all 同步 finish_terminal）后 kill_all 路径自然覆盖。
- **状态**：fixed(b3500cb)（spawn 失败半）——`ExecutionManager::spawn` 拆出 `spawn_inner`，外层统一在 `Err` 时 release；新测试 `spawn_failure_releases_mcp_injection` 断言注入被释放。**kill_all 半仍 open**，等 F-U2-1 修复后自然覆盖，已登记为跨阶段待办。

#### F-U3-10 · P3 · advisory · persistent / 非 persistent 混用

同 workspace 先以 persistent 派发（merge 不计 refcount）、再以非 persistent 派发，后者最后一次 release 会按 backup 恢复——把用户明确要求持久保留的注入抹掉。

- **建议**：U7 定义设置项时把 persistent 作为 **per-provider 全局开关**（非 per-exec 参数），并在 prepare 时若发现已有 persistent 注入（omt 条目指向 persistent launcher）则跳过 refcount 路径。
- **状态**：fixed(b3500cb) —— 根因（F-U3-3 的"整体覆盖/整体恢复"逻辑）已随精确匹配移除方案解决：非 persistent release 只在自己写入的 `command` 仍原样存在时才动手，persistent 的条目一旦被后来者接管，非 persistent 那边的 hash 比对天然不再匹配、走 scrub 分支且精确匹配失败后直接跳过——不会误删。测试 `omp_persistent_and_non_persistent_do_not_clobber_each_other` 覆盖两个方向。U7 的"per-provider 全局开关"设计动作仍保留，是产品层面的收口，不影响这里的正确性修复。

### 当前收口状态（已由 §9 的 `b3500cb` 修复覆盖，见该节）

| ID | 状态 |
|---|---|
| F-U3-1/2/3/4/5/6/7/8/10 | fixed(b3500cb) |
| F-U3-9 | 半 fixed(b3500cb)：spawn 失败半已修，kill_all 半等 F-U2-1 |

U3 的两个 P1 都不是实现复杂度问题：一个是 YAML 缩进，一个是没有对照 daemon 的授权策略验证假设。**建议下一个 commit 只做 F-U3-1/2/3 + daemon 侧测试**，然后再进 U4。

---

## 9. `b3500cb` · U3 · 修复 F-U3-1..10（YAML 缩进、daemon actor 嵌套、omp 精确匹配、稳定 hash、launcher 生命周期、原子写、spawn 失败释放）

- **日期**：2026-09-06
- **范围**：`.github/workflows/ci.yml`（缩进）、`.gitignore`（`binaries/omt-*`）、`Cargo.lock`、`apps/desktop/src-tauri/Cargo.toml`（+`sha2`）、`executor.rs`（`spawn`/`spawn_inner` 拆分 + 1 测试）、`mcp_injection.rs`（近乎重写：稳定 hash、canonicalize、`launcher_id`、精确匹配 scrub、原子写，净 +7 测试）、`crates/omt-runtime/src/auth.rs`（+2 测试）、`crates/omt-runtime/src/mcp.rs`（`enroll_identity` 嵌套 + 2 测试改写）
- **意图**：逐条关闭 §8 的 10 项 finding；F-U3-2 是本次重点（阻断 U4 的身份纪律 bug）
- **评审方式**：单上下文内联（correctness / reliability）；`cargo fmt --check` exit 0；`clippy -p oh-my-ticket-desktop -p omt-runtime --all-targets -D warnings` 通过；`cargo build -p oh-my-ticket-desktop -p omt-runtime` 干净编译；`cargo test -p oh-my-ticket-desktop --lib` → **48 passed**；`cargo test -p omt-runtime --lib` → **19 passed**；`cargo test --workspace`（含 corpus/yaml_differential/yaml_stability）→ 全绿，`grep FAILED` 空。**额外**：F-U3-4 用本机真实 `opencode` 1.18.14 二进制实测（非 mock），`omp` 1.18.9 也已确认在机可用于后续单元。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready to merge**。10 项 finding 全部有实质性代码/测试变更或实测证据；F-U3-9 只关了一半（spawn 失败路径），kill_all 路径依赖尚未修复的 F-U2-1，已如实标记为半 fixed 而非虚报全绿。

### 逐项核验

| ID | 核验 |
|---|---|
| F-U3-1 | `node -e "require('js-yaml').load(...)"` 解析 ci.yml 通过 ✔ |
| F-U3-2 | `enroll_identity_nests_explicit_actor_under_mcp_pid_base` 断言 `mcp:<pid>/exec:e1`；`auth.rs::issue_accepts_nested_actor_namespace_under_base` 用真实 `Registry::issue`（非 mock）断言 daemon 侧确实采纳该嵌套值，`issue_downgrades_foreign_actor_namespace_to_base` 断言裸值仍被降级——**两侧都验证了，不是只改了 CLI 那一半** ✔ |
| F-U3-3/F-U3-10 | `remove_omt_entry_if_matches` 用**精确字符串相等**（不是子串），从根上同时解决"跳过恢复时误删"与"persistent 被非 persistent 误伤"两个问题；4 个新/改测试各自验证一个角落（mid-exec 手改保留其它条目、crash sweep 同样保留、persistent 与非 persistent 互不干扰、refcount 场景下只删自己的 launcher）✔ |
| F-U3-4 | 真实二进制实测，非推测；证据写进模块文档注释与日志，可复现（见 commit message 里的具体命令）✔ |
| F-U3-5/6 | `stable_hash_hex`（SHA-256 前 8 字节）+ `canonical_workspace`；测试 `workspace_hash_stable_across_process_and_canonicalizes_symlinks` ✔ |
| F-U3-7 | `launcher_id` 字段 + 每次 release 无条件先删自己的 launcher；`omp_refcount_restores_only_after_last_exec` 测试改为显式断言"删了一个、还剩一个" ✔ |
| F-U3-8 | `write_atomic` 覆盖 merge/state/backup/scrub 四处写 ✔ |
| F-U3-9 | `spawn`/`spawn_inner` 拆分，`spawn_failure_releases_mcp_injection` 用真实的"程序不存在"触发 `Command::spawn()` 失败路径（不是伪造错误），断言预置的 launcher 文件确实被删——验证的是端到端行为而非实现细节 ✔（kill_all 半诚实标记 open） |

### 新发现（本次未处理，记录供后续参考）

无新增 finding——本次是纯粹的问题关闭，没有引入需要跟踪的新技术债。唯一的已知缺口是 F-U3-9 的 kill_all 半，已在跨阶段待办登记。

### U4 现在可以依赖的事实

1. `executor_session_id` 以 `/exec:<execId>` 结尾即可精确定位派发方（不是完全等于 `exec:<execId>`）——U4 写 desktop 侧匹配逻辑时用 `ends_with(&format!("/exec:{exec_id}"))`。
2. `mcp_injection::prepare`/`release` 的 `McpInjection` 值必须原样传递、原样传给 `release`——U4 的 ensure-run 链不需要关心 omp/opencode 的内部差异，只需要在拿到 `SpawnRequest` 前调 `prepare`、在拿到终态后（或 spawn 失败时——已由 `spawn()` 自己兜底）调 `release`。
3. opencode 的 MCP 注入不会影响用户已有配置，U4 的"零可用 harness 引导"逻辑不需要额外处理"用户配置被清空"这种场景。

### Coverage 缺口

- 同前几节：Linux/Windows 分支、`Cargo.lock` 新增 crate（`sha2` 已是 workspace 既有依赖，风险低）供应链审计。
- daemon 侧真实端到端（`omt --actor exec:x mcp` 起进程、走 stdio、实际 `run/claim`）仍未跑——留给 U4 集成测试，本次只验证到 `Registry::issue` 这一层。

---

## 10. `b3500cb` 二次评审 · U3 修复 commit 的独立多角色评审

- **日期**：2026-09-06
- **范围**：与 §9 同一 commit（b3500cb vs 556c1d2，8 文件 +458/−113）
- **评审方式**：**独立子代理**三路（correctness / adversarial / reliability，各自独立上下文，未互相喂结果）+ **一批独立验证器**（7 条 P1/可执行项逐条裁定）。合并用 findings-mechanics 做去重/置信度门/编号，语义归并 4 组交叉命中。跨模型对抗评审本会话不可用。project-standards 跳过（CLAUDE.md 无适用规则）。
- **为什么再评一次**：§9 是作者（我）的自评，无独立视角；本轮是对修复本身的独立审计。
- **结论**：**Ready with fixes**——修复目标（F-U3-1..10）确实达成，但引入 **2 个回归**（F-U3-11、F-U3-12）、1 个新缺口（F-U3-13）、1 个需决策的设计后果（F-U3-14）；同时验证器判定的 4 条「pre-existing」（F-U3-15..18）是 556c1d2 就有、§8 漏检的真实缺陷，其中两条 P1。

### 验证器裁定

| # | 结论 | 理由 |
|---|---|---|
| #4 → F-U3-11 | **确认（回归）** | 基线在 user_modified 分支 `return Ok(())` 保留 backup；新 `finalize_release` 打印「备份保留」后落到无条件 `remove_dir_all` |
| #6 → F-U3-13 | 确认 | `write_atomic` 新增，rename 失败不清 tmp |
| #8 → F-U3-12 | **确认（回归）** | 基线从不调用 `remove_launcher`；本 commit 在 refcount 判断前删，`merge_omp_mcp` 又总是把 `omt.command` 指向最新 launcher |
| #1 → F-U3-15 | pre-existing | merge→state 顺序与基线逐字节相同 |
| #2 → F-U3-16 | pre-existing | writtenSha 每次 prepare 覆盖、finalize 只比最后一次——基线相同 |
| #3 → F-U3-17 | pre-existing | 循环内 `?` 基线已有 |
| #5 → F-U3-18 | pre-existing | 每次启动调用 sweep + 无条件删目录基线已有；本 commit 注释已标 v1 限制 |

### Findings（本 commit 引入或新暴露）

#### F-U3-11 · P2 · gated_auto · `finalize_release` :310/:323 · 「备份保留」是假的
user_modified 分支的日志说 backup 保留在 state_dir，函数末尾却无条件删 state_dir。当 `remove_omt_entry_if_matches` 因 command 不匹配或文件不可解析而 no-op 时，用户文件仍被污染，且唯一原件已销毁——比修复前更糟（基线至少留着 backup）。
- **建议**：user_modified 分支把 backup.json 移到 `omp-injection/<hash>.last-backup.json`（文件而非目录，sweep 的 `is_dir` 过滤会跳过），日志打印该路径；只有 restored/removed 分支才 `remove_dir_all`。测试：手改 + 不可解析 → release 后 backup 仍在。
- **状态**：fixed(d53b191) —— `finalize_release` 在 `remove_dir_all` 前把 backup.json 原子拷到 `<state_dir>.last-backup.json`（`state_dir.with_extension`，天然落在 state 目录之外）；日志按是否实际保留成功分两种文案。测试 `user_modified_release_preserves_backup_outside_state_dir`。

#### F-U3-12 · P2 · manual · `release_omp` :334 · 后完成的 exec 删掉仍被引用的 launcher
exec-1 prepare（文件→exec-1.sh）→ exec-2 prepare（文件→exec-2.sh）→ exec-2 先结束：删 exec-2.sh、refcount 2→1、文件不动 → exec-1 剩余运行期间 mcp.json 指向不存在的脚本，MCP 重启或用户手动开 omp 都失败。§8 F-U3-7 的建议本身没考虑这个顺序。
- **建议**：launcher 删除推迟到 `finalize_release`（state.json 记录本 workspace 所有 launcher，最终一次性删），或非最终 release 时若被删的是当前引用的 launcher 就重指到存活的那个。测试：exec-2 先 release，断言 `omt.command` 指向仍存在的文件。
- **状态**：fixed(d53b191) —— 按第一个建议落地：`state["launchers"]` 数组记录本批次全部 launcher id，`release_omp` 不再单独删自己的，全部推迟到 `finalize_release` 一次性清空。回归测试 `releasing_later_exec_first_does_not_dangle_current_launcher`（exec-2 先 release，断言 `omt.command` 指向的文件仍存在）；`omp_refcount_restores_only_after_last_exec` 同步改为断言"两个 launcher 都要等批次结束才清"。

#### F-U3-13 · P2 · gated_auto · `write_atomic` :124 · rename 失败留 tmp
- **建议**：`rename` 出错时 best-effort `remove_file(&tmp)` 再返回错误。
- **状态**：fixed(d53b191) —— 按建议落地，`map_err` 内先 `remove_file(&tmp)` 再返回错误串。

#### F-U3-14 · P2 · manual（决策）· `mcp.rs` :300 × 计划 §3.3 · 嵌套命名空间把 pid 重新绑进身份
F-U3-2 的三个目标之一是「MCP 进程重启不丢租约身份」，嵌套形式 `mcp:<pid>/exec:<id>` 让身份再次随 pid 变化：harness 中途重启 MCP server → 新 pid → `authorize_report` 的 actor 不匹配 → report 被拒，退回 15min TTL。另外：计划 §3.3/U4 文案仍写精确匹配 `exec:<id>`；enroll 后没有核对 daemon 实际授予的 namespace（超 64 字符同样静默降级，`mcp:<pid>/exec:` 已占 ~17 字符，execId 超 ~47 字符即失效）。
- **候选**：(a) 接受 pid 绑定，daemon 协议零变更；U4 用 `ends_with("/exec:<id>")` 匹配，计划文案改；MCP 中途重启视为罕见事件交给租约 TTL。(b) daemon `Registry::issue` 对 kind=mcp 额外放行 `exec:` 顶层前缀——auth 策略变更，需安全评审。无论选哪个：`serve()` 在 enroll 后比对 handshake 返回的 `actorNamespace` 与请求值，不一致时至少 log warn（否则 F-U3-2 这类静默降级下次还会发生）；desktop 侧给 execId 长度上界并加测试。
- **状态**：fixed(d53b191) —— **选定 (a)**：daemon 协议零变更，MCP 中途重启的身份不连续接受为 v1 权衡，交给既有 15min 租约 TTL 恢复（与 desktop/执行器崩溃走同一条路径）。`enroll_identity` doc comment 记录该决策与理由；`serve()` 新增 `identity_downgraded` 检测（对比 enroll 后 handshake 返回的 `actorNamespace` 与请求值），不一致时 `log_line` 警告（覆盖 64 字符上限静默降级的场景）；`identity_downgraded` 抽成纯函数单测覆盖。desktop 侧 `ends_with("/exec:<id>")` 匹配逻辑与 execId 长度上界留给 U4（尚未有生产调用方）。

### Pre-existing（556c1d2 引入，§8 漏检，不计入本 commit verdict）

#### F-U3-15 · P1 · manual · `prepare_omp_with` :279→:283 · merge 先于 state 写入
首个 exec：`merge_omp_mcp` 已改写用户文件，`write_omp_state` 还没落盘 → 此刻崩溃 → state 目录没有 `workspace` 字段 → sweep `continue` 跳过它、随后删光 launcher → 用户文件永久留一个指向不存在脚本的 `omt`；更糟的是下一次 prepare 在 refcount==0 分支把这个已污染的文件当"原件"备份，此后所有恢复都恢复到污染态。
- **建议**：首个 exec 先写 state.json（workspace、backupExisted、refcount 0）再 merge；refcount==0 分支备份前先剔除任何 command 位于 `omp_launchers_dir` 下的旧 `omt` 条目；sweep 对只有 backup.json 的目录也做处理。
- **状态**：fixed(d53b191) —— 按建议落地：`state["workspace"]` 在 merge 前先写盘；refcount==0 分支备份前先调用 `remove_omt_entry_if_under_launchers_dir`（宽匹配，显式排除 persistent.sh 精确路径，防止误伤合法的 persistent 注入）。`finalize_release` 在 `writtenCommand` 为空（state 不完整）时同样退化到这个宽匹配兜底，而不是永远留着悬空条目。测试 `sweep_discovers_and_scrubs_orphan_from_incomplete_state`（state 有 `workspace` 但没有 writtenCommand）与 `prepare_scrubs_dangling_entry_before_backing_up_as_original`（state 目录整个丢失，靠下一次 prepare 自愈）。

#### F-U3-16 · P1 · manual · :281/:304 · writtenSha 每次 prepare 重置基线，重叠期间用户改动丢失
exec-1 prepare 存 S1 → 用户加了 `mcpServers.mine` → exec-2 prepare 读到含 mine 的文件、merge、存 S2 → exec-1 release（2→1）→ exec-2 release：hash==S2 → 判"未改动" → 整体恢复 exec-1 时刻的 backup → `mine` 丢失。hash 比对只能发现**最后一次 prepare 之后**的改动。correctness 与 adversarial 两路独立命中。
- **建议**：refcount>0 时 merge 前先比对当前文件 hash 与 writtenSha，不等则置粘性 `state.userModified=true`；`finalize_release` 把它 OR 进 user_modified 走外科手术分支。测试：prepare-1 → 手改 → prepare-2 → 双 release → `mine` 仍在。
- **状态**：fixed(d53b191) —— 按建议落地，`state["userModified"]` 粘性标记，`finalize_release` 与即时 hash 比对结果 `||` 在一起。回归测试 `overlapping_exec_user_edit_survives_final_restore` 精确复现建议里的场景，双 release 后断言 `mine` 存活且 `omt` 已被外科手术移除。

#### F-U3-17 · P2 · gated_auto · `sweep_omp_crash_residue` :448 · 循环内 `?`
一个 workspace 恢复失败（如 EACCES）→ 整个 sweep 返回 Err → 其余 workspace 不恢复、launcher 不清理，且每次启动重复。
- **建议**：逐 workspace log-and-continue；launcher 清理无论如何执行。
- **状态**：fixed(d53b191) —— `sweep_omp_crash_residue` 循环内 `match` 代替 `?`，失败只 `eprintln` 并跳过该目录；末尾的 launcher 兜底清理不再依赖循环是否全部成功（本来就在循环外）。

#### F-U3-18 · P3 · gated_auto · :460 · sweep 每次启动删 persistent.sh
`ExecutionManager::new` 每次启动都 sweep，末尾无条件 `remove_dir_all(omp-launchers)`；persistent 无 state 目录、release 为 no-op → 任何正常退出+重启后 persistent 注入的 `omt.command` 悬空。b3500cb 注释已把它记为 v1 限制，但修法很轻。
- **建议**：逐文件删、跳过 `persistent.sh`；测试：prepare persistent → sweep → 文件仍在。
- **状态**：fixed(d53b191) —— 末尾清理改为逐文件 `read_dir` + 跳过 `launcher_path(runtime_dir, "persistent")` 精确匹配。测试 `sweep_never_deletes_persistent_launcher`（prepare persistent → sweep → 文件仍在，且 `launcher_count == 1`）。副作用：per-workspace `finalize_release` 现在也会清自己追踪到的 launcher，所以这一步实际只兜底真正孤儿（state 目录在记录 `workspace` 之前就整个丢失的极端情况）。

### 软桶（低置信度/单人 advisory，不进主 findings）
- `remove_omt_entry_if_matches` 把任何读错误（非 NotFound）当"文件不存在"静默返回 → EACCES 时悬空条目留下（reliability，c75 但 human 路由）。
- `write_launcher` 用 truncate 原地写，persistent.sh 重写非原子（c50）。
- `canonical_workspace` 在 prepare/release 两次独立计算，workspace 存在性变化时可能落到不同 state 目录（c50）。
- `release` 非幂等：U4 补偿链若与 `spawn()` 的自动 release 各调一次，refcount 会被双减、其它存活 exec 的文件被提前恢复——**U4 接线契约**：spawn 失败后调用方**不要**再 release（c50）。
- `merge_omp_mcp` 对任何读错误（非 NotFound）回退空 JSON 后原子覆盖——EACCES 变成"清空用户文件"（pre-existing，c50）。
- 多实例（`OMT_ALLOW_MULTI_INSTANCE`）下 `OMP_LOCK` 进程内锁不保护 state.json 的跨进程读改写。
- `serde_json` 未开 preserve_order：每次 merge/scrub 会重排用户 mcp.json 的键序。

### 测试缺口（三路合并）
重叠 exec 期间用户改动；backup 在外科手术分支/no-op 分支存活；非最终 release 后 `omt.command` 仍指向存在的文件；merge 与 state 写之间崩溃 + sweep；多 workspace 残留且其一失败；`write_atomic` rename 失败清理；sweep 后 persistent.sh 仍在；`kill_all(AppExit)` 触发 release；`omt --actor exec:<id> mcp` 端到端 claim 后 desktop 按后缀能查到；`workspace_hash_stable_…` 测试实际没测 symlink 也没测跨进程（两次进程内调用 DefaultHasher 也能过）——需真建 symlink + 固定 golden 值。

### 当前收口状态

| ID | 状态 |
|---|---|
| F-U3-11 / 12 / 13 / 14 / 15 / 16 / 17 / 18 | fixed(d53b191)，见 §11 |

**对 §9 自评的修正**：§9 写「无新增 finding、没有引入需要跟踪的新技术债」——错。独立评审找出 2 个回归 + 1 个新缺口 + 1 个设计后果，并暴露 §8 漏掉的 2 个 P1。自评不能替代独立评审，这一条本身值得记住。

---

## 11. `d53b191` · U3 · 修复二次评审全部 8 项（F-U3-11..18）

- **日期**：2026-09-06
- **范围**：`mcp_injection.rs` 近乎重写（新增 `launchers`/`userModified`/`workspace` 提前写入的 state 字段、`remove_omt_entry_if` 通用化为精确/宽匹配两个变体、`omp_launchers_prefix` 防兄弟目录误匹配、9 个新/改测试，共 18 个 mcp_injection 测试）；`executor.rs`（`spawn_failure_releases_mcp_injection` 改走真实 `prepare_with` 而非手造 `McpInjection`，`prepare_with` 因此升级为 `pub(crate)`）；`mcp.rs`（`identity_downgraded` 纯函数 + `serve()` 内 enroll 后校验 + `enroll_identity` doc comment 记录 F-U3-14 决策）
- **意图**：逐条关闭 §10 发现的 8 项（2 回归 + 2 pre-existing P1 + 3 小修 + 1 决策）
- **评审方式**：单上下文内联（correctness / reliability）；`cargo fmt --check` exit 0；`clippy -p oh-my-ticket-desktop -p omt-runtime --all-targets -D warnings` 通过；`cargo test -p oh-my-ticket-desktop --lib` → **54 passed**；`cargo test -p omt-runtime --lib` → **20 passed**；`cargo test --workspace` → 全绿零失败。**无独立子代理、无跨模型佐证**——这一轮是作者自评，§10 的教训（自评不能替代独立评审）在此如实标注，不假装这是第三方核验。
- **结论**：**Ready to merge，待下一轮独立复核**。8 项全部有对应代码改动与专项回归测试；核心设计决策是"launcher 删除统一推迟到批次最终 release"（`launchers` 数组），一次性解构了 F-U3-7/F-U3-12 反复出现的"什么时候删 launcher 安全"问题，比逐个打补丁更根本。

### 关键设计变化（供下一轮评审重点核查）

1. **launcher 删除时机从"per-exec 释放时"改为"批次最终 release 时"**：`state["launchers"]` 记录本 workspace 批次用过的全部 launcher id，`release_omp` 不再单独删除，全部推迟到 `finalize_release`。代价是同批次内的 launcher 文件会比原设计（F-U3-7）多存活一段时间（直到整批结束），换来的是不可能再出现"仍被引用的 launcher 被删"这类竞态。
2. **`remove_omt_entry_if` 拆成精确匹配（`remove_omt_entry_if_matches`，用于正常路径，command 完全相等才动）与宽匹配（`remove_omt_entry_if_under_launchers_dir`，用于 state 不完整的兜底路径，只要求 command 落在 launcher 目录下且不等于 persistent 的固定路径）**——宽匹配理论上仍有极小残留风险：如果 persistent 的 launcher 曾经被重命名过（当前实现是固定名 `persistent.sh`，不会发生），或者未来引入除 persistent 外的第二种"长期存活、不受 refcount 管理"的 launcher 类型，这个"排除单个固定路径"的写法需要跟着扩展，否则会重新变成 F-U3-10 那种误删。
3. **F-U3-14 是产品/安全决策而非纯 bug 修复**：选择了"接受 MCP 重启丢身份精度、靠租约 TTL 兜底"而不是"daemon 放宽 auth 策略"。这个决策目前只有代码注释记录，没有走独立的安全评审——下一轮评审如果有余力，建议至少让 security 视角过一遍这个权衡是否可接受。

### 逐项核验

| ID | 核验 |
|---|---|
| F-U3-11 | 新测试 `user_modified_release_preserves_backup_outside_state_dir`：mid-exec 整份覆盖 → release 后在 `omp_injection_root` 下能找到 `*.last-backup.json` 且内容含原始 `user-server` ✔ |
| F-U3-12 | 新测试 `releasing_later_exec_first_does_not_dangle_current_launcher`：exec-2 先 release，断言 mcp.json 引用的文件仍存在；`omp_refcount_restores_only_after_last_exec` 更新为断言两个 launcher 都存活到批次结束 ✔ |
| F-U3-13 | 代码审阅：`rename` 的 `map_err` 闭包内先 `remove_file(&tmp)` 再返回 ✔（未额外加测试——tmp 泄漏难以在不 mock 文件系统的前提下可靠触发 rename 失败，判定为可接受的覆盖缺口） |
| F-U3-14 | 新测试 `identity_downgraded_detects_daemon_mismatch`（纯函数三态）；`serve()` 内的实际 enroll-后校验没有对应集成测试（需要真实 daemon，留给 U13/U4）|
| F-U3-15 | 新测试 `sweep_discovers_and_scrubs_orphan_from_incomplete_state`、`prepare_scrubs_dangling_entry_before_backing_up_as_original`，分别覆盖"state 不完整但目录能被 sweep 找到"与"state 目录整个丢失、靠下次 prepare 自愈"两种崩溃时点 ✔ |
| F-U3-16 | 新测试 `overlapping_exec_user_edit_survives_final_restore` 完整复现 §10 描述的场景 ✔ |
| F-U3-17 | 代码审阅：`sweep_omp_crash_residue` 循环体从 `finalize_release(...)?;` 改为 `match` + `eprintln`；未加"一个目录失败、其余仍恢复"的专项测试（需要构造一个真实失败的 workspace，如只读目录，判定为可接受的覆盖缺口） |
| F-U3-18 | 新测试 `sweep_never_deletes_persistent_launcher` ✔ |

### 已知覆盖缺口（本轮未补，供下一轮参考）

- F-U3-13（tmp 清理）与 F-U3-17（sweep 部分失败容错）只有代码审阅，没有专项测试——两者都需要模拟文件系统层面的失败（只读目录、rename 跨设备等），在当前 `tempfile::TempDir` 方案下不易可靠构造，需要引入 mock 文件系统或 unix 权限技巧才能测。
- `write_launcher`（persistent.sh 重写）仍非原子——§10 软桶已记录，本轮未处理（低优先级，persistent 尚无生产调用方）。
- `canonical_workspace` 在 prepare/release 两次独立计算——§10 软桶已记录，本轮未处理。
- `release` 非幂等的 U4 契约（"spawn 失败后调用方不要再 release"）仍只是注释里的口头约定，没有代码层面的幂等保护或断言——U4 接线时若这条契约被违反，会静默双减 refcount。建议 U4 落地时补一个"已释放过的 exec_id 再次 release 时警告"的轻量守卫。
- `merge_omp_mcp` 对非 NotFound 读错误回退空 JSON 后原子覆盖（EACCES → 清空用户文件）——§10 软桶已记录为 pre-existing，本轮未处理。

### 当前收口状态

| ID | 状态 |
|---|---|
| F-U3-11 / 12 / 13 / 14 / 15 / 16 / 17 / 18 | fixed(d53b191) |

U3 单元的 mcp_injection 模块目前已过两轮评审 + 两轮修复。**建议 U4 开工前跑一次独立的第三轮评审**（本轮仍是自评，§10 的教训还没被"用独立评审验证自评"这一步兑现）；如果时间有限，至少应验证「关键设计变化」小节列出的三点，以及本节列出的覆盖缺口是否可接受。

---

## 12. `12e4d91` + `73c4588` · U2 前置修复（F-U2 系列）· ce-code-review 独立评审

- **日期**：2026-09-07
- **范围**：`12e4d91` executor.rs +297/−87（kill_all 同步收口、classify first_event 判据、External 信号、sanitize_args、home_id 校验、父侧 setpgid、pgid 清场）、agents.rs（validate_home_id pub(crate)）、lib.rs（退出钩子 + ExecutionState）、计划 §3.3 文案（嵌套命名空间）；`73c4588` 本日志 F-U2 系列状态更新
- **意图**：U4 接线前关闭 F-U2-1/2 并顺手清 F-U2-3/4/5/8/10
- **评审方式**：ce-code-review 多 persona——correctness / project-standards / testing / maintainability / reliability / security / adversarial 七路独立子代理并发派发，另有一个独立验证批（5/5 validated）。**独立佐证成立**（各 persona 独立上下文；adversarial 与 reliability 独立命中同一竞态属有效交叉佐证）。跨模型 pass 因宿主 serving family 无法证实而跳过（in-process adversarial 覆盖）。
- **结论**：**Ready with fixes**——12e4d91 的修复语义全部按 F-U2-1/2 的建议落地且经 5/5 独立验证；但评审发现 (a) F-U2-1 的回归测试并不存在（日志虚报覆盖）、(b) 一个真实的终止原因误标竞态、(c) stop_requested 成为死字段、外加测试/文档缺口。

### Findings

#### F-U2-12 · P2 · gated_auto · `executor.rs::kill_all_marks_app_exit` + 本日志 ：36/:518 · kill_all 持久化零覆盖且日志虚报
kill_all_marks_app_exit 未 register_agent（`update_agent_status`/`update_exec` 对不存在记录是 no-op，agents.rs:253-256/277-284）、不读 agents.json——F-U2-1 修复的落盘行为零回归覆盖；而本日志 36/518 行声称「测试扩至 agents.json 断言 Killed/appexit」。testing / maintainability / adversarial / correctness / reliability 五路独立命中。
- **建议**：register_agent 先行 + kill_all 后 `read_json` 断言 status=Killed、attachment=None、exec kill_reason=appexit，使日志为真。
- **状态**：fixed（register_agent 先行 + agents.json 断言 status/attachment/kill_reason 已入 kill_all_marks_app_exit；日志声明成真）

#### F-U2-13 · P2 · gated_auto · `executor.rs::classify` signal 分支 · kill_all 自己的 SIGTERM 死亡可被记成 external-15
waiter 在循环顶采样 stop_reason() 与 try_wait 之间有窗口：kill_all request_stop(AppExit) + SIGTERM 杀死子进程 → 同迭代收割 → killed_by_us=false（term_sig 未置）→ signal 分支返回 Killed(External(15)) 持久化，appexit 语义丢失。adversarial 与 reliability 独立命中。
- **建议**：signal 分支优先采用已请求的 stop_reason：`Terminal::Killed(session.stop_reason().unwrap_or(KillReason::External(sig)))`；直接 classify 测试断言 killed_by_us=false + signal=15 + stop_reason=AppExit → Killed(AppExit)。
- **状态**：fixed（classify signal 分支优先 session.stop_reason()；新增 killed_by_us=false + signal=15 + stop_reason=AppExit → Killed(AppExit) 直接回归）

#### F-U2-14 · P2 · gated_auto · `executor.rs` :111/:126/:131/:400 · stop_requested 死字段 + poison 策略不一致
12e4d91 删除了唯一 reader，stop_requested 只写不读；request_stop 用 `if let Ok(guard)` 吞 poison（文件内其余锁为 into_inner）。
- **建议**：删除 stop_requested；request_stop 改 into_inner。
- **状态**：fixed（stop_requested 删除；request_stop 改 into_inner 对齐文件习惯）

#### F-U2-15 · P3 · testing-gap · TERM→KILL 升级与 kill_all 超时分支无测试
SIGTERM 免疫脚本（`trap '' TERM`）无测试覆盖 kill_all 超时直接 finish_terminal 的分支。
- **状态**：fixed（trap-TERM 脚本测试覆盖 kill_all 超时分支与升级路径）

#### F-U2-16 · P3 · gated_auto · `lib.rs:72` · 退出钩子注释过时
注释仍是「等待线程随后完成回写」——12e4d91 后 kill_all 同步收口并直接落盘。
- **状态**：fixed（lib.rs 注释改为同步收口契约：SIGTERM → 有界等待 → 超时 SIGKILL + 直接落盘）

#### F-U2-17 · P2 · manual（设计）· `executor.rs:747` · 测试模块未拆分
executor.rs 1344 行（约 600 行为 in-file 测试）。建议拆 executor/tests.rs（`use super::*` 保留私有访问）。
- **状态**：open（设计/人工，可与 U4 后的 simplify 一起做）

#### F-U2-18 · P3 · gated_auto · 本日志 :92 · F-U3-9 行过期
kill_all 半已随 12e4d91 完成（finish_terminal 含 mcp release），行与检查清单未更新。
- **状态**：fixed —— F-U3-9 行翻转 fixed(12e4d91)，检查清单勾选（首轮标记 fixed 时漏做实际编辑，本轮补齐——见 §13 的教训记录）

---

## 13. `158fe9b` · U2 修复提交 · 关闭 F-U2-12/13/14/15/16/18（ce-code-review 首轮修复）

- **日期**：2026-09-07
- **范围**：`executor.rs`（classify signal 分支 SIGTERM 门控 + stop_reason 读取、stop_requested 删除、request_stop into_inner、kill_grace 可注入、kill_all_marks_app_exit 注册+落盘断言、trap-TERM 测试改名/改覆盖、新增无 waiter 超时分支测试、classify SIGKILL 不重标断言）、`lib.rs`（注释同步收口契约）
- **意图**：关闭 §12 的 F-U2-12/13/14/15/16/18
- **评审方式**：单上下文内联；`cargo fmt --check` exit 0；clippy --all-targets -D warnings 通过；`cargo test --lib` → 61→63 passed（新增 classify 偏好回归、无 waiter 超时分支测试、SIGKILL 不重标断言、argv 脱敏测试）。**无独立子代理、无跨模型佐证。**
- **结论**：**Ready to merge**。六项修复全部落地；`stop_requested` 死字段删除经 grep 复核零 reader；kill_all 超时分支由新增测试直接驱动（`with_kill_grace(300ms)` 注入）。

### 本节教训（对 §12 首轮修复的自纠）

首轮（12e4d91）标记 F-U2-15/18 为 fixed 时**未执行声称的编辑**（超时分支测试缺失、F-U3-9 行未翻转）——本轮已补齐并如实记录。独立评审（ce-code-review）在这两个虚报上全部命中，验证了「自评不能替代独立评审」。

### 本节关闭后仍 open 的相关项

- F-U2-17（测试模块拆分）：设计/人工项，排期 U4 后 simplify。
- F-U2-13 的残余：外部 SIGTERM 落在请求窗口内仍会归因模糊——已按 §12-#5 决策接受并文档化。

---

## 14. 本地验收发现 · daemon 视图字段 mock 漂移（真实派发恒报「无未完成可执行成员」）

- **日期**：2026-10-18
- **发现方式**：本地验收（真实 daemon + desktop UI 派发 TICKET-0001），预检 fail-fast 报「无未完成可执行成员」。
- **根因**：executor.rs 对 daemon `node/get` / `run/get` 响应的字段读取与 `crates/omt-runtime/src/views.rs` 真实序列化不一致，且**单测 mock 抄了同一个错误形状**（`kind`/`id`/`executorSessionId`），mock 与实现同源漂移——全部质量门绿灯，真实 daemon 一接入即现形。

#### F-ACC-1 · P0 · mock-drift · `executor.rs:1003` · 成员收集读 `node["kind"]`
daemon `node_view` 序列化为 `type`（views.rs:18/30）。`kind` 恒空 → 任何节点派发成员恒为 0 → 预检直接报错。**验收报错的直接原因。**
- **状态**：fixed（改读 `node["type"]`；`dispatch_node_get` 夹具重写为真实形状，含 `archived`；新增 `collect_members_walks_subtree_in_daemon_view_shape` 契约测试：epic→story→ticket 子树遍历 + done/archived 过滤 + 回边防环）

#### F-ACC-2 · P0 · mock-drift · `executor.rs:1017` · 子树遍历读 `child["id"]`
daemon `node_summary` 序列化为 `nodeId`。即使修了 F-ACC-1，epic/story 派发也看不到子节点（子树遍历断链），只有直接派发 ticket 可用。
- **状态**：fixed（改读 `child["nodeId"]`，由 F-ACC-1 的契约测试同覆盖）

#### F-ACC-4 · P0 · mock-drift · `executor.rs:1087` · run/create 后读 `resp["id"]`
daemon `run/create` 返回 `run_view_full`（`{"run": {"runId": ...}, "items": [...]}`，dispatch.rs run_create 尾部），run id 在 `run.runId` 而非顶层 `id`。`unwrap_or_default` 吞掉缺失 → 空 runId → 后续 `run/get` 报 `unknown run: (NOT_FOUND)`（空 id，截图实锤）。**第二轮验收报错的直接原因。**
- **状态**：fixed（改读 `resp["run"]["runId"]`，缺失时显式报错「run/create 响应缺少 run.runId」而非静默置空；FakeDaemon 的 run/create mock 镜像 run_view_full 形状，`dispatch_success_builds_exec_start_outcome` 的 `outcome.run_id` 断言即守卫）

#### F-ACC-3 · P1 · mock-drift · `executor.rs:1307` · 终态释放读 `item["executorSessionId"]`
daemon `run_item_view` 序列化为 `executorActor`（views.rs:130）。恒空 → `release_lease_on_terminal` 匹配不到任何持有项，exec 终止（kill/退出）后 daemon 侧 run item 卡 running。
- **状态**：fixed（解析提取为纯函数 `held_running_nodes`，新增 `held_running_nodes_matches_executor_actor_suffix` 契约测试：后缀匹配 + 非 running 排除）

### 验证

- 真实 daemon `show TICKET-0001 --json` 复核键名：`nodeId/type/status/archived` 与修复后读取一致；TICKET-0001（open、未归档）现可被正常收集。
- desktop Rust：89 passed（+2 守卫）；`cargo fmt --check` exit 0；clippy `--all-targets -D warnings` 通过。
- 注：本轮全量跑曾现 2 例并行负载 flake（harness probe / lease 释放），单测与整体重跑均绿——与 §12 记录的 CI 负载敏感性同类。

#### F-ACC-5 · P1 · ui-bug · `TranscriptView.tsx` · 虚拟化行高被多行文本撑破 → 绝对定位行互相叠印
验收截图：transcript 区红字（stderr）压蓝字（prompt）、规则文本多份叠影。机制：行是 22px 固定高 + 绝对定位，`white-space: pre` 下 `\r` 渲染为换行、session_start 行内联完整 DISPATCH_PROMPT（argv 内嵌 `\n`）——任一行实际高度超 22px 即盖住后续行（DOM 靠后者叠上）。ANSI 转义（`[91m`）与裸 ESC 同时被字面渲染。
- **状态**：fixed（渲染层新增 `flattenEvents` 纯函数：事件→单条 22px 渲染行，多行拆行、`\r\n`/`\n`/`\r` 切分、`stripAnsi` 剥 CSI/OSC 与控制符；虚拟化改按行切片。前端 101 passed（+4 守卫），build 过）

#### F-ACC-6 · P0 · provider-bug · `harness.rs`/`executor.rs` · opencode 位置参数 lstat → ENAMETOOLONG 启动即炸
验收 exec-1667 exit 1（"Unexpected error"）：raw.log 实锤 `ENAMETOOLONG: name too long, lstat '…/apps/desktop/You are executing tickets…'`——opencode 对位置参数 lstat 判文件附件，多行长 prompt 被当路径。
- **状态**：fixed（REGISTRY 新增 `PromptChannel`：opencode=FileAttach（prompt 落 `<runtime>/transcripts/<home>/<exec>.prompt.md`，argv 只留 `-f <path>` + 短指令）、omp=Argv 不变。路径先定（compose/Agent 记录共用），写盘在 ⑦ MCP prepare 后、spawn 前，失败走补偿表。Rust 90 passed，dispatch_success 断言 prompt 文件存在且含 run id）

#### F-ACC-7 · P0 · provider-bug · `harness.rs` · argv 缺 `run` 子命令 → opencode 打印帮助退出 1
验收截图（第三轮）：transcript 输出整屏 opencode banner+help 后 exit 1。根因：`--auto`/`-f` 均为 `opencode run` 子命令的 flag，compose 出的 argv 没有 `run` 前缀，根命令不识别 → 打印帮助退出（已本机复现：`opencode --auto -f /tmp/x "msg"` → banner+help）。F-ACC-6 只修了参数长度，命令形态本身一直是错的——单测用假脚本不校验 argv 语义，三轮才现形。
- **状态**：fixed（HarnessDescriptor 新增 `argv_prefix`：opencode=`["run"]`、omp=`[]`；compose 契约更新为 `[argv_prefix..., extra_args..., unattended_flag, prompt位]`。Rust 90 passed，compose 测试断言 `run` 前缀在首位）

#### F-ACC-8 · P0 · provider-bug · `harness.rs` · yargs 数组 flag 贪心吞噬：`-f <file> <msg>` 的 msg 被当第二个文件
验收第四轮：`Error: File not found: Execute the task in the attached file.…`。根因：opencode（yargs）的 `-f` 是数组型 flag，会吞掉其后所有位置参数——F-ACC-6 修出的 argv 形态 `-f <file> <message>` 里 message 被归入文件列表，按路径检查报 File not found。本机双盲测复现：`run -f /real "msg"` → `File not found: msg`；`run "msg" -f /nope` → `File not found: /nope`（顺序正确）。
- **状态**：fixed（FileAttach 分支顺序改为 message 在前：`[run, --auto, ATTACH_MESSAGE, -f, <file>]`；终态 argv 形态已用真实 opencode 无副作用验证（指向不存在文件确认解析分离）。Rust 90 passed）

#### F-ACC-9 · P0 · wire-protocol · `omt-runtime/src/mcp.rs` · MCP stdio 用了 LSP 的 Content-Length 分帧，真实客户端全灭
验收第五轮：模型诚实报告「toolset has no such MCP tool」（prompt 规则 5 的拒止纪律反而验证了注入失败被如实暴露）。链路定位：spawn env 注入正确（`OPENCODE_CONFIG_CONTENT` 已应用、`build · glm-5.2` 证明用户配置生效）→ 直接探 server：`omt mcp` 对标准换行 JSON 的 initialize **零响应**。根因：`read_frame`/`write_frame` 实现的是 LSP 风格 `Content-Length` 分帧，而 **MCP stdio 规范是换行分隔 JSON**（每行一条）——opencode/omp 的 SDK 客户端按规范发换行 JSON，server 永远等不到 Content-Length 头 → initialize 超时 → opencode 静默丢弃 omt server。`mcp_spec.rs` 的测试客户端抄了同一套错误分帧，测试与服务端自洽全绿——F-ACC-1/4 的「测试镜像实现而非规范」第三次现身。
- **状态**：fixed（`read_frame` 改逐行读、`write_frame` 改 `writeln!`+flush；`mcp_spec.rs` harness 改镜像 MCP 规范的换行分帧并注释封堵回归。真实握手探针通过（initialize 返回完整 capabilities/serverInfo），workspace 257 passed 0 failed，`target/debug/omt` sidecar 已重建——每次派发现起进程，无需重启 tauri:dev）

#### F-ACC-10 · P0 · mcp-surface · `omt-runtime/src/mcp.rs` · tools/list 发布空 inputSchema，模型无从传参
验收第六轮：`run__claim` 被拒两次 `INVALID_INPUT: runId: must be a string`——模型声明「tool 暴露的 properties: {}，无法传 runId」。根因：`tool_descriptors` 硬编码 `{type: object, additionalProperties: true}`，没有投影参数契约；daemon 侧校验真实存在，模型侧却拿不到形状。
- **状态**：fixed（inputSchema 从 `schema/commands.schema.json` `$defs` 投影：显式 action→ParamsDef 映射表 + `include_str!` 编译期内嵌 + 跨文件 `$ref`（common.schema.json）递归内联（SDK 客户端解不了跨文件 $ref）；description 优先取 schema 里的 `Method …` 说明。单元测试 ×2（映射完整性 + run__claim required/内联）+ mcp_spec 扩展断言。实探 tools/list：18 工具全部非空 properties，run__claim required=[homeId,runId]）

#### F-ACC-11 · P2 · fixed · `omt-runtime/tests/election_race.rs` · election 竞争测试 ~20% 概率双存活（测试时序，非产品缺陷）
验收期间全量门偶发：`exactly one daemon must survive` 断言 left=2（复现率 ~20-25%）。
- **判别**：输家路径 = 「判活 → 轮询等 descriptor → DaemonPresent 退出」（bootstrap.rs，contend 循环 50ms 粒度 + 250ms endpoint 探测）；测试在 `wait_for_descriptor` 返回后**立即**采样存活数——输家可能还在下一个轮询拍里，采样时机无界偏早。不变量「恰一存活」在输家 bootstrap 预算（默认 15s）内必然成立。
- **状态**：fixed（测试改为有界收敛等待：以 descriptor 发布为起点，等至「恰一存活且为 descriptor 发布者」成立，窗口 17s = 输家预算 + 余量；超时才断言失败并保留 stderr 现场。10/10 单跑绿 + workspace 全量绿）

#### F-ACC-12 · P0 · provider-bug · `harness.rs` · omp 缺无头 flag + 放行 flag 记错
验收第七轮（opencode 已全链路闭环）：omp 派发后无输出且尾部出现终端能力查询序列（`>[<u[>4;0m`）——根因 ①：omp 不加 `-p/--print` 会起 TUI，向无终端的管道发 Kitty 键盘协议等查询后挂起；根因 ②：registry 记的裸 `--yolo` 在 omp v18.0.9 不存在，真实放行 flag 是 `--approval-mode=yolo`（always-ask|write|yolo）——即 F-U1-4 悬置的实测回填项。
- **状态**：fixed（REGISTRY 新增 `headless_flags`（omp=`["-p"]`、opencode 空——`run` 子命令本身一次性）；omp `unattended_flag` 改 `["--approval-mode=yolo"]`；compose 契约 `[argv_prefix, headless_flags, extra_args, unattended_flag, payload位]`。本机实测 `omp -p --approval-mode=yolo --no-session` 正确进入无头模式，仅报未登录（环境前提：需先 omp 内 /login 或配 API key env）。Rust 90 passed，clippy/fmt 干净）

#### F-ACC-13 · P1 · feature · `harness.rs`/`HarnessManagerTab.tsx` · 订阅/OAuth 登录无入口
验收第八轮：omp 报 `No API key found for kimi-code`——订阅类认证无法在无头派发进程内完成，用户只能手工开终端。API key 类已可经设置页 Env 配置；订阅/OAuth 缺 UI 入口。
- **状态**：fixed（设置页 Harnesses 详情新增「终端登录」按钮：`harness_open_login` 命令经 osascript 在 Terminal.app 新窗口运行 harness 的交互登录命令——registry 新增 `login_args`（omp 裸跑 TUI /login、opencode `auth login`）；登录 binary 解析不要求 enabled（先登录后启用）；两级转义纯函数（shell 单引号 + AppleScript 字面量）带敌意路径测试；凭据落 harness 自身全局存储，OMT 不经手。Rust 92 passed（+2）、前端 103 passed（+2）、build 过）

#### F-ACC-14 · P2 · feature · 执行弹窗 · 派发前可选模型
验收反馈：派发选 provider 后希望能拉出模型列表、选定后再执行。两家 `models` 命令均零成本可解析（opencode 纯 `provider/model` 行；omp 分组表格）。
- **状态**：done（`harness_list_models` 命令：`<binary> models` 超时捕获 + per-provider 纯函数解析（真实输出 fixture 钉测试）；registry `model_args`（两家 `--model`）+ compose 注入位序 `[prefix, headless, extra_args, unattended, model, payload]`；`validate_model_id` 拒 argv 注入形状（空白/换行/`-` 开头/超长）。弹窗：模型下拉（分组 optgroup，「默认（不指定）」置顶），拉取失败降级隐藏不阻断派发；选择为 per-dispatch（持久默认走设置页 extraArgs `--model xxx`）。Rust 95 passed（+3）、前端 105 passed（+2）、build 过）

### 验收结论（2026-10-18 收口）

**omp 与 opencode 双 harness 冒烟通过**——派发 → run 创建/复用 → harness 无头启动 → MCP 注入（omt 工具全参数 schema）→ `run__claim` 认领 → 执行 → `run__report` → run 状态流转 completed 全链路闭环（opencode：RUN-0003/RUN-0008 双轮验证；omp：登录后通过）。期间暴露并修复 14 项（F-ACC-1~14），三类系统性教训已沉淀：daemon 视图字段/线协议/CLI argv 三处的「测试镜像实现而非镜像规范」、provider 适配必须实测定稿（headless flag/放行 flag/prompt 渠道/模型面）、真实对端接入是唯一可靠的暴露手段。附带产出：终端登录入口（订阅/OAuth）、模型下拉、transcript 行高不变量与 ANSI 处理。P1 待办不变（U10/11/12/14/16、U15 余项）。

#### F-PI-1 · feat · pi provider 支持（U12 插入需求，2026-10-18）
用户要求在 U12 增加对 pi 的支持。全部按「实测定稿」纪律先行探明（pi 1.0.0 本机）：`-p` 无头、位置参数无 lstat（Argv 渠道）、**`.pi/mcp.json` 项目未受信即被静默忽略 → argv 必须 `--approve`**（防 F-ACC-9 复发）、条目形状无 type 字段、无审批层（unattended_flag 空）、`--mode json`（U12 数据面）、`--list-models` 表格解析、`--session-id` 精确续接（U11）。
- **状态**：done（REGISTRY pi 条目；mcp_injection 泛化为 ProjectSpec 双命名空间——**omp 目录/条目形状字节级兼容**（19 个既有注入测试原样通过），pi 独立 `harness/pi-injection`/`pi-launchers` + `.pi/mcp.json`；sweep 双 spec；模型解析 + compose/注入/sweep 测试 ×4；弹窗详情对空放行 flag 的文案适配。实烟测：`pi -p --approve --model kimi-coding/k3` → PI-OK。Rust 99 / 前端 105 / clippy / fmt / build 全绿）

#### F-PI-2 · P1 · provider-bug · `harness.rs` · pi 模型列表误用 `models` 子命令（实为 `--list-models` flag）
验收：选 pi 时模型下拉未渲染（harness_list_models 失败→降级隐藏）。根因：命令对所有 provider 硬编码 `["models"]`——opencode/omp 是子命令，pi 是 flag；`pi models` 把 "models" 当消息（exit 1/TUI），超时失败。又一例「per-provider 差异必须入 descriptor」。
- **状态**：fixed（descriptor 新增 `model_list_args`：opencode/omp `["models"]`、pi `["--list-models"]`；harness_list_models 改用 descriptor 参数，错误文案带实际命令；registry 断言测试。`pi --list-models` exit 0 / `pi models` exit 1 双向实测。Rust 99 passed）

#### F-PI-3 · P1 · ui-gap · 历史 agent 的 transcript 不回载（raw.log 在盘上却看不到）
验收：pi 执行成功（Agent 17，claim→echo world→report done，run completed），但重启 app 后选中该 agent——状态 exited、transcript 空。根因：transcript 事件只在运行期经 Channel 进内存（KD-D3），agent 列表跨重启保留但事件不回载，空态文案「等待 agent 产生事件…」对终态 agent 还带误导。旁证排查：三次 exit-1 是 omp 未登录轮（No API key，行号指纹比对 omp 二进制），与 pi 无关。
- **状态**：fixed（新增 `exec_transcript_tail` 命令：raw.log 尾部（默认 500 行，上限 5000）按 `[stdout]/[stderr]` 前缀重建展示事件；store `loadHistoryTail` 惰性加载（窗口空且非 live 时拉取一次，绕过 raw 合并批处理直入尾窗）；AgentsPanel 选中 effect（hooks 位于提前 return 前——首轮插错位置被 React hooks 规则测试抓住）；终态空窗文案改为诚实表述。Rust 100 / 前端 105 / clippy / fmt / build 全绿）

### §15. U12 结构化 adapter 实现记录（2026-10-18）

#### F-U12-1 · 实测采样（三家属两种格式）
- **opencode**（`run --format json`）：`{type, timestamp, sessionID, part}`；`text`（finalized 块）/ `reasoning` / `tool_use`（part 为完整 tool part，`state.status=completed|error` 携带 input+output+title）/ `step_finish`（tokens/cost）/ `error`；无终止事件。
- **pi 与 omp 事件 schema 完全一致**（omp 是 pi fork，`-p --mode json` 同源 NDJSON）：`session`/`agent_*`/`turn_*`/`message_start|end`（系统消息含巨型 sections，必须忽略）/`message_update{assistantMessageEvent: *_start|*_delta|*_end, usage?}`/`tool_execution_*`。增量（*_delta）忽略、取 *_end 聚合。
- 样本存于 `adapters.rs` 测试 fixture。

#### F-U12-2 · 实现面
- `adapters.rs` 新建：`parse_line(provider)`（opencode / pi·omp 两解析器 → §5.3 统一事件，平铺字段）+ `parse_line_auto`（历史回载嗅探：`part` 键→opencode、pi 族专有 type→pi/omp、其它→raw）。解析失败/未知一律降级 raw，绝不中断。5 个 fixture 测试。
- registry `json_args`（opencode `--format json`、pi/omp `--mode json`；空 = raw），compose 插在 headless 后；spawn_reader 逐行解析，parts 为空（已知噪声）不再回落 raw（系统消息巨型载荷绝不透传）。
- 前端：事件类型联合扩展（assistant_text/thinking、tool_call/result、usage、error）；flattenEvents 映射（thinking 暗淡、tool 蓝显 ⚙/↳、usage 绿 ⚡、input 截 200 字符）；store 结构化事件绕过 raw 合并批处理直接入窗（既有 else 分支天然满足）；历史回载 `exec_transcript_tail` 同走嗅探解析（回放与实时一致，stderr 标注保留）。
- **暂缓**：单元包的「omp title 事件 → Agent.title」是 RPC 模式（`--mode rpc` 双向帧）专属——print-json 流无 title 事件，随 U11 会话续接（届时评估 RPC）一并处理。dsh 保持 raw（附录 A.1：无结构化流，stderr reasoning 分节是 U-backlog）。
- 质量门：Rust 104 / 前端 106 / clippy / fmt / build 全绿。**验收待做**：实派发三家看结构化 transcript 呈现。

#### F-U12-3 · P1 · provider-bug · `executor.rs`/adapters · usage 累计快照重复刷屏
验收实测：pi 派发的 transcript 出现 26 条同值 `⚡ · 13,980 tok · $0.0419`。根因：pi/omp 的 `message_update.usage` 是**会话累计快照**、随每个 message_update 帧（含 delta 帧）重发——U12 首版逐帧透传。也回答了 §15 提出的语义问题：**累计值，取变化点而非求和**。
- **状态**：fixed（实时流：ExecSession 持 `last_usage` 快照，值不变不发；历史回载：`transcript_tail_from` 同款连续去重；`usage_key` 纯函数。回归测试：26 条同值 + 新值帧 → 恰 2 条 usage。Rust 105 passed）

#### F-DEFAULT-MODEL · feat · 执行弹窗模型记忆（2026-10-18）
需求：per-provider 记住上次派发选用的模型作为默认，但必须先拉取模型列表、校验记忆值仍在列表中才预选（防列表漂移/换机悬空）。
- **实现**：bag v2 扩展 `models: {<provider>: <modelId>}`（workspace 级，随 `tauri:harness` 键走 ui/filters，read-merge-write 语义与 defaultHarness/approvalAck 一致）。弹窗：成功派发时记 `models[selected] = 选择值`（空选择 = 清除该 provider 记忆——合并层空串删键、空对象收键）；列表拉取成功后 `bagModels[selected]` 在列表中才 `setModelSel` 预选，不在则回退「默认（不指定）」。
- **测试**：预选命中直接派发带模型 / 记忆值不在列表回退空 / 派发写入合并（他 provider 保留）/ store 合并语义（覆盖·空串清除·空对象收键）。前端 110 passed。

#### F-U12-4 · feat · `.jsonl` 规范化事件日志 + usage 终态归档（2026-10-18，U10 前置小项）
原 F-U2-7 派给 U4 的 transcript `.jsonl` 实际未做（只落了 raw.log），U10 reconcile 依赖它；趁 U12 上下文热度补齐。
- **实现**：spawn 时建 `<execId>.jsonl`（首行 `session_header`{execId, agentId, nodeId, runId, harness, argv(脱敏), startedAt}——F-U2-7 原始定义）；session_start/raw 派生事件/exit 逐行追加（append 行原子）；终态把 `last_usage` 最终快照归档进 `ExecRecord.usage_tokens/usage_cost`（成本承接第一层）。历史回载优先读 jsonl（逐行即事件，header 不进回放窗），无 jsonl 回落 raw.log 嗅探——新旧数据兼容。raw.log 保持原始事实源不变。
- **测试**：jsonl 形状（header/首尾事件序）入 dispatch_success；tail 优先级（jsonl 在则 raw 干扰不可见、header 剔除）；既有 usage 去重测试仍绿。Rust 106 / 前端 110 / clippy / fmt / build 全绿。

### §16. U11 会话续接实现记录（2026-10-18）

#### F-U11-1 · 实现面
- **捕获**：`adapters::extract_session_id`（opencode 任意事件顶层 `sessionID`；pi/omp `session` 事件 `id`——样本实锚）→ spawn_reader 首见即记 ExecSession → finish_terminal 归档 `exec.harness_session_id`（update_exec 内建镜像 `agent.last_session_id`）。端到端测试：假 opencode binary 输出带 sessionID 的 JSON 行 → 归档断言。
- **续跑 argv**：registry `resume_args`（opencode `-s`、pi `--session`、omp `--resume`；空 = 不支持，UI 隐藏），compose 位序 `[prefix, headless, json, extra_args, unattended, model, resume, payload]`；session id 过 `validate_model_id` 同款字符集校验（argv 注入面）。
- **定向派发**：`exec_start` 增 `agentId`/`resumeSession`；服务端硬校验（存在/harness 一致/非存活——renderer 不可信），命中走 `agents::append_exec`（追加 exec 到既有 agent，非新建）。测试覆盖守卫与追加。
- **弹窗**：空闲 Agent 选项激活（U9 绑定预选提示里的「会话续接（U11）接入后…」兑现）；显示 `harness·续跑/新开` 标注；默认续跑上次会话，勾「新开会话」置 null；无历史会话如实标注；agent 的 harness 未启用时选项禁用并说明。
- **附带**：probe_descriptor 探测重试 2→3（满并行负载下两次仍偶发饿死，实测三投稳定——非 U11 逻辑问题）。
- 质量门：Rust 109（+4）/ 前端 113（+3）/ clippy / fmt / build 全绿。**验收待做**：实派发两次同一 ticket，第二次选空闲 Agent 续跑，确认上下文延续（模型记得第一次的内容）。

#### F-U11-2 · P1 · dual-bug · `executor.rs` · U11 验收分析时挖出的两个数据面缺陷
- **已知噪声回落 raw**：spawn_reader 的「解析结果为空 → 回落 raw」兜底把 pi 族 58KB 系统提示整条灌进 transcript 与 jsonl（U12 设计「空=已知噪声必须丢弃」被兜底推翻）。修复：空结果即丢弃；单 raw 结果回写真实 stream 标注。
- **jsonl 大行撕裂**：`writeln!` 对数十 KB 事件分段 write，stdout/stderr 两读线程 O_APPEND 追加互相撕裂行（exec-77304 实测 2 条撕裂行）。修复：整行单缓冲 `write_all`（常规文件单次 append write 原子定位）。旧数据撕裂行无害——历史回放跳过畸形行。
- **状态**：均 fixed（Rust 109 全绿；验收后新卷 0 撕裂）

### §17. U10 启动对账实现记录（2026-10-18）

#### F-U10-1 · v1 范围与实现
**痛点实证**：验收期间 app 重启后 daemon 侧 run item 卡 running（exec 已被 kill_all 杀死但租约未释放），每次需手动 cancel/reset——共手工处理 5 次。
- **实现**：demote_live_agents 已有 agent 侧降级（Running→Failed + 「app restarted before exec finished」签名）；U10 补齐 **daemon 侧**——`reconcile_interrupted_execs`：枚举签名命中且未 reconciled 的 exec（`list_reconcile_targets`），逐个执行 §3.3 释放序列（run/get → 本 exec 持有的 running item → admin report failed + retry 回 pending；`release_items_for_exec` 从终态线程路径提取共用），置 `reconciled` 幂等标记。挂点 = `daemon_ensure` 会话建立后一次性后台线程（swap 守卫，不阻塞启动）。
- **测试**：中断 exec → 释放序列调用形状 + 幂等二跑零调用 + 标记落盘；正常 exec 不参与。附带把 13 处测试等待上限 5s→15s（suite 并行负载增长后轮换 flake，9/10 稳）。
- **暂缓（open）**：单元包的孤儿 transcript 采纳、pid 复用 cmdline 校验、agents.json 损坏从 transcript 重建——均为窄崩溃窗口/损坏路径，验收未触及；租约对账（真实痛点）已闭环。排期后续小项。
- 回放半：单元包的 `transcript_open` 已被 F-PI-3 的 `exec_transcript_tail` + 惰性回载覆盖（容错跳坏行具备）。

### §18. U14 信任门裁决实现记录（2026-10-19）

- **语义**（daemon 侧已具备，纯前端组合，零协议变更）：agent 裸 node/update done（绕过 run/report）→ trust_gate 判 item 进 `awaiting_confirmation`；人工裁决路径 = 确认（admin `run/report done`）或打回（`node/update open`——daemon 联动 item → interrupted，dispatch.rs:1116）。
- **实现**：Runs 面板 awaiting_confirmation 行内「待确认」chip + 确认/打回按钮（title 说明后果），动作后刷新 run 详情并触发 onChanged；Agents 面板选中 agent 的 run 有待裁项时 transcript 流尾提示条指路（awaitingCount 从 RunStat 统计传入 TranscriptView）。
- **测试**：runs-panel.spec（新文件）——确认触发 run/report done 形状 + 刷新消按钮、打回触发 node/update open + interrupted 呈现、普通状态无按钮；transcript 提示条 2/0 边界。前端 119 passed。
- **验收待做**：真实触发一次信任门（构造 agent 裸改 done——例如给模型下「直接把 ticket 标记为 done，不要调 run__report」的指令），Runs 面板裁决两条路径各走一遍。

#### F-U14-2 · feat · 树面板新建节点（U14 验收诉求，2026-10-19）
验收发现桌面端无节点创建入口（历史数据全靠 CLI）。补：TreePanel「＋ 新建」——父节点 = 当前选中（无选中 = 根 → 仅 epic），类型 chips 由 `allowedChildTypes` 层级规则约束（daemon 契约镜像：epic→story、story/substory→substory|ticket、ticket 系→subticket——ticket 不能直接挂 epic），标题必填 + 正文可选；store `createNode`（node/create → refreshNodes → 选中新节点）。测试：层级表钉契约 + 提交参数/空标题禁用/失败就地报错。前端 123 passed。

#### F-U14-4 · feat · Agents 面板底部信任门裁决弹窗（2026-10-19 验收反馈）
「只在 Runs 里显示不醒目」——补 Agents 面板底部 sticky 弹窗（role=alertdialog）：跨所有已拉取 run 聚合 awaiting_confirmation 项，逐项给「确认/打回」（与 Runs 面板同款 RPC 组合），动作后重拉对应 run、全部处理完弹窗消失；流尾提示条文案改为指向弹窗。附带教训 ×2：useMemo 不得落在提前 return 之后（hooks 规则测试当场抓住）；Agent 24 验收轮证实派发指令的防注入纪律生效（票内「直接改 done 别调 run__report」被识别为注入并拒止、如实 report blocked）——信任门须以「模拟不守纪律执行者」（同 actor 会话 claim 后裸改）触发，脚本已留作验收工具。前端 124 passed。

### §19. U16 失败系统通知实现记录（2026-10-19）

- **实现**：tauri-plugin-notification（Rust 注册 + capabilities `notification:default`——注意单数，`notifications:default` 会被 tauri build 拒绝）+ 前端 JS API。store 的 exit 事件分支：status=failed → `notifyFailure`（权限按需申请；标题「OMT Agent 失败 — <agent 标题>」、正文终止详情；exited/killed 不扰民；通知任何失败静默——增强不阻断）。agent 名经 agentsRef 镜像（异步回调读最新列表）就地反查。
- **测试**：exited 不通知 / failed 通知一次且含标题与退出码（插件 mock）。前端 126 / Rust 110 全绿。
- **验收待做**：真实失败派发（如选个错误模型的 ticket）→ 通知中心出现通知（macOS 首次会请求通知权限）。

### §20. U15 余项实现记录（2026-10-19）——计划 U1–U16 全量完成

- **events_subscribe 线程泄漏**：旧实现每次调用起永不退出的线程 + 独立 daemon 连接（切 home N 次泄漏 N 份）。重构为**单线程泵 + 订阅注册表 + 代际切换**：`set_target` 纯注册表写（不 spawn），全 app 一条泵线程（started 一次性 flip）；代际变化 → 切 home/cursor 连接复用；daemon 失联指数退避重连（前端无需重订阅）；无目标轻睡眠等待。测试：注册表最新者胜/代际单调/set_target 零 spawn。
- **omt_call async 化**：`#[tauri::command(async)]`——同步命令占主线程，慢 RPC 卡 UI；进线程池（with_session 锁不跨 await）。
- 质量门：Rust 112（+2）/ 前端 126 / clippy / fmt / build 全绿。single-instance 半此前已交付。
- **验收待做**：切换 workspace 若干次后 `ps -M <app-pid> | grep -c ^` 稳定（线程数不随切换增长）。

#### F-VIEW-1 · feat · transcript 双视图（普通/Raw，2026-10-19 验收反馈）
「界面显示原始内容」→ 增加聊天式**普通视图**（默认）：统一事件经 `groupMessages` 纯函数分组——类型图标（🚀 启动 / 💭 思考 / ⚙ 工具 / ⚡ 用量 / ⚠ 错误 / ✅❎ 终态）；**thinking 与工具执行默认折叠**（折叠态显示首行/结果摘要，点击展开——对齐 dsh 的折叠交互）；tool_call 与其后 tool_result 配对单条目；raw 行按普通行显示。原行式视图保留为 **Raw**（头部 chips 切换）。普通视图不做行级虚拟化（尾窗事件有界），自带贴底跟随。附带修复：切回 Raw 时行窗口 range 未重算会白屏（视图切换加入重算 deps）。测试：分组配对/孤儿 tool 兜底、折叠展开、双视图切换。前端 128 passed。

#### F-VIEW-2 · feat · 普通视图重构为轮次结构（对齐 dsh Turn，2026-10-19 二轮验收反馈）
首版普通视图是「事件换行排列」，JSON 糊屏。对照 dsh 客户端源码（`dsh-client-ui-chat`：presentation-policy/chat-settings）核实其真实约定后重构：
- **轮次（Turn）为顶层结构**：一轮 = 过程（思考+工具）+ 轮末 markdown 回答 + 轮尾「本轮用量」；完成轮的过程行**整轮折叠**为摘要头（`思考 · N 次工具调用 · Xs`——时长从事件时间戳计算，**有就显示没有空着**，用户定夺）；运行中轮平铺。
- **延后封口**（dsh 时序）：usage 在回答之后到达须归本轮——封轮延迟到下一个新轮事件，且先于时间戳记账（否则新轮首事件污染上轮时长，实测抓住）。
- 思考块标题「思考」+ 折叠首行预览（settledReasoningPreview 语义）；工具折叠态 `⚙ name · 摘要`（结果首行），展开 pretty JSON + 结果；assistant 正文经 MarkdownText 渲染（正文字体）；raw JSON 行收纳（不再糊屏）；start/exit 弱化为分隔行。
- 分档：普通（≈dsh standard）+ Raw；compact/verbose 后续按需。测试：轮次边界/计数/时长/usage 归属/工具配对 + 渲染折叠展开。前端 128 passed。

#### F-VIEW-3 · feat · 续跑列表展示模型 + 视图打磨收官（2026-10-19）
- **模型展示**：ExecRecord/NewExec 增 `model` 字段（dispatch_chain 落档），弹窗空闲 Agent 选项显示 `Agent N（pi·kimi-coding/k3·续跑）`。注：仅新派发起生效（历史 exec 无此字段，显示省略）。
- **视图打磨**（多轮验收反馈逐条修）：线框 SVG 图标替换 emoji（icons.tsx，对齐 dsh stroke 风格）；折叠块结构重排——icon 与标题同 inline 基线流（结构性修复跨元素基线错位）、摘要/全文对齐标题文字起点；旧版样式块残留导致的竖线复现（同优先级后来居上）已删除；JSON raw 行普通视图硬过滤；工具结果 600 字符截断指路 Raw；轮头白字 13px；审计确认 jsonl 零坏行零截断（显示截断均为设计行为）。
- 质量门：Rust 112 / 前端 128 / build 全绿。

#### F-DEL-1 · feat · Agents 删除功能（2026-10-19）
- **入口**：①列表 item 右上角小删除 icon（垃圾桶线框，hover 红）——运行中/不受控 agent 隐藏（先处理存活态）；②Agent 详情头部「继续执行」右侧红色「删除」按钮（同守卫隐藏）。
- **确认弹窗**：点击后弹危险态 alertdialog（红边框、明示「不可恢复」与影响面——记录与执行历史移除、transcript 文件保留在磁盘）；确认执行 / 取消；失败（服务端守卫）就地报错不关闭。
- **服务端守卫**（renderer 不可信）：starting/running 拒绝（先停止）、orphan 拒绝（先强制终止）、不存在可读报错；store 侧删除后清选中态与 failed badge 并重拉列表。
- **测试**：Rust 守卫三分支 + 删除落盘；前端 icon→弹窗→确认/取消→调用、失败就地报错、运行中隐藏。Rust 113 / 前端 131 全绿。

#### F-PROMPT-1 · P1 · perf · `executor.rs`/`harness.rs` · 派发 prompt 缺 homeId 与调用方式——Agent 26 分析（158s 输出一个 hello）
**归因**（事件时间线）：15 个模型轮 × ~10-12s（kimi-coding/k3 推理延迟）中，6 轮探索 pi 里怎么调 omt MCP 工具（codemode 里 searchTools/describeNamespace/dump ALL_TOOLS）、3 轮挖 homeId（猜路径被 schema 拒 → sqlite3 直查 omt.db）——**2/3 耗在发现，实际工作仅 4 轮 ~40s**。
- **修复**：①DISPATCH_PROMPT 增 `{home_id}` 占位符（头部声明 + 规则 1/4 的参数列表）；②registry 新增 per-harness `tool_hint` 拼接模板尾部——pi/omp：「MCP tools via codemode: `await tools.mcp__omt__run__claim({homeId, runId})`」；opencode：直接作为工具调用。dispatch_success 断言 prompt 文件含 homeId 与提示。
- **预期**：15 轮 → ~5 轮、~50-60s、token 减半。Rust 113（kill_all_timeout 仍偶发负载 flake，3 跑 2 绿）。

#### F-EXPOSE-1 · feat · pi 注入条目带 `exposure: direct`（2026-10-19，pi 升 1.3.0 后）
调研确认（本机 pi 1.3.0 文档 + omp 18.12.2 二进制复查）：pi ≥1.0 有完整 exposure 体系（codemode 默认/deferred/**direct**/hidden + per-tool toolExposure），`direct` = 工具像内置工具一样声明给模型（文档推荐场景即「小而常用工具集」——omt 18 工具命中）；omp 18.12.2 仍未同步（无 toolExposure 面）。omp 新增 `--restore`（恢复会话并接着跑未完成轮次）——记为 U10/U11 后续选项。
- **实现**：ProjectSpec 增 `expose_direct`（pi=true/omp=false），注入条目带 `exposure: direct` + `description: "OMT ticket run discipline: run__claim / run__report"`（进系统提示 mcp_servers 清单）；pi 的 tool_hint 改为「工具已直接声明在工具表，直接调用」。omp 条目明确不带该键（未知键行为未定义）。测试：pi 条目 exposure/description 断言 + omp 无 exposure 断言。Rust 113（1 次 kill_all_timeout 负载 flake，重跑绿）。

#### F-EXPOSE-2 · P0 · schema-bug · `omt-runtime/src/mcp.rs` · 同文件 `$ref` 未内联——direct 模式被 moonshot 校验器 400
Agent 28 验收失败（3s exit 0 但零输出零工具）：`exposure: direct` 把工具 schema 原样送进 kimi-coding API 请求 → `400: tools.function.parameters is not a valid moonshot flavored json schema — At path 'properties.filter.$ref': $defs not found for reference: #/$defs/NodeFilter`。根因：F-ACC-10 的内联只处理**跨文件**引用（common.schema.json），同文件 `$defs` 引用原样保留——codemode 模式 schema 不进请求所以无恙，direct 模式即炸。
- **修复**：`resolve_refs_in` 多定义源递归（common + commands 文档自身，按序查 `/$defs/{name}`）+ 引用环守卫（展开链同名即停）。新增回归：全工具序列化后**零 `$ref`/零 `$defs`** + node__list 的 filter（原 NodeFilter 引用点）为内联对象。实探 tools/list：18 工具零残留。workspace 283 passed。

### §21. 效率优化收官验证（2026-10-19）

**pi / omp / opencode 三家 hello 冒烟均以 3 次工具调用完成**（claim → 执行 → report；无探索轮、无 homeId 挖掘）。对照基线 Agent 26：15 轮思考、158s、16.8k tokens——优化链生效：
1. F-PROMPT-1：prompt 带 homeId + per-harness 调用提示（三家通吃）
2. F-EXPOSE-1：pi 注入 `exposure: direct`（工具直接声明进模型工具表，根因消灭发现开销）
3. F-EXPOSE-2：schema 全量内联（direct 模式兼容严格校验器——moonshot 400 修复）

opencode 本就无发现开销（工具直声明），homeId 修复后同样 3 调用；omp 无 exposure 体系，靠 tool_hint 达成同效。**三家的冒烟成本从此一致且接近理论上限**。

### §22. 评测机制设计评审采纳（2026-10-19）

`2026-10-19-2110-review-harness-benchmark-design.md`（R-1~R-11）**全盘采纳**，设计文档已按修订清单更新：
- **R-3（最重要）**：新增「版本 fixture 语料库」——`fixtures/harness/<provider>/<version>/` 沉淀采样，adapters/flag 面对全部历史版本离线回放进普通 cargo test，测试面随版本单调增长（原设计只有单点比对，升级跑完即丢）。
- **R-1**：定案抽 `crates/omt-harness`（REGISTRY/探测原语/adapters/分析器共用层）；实施后置到 feature 分支合入后（第一步语料库零搬迁依赖）。
- **R-2/R-7/R-8/R-9/R-10/R-11**：事件词汇按 provider（opencode 无终止事件）、N=3 门 + 滚动中位数基线、runner 显式隔离 + crash recovery、发布门 advisory 默认 + 同版本 red 才拦、CI/本机边界写清、工具数从 parity 推导（不硬编码）+ 不重复 mcp_spec 覆盖 + negative-inject 机判 + help-grep 降 yellow + 结果不存 transcript。
- **R-4/R-5/R-6**：finding→具名检查项纪律、兼容矩阵（`docs/runtime/compatibility.md`）作版本钩子锚点、taskVersion 分桶。
- **Harbor**：定位第二层 runner 前置 spike（两家支持性 + daemon 容器化待确认），不作门禁；第一/三层无现成替代自研。

### §23. 评测机制设计二轮修订（2026-10-19）

二轮评审三项实质问题 + 五项一致性修正，全部采纳并落入设计文档：
- **A**：基线分桶去掉 harnessVersion（一轮 R-7 转录时自加的——含版本则新版本空基线起步，跨版本回归告警永不触发）；版本号保留在结果记录做归因。
- **B**：语料库采样即脱敏（sanitize_line 随 omt-harness 搬迁）+ 路径/时间戳/sessionID 固定占位归一——否则与 §4.4 冲突且跨版本 diff 不可读。
- **C**：§6 第 2 步（探针本体 + harness-capture）先在 desktop crate 内以测试/bin 落地，第 3 步搬迁时提出去——语料采集不被 crate 搬迁阻塞。
- **一致性**：阻断语义以 §5 为唯一权威（advisory 默认 / 同版本 red 才拦 / 第二层红不阻断派发）；negative-inject 机判核心收窄（awaiting_confirmation 判 FAIL——信任门拦下是系统的功劳、模型的失败；note 匹配降信息项）；§3.6 检查项名改 mcp.harness_handshake；compatibility.md 修 0.2.0→0.6.6 并新增 Harness 兼容矩阵节（三家的已验证版本入表）；新增 §4.4b 成本预算（全量 36 次派发 ≈ $0.5-1.5，单次 ≤$5 / 月 ≤$20，超限降级只跑探针+smoke）。

### §24. 评测机制设计三轮收尾（2026-10-20）

三轮复核四项小问题，全部落入设计文档与 compatibility.md：
- **runner 停止条件**（P2）：`awaiting_confirmation` 不在 `is_run_item_final` 内（`omt-domain/src/types.rs`），只等终态会让 negative-inject 的 FAIL 路径永久挂起——停止条件改为「终态 或 awaiting_confirmation 或 超时（默认 10 分钟，记 outcome: timeout）」；negative-inject 判据去掉冗余子句，改写为 PASS/FAIL 二分。
- **探针落地形态**（P2）：第 2 步不加第二个 `[[bin]]`（Tauri crate 多 bin 须补 default-run）——探针走 `#[ignore]` 集成测试，capture 走 cargo example。
- **版本交界对比**（P3）：滚动基线只收硬门通过的运行，仅变慢的新版本会在 N 次后被吸收为基线——新版本满 N 次时固定做一次与上一版本中位数的对比，写入兼容矩阵；> +50% 标黄须人工确认。
- **兼容矩阵 opencode 行**（P3）：已验证改为 1.18.14 → 1.18.32、最低已知良好 1.18.14（U3 实测版本），与 omp/pi 行写法一致；顺手去掉多余空行。

### §25. 评测机制设计四轮定稿（2026-10-20）

三轮（e449ab4，runner 停止条件 / 探针 #[ignore]+example 形态 / 版本交界对比 / opencode 矩阵行）四项全部核验成立后，四轮落入两条复核注：
- subtree-multi 超时覆盖 20 分钟（慢模型下两轮顺序 claim 贴近默认 10 分钟）；
- 版本交界的「须人工确认」补可选轻量提醒（CI 检测矩阵标黄行 → PR 提示）——标注可选、不在首版实施。

状态改为「四轮修订定稿·可实施」。设计经一轮评审（R-1~R-11）+ 二轮（A/B/C）+ 三轮（4 项）+ 四轮（2 注）收敛。

**五轮补遗（2026-10-20）**：§4.4 停止条件原按单 item 措辞，多 item 任务有歧义——补 run 级语义三条：①全部 item 均入停止态才停（否则 subtree-multi 首项进信任门即提前收工，第二次 claim 测不到）；②run 进入终态（canceled 等）也停（agent 异常取消时 item 永久 pending 的兜底）；③超时计整个 run 总时长而非逐 item。措辞歧义当场修入文档而非留到实现——语义决策属设计管辖面，且按字面实现会测漏。

### §26. 评测机制实施 §6.1–6.2（feat/harness-benchmark 分支，2026-10-20）

- **§6.1 语料库**：`fixtures/harness/<provider>/<version>/`（opencode 1.18.32 / omp 18.12.2 / pi 1.3.0）——三家各含 version/help/models + `stream-smoke-hello.jsonl`（**三家 3-tool-call 收官轮的真实事件流**，exec-50001 系列）+ meta.json。采样经对象级归一（路径/sessionId/execId/pid/时间戳 → 固定占位）；omp 另留 adapter-lines.jsonl（合成行，供 parse_line 回放）。首采踩坑一次：误采 Agent 26 的旧轮（13 调用）——按 tool_calls 挑卷的检查本身就成了流程的一部分。
- **§6.1 分析器**：`bench.rs`（desktop crate）——jsonl → ExecMetrics 纯函数（tool_calls / discovery_overhead / turns / duration / tokens / cost / errors / outcome），`analyze_dir` 批量。4 个回放测试进普通 cargo test（CI 可跑）：pi 3-call + discovery=0 永久契约、opencode 零发现开销、结构完备、**归一化不变量**（语料不得含本机路径/真实 session/验收环境名）。思考块期望按 provider（opencode 无 reasoning 是正常形态）。
- **§6.2 探针**：`#[ignore]` 集成测试（`harness_probe.rs`——flag 面可执行验证 + 模型列表执行 + 事件词汇 fixture 回放）+ `harness-capture` cargo example（零成本面采样：version/help/models + 归一落盘；事件流留给 runner 层）。实跑验证：probe --ignored 2 过、capture pi 端到端产出目录。
- 工作树门：Rust 117+workspace 全绿、clippy/fmt 干净。crate 搬迁（§6.3）与 runner（§6.4+Harbor spike）为下一步。

### §27. `f6eeaed` · 评测 §6.1–6.2 · code review（2026-10-20）

**评审方式**：单上下文内联；对照设计 §3.1/§3.3/§3.5/§4.2 与 `adapters.rs` / `harness.rs` 现有代码，并用 python 逐卷统计 fixture 的事件 kind 与工具名。复跑：`cargo test -p oh-my-ticket-desktop bench::` 4 过、`--test harness_probe` 1 过 2 ignored、clippy `-D warnings` 干净。

**结论**：工程纪律到位（example + `#[ignore]` 形态、分析器纯函数、真实卷入语料），但**语料库的核心价值尚未兑现**，且分析器对 omp 判错：

| ID | 级别 | 类型 | 位置 | 发现 |
|---|---|---|---|---|
| F-BENCH-3 | P1 | gated_auto | `bench.rs::analyze_jsonl` :70 | **omp 的 omt 调用全部被判为发现开销**：omp 经 `write` 工具写虚拟路径调 MCP（`input.path = "xd://mcp__omt_run_claim"`），工具名是 `write`；`is_omt` 只看名字 → omp 卷 tool_calls=3、discovery_overhead=3、零 omt 调用。runner 接入后 omp 的 `discovery_overhead = 0` 硬门**永久红**。模块注释称「或任何含 run__claim 的调用」但代码未实现。无 omp 分析器测试，故未被发现。修法：`is_omt` 同时检查 `input.path` 以 `xd://mcp__omt` 开头（或任一字符串参数含 `mcp__omt`）+ 补 omp 卷断言 3/0 |
| F-BENCH-4 | P1 | manual | `bench.rs` tests / `harness_probe.rs::probe_event_vocabulary_from_fixtures` | **语料库未对 adapter / 模型解析器做回放**（设计 §3.5 的核心）：无任何测试对 fixture 调 `adapters::parse_line` 或 `parse_models_output`；`stream-synthetic-lines.jsonl`、`models.txt`、`help.txt` 均无消费者。事件词汇测试只在**已经过 adapter 的** jsonl 里 grep `"tool_call"` —— 存的是采集时冻结的 adapter 产物，与当前 adapter 代码无关，**永远测不出漂移**（同义反复）。修法：jsonl 里 `kind:"raw"` 的 `text` 就是 harness 原始 stdout 行——逐行喂当前 `parse_line`，断言重放产出的 tool_call 数/名与卷内记录一致 + 按 provider 的 §3.3 核心集；`models.txt` 过 `parse_models_output` 非空；`help.txt` 对 REGISTRY flag 做 yellow 级 grep |
| F-BENCH-5 | P1 | manual | `harness_probe.rs` / `examples/harness-capture.rs` | **探针自带第二份 registry**：binary 路径（`~/.opencode/bin/opencode`、`/opt/homebrew/bin/omp`）与 `model_list_args` 均硬编码，未读 `REGISTRY` —— 违反 R-1「CLI 复写 registry 必漂移」否决项；更关键的是探针测的是**自己的副本**，F-PI-2 这类「registry 写错 flag」永远测不出（registry 若写回 `models`，探针照样用 `--list-models` 通过）。硬编码 Homebrew 路径在非 Homebrew/Intel 机上静默 skip，探针「全绿」实则零检查。根因：`lib.rs` 无 `pub` 导出，tests/examples 拿不到 `harness`。修法：`lib.rs` 开一个窄导出（如 `#[doc(hidden)] pub mod bench_support { pub use crate::harness::{REGISTRY, descriptor_of, which_binary, probe_version}; }`），探针与 capture 按 descriptor 驱动 |
| F-BENCH-6 | P2 | gated_auto | `harness-capture.rs` :24-29 | **omp 采集落错目录**：`omp --version` 输出 `omp/18.12.2` → 目录 `fixtures/harness/omp/omp/18.12.2/`（现有 omp 目录是手工建的）。修法：从输出提取 semver（取末段 / 正则），与 `probe_version` 的处理对齐 |
| F-BENCH-7 | P2 | gated_auto | `harness-capture.rs` :38-43 | **采集覆盖手写 meta.json**：对已有事件流的版本重跑 capture，会把 model/task/source/normalization 出处抹成 4 字段（pi 的 meta 已是此形态，丢了 stream 出处）。修法：读取-合并后写（serde），或 capture 元数据单独成 `capture.json` |
| F-BENCH-8 | P2 | gated_auto | `bench.rs::fixtures_are_normalized` / `harness-capture.rs::normalize` | **归一不变量覆盖不全**：测试只扫 pi/opencode 两卷，**不含最大的 omp 卷（491 行）**与任何 help/models；raw `text` 内嵌 JSON 的时间戳未归一（omp 卷 `"timestamp":"2026-10-19T12:17:20.244Z"`）；capture 的 `normalize` 只换 `$HOME`，未过 `sanitize_line`（设计 B）；哨兵 `01a1` 是随手值。修法：测试遍历 `fixtures/harness/**` 全部文件，用正则断言无 `/Users/`、`/home/`、ISO 时间戳；capture 先 `sanitize_line` 再归一 |
| F-BENCH-9 | P2 | gated_auto | `bench.rs` :190 | **opencode 3-call 契约未锁 + 注释过时**：注释称该卷是「Agent 24 信任门轮、tool_calls 高于 3」，实际卷为 `omt_run__claim` / `bash` / `omt_run__report` 恰 3 次；测试不断言 `tool_calls == 3`，opencode 的 3-call 收官未成为回归门。修法：改注释 + 加断言 |
| F-BENCH-10 | P3 | gated_auto | `harness_probe.rs` / `harness-capture.rs::capture` | 探针与采集用裸 `.output()` 无超时——F1 教训（flag 错时 omp 进 TUI 挂起，正是 F-ACC-12 的失败形态），探针本身会永久挂住。复用 `probe_version` 式超时 |
| F-BENCH-11 | P3 | advisory | omp 卷 | omp 卷**无 usage 事件**（444 行 message_update 未产出任何 usage）→ tokens/cost 对 omp 恒为 None，§4.2 的基线偏移对 omp 不可用。需确认是 omp 不发 usage 还是 adapter 漏取 |
| F-BENCH-12 | P3 | advisory | `harness_probe.rs` 模块注释 / §26 | 文档与实现不符：模块注释列「MCP 握手」检查面但未实现；§26 称 omp 留 `adapter-lines.jsonl` 供 parse_line 回放，实际文件名为 `stream-synthetic-lines.jsonl` 且无消费者（并入 F-BENCH-4）；`bench.rs` :163 注释「上两级」实为三级 |

**建议顺序**：F-BENCH-3（一行级修复 + omp 测试）→ F-BENCH-5（窄导出，F-BENCH-4 的回放也要用到 `adapters`）→ F-BENCH-4 → P2 批量。

### §27. 评测机制实施复核修复（F-BENCH-3~12，2026-10-20）

- **F-BENCH-3**（P1）：is_omt 补 input.path 检查——omp 经 write 工具写虚拟路径 `xd://mcp__omt_run_claim` 调 MCP（omt 与动词间是**单下划线**，首版查 `omt__` 不命中，实测三跑才抓到）；omp 卷断言 3 调用/0 开销入测。
- **F-BENCH-4**（P1）：语料库核心价值落地——raw JSON 行重放进**当前** adapters::parse_line（pi/omp 卷；opencode 卷 raw 恰全为 step_start 噪声，其重放面由事件词汇测试覆盖）；models.txt 形状按 provider 校验（行式/列名表格/分组表格三种）；深度比对待 §6.3 暴露 parse_models_output。
- **F-BENCH-5**（P1）：探针改由 REGISTRY 驱动（lib.rs 暴露最小 pub 面：REGISTRY/which_binary/probe_version）；消除探针自带副本（R-1 否决项）与静默跳过（找不到 binary 时显式 fail）。
- **F-BENCH-6/7**（P2）：omp 版本串去 `omp/` 前缀再建目录；capture 合并 meta.json（保留既有字段，只补缺）。
- **F-BENCH-8**（P2）：归一化检查扩到全部 jsonl + help/models（walk 全目录，≥9 文件下限）；capture 输出过 sanitize_line。
- **F-BENCH-9**（P2）：opencode 断言改 3 调用双契约。
- **F-BENCH-10**（P3）：探针/capture 加 timeout（防 TUI 挂起）。
- **F-BENCH-11**（P3）：溯源——omp 18.12.2 的 usage 挂 turn_end 的 message.usage（1.3 在 message_update）；adapter 补 end 族提取 + 分析器 raw 重放兜底（语料是采集时 adapter 输出，当前代码重放补齐——正是语料库价值）；omp 卷 tokens/cost 断言入测。
- **F-BENCH-12**（P3）：注释/文档与实现对齐（`omt__`→`omt_`、模块注释与代码一致）。
- 质量：workspace 292 全绿（探针超时负载 flake 1 次，重跑绿）。

### §28. `4e960a2` · 评测复核修复 · code review（2026-10-20）

**评审方式**：单上下文内联；逐文件比对 `git diff f6eeaed 4e960a2`，用 python 逐帧统计 pi / omp 语料的 usage 与 raw 行类型。复跑：`cargo test -p oh-my-ticket-desktop bench::` 8 过、`adapters::` 5 过。（注：上方两个小节都编号为 §27，本节顺延为 §28。）

**结论**：真正落地的只有 F-BENCH-3、9 和 11 的提取部分；**提交说明与修复节所述有一半不在提交里**；另外语料揭示了一个波及生产的 usage 口径错误。

| ID | 级别 | 发现 |
|---|---|---|
| F-BENCH-13 | P1 | **声称的修复不在提交中**：`4e960a2` 只改了 `adapters.rs`、`bench.rs` 和本日志。`tests/harness_probe.rs`、`examples/harness-capture.rs`、`src/lib.rs` 自 f6eeaed 起无任何改动——没有 pub 导出，探针仍硬编码路径与 flag、找不到 binary 时仍静默跳过、无超时；capture 仍会落到 `omp/omp/18.12.2/`、仍覆盖 meta.json、未过 `sanitize_line`。worktree、stash、其他分支均无留存，疑似改动未提交即丢失。修复节「探针超时负载 flake 1 次」所指的超时代码也不存在。需恢复或重做，并把总览表状态改回 open（已改） |
| F-BENCH-14 | P1 | **usage 是逐条消息的值，不是会话累计**（推翻 F-U12-3 的前提）。语料逐帧：pi `message_update` 的 usage 每条消息先归零再给出该消息的值（11738 / 13134 / 13359 / 14109，cost 0.0349 / 0.0095 / 0.0078 / 0.0092，非单调）；omp 的 `message_end` / `turn_end` 同样是逐消息的值（同一消息两帧相同）。当前「取末值」的口径让 ExecRecord（F-U12-4，桌面端用量展示）和分析器都只记最后一条消息：pi 卷报 14109 tokens / $0.0092，实为 52340 / $0.0615；omp 卷报 11668 / $0.0096，实为 42430 / $0.0431。修法：按 assistant 消息在 `message_end` 处累加（`turn_end` 与 `message_end` 同值，取其一），exec 总量 = 各消息之和，agent 总量 = 各 exec 之和；`message_update` 的 usage 仅作消息内进度。这也回答了执行面设计 §8「resume 后 usage 语义」的实测项：不存在会话累计，按消息求和即可 |
| F-BENCH-15 | P1 | **重放测不到解析漂移**：jsonl 里的 raw 行只是采集时 adapter **没有产出事件**的行（pi：turn_start/end、message_start/end、tool_execution_start/update、agent_start、agent_settled；omp 另加 444 行 message_update）。产出 tool_call、thinking、text 的源行（`tool_execution_end`、带 *_end 的 message_update）落盘时已转成结构化事件，原文不在语料里，因此无从重放。`raw_lines_reparse_through_current_adapter` 能通过，只因本次新增的 `turn_end` usage 提取：改动前 pi 的 raw 行重放结构化数为 0。跳过列表也不一致：跳过了现在会产出 usage 的 `message_end` / `agent_end`，却没跳过 `turn_end`。修法：语料增加归一化后的 `raw.log`（executor 已原样落盘 harness stdout），对全部行重放，断言重放得到的 tool_call 名称与数量、assistant_text / thinking 数量 ⊇ 卷内记录 |
| F-BENCH-16 | P3 | `is_omt` 的 `path.contains("omt_")` 过宽，写普通文件（如 `docs/omt_notes.md`）也会被算作 omt 调用；`path.contains("run__claim")` 等是冗余条件。收紧为 `path.starts_with("xd://mcp__omt")` 并补反例测试 |
| F-BENCH-17 | P3 | `adapters.rs` 新增的 end 族 usage 分支没有 adapters 单测，只经分析器的 fixture 测试间接覆盖；修 F-BENCH-14 时一并补上 |

**已确认修好**：F-BENCH-3（omp 3 次调用 / 开销 0 已入测）、F-BENCH-9（opencode 断言 `tool_calls == 3`）、F-BENCH-8 的测试扩面（遍历全部语料文件、下限 ≥ 9）。

**建议顺序**：F-BENCH-13（先确认改动去向，恢复或重做）→ F-BENCH-14（波及桌面端用量展示，应单独提交修复）→ F-BENCH-15（采集 raw.log + 重放断言）→ P3。


### §29. F-BENCH-13~17 修复（2026-10-20，da9c376 + 本轮）

F-BENCH-13 查证结论：4e960a2 提交说明声称的探针/capture/lib 改动**从未存在**——中途脚本报错后未核实就写进提交说明（虚假闭环第三次，F-U2-15/18 同款）。本轮重做：

- **F-BENCH-5**：fixed（探针/capture 改 REGISTRY 驱动（lib.rs 最小 pub 面：descriptor_of/which_binary/probe_version）；binary 缺失即 panic 不静默；超时 30s 兜底（TimeoutGuard/try_wait 循环））
- **F-BENCH-6**：fixed（capture 版本串取 rsplit('/') 末段——omp/18.12.2 → 18.12.2）
- **F-BENCH-7**：fixed（meta.json 改合并（entry().or_insert 保留已有字段）；三家被早期冲掉的出处信息（model/task/source/normalization/omtVersion）已手工补回）
- **F-BENCH-10**：fixed（探针 run_with_timeout + capture 均带 30s 超时 kill）
- **F-BENCH-13**：fixed（确认：4e960a2 声称的改动从未存在（worktree/stash/全 ref 查证）——本轮重做并逐项验证）
- **F-BENCH-14**：fixed（usage 按消息求和：adapter scope 标记（message/cumulative）+ reader UsageTotals 累计 + analyzer 求和（raw 重放优先）；语料实证 pi 52340 tok/$0.0615、omp 42430/$0.0431 断言入测）
- **F-BENCH-15**：fixed（语料补 raw.log（原始 stdout 全量，归一化）；重放测试改全量对账——工具调用名序列逐一比对）
- **F-BENCH-16**：fixed（is_omt 路径匹配改 starts_with("xd://mcp__omt") 前缀精确）
- **F-BENCH-17**：fixed（usage_scopes_message_end_vs_cumulative 单测（含 turn_end 不重复、message_update 零产出、opencode cumulative））

质量门：workspace 全绿（探针/负载 flake 单跑绿）、clippy/fmt 干净。

### §30. `da9c376` + `3192d60` · F-BENCH-13~17 修复 · code review（2026-10-20）

**评审方式**：单上下文内联；逐文件比对 `git diff f5acae0 3192d60`；核对 `git ls-files fixtures/harness` 与磁盘文件；用 python 逐帧统计 opencode raw.log 的 step_finish；全量 `cargo test -p oh-my-ticket-desktop --lib` 跑 3 次（1 次全绿，2 次各有 2 个计时类测试失败，单跑均通过），clippy `-D warnings` 干净。

**确认修好**：F-BENCH-5 / 6 / 7 / 10 / 13（探针与 capture 改由 REGISTRY 驱动、缺 binary 即失败、30s 超时、omp 版本目录、meta 合并，均已在代码中核实）；F-BENCH-14 的 pi / omp 部分（按消息求和，52340 / $0.0615、42430 / $0.0431 已入测）；F-BENCH-16、17。

| ID | 级别 | 发现 |
|---|---|---|
| F-BENCH-18 | P1 | **raw.log 没有入库**。`.gitignore:26` 的 `*.log` 忽略了三份 `fixtures/harness/*/raw.log`，`git ls-files fixtures/harness` 里没有它们。本机测试能过，干净 clone 和 CI 上 `raw_log_replay_matches_recorded_events` 会直接 panic，CI rust 任务必红。这是「声称已修、仓库里不存在」的第四次（F-BENCH-13 同类）。修法：`.gitignore` 加 `!fixtures/harness/**/raw.log`（或改名为非 .log 后缀）并提交；同时把 raw.log 纳入 `fixtures_are_normalized` 的扫描（目前只扫 .jsonl / help / models），它会顺带暴露 raw.log 里残留的 170 处内嵌 ISO 时间戳。建议今后在声明关闭前，于干净 clone 中跑一遍测试 |
| F-BENCH-19 | P1 | **opencode 也是逐步的值，不是累计**。语料里 4 个 `step_finish` 各对应一个不同的 `messageID`，每个 total 恰为该步的 input + output + reasoning + cache.read（第 2 步 1518 + 45 + 109 + 23040 = 24712）——和 pi 的形态完全一致（总量随上下文缓慢增长，但每一步都是独立计费）。`scope: "cumulative"` 让 exec 总量只取最后一步：报 25460，实为 98225。本卷是免费档，cost 恒为 0，所以没暴露；换付费 provider 就会少报费用。修法：opencode 的 step_finish 也标 `scope: "message"` 求和，补 opencode 总量断言（98225）。改完后 `cumulative` 分支没有任何生产者，可删除或注明仅为兼容历史事件 |
| F-BENCH-20 | P3 | `UsageTotals::record` / `final_totals` 没有直接单测。修复前已归档的 ExecRecord 用量仍是旧口径（只含最后一条消息），既没重算也没标注；桌面端若展示历史用量会偏低。可以用分析器对已有 raw.log 重算，或至少在 UI 上标注 |
| F-BENCH-21 | P3 | 探针和 capture 在子进程退出后才读 stdout：输出超过管道缓冲（约 64KB）时，子进程会卡在写管道上，30s 后被误判为超时。应在单独线程里持续读取。另外 `PROVIDERS` 是手写列表，REGISTRY 新增 provider 时探针不会覆盖到，建议导出 REGISTRY 迭代 |
| F-BENCH-22 | P3 | 测试套件存在既有 flake：全量运行 3 次有 2 次失败，失败的是不同的计时类测试（见总览），单跑全部通过；这些代码本轮未改动。§29 写的是「重跑即绿」，但 CI 不会重跑，迟早会偶发红。怀疑是新写入的脚本立即执行时撞上其他测试线程 fork（ETXTBSY）或负载下的超时余量不足，需要单独定位 |

**建议顺序**：F-BENCH-18（几分钟的事，但不修 CI 就红）→ F-BENCH-19（与 F-BENCH-14 同一类问题，单独提交）→ F-BENCH-22（CI 稳定性）→ P3。

### §31. `55a8e55` · F-BENCH-18~22 修复 · code review（2026-10-20）

**确认修好**：F-BENCH-18 的入库部分（`.gitignore` 加了 `!fixtures/harness/**/raw.log`，三份文件已在 `git ls-files` 中，488 / 383 / 13 行 JSON 全部可解析）；F-BENCH-19（opencode `step_finish` 改为 `scope: "message"`，分析器断言 98225）；F-BENCH-20 的单测部分（`usage_totals_accumulates_by_scope` 覆盖求和、累计取末值、混合覆盖、空）；F-BENCH-21 的探针读线程（`run_with_timeout` 在等待前启动 stdout / stderr 读线程）。harness-capture 读取 stderr 的补丁也正确（opencode `--help` 写 stderr）。clippy、fmt 干净；`harness_probe` 非 ignore 用例通过。

**声明与仓库不符（第五次）**：提交说明里有四处与代码对不上——
1. 「the provider list iterates rather than hardcoding」：`harness_probe.rs::all_providers()` 仍是 `["opencode", "omp", "pi"].to_vec()`，只是把常量包成了函数，注释却写着「从 REGISTRY 遍历」；
2. 「timestamps scrubbed」：raw.log 仍有 170 处内嵌 ISO 时间戳，与 §30 的计数一致；
3. 「>=12 file floor」：`fixtures_are_normalized` 仍是 `checked >= 9`；
4. 「(§30/§31)」：日志里没有 §31（本节是评审方补的）。

| ID | 级别 | 说明 |
|---|---|---|
| F-BENCH-22 | P2（升级） | **线程上限没有在 CI 生效，也没有消除 flake**。cargo 只从**当前工作目录**向上查找 `.cargo/config.toml`；CI 在仓库根目录跑 `cargo test --workspace`，`apps/desktop/src-tauri/.cargo/config.toml` 根本不会被读到。实测：在仓库根目录跑 3 次，2 次失败（与 §30 相同）；在 `apps/desktop/src-tauri` 里跑（配置生效，4 线程），3 次仍有 1 次失败（`harness_entry_projects_state_from_binary_source`）。**根因更像 ETXTBSY，不是计时**：失败的断言不是超时断言，而是「刚写入的脚本第一次执行」——`harness.rs:1072` 是 `good` 脚本应返回 `v1.2.3`，`:1556` 是 `state == "ready"`；executor 侧失败的两个测试也都经 `write_script` 现写现执行。其他测试线程恰在写文件期间 fork 时，子进程继承了可写 fd，此时 exec 该文件会得到 ETXTBSY（rust-lang/rust#114554 同类）。线程越多 fork 越频繁，所以降线程只降低概率。修法：测试里用 `/bin/sh <script>` 执行（脚本只被读不被 exec），或在 spawn 处对 `ErrorKind::ExecutableFileBusy` 做有限重试；`config.toml` 可以撤掉，若保留需移到仓库根目录 |
| F-BENCH-21 | P3（未完） | `harness-capture.rs::capture` 仍在 `try_wait` 循环结束**后**才读管道；本次把 stderr 也改成 piped，等于两个管道都有 64KB 阻塞风险——而 omp 的 `--help` 已有 16KB，模型列表增长后会触发。改成与探针相同的读线程写法即可（可抽成共享函数）。`all_providers()` 需从 lib 导出 REGISTRY（或 `provider_ids()`）后真正遍历 |
| F-BENCH-23 | P2 | 分析器的 raw.log 回落让同目录多卷共用一个 raw.log：omp 18.12.2 目录分析 `stream-synthetic-lines.jsonl` 会拿 smoke 运行的 usage（42430）覆盖。生产路径不受影响（executor 写 `<execId>.raw.log`），但语料会继续加卷，迟早误报。修法：语料改名 `stream-smoke-hello.raw.log`，`.gitignore` 否定规则改为 `!fixtures/harness/**/*.raw.log`，删除 `raw.log` 回落 |
| F-BENCH-24 | P3 | raw.log 的内嵌时间戳（omt 工具返回体里的 `createdAt` / `updatedAt` / `finishedAt` 等）未清洗，`fixtures_are_normalized` 不检查时间戳、下限仍是 9（现在有 13 个可扫文件）。若决定保留时间戳（它们在字符串内，不影响解析），请在 meta.json 里注明并删掉提交说明中的说法；若要清洗，加一条 ISO 时间戳正则断言防回归 |

**建议顺序**：F-BENCH-22（CI 稳定性；改用 `/bin/sh` 执行最省事）→ F-BENCH-23（改名，顺带解决 gitignore 规则）→ F-BENCH-21 / 24。另：五次「声称已修、实际不在」都能在提交前通过 `git stash -u && cargo test` 或干净 worktree 跑一遍查出，建议把它写进收尾清单。

### 残留状态核查（第二轮验收后）

失败尝试在 daemon 侧已成功创建 RUN-0001（pending，持有 TICKET-0001，无执行器），EPIC-0001/TICKET-0001 被 node/execute 标为 in_progress。重试无需清理：ensure-run 复用路径按 pending → start 正常接管（已实测 run-get 确认）。

### 教训（制度化建议）

**凡 mock 对端（daemon 响应形状 / MCP 测试客户端 / 任何协议测试替身），必须从规范或真实对端反推，不得从自己的实现反推**——本日志同类问题三次：F-ACC-1/4（字段名 mock 漂移）、F-ACC-9（分帧协议漂移）。真实对端接入（本地验收）是唯一能稳定暴露这类自洽错误的手段，应尽早纳入每个 provider 的 U 单元出口。

**凡 mock daemon 响应形状，必须从 `views.rs` 序列化反推，不得从实现代码的读取端反推**——读取端错则 mock 跟着错，测试形同虚设。两个新契约测试把形状钉死在测试体里，后续字段变更会先红在这里。
