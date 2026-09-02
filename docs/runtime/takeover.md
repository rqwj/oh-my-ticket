# Home 接管（takeover）

## 语义

`omt takeover <home-path>`：对 bridge 时代 home 的静默接管——快照打包 →
独占迁移 → 代际栅栏 → 持久 legacy fence。拒绝场景（HOME_LOCKED /
DAEMON_OWNS_HOME）携带可执行指引。

## 规则

- ts-bridge 标记（live 或 stale）**永不**被自动窃取；doctor 只报告
  cohort。
- 外部 daemon 持有的 home 拒绝 declare/takeover，details 附 owner pid。
- 接管产物：`omt doctor` 的 cohorts 反映 before/after；快照存
  `<runtime>/backups/`。

详细实现语义见 `crates/omt-runtime/src/takeover.rs` 与
`crates/omt-runtime/tests/takeover.rs` 验收矩阵。
