# oh-my-ticket desktop (U8)

Tauri 2 desktop surface — a PEER member of the multi-surface runtime
(KD4: fresh UI, no DSH plugin component reuse; KD5: the bundled daemon is
the same-source workspace build, never a fork).

## 运行

```sh
pnpm install
pnpm --filter oh-my-ticket-desktop tauri:dev    # dev: vite + dev binary
pnpm --filter oh-my-ticket-desktop tauri:build  # build: .app + .dmg
```

`pnpm tauri dev/build` 直接调用亦可（绕过 sidecar 暂存脚本时需先手工
`node scripts/copy-sidecar.mjs [--dev]`）。

## Sidecar 契约（KD5/R12）

- 源：`target/{release,debug}/omt-daemon`（同一 workspace 构建，版本与
  产品一致 — KTD1）。
- 暂存：`scripts/copy-sidecar.mjs` 复制为
  `src-tauri/binaries/omt-daemon-<host-triple>`（bundler 强制 triple 后缀；
  打包时剥后缀落 `Contents/MacOS/omt-daemon`，已验证）。
- 运行期解析：`current_exe().parent()`（NOT resource_dir）；dev 模式下
  桌面二进制与 daemon 共享 workspace `target/debug/`，天然相邻。

## 加固基线（渲染进程）

- CSP：`default-src 'self'`，script 仅 self，无远程 URL（tauri.conf.json
  `app.security.csp`）。
- capabilities：仅 `core:default`。daemon 由 Rust 侧 `ShellExt` 拉起，
  不穿越前端权限层（U9 接入 sidecar spawn 时同样不需要放开
  shell:allow-*；仅当前端未来直接 spawn 才需按研究快照补
  `shell:allow-spawn` scoped 条目）。
- `app.withGlobalTauri: false` — 无全局注入。

## 待定

- **identifier `com.ohmyticket.desktop` 为占位**：正式反域名随签名身份
  决策（KTD5 Developer ID）一并定案。
- 图标为 RGBA 占位图（32×32 纯色）；正式图标集属 U10 打磨范围。
- `bundle.icon: []` 当前为空数组——dmg 打包已通过；上架前需补
  icns。
