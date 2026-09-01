# 版本与兼容性（KTD1 / R22）

## 三套版本号的关系

| 版本 | 位置 | 语义 |
|------|------|------|
| **产品版本** | 根 `Cargo.toml` `[workspace.package] version`（当前 0.2.0） | 唯一事实源。`omt`/`omt-daemon` 二进制、release tag（`v<X.Y.Z>`）、桌面包、npm 平台包全部由此派生（release workflow 校验 tag==版本，不一致 fail）。 |
| **npm 根包 SemVer** | 根 `package.json` `version` | DSH 插件分发节奏独立演进；声明 runtime 兼容下限而非锁步。 |
| **协议版本** | 握手 `protocolVersion`（当前 1.0） | MAJOR 断裂才升级；MINOR/特性以 `features` 开放映射追加（如 `homeDeclare`）。 |

## 版本漂移行为（F4）

- 新客户端 + 旧 daemon：`features` 缺位即回退——DSH 端 declare 流程退回
  报错文案（不再指向 --home，而是提示升级 daemon）。
- `omt doctor` 在线前导输出 `runtime.match: true|false|"unknown"`——
  升级后第一时间可核对「已装二进制 vs 运行中 daemon」（R10）。
- daemon 空闲退出后，下一次 spawn 自然换代到新二进制。

## 平台支持

仅 macOS arm64。Windows 明示不支持；x86_64/Linux 预留模板位但无资产
（见 distribution.md）。

## 迁移面

- pre-U7a `<home>/ui-filters.json`：首次连接一次性导入 daemon 存储
  （写 `dsh:ui` 前缀键，U4），文件改名 `.imported`。
- 裸 `'ui'` bag 键：读回退 + 写穿透，孤儿键不清理。
- per-session recent 键：孤儿化，共享键 `'recent'` 接管。
- cli-credential.json：home 范围拒绝带 requiresRehandshake 提示时删除并
  重新 enroll 恰好一次（U7 动词路径自愈）。

## 行为语料库（corpus）处置

`corpus/scenarios/*.json` 保持语言中立的冻结规范；**Rust leg 为唯一活跃
执行者**（`crates/omt-domain` corpus 测试）。TS leg（`corpus/runner/ts`）
自 U7a 删除 TS core 后退役——按上游 U10b 决策：记录结论为「Rust leg
晋升规范执行者，TS leg 已退役」，不再双跑。
