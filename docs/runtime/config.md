# 配置解析契约（U2 / R1-R2 / KD2）

本文件是 OMT 三端（DSH 插件、桌面应用、CLI/MCP）共用的配置解析**单一权威出处**。
任何一端的解析实现与本文件冲突时，以本文件为准并修复实现。

## 解析优先级

统一规则：**显式参数 > 环境变量 > 默认值**，三端一致。
环境变量为空字符串或纯空白时视为未设置（回落到下一级）。

| 变量 | 显式参数 | 环境变量 | 默认值 |
| --- | --- | --- | --- |
| runtime dir | `--runtime-dir <dir>` | `OMT_RUNTIME_DIR` | `~/.omt/run` |
| 全局 home | `--home <dir>`（可重复） | `OMT_HOME` | `~/.omt` |

- `--home` 未提供时 daemon 仅带全局 home 启动；重复 `--home` 按顺序注册多个 home。
- runtime dir 是 daemon 专属位置：descriptor、选举锁、socket、日志都在其中
  （布局见 `crates/omt-runtime/src/paths.rs` 头注释）。

## 多端同一 runtime dir 契约

所有 UI 层必须解析到与 daemon 相同的 runtime dir——客户端凭 descriptor
发现 daemon，解析不一致等于把同一个用户劈成两个互不可见的拓扑
（两个 daemon、两套 home、偏好互不同步）。因此：

- 客户端不得引入平台缓存目录等"第二事实源"；
- 测试/sandbox 经 `OMT_RUNTIME_DIR` 整体重定向；
- 跨端 parity 由 `tests/config-parity.spec.ts` 锁定（R2）。

## 权威实现坐标（防漂移反向链接）

| 层 | 实现 |
| --- | --- |
| Rust（daemon + CLI 共用） | `crates/omt-runtime/src/paths.rs::resolve`（runtime dir）；`server.rs` 全局 home 段 |
| TS 共享客户端 | `packages/client-ts/src/client.ts::OmtClient.resolveRuntimeDir` |
| DSH 适配层 | `src/host/service.ts`（经 `OmtClient.resolveRuntimeDir` 取 runtime dir；daemon 二进制解析见下节） |

## descriptor 增量字段不变式

descriptor.json 的 `schemaVersion === 1` 期间：

- 新增字段对两端都安全——TS reader 只硬校验 `schemaVersion === 1`
  （`packages/client-ts/src/client.ts:136`），未知字段容忍；
- Rust 写端追加字段无需升版本；删除/改型字段才需要 schemaVersion 递增；
- parity 不变式由 U3 场景「descriptor 追加未知字段后 readDescriptor 仍成功」钉住。

## daemon 二进制覆盖（TS 适配层）

DSH 插件拉起 daemon 时按以下顺序取二进制（KTD7，实现在 U13）：

1. 显式 `daemonPath` 选项；
2. `OMT_DAEMON` 环境变量；
3. 系统 `PATH` 查找与已知前缀探测（`~/.local/bin`、`/opt/homebrew/bin`、
   `/usr/local/bin`）;
4. npm 平台包 `require.resolve` 兜底。

v1 取首个命中，不做 semver 择优（deferred）。产品渠道安装
（brew / install.sh）落在第 3 级即命中，无需 npm 兜底。
