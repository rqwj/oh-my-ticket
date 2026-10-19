---
title: Harness 调用性能评测机制设计 - 评审
type: review
date: 2026-10-19
reviewed:
  - docs/plans/2026-10-19-harness-benchmark-design.md（下称「设计」）
method: 单上下文内联评审——逐条对照 `apps/desktop/src-tauri`、`crates/omt-runtime` 现有代码核验设计断言；外部基准部分基于 2026-10-19 网络检索
---

# Harness 调用性能评测机制设计 - 评审

## 0. 结论摘要

**方向正确**：三层按成本排布（零成本契约探针 → 付费效率基准 → 趋势门禁）合理；§1 历史教训表把每层要拦截的失败模式钉在真实事件上，§7「明确不做」克制得当。

**但对核心目标「随 harness 新版本持续累积测试」不够**：设计只做「当前版本 vs 代码当前期望」的单点比对，没有**按版本沉淀样本**的机制——每次升级跑完即丢，测试面不会随版本增长。另有 2 个结构性问题须在动工前定案：

1. **探针归属错层**（R-1）：`omt harness-check` 拟放在 `omt` CLI（`crates/omt-runtime`），但它依赖的 REGISTRY / 探测原语 / adapters / jsonl 全在 `apps/desktop/src-tauri`，runtime crate 不依赖也不应依赖 desktop。
2. **opencode 缺位**（R-2）：REGISTRY 有 opencode / omp / pi 三家，设计的事件词汇探针只写了 pi/omp 词汇。

**外部现成基准**：没有可直接替代本设计的。Harbor（Terminal-Bench 2.0 官方 harness）是最接近的，值得 spike 作为第二层 runner；MCP 类基准（MCPMark / MCP-Universe / MCP-Bench）测的是模型通用 MCP 能力，不测本项目的集成契约（§5）。

---

## 1. 代码事实核验

| # | 设计断言 | 核验 | 证据 |
|---|---|---|---|
| 1 | REGISTRY descriptor 字段 `argv_prefix`/`headless_flags`/`unattended_flag`/`json_args`/`resume_args`/`model_args`/`model_list_args` | 属实（另有 `probe_args`、`login_args` 未列入探针面） | `apps/desktop/src-tauri/src/harness.rs::HarnessDescriptor` |
| 2 | 「已封装的探测原语可复用」 | 属实，但在 desktop crate | `harness.rs::probe_version` / `which_binary` |
| 3 | 指标「全部可从现有 `.jsonl` 提取」 | 属实：`<execId>.jsonl` 首行 session_header，usage 为去重后的累计快照并归档进 ExecRecord | `executor.rs` F-U12-3/F-U12-4 段 |
| 4 | 「工具数 = 18（与现有 mcp_spec 断言同源）」 | mcp_spec **不硬编码 18**，而是从 parity 矩阵推导（agent_available − home 族） | `crates/omt-runtime/tests/mcp_spec.rs::mcp_tools_list_matches_parity_matrix_minus_home_family` |
| 5 | `tools/list` 零 `$ref`/`$defs` 不变量 | 已有单测覆盖 | `crates/omt-runtime/src/mcp.rs:502` |
| 6 | 事件核心集 `session`/`agent_start`/`agent_end`/`message_update`/`tool_execution_end` | 仅适用 pi/omp；opencode 为 `text`/`reasoning`/`tool_use`/`step_finish`/`error`，**无终止事件** | `adapters.rs` 模块注释 |
| 7 | 历史样本存在 | 部分：`adapters.rs` 测试内联了 2026-10-18 实测样本，但未按版本组织 | `adapters.rs` tests |

---

## 2. 结构性问题（动工前须定案）

### R-1 探针 / runner 的 crate 归属（P1，manual）

§3 `omt harness-check` 与 §4.4 `omt bench run` 都拟挂在 `omt` CLI，但：

- REGISTRY、`probe_version`、`adapters::parse_line`、jsonl 写入全部在 `apps/desktop/src-tauri`；
- `crates/omt-runtime` 不依赖 desktop crate，且按分层不应依赖（runtime 是纯协调面）。

候选方案：

