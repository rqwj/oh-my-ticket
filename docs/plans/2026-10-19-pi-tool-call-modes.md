# pi 新旧版本 MCP 工具调用模式对比（简要总结）

> 基于 pi 1.0.0–1.3.0 官方文档（本机 `docs/mcp.md` / `docs/codemode.md`）与
> OMT 验收实测（2026-10）。目的：说明 exposure 体系前后，模型调用 MCP 工具
> 的使用差别，及对 OMT 派发的影响。

## 一、旧模式：codemode 间接调用（pi 早期默认，仍是默认值）

**调用路径**：模型工具表里只有 4 个内置工具（read / bash / edit / write）。
MCP 工具**不进**模型工具表，只在 `codemode` 工具的 QuickJS 沙箱内部可达：

```js
// 模型调用 codemode，脚本内：
const res = await tools.mcp__omt__run__claim({ homeId: "h_xxx", runId: "RUN-1" });
text(JSON.stringify(res));
```

**关键特性**：
- 工具发现靠脚本内 API：`searchTools()` / `describeTool()` / `describeNamespace()`
  / `ALL_TOOLS`（默认 exposure 下 codemode 描述**不列**这些工具）
- MCP 调用解析为完整 `CallToolResult`（含 `isError` / `structuredContent`）
- 沙箱无 Node/文件系统/网络；可并行（`Promise.allSettled`）、可 `store/load`
  跨调用存 JSON 小值
- 系统提示仅一句模糊提示："可能有其它 custom tools"

**对 OMT 的影响（实测，Agent 26）**：模型不知道约定 → 6 轮探索工具 +
3 轮挖 homeId = 158s / 15 轮 / 16.8k tokens 才完成一个 echo hello。

## 二、新模式：exposure 体系（pi ≥1.0 提供，1.3.0 确认）

每个 MCP server（或单个工具，`toolExposure` 支持通配）可配四种暴露模式：

| exposure | 行为 |
|---|---|
| `codemode`（默认） | 同旧模式——仅沙箱内可达 |
| `deferred` | 不声明；`tool_search` 工具按需搜索后才声明给模型 |
| **`direct`** | **像内置工具一样直接声明进模型工具表**，同时保留沙箱可调 |
| `hidden` | 注册但不可达 |

```jsonc
// .pi/mcp.json
{ "mcpServers": { "omt": {
    "command": "...", "args": [],
    "exposure": "direct",
    "description": "OMT ticket run discipline: run__claim / run__report"
} } }
```

**direct 模式下的使用差别**：
- 模型直接发 `tool_use`（工具名形如 `mcp__omt__run__claim`），与 bash 等
  内置工具平级，**零发现成本**
- `description` 会进系统提示的 `mcp_servers` 清单（一句自述）
- 首个 prompt 会**等待** direct 工具的 server 连接完成（最多 10s）才发出
- `codemode` / `tool_search` 仅在有对应 exposure 的 server 连上时自动激活
- 工具结果 >20KB 时中间截断（`…N chars truncated…`），完整结果存临时文件

## 三、OMT 的适配现状

| 项 | 决策 |
|---|---|
| pi 注入 | `exposure: direct` + description（F-EXPOSE-1）；tool_hint 改为「工具已直接声明」 |
| omp | 18.12.2 **未同步** exposure 体系（二进制无 toolExposure 面）——保留 codemode 提示 |
| opencode | 等价于全 direct（MCP 工具一向直声明进工具表），无需配置 |
| 前置条件 | direct 模式把工具 schema 原样送进模型 API——**schema 必须全量内联**（F-EXPOSE-2：同文件 `$ref` 残留会被 moonshot 等严格校验器 400） |

## 四、效果对照（OMT smoke-hello 实测）

| 模式 | 工具调用 | 轮数 | 时长 | tokens |
|---|---|---|---|---|
| codemode（旧，Agent 26） | 含 6 次探索 | 15 | 158s | 16.8k |
| direct（新，优化后） | **3**（claim→echo→report） | ~2 | ~30-40s | ~4k |

**结论**：对「小而常用」的工具集（文档原话 direct 的推荐场景），direct 是
严格更优的选择；codemode 的价值保留在「脚本组合/过滤大量调用」的场景，
OMT 的 claim/report 纪律不需要它。omp 侧待其同步 exposure 体系后同样切换。
