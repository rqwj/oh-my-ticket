# Canary 发布门禁清单（R23 / U15）

分阶段证据门：每阶段证据归档后才进下一阶段。占位栏在实际发布时填写
（链接/输出摘要/日期/执行人）。

## 阶段 1：CLI 渠道安装冒烟

- [ ] tag `vX.Y.Z` 推送后 Release 资产齐全（tar.gz + SHA256SUMS +
      桌面 .dmg/.app）。
      证据：________
- [ ] 干净环境 `sh scripts/install.sh`（或 one-liner）→ `omt --version`
      输出 `omt X.Y.Z`；`omt doctor` 无 daemon 路径退出码 0。
      证据：________
- [ ] 校验和篡改用例：坏 tar.gz → 非零退出、无半成品。
      证据：________
- [ ] brew 渠道：`brew tap rqwj/omt && brew install omt` →
      `omt --version` / `omt doctor` 通过；`brew audit`（首发布执行）。
      证据：________

## 阶段 2：DSH 插件 + declare 链路

- [ ] 已装 CLI 的机器上 DSH 插件零配置连接（KTD7 解析链命中系统安装）。
      证据：________
- [ ] 打开未注册工作区 → 自动 declare → CRUD 可用；重启后状态保留。
      证据：________
- [ ] 旧 daemon 在场 → F4 回退文案出现（提示升级而非 --home）。
      证据：________
- [ ] `omt doctor` 在线前导 `runtime.match` 符合预期（升级场景=false）。
      证据：________

## 阶段 3：桌面 bundle

- [ ] 下载 .dmg 安装；首次启动窗口出现且树渲染（连接共享 daemon）。
      证据：________
- [ ] 关窗退出后 daemon 存活（`omt daemon-status` 仍 running）；
      DSH 会话不受影响。
      证据：________
- [ ] 桌面与 DSH 同 home 的 recent 列表互通；filters 各用各的前缀键。
      证据：________
- [ ] 桌面 declare 新 home → 列表出现 → CRUD 可用。
      证据：________
- [ ] 未签名 Gatekeeper 路径文档化验证（右键 Open / xattr -cr）。
      证据：________

## 放行标准

三阶段全部证据归档 → 该 tag 视为正式发布。任一阶段失败：在对应 ticket
记录 blocked 原因，修复后从失败阶段重跑（前序阶段证据不失效）。
