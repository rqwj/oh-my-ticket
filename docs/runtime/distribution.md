# Distribution: Releases 发布流水线与安装渠道（U11/U12）

GitHub Releases 是唯一的二进制事实源（KD1）。本文是发布流水线与产品安装
渠道的短指针；完整契约见
`docs/plans/2026-08-25-2209-feat-multi-surface-runtime-desktop-distribution-plan.md`。

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

## 安装渠道（U12）

两个渠道都是 Releases 的薄封装（R20/KD1），消费上文归档契约，不产生第二事实源。

### 一键脚本（macOS 优先）

```sh
curl -fsSL https://raw.githubusercontent.com/rqwj/oh-my-ticket/main/scripts/install.sh | sh
```

行为：`uname -s/-m` 探测 triple（aarch64 / x86_64-apple-darwin）→ 解析版本
（默认 latest，经 `releases/latest` API 取 `tag_name`；可 `--version v0.2.0`
钉版）→ 下载 tar.gz + SHA256SUMS 至临时目录 → 强制 SHA256 校验 → 装入
`${OMT_INSTALL_DIR:-~/.local/bin}`（或 `--bin-dir DIR`，校验通过后才创建）→
PATH 提示（列出检测到的 rc 文件并给出 export 行，从不自动改 rc）→
`<dir>/omt --version` 冒烟，并提示 `omt doctor` 做运行时诊断。

- KTD8 卫生：仅对固定 Release URL 发起普通 curl；下载内容不 eval、不
  pipe 进 shell、校验通过前不执行；没有 `--no-verify` 开关，校验失败即
  非零退出且不落任何文件。
- 首个 tag 发布后建议把 one-liner 钉到 tag（`.../raw/v0.2.0/scripts/install.sh`）
  而非跟随 main 漂移。
- 离线验证（无网络）：`--from-dir <dir>` 把本地目录当作 Release 根，
  例如 `sh scripts/install.sh --from-dir target/release-dist --bin-dir /tmp/bin`。

### Homebrew tap

formula 源随仓库维护在 `packaging/homebrew/omt.rb`（标准 url+sha256 双二进制；
两二进制都进 `bin`——KTD7 的 daemon 探测只认 bin 型前缀，sbin 会躲过
discoverOrSpawn）。org tap 的推送是 Release checklist 步骤（见下），推送后：

```sh
brew tap rqwj/omt https://github.com/rqwj/homebrew-omt
brew install omt        # 或 brew install rqwj/omt/omt
```

当前 Releases 仅 arm64 asset，formula 显式 `depends_on arch: :arm64`；
x86_64 leg 待 R19 扩矩阵后补 per-arch url/sha256。

### 源码兜底

```sh
cargo install --git https://github.com/rqwj/oh-my-ticket   # 同时装入 omt 与 omt-daemon
```

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
  - `npm-platform-packages`：`if: hashFiles('npm/platform-packages/**') != ''`，
    U13 模板落地前整体 no-op；下载 job1 归档并校验 SHA256SUMS 后按
    目录名→triple 映射填充各平台包，`npm pack --dry-run` 验证内容；
    `NPM_TOKEN` 未配置时以 notice 跳过 `npm publish --access public`
    （与签名同一跳过哲学），配置后逐包发布。

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

## RELEASE CHECKLIST（手动步骤）

发布一个新版本时按序执行；每步证据留档后方可进下一步（staged canary 门禁）：

1. **push tag**：确认根 `Cargo.toml` `[workspace.package] version = X.Y.Z`，
   `git tag vX.Y.Z && git push origin vX.Y.Z`（`release.yml` 校验 tag 与
   workspace 版本一致，不一致 fail）。
2. **verify assets**：等 `binary-assets` job 完成，核对 Release 页资产齐全：
   `omt-aarch64-apple-darwin-vX.Y.Z.tar.gz` + 相对路径 `SHA256SUMS`。下载后
   `shasum -a 256 -c SHA256SUMS` 全 OK、解包冒烟 `./omt --version` 输出
   `omt X.Y.Z`。
3. **update formula**：改 `packaging/homebrew/omt.rb` 的 `version` / `url` /
   `sha256`——sha256 必须对真实资产重算（`shasum -a 256`），禁止沿用旧值或
   手造；`ruby -c packaging/homebrew/omt.rb` 过语法。
4. **push to tap repo**：把更新后的 formula 复制为 org tap 仓库
   `rqwj/homebrew-omt` 的 `Formula/omt.rb` 并提交推送（本仓库内的
   `packaging/homebrew/omt.rb` 同步提交，保持两处一致）。
5. **smoke both channels**：
   - install.sh：干净目录跑 one-liner 或本地 `--from-dir` 路径；
   - brew：`brew tap rqwj/omt <tap-url> && brew install omt`，
     两者各跑一次 `omt --version` 与 `omt doctor`。

## npm 平台包降级（U13 已落地）

DSH 适配层解析 daemon 二进制的优先级（KTD7）：显式选项 / `OMT_DAEMON`
env → PATH + 产品渠道前缀（`~/.local/bin`、`/opt/homebrew/bin`、
`/usr/local/bin`）→ 已安装的 npm 平台包兜底。平台包模板在
`npm/platform-packages/{darwin-arm64,darwin-x64,linux-arm64,linux-x64}/`，
由 release workflow 的 `npm-platform-packages` job 从 Release 资产填充
（先校验 SHA256SUMS）后发布。**时序约束**：根包 `optionalDependencies`
声明在首次平台包发布时才落地（pnpm 对不可解析的 optional 依赖静默忽略
lockfile，导致 `--frozen-lockfile` specifier 校验失败——详见
`npm/platform-packages/README.md`）。冒烟：`node scripts/pack-smoke.mjs`。

## 平台支持声明

当前发布矩阵仅 **macOS arm64**（aarch64-apple-darwin）。**Windows 明示
不支持**；x86_64 macOS 与 Linux 的二进制在 workflow 中预留模板位但无
资产产出（公式与安装脚本对缺失 triple 以清晰报错指引到 Releases 页）。

## 明确延后

正式 Developer ID 签名与公证（等证书决策）；x86_64/Linux 目标
（formula 的 Intel leg 随 x86_64 asset 一并补）；action SHA 钉扎执行。