| 方案 | 做法 | 代价 |
|---|---|---|
| A（推荐） | 抽 `crates/omt-harness`：REGISTRY + 探测原语 + adapters + jsonl 分析器，desktop 与 CLI 共用 | 一次搬迁；desktop 改 import；获得可独立 `cargo test` 的 harness 契约层 |
| B | 探针做成 desktop crate 的独立 bin / `#[ignore]` 测试层 | 不搬代码，但 CLI 用户（无 desktop）用不了 |
| C | CLI 重新实现一份 registry | **否决**：双份 registry 必然漂移，恰是本设计要防的 |

§6 的「≈1 天」估算未含此搬迁，需重估。

### R-2 opencode 缺位（P1，gated_auto）

- §3.3 事件词汇核心集须**按 provider 定义**（opencode 无终止事件，判终止只能靠进程退出）；
- §1 历史教训表中 F-ACC-7（argv 缺 `run` 子命令）正是 opencode 事件，但其余探针描述全按 pi/omp 写；
- §4.3 记录样例与 §5 门禁都应显式覆盖三家。

---

## 3. 「持续随新版本累积测试」所缺的机制（核心缺口）

### R-3 按版本沉淀的 fixture 语料库（P1，manual——本评审最重要的补充）

每个 provider × 版本落一份采样：

```
fixtures/harness/<provider>/<version>/
  version.txt          # --version 原文
  help.txt             # --help 原文（及关键子命令 help）
  models.txt           # model_list_args 输出
  stream-<task>.jsonl  # 一次真实派发的原始 JSON 事件流
  meta.json            # 采样时间、模型、omtVersion、采样命令
```

收益：

- `adapters::parse_line`、`parse_models_output`、flag 面校验对**全部历史版本**做离线回放，进普通 `cargo test`，零 LLM 成本、可进 CI；
- 支持新版本 = 跑一条采样命令（如 `omt harness-capture <provider>`）+ 提交目录 → 测试自动显示差异；测试面随版本单调增长；
- 现有 `adapters.rs` 内联样本迁入此目录即是第一批语料。

### R-4 「每个升级事故 → 永久检查项」纪律（P2，advisory）

§1 历史表是一次性清单。应约定：每个因 harness 升级产生的 finding（F-ACC-*/F-PI-*/F-EXPOSE-* 类）必须同时落一个**具名检查项** + 一份触发样本，检查项名引用 finding ID。表格从此是活的回归清单。

### R-5 兼容矩阵（P2，gated_auto）

在 `docs/runtime/compatibility.md` 记录每个 provider 已验证版本、最低已知良好版本、对应 fixture 目录与探针 verdict。§5「版本变更钩子」需要以此作为比对锚点（「上次记录」目前没有定义存在哪）。

### R-6 任务集版本化（P3，gated_auto）

§4.1 任务修改后旧基线不可比。结果记录加 `taskVersion` 字段，基线按 `(harness, model, task, taskVersion)` 分桶。

---

## 4. 第二层基准与门禁的问题

### R-7 单次运行 + 硬门 = flaky（P1，manual）

LLM 输出非确定，`tool_calls ≤ min+1` 单跑一次必然偶发红。§7「不做统计显著性」可以保留，但最低限度：

- 每任务跑 N=3，按 pass^k 思路判定（如 ≥2/3 通过）或取中位数；
- **基线定义缺失**：建议为同 `(harness, model, task, taskVersion)` 最近 N 次通过运行的滚动中位数；
- 时长/tokens 的 +50% 偏移阈值也应基于中位数而非单次。

### R-8 隔离未定义（P1，gated_auto）

§4.4「建临时 workspace（或复用验收环境语义）」过于含糊。runner 必须显式使用独立的临时 workspace、OMT home、runtime dir（按 `docs/runtime/config.md` 的 explicit arg 优先级传入），否则会接入开发者正在用的 daemon 与 `~/.omt`。MCP 注入（`.pi/mcp.json` / `.omp/mcp.json`）在基准中途崩溃时的清理也须复用 `mcp_injection` 的 crash recovery。

### R-9 「red 阻止派发」缺陈旧/缺失策略（P2，manual）

