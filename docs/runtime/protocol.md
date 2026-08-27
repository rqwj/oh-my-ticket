# 协议要点（多端运行期）

omt-daemon 的 JSON-RPC over UDS 协议的 surface-facing 要点。Schema 为唯一
权威（`schema/*.schema.json` → `pnpm gen` 生成 TS 绑定）；本文是叙事性
导览，矛盾时以 schema + `crates/omt-runtime/src/` 实现为准。

## 连接与握手

- 发现：`<runtime-dir>/descriptor.json`（解析优先级见 config.md）→ pid
  存活 + endpoint 连通双探针 → 否则由客户端 spawn（detached，选举保证
  单活）。
- 握手 `handshake/request`：`client.kind` ∈ {dsh, cli, desktop, mcp,
  external}；`requestedScopes` 可收窄 homes / operations / actorNamespace，
  服务端只会相交不会放大。凭据 12h TTL、内存态、随 daemon 代际消亡。
- `features` 开放映射：新能力以新键追加；缺位即不支持。当前已知位：
  `actionParityMatrix`、`eventResume`、`idempotencyKeys`、
  **`homeDeclare`**（U5 起，服务端恒发 true；客户端对 pre-U5 daemon 的
  缺位必须容忍并走回退路径——F4 版本漂移）。

## home/declare（U5/U6，R6-R9）

运行中的 daemon 可幂等收录磁盘上已存在的 home 目录：

- 请求 `{ path }`；响应 `{ homeId, requiresRehandshake: true, name, kind }`。
- 规范化路径去重（symlink/`/var` vs `/private/var` 视为同一）；并发 declare
  同一路径恰一成功一幂等。
- `requiresRehandshake: true` 表示内存凭据的 home 授权不含新 home——
  客户端须重握手（forceReconnect）后新 home 才可用。
- 失败映射：外部 daemon 占用 → `DAEMON_OWNS_HOME`（details 带 owner
  pid）；ts-bridge 标记 → `HOME_LOCKED` 附 takeover 指引；路径不存在或
  非目录 → 打开前结构化拒绝；超 `max_open_homes` 配额 →
  `QUOTA_EXCEEDED`。
- **KTD3 提示分裂**：home 范围拒绝（`FORBIDDEN home-not-scoped` /
  `NOT_FOUND kind:home`）的 details 带 `requiresRehandshake: true`；
  操作族 `FORBIDDEN`（凭据缺该操作族）**不**带提示——重注册无法授予被
  排除的族，提示只会诱发死循环。

## UI bag 键作用域（U4，R3-R5/KD3）

`ui/filters-*` / `ui/recent-*` 为 adapter-only 通道，键约定：

- filters：每 surface 前缀键——DSH 写 `dsh:ui`，桌面写 `tauri:ui`。
  服务端本代不强制前缀（R3）。裸 `'ui'` 遗产键由适配层读回退 +
  写穿透迁移，孤儿键不清理（无删除 RPC）。
- recent：**唯一跨面共享键 `'recent'`**（KD3 特例）——所有 surface 读写
  同一份最近列表；旧的 per-session 键成为孤儿。

## known-homes 目录（home/list-known）

runtime 级 SQLite（`<runtime>/known-homes.db`）持久化 daemon 曾打开或
declare 过的每个目录，跨代际存活。持久键是 **canonical 路径**——homeId
存于 home 自身库内，同路径重开保持不变（仅在库被抹除重建时变化），故
`lastHomeId` 仅作信息展示。每个条目实时标注 `open`（当前已开，可直接
切换）与 `missing`（磁盘探测失败——目录被移动/删除/卸载；条目永不自动
清除）。桌面设置页据此渲染「已知未开」区，点击即 declare 切换。adapter_only
分类：mcp 凭据无 home 族被拒（R7）。

## homes 列表新鲜度契约（拉取式）

v1 无跨面 home 变更推送信号：桌面端聚焦窗口时重新列出 open homes
（`daemon_homes` 走握手投影）。**禁止**把 homes-changed 塞进 per-home
outbox（retention/backlog 语义不符）。

## 事件流

`events/resume`（游标化回填 + live 订阅）；订阅保活抑制 home 空闲退出
（U5 修复）。桌面端经 Rust 桥转发（`omt://event` Tauri 通道）；DSH 端经
client-ts 的 cursor resume。

## 版本字段

握手 `daemon.version` 为 workspace 产品版本（U1）；`omt doctor` 在线前导
对比 `daemonVersion` vs `cliVersion`（R10，见 operations.md）。

## 凭证面与 MCP

`omt mcp` 以 kind:"mcp" 握手并申请受限 operations `[node, run, events]`
——home 族与 ui 族在服务端即被拒绝（R7 最小权限）。MCP 工具面 =
parity 矩阵 agent_available ∩ 非 human_administrative 减去 home/declare
（mcp_spec.rs 对 schema 文件做逐一比对）。
