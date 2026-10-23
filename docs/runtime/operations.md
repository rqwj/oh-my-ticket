# 运行与运维（operations）

## 安装后自查

```sh
omt --version            # 应为 'omt <workspace 版本>'（KTD1）
omt doctor [<home>]      # 在线前导 + 离线深度探针
omt daemon-status        # descriptor/代际/pid
```

## doctor 在线前导（U7/R10）

`omt doctor` 先输出观察性 runtime 段（**不**取任何锁）：

```json
"runtime": {
  "descriptorFound": true,
  "daemonVersion": "0.2.0",
  "cliVersion": "0.2.0",
  "match": true,
  "generation": 3
},
"adminGrants": { "totalEntries": 2, "deadPidEntries": [] }
```

- `match: false` → brew/install.sh 升级后旧 daemon 仍在跑：空闲退出后
  下次 spawn 自然换代，或 `omt daemon-stop` 后由任一客户端拉起。
- `match: "unknown"` → 握手失败或缺版本字段；不报错、退出码 0。
- 活跃 daemon 服务时深度离线探针让位（note + refusal 字段，退出码 0）；
  无 daemon 时离线探针照常（cohorts/orphans/schemaTooNew）。
- `deadPidEntries`：admin-grants 中嵌死 pid 的条目（命名空间
  `<prefix>:<pid>` 解析）——清理提示。

## 常见运维动作

| 场景 | 动作 |
|------|------|
| daemon 未运行 | 任一客户端自动拉起（discover-or-spawn）；或 `omt daemon-start` |
| 升级后版本漂移 | `omt doctor` 看 `runtime.match`；`daemon-stop` 换代 |
| home 未被收录 | DSH/桌面自动 declare（U5/U6）；旧 daemon 提示升级 |
| ts-bridge 占用 | `omt takeover <home>`（显式，绝不自动） |
| 配额打满 | 调 `daemon.json` 的 `limits.max_open_homes` 或重启 daemon |
| CLI / MCP 派发被拒 | `omt exec policy` 看生效值与来源（default / file）；桌面端「设置 → 派发策略」改 |

## 本地工具与派发策略（exec 子项目 2）

`omt` 的本地工具动词（读操作不经 daemon 往返；派发受
`<runtime-dir>/exec/policy.json` 约束，详见下节）：

```
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

### 派发策略（`<runtime-dir>/exec/policy.json`）

- **文件与默认值**：缺省为 `allowFromCli: true`、`allowFromMcp: false`、
  `harnesses: ["pi","omp","opencode"]`、`workspaces: "homeWorkspaceOnly"`、
  `maxConcurrent: 4`、`maxDurationSec: 1800`、`approvalMode: "inherit"`。文件
  存在但无法解析或含未知字段 = **fail closed**：CLI / MCP 的派发一律
  `POLICY_INVALID` 拒绝；桌面端派发不受影响。调用方请求的 `maxDurationSec`
  只能收紧（min(请求, policy)），不在白名单的 harness 一律拒绝。
- **停止不受 policy 约束**：`exec stop` 不检查 `allowFrom*` 与文件有效性——
  拒绝启动是安全方向，拒绝停止反而危险（关掉派发后仍要能停掉失控执行）。
- **CLI / MCP 派发总是无人值守**：自动附加 harness 的无人值守参数（pi
  `--auto`、omp `--approval-mode=yolo` 等），与桌面端派发等效但**没有**
  桌面端那一步人工确认弹窗。`approvalMode: "inherit"` 指「派发方不能改
  审批模式」，不是读取某个 harness 级开关（harness 配置里没有该开关）。
- **只关 MCP 拦不住主控**：「允许经 CLI 派发」同样覆盖任何能执行 shell 的
  agent（如 Claude Code 的 Bash）——只关 `allowFromMcp` 不能阻止这类主控
  派发；要收紧必须同时关 `allowFromCli`。
- **workspace 限制**：CLI / MCP 派发只允许目标为 workspace 内 `.omt` 的
  home；全局 home（handshake `kind: "global"`）或 workspace 根等于用户主
  目录时拒绝（`POLICY_DENIED`，field `workspaces`）。
- **被派发的 agent 不能再派发或停止**：shim 注入保留环境变量
  `OMT_EXEC_ID=<execId>`；带该变量的进程里 `exec start` / `exec stop` 均
  被拒（field `nestedDispatch`），`omt mcp` 注入桥不暴露任何本地工具。
- **威胁模型**：policy 约束的是经 omt 自身工具面（CLI / MCP）行事的
  agent，属**纵深防御而非安全边界**——同一用户的进程（包括手握 shell 的
  无人值守 harness 或主控）仍可清除环境变量、直接改写 runtime dir 下的
  `policy.json` / `admin-grants.json`。

## 已知约束

- **UDS sun_path 上限（约 104 字符）**：runtime dir 路径过长会导致
  daemon 在 socket bind 阶段失败。测试与沙箱环境使用短路径（如
  `/tmp/omt-*`）；深层 `$TMPDIR` 前缀 + 长标签会触限（U6 实施期实测）。
- 空闲退出：无活跃连接且无订阅者时按 `daemon.json` 的 quiet 期退出；
  订阅保活抑制退出（U5）。