探针从未跑过、或结果早于当前 harness 版本时怎么办？硬阻断会让用户在升级 harness 后直接无法派发。建议：默认 advisory（UI 标红 + 提示运行探针），提供显式 override；仅在「同版本已确认 red」时阻断。

### R-10 CI 可行性（P2，advisory）

CI 为 macOS-only，无 harness 二进制、无 API key。第一层中依赖 fixture 回放的部分可进现有 CI；需要真实 harness 的探针与第二层只能走独立 scheduled workflow（带 secrets）或明确标为本机执行。§5「CI / 定期」需二选一写清。

### R-11 小项（P3）

- §3.2 工具数「= 18」改为从 parity 矩阵推导，与 `mcp_spec.rs` 同源（见核验 #4）；
- §3.2 MCP 分帧与 `tools/list` 不变量已由 `mcp_spec.rs` 与 `mcp.rs` 单测覆盖，探针只需补 **harness 侧**部分（真实 harness 能否握手并列出工具），不要重复实现；
- §4.1 `negative-inject` 的通过判据未定义（「拒止并如实上报」需落成可机判条件，如 run item 终态 + report 内容断言）；
- §3.1 依赖解析 `--help` 文本较脆（各家格式不同）：建议 help-grep 只给 yellow，能无 LLM 成本实际执行的 flag（`--version`、`model_list_args`）以执行结果为准；
- §4.3 结果进 git 时只存指标，不存 transcript（可能含路径/密钥片段）。

---

## 5. 外部现成基准评估

| 候选 | 是什么 | 对本项目 |
|---|---|---|
| [Harbor](https://github.com/harbor-framework/harbor) + Terminal-Bench 2.0 | Terminal-Bench 团队的 agent 评测框架，Terminal-Bench 2.0 官方 harness；Docker 中跑 CLI agent（`harbor run --dataset terminal-bench@2.0 --agent … --model …`），支持自定义任务集 | **最接近**。建议 spike（≈1 天）作为第二层 runner，把 §4.1 任务写成 Harbor 任务，而非从零写 `omt bench run`。待确认：是否支持 opencode/pi/omp（`harbor run --help`）；omt-daemon 能否在其 Linux 容器内运行。另可作为「harness 版本能力回归」的周期性信号，**不作门禁** |
| [MCPMark](https://arxiv.org/html/2509.24002v1)（ICLR 2026，127 任务） / [MCP-Universe](https://arxiv.org/pdf/2508.14704)（11 个真实 MCP server） / [MCP-Bench](https://web3.arxiv.org/pdf/2508.20453)（28 server、250 工具） | 评测**模型**使用 MCP server 的通用能力 | **不适用**：测的是它们自己的 server，不测本项目 MCP server 与 harness 集成契约 |

结论：第一层（契约探针 + fixture 语料）与第三层（门禁）没有现成替代，保留自研；第二层 runner 先 spike Harbor 再决定是否自研。

---

## 6. 建议的设计修订清单（按优先级）

| 优先级 | 编号 | 修订 |
|---|---|---|
| P1 | R-1 | 新增「组件归属」节，定案 `crates/omt-harness`（方案 A），重估 §6 工期 |
| P1 | R-2 | §3.3 / §4 / §5 按 provider 覆盖三家，补 opencode 事件核心集 |
| P1 | R-3 | 新增「版本 fixture 语料库」节 + `harness-capture` 采样命令；§6 实施顺序第 1 步改为「语料目录 + 现有样本迁入 + 回放测试」 |
| P1 | R-7 | §4.2/§5 加 N 次运行判定规则与基线定义 |
| P1 | R-8 | §4.4 写明 runner 隔离（显式 runtime dir/home/workspace）与崩溃清理 |
| P2 | R-4 | §1 加「事故 → 具名检查项」纪律 |
| P2 | R-5 | 新增兼容矩阵（`docs/runtime/compatibility.md`）并作为版本钩子锚点 |
| P2 | R-9 | §5 发布门加陈旧/缺失策略与 override |
| P2 | R-10 | §5 写清 CI 与本机执行的边界 |
| P2 | — | §6 加「Harbor spike」作为第二层 runner 前置决策 |
| P3 | R-6 | 结果模型加 `taskVersion` |
| P3 | R-11 | 小项逐条修订 |
