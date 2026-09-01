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

## 已知约束

- **UDS sun_path 上限（约 104 字符）**：runtime dir 路径过长会导致
  daemon 在 socket bind 阶段失败。测试与沙箱环境使用短路径（如
  `/tmp/omt-*`）；深层 `$TMPDIR` 前缀 + 长标签会触限（U6 实施期实测）。
- 空闲退出：无活跃连接且无订阅者时按 `daemon.json` 的 quiet 期退出；
  订阅保活抑制退出（U5）。
