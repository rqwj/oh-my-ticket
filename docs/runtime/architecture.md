# 架构总览（多端运行期）

## 进程拓扑

```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│ DSH 插件     │  │ 桌面 (Tauri) │  │ MCP harness │  │ omt CLI     │
│ (client-ts) │  │ (omt-client) │  │ (omt mcp)   │  │ (omt-client)│
└──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘
       │  JSON-RPC over UDS（per-user runtime dir，单活选举）
       ▼                ▼                ▼               ▼
┌────────────────────────────────────────────────────────────┐
│ omt-daemon：bootstrap 选举 → per-home actor 线程 → flock   │
│ 所有权 → 事件 outbox（cursor resume）→ 空闲看门狗退出        │
└────────────────────────────────────────────────────────────┘
```

## 分层

| 层 | 位置 | 职责 |
|----|------|------|
| omt-domain | crates/omt-domain | 纯领域逻辑（树规则、markdown、store 语义） |
| omt-storage | crates/omt-storage | SQLite + 文件落盘、home 锁、journal/recovery |
| omt-contracts | crates/omt-contracts | typify 生成的协议类型（schema 为源） |
| omt-client | crates/omt-client | 薄共享客户端：descriptor/UDS/帧/握手/凭据策略（KTD4） |
| omt-runtime | crates/omt-runtime | daemon 服务器 + `omt` CLI + `omt mcp` |
| client-ts | packages/client-ts | TS 客户端库（DSH 适配层消费） |

## 关键不变式

- **单活**：同一 runtime dir 只有一个 daemon（bootstrap.lock 选举，
  create_new 占位 + 心跳过期判定）。
- **home 所有权**：flock；actor 退出（idle/shutdown 一致）从注册表驱逐
  自身条目（配额只计活条目，U5 修复）。
- **事件游标**：per-home outbox，cursor 严格单调；page/live 边界去重。
- ** lease 栅栏**：claim/report 以 leaseToken fencing；过期 report 被拒。
- **同源 sidecar**：桌面打包的 daemon 与 CLI/brew/npm 渠道字节同源
  （KD5，同一 workspace 构建）。

## 各 surface 角色

- **DSH 插件**：kind:"dsh"，全量操作族；自有治愈循环（TICKET-0132
  白名单+冷却）。
- **桌面**：kind:"desktop"；Rust 桥持有连接（KTD6），前端只走 tauri
  command；setsid detached spawn（关窗不杀 daemon，R14）。
- **MCP**：kind:"mcp"；operations 受限 `[node, run, events]`（R7）。
- **CLI**：kind:"cli"；cli-credential.json 跨调用持有租约身份；
  陈旧凭据删除重注册恰好一次（U7）。
