# 安全模型（多端运行期）

## 信任边界

本机 OS 账户即信任边界（KTD9）：daemon 监听 per-user runtime dir 下的
UDS，目录权限 0700；凭据 token（128-bit hex）仅存在于握手响应与 daemon
内存注册表——**不**出现在 argv/env/日志/错误中（secret redaction 由
测试钉住：mcp_spec 的 stderr 扫描、cli 的 0600 凭据文件权限）。

## 凭据与授权

- 握手派生 `{ token, principalId "<kind>:<pid>", actorNamespace, homes[],
  operations[], expiresAt = now+12h }`；内存态，随代际消亡。
- requestedScopes 只收窄不放大：homes 与 open homes 求交；
  actorNamespace 仅允许等于或嵌套于服务端分配基座（`<base>/<suffix>`）。
- 操作族按方法命名空间（`node`/`run`/`events`/`ui`/`home`）逐项授予；
  `*` 为全量。MCP surface 恒为受限集 `[node, run, events]`（R7）。
- 管理能力不出协议：`admin-grants.json` 带外文件，每次判定现读；
  desktop 条目单条替换（U9——绝不追加，缓解 pid 回收继承面）。

## Home 所有权

flock 所有权标记 + bootstrap 选举锁；ts-bridge 标记永不被自动接管
（HOME_LOCKED 附 takeover 指引）；外部 daemon 占用报 DAEMON_OWNS_HOME
附 owner pid。hostile store（损坏/schema 违例）fail closed——
declare/open 失败不留半成品 actor。

## 供应链（KTD8）

- 安装脚本：固定 URL 拉取 + 强制 SHA256 校验，无 `--no-verify` 绕过，
  无 curl|bash eval，校验失败不留半成品。
- workflow：仅 tag 触发 release；permissions 最小化；无
  pull_request_target；第三方 action bootstrap 期以 tag 引用、稳定前
  必须钉 SHA（distribution.md 供应链节）。
- npm 平台包：填充前必验 SHA256SUMS；`NPM_TOKEN` 未配置时干净跳过发布
  但保留 pack 验证。

## 桌面渲染进程加固（U8 基线）

CSP `default-src 'self'`（无远程 URL）；capabilities 仅
`core:default`；daemon 由 Rust 侧 ShellExt 拉起、不穿越前端权限层；
`withGlobalTauri: false`。
