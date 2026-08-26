# docs/runtime — 多端运行期文档矩阵

| 文档 | 内容 |
|------|------|
| [architecture.md](architecture.md) | 进程拓扑、crate 分层、关键不变式、各 surface 角色 |
| [config.md](config.md) | 配置解析契约（优先级/多端同 dir/descriptor 不变式/KTD7 覆盖序） |
| [protocol.md](protocol.md) | 握手与 features、home/declare、bag 键作用域、homes 新鲜度、事件流、MCP 面 |
| [distribution.md](distribution.md) | 版本/tag 契约、归档布局、install.sh/brew/npm 渠道、签名与供应链、RELEASE CHECKLIST |
| [compatibility.md](compatibility.md) | 三套版本号关系（KTD1）、版本漂移行为、平台支持声明、corpus 处置 |
| [security.md](security.md) | 信任边界、凭据/授权、home 所有权、供应链卫生、渲染进程加固 |
| [operations.md](operations.md) | 安装自查、doctor 在线前导读法、常见运维动作、已知约束（UDS 路径上限） |
| [migrations.md](migrations.md) | pre-daemon/旧 daemon/bag 键/凭据自愈迁移路径 |
| [takeover.md](takeover.md) | home 接管语义与规则 |
| [canary-checklist.md](canary-checklist.md) | 三阶段发布证据门禁（R23） |
| [bench-baseline.md](bench-baseline.md) | 性能基线 |
