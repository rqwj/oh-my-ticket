# Distribution: Releases 发布流水线（U11）

GitHub Releases 是唯一的二进制事实源（KD1）。本文是发布流水线的短指针；
完整契约见 `docs/plans/2026-08-25-2209-feat-multi-surface-runtime-desktop-distribution-plan.md`。

## 版本与 tag

- 产品版本的唯一来源：根 `Cargo.toml` 的 `[workspace.package] version`（KTD1，
  当前 `0.2.0`），四个 crate 以 `version.workspace = true` 继承。
- Release tag 约定：`v<version>`（如 `v0.2.0`）。`release.yml` 在构建前校验
  tag 名与 workspace 版本一致，不一致直接 fail。

## 归档布局契约

`scripts/assemble-release-archive.sh` 产出：

- `omt-<triple>-v<version>.tar.gz`，解包为单一顶层目录
  `omt-<triple>-v<version>/`，内含 `omt-daemon`、`omt`、`README.md`
  （两个二进制在打包前逐一 ad-hoc codesign：`codesign --sign - --force`）。
- 同目录 `SHA256SUMS`（相对路径条目，`shasum -a 256 -c SHA256SUMS` 可验）。

本地用法：

```sh
cargo build --release -p omt-runtime --bins   # 或带 --target <triple>
bash scripts/assemble-release-archive.sh [--target <triple>] [--out-dir DIR]
```

默认输出在 `target/release-dist/`（随 target/ 出 git）。

## Workflows

- `.github/workflows/ci.yml` — 最小门禁：rust job（fmt --check / clippy -D warnings /
  test --workspace）+ node job（pnpm frozen install / typecheck / test）。
  push main 与全部 PR 触发，同 ref 并发组取消旧跑。
- `.github/workflows/release.yml` — 仅 `v*` tag 触发：
  - `binary-assets`：macos-latest，交叉构建 `aarch64-apple-darwin` 双二进制 →
    组装脚本 → 冒烟 `./omt --version` → 上传 tar.gz + SHA256SUMS 到 Release。
  - `desktop-bundle`：`if: hashFiles('apps/desktop/src-tauri/tauri.conf.json') != ''`，
    U8 脚手架落地前整体 no-op；用 tauri-action（`projectPath: apps/desktop`）
    构建 .app/.dmg 附到同一 Release；sidecar 取自本 Release 的 daemon 构建
    （KD5/R12），按 `binaries/omt-daemon-aarch64-apple-darwin` 暂存。

## 签名策略（KTD5）

不注入身份时全链路跳过签名（bundler 原生行为，缺 secret 不 hard-fail）：
二进制为 ad-hoc 签名，.app/.dmg 未签名。注入 `APPLE_SIGNING_IDENTITY` /
`APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` 后 bundler 自内向外重签并公证。
未签名产物的 Gatekeeper 限制：右键 Open 放行，或
`xattr -cr <.app>` 清除隔离属性后运行。

## 供应链卫生（KTD8）

脚本与 workflow 无任何 curl|bash 步骤；checksums 始终产出。第三方 action
当前以 tag 引用（bootstrap 期容忍），稳定分发前必须钉扎到不可变 commit SHA；
permissions 最小化，仅 tag 触发 release，无 pull_request_target。

## 明确延后

正式 Developer ID 签名与公证（等证书决策）；x86_64/Windows/Linux 目标；
action SHA 钉扎执行；install.sh / Homebrew formula（U12）与 npm 平台包降级
（U13）另行落地。
