# Oh-My-Ticket（OMT）执行计划

> 依据：[`FEASIBILITY.md`](FEASIBILITY.md)（已定稿方案）+ [`RESEARCH-dsh-ticket-plugin.md`](RESEARCH-dsh-ticket-plugin.md)（代码证据）
> 方案要点回顾：单仓三部件（Host 插件 + Client 插件 + 内嵌 Skill）；SQLite 元数据 + 磁盘 Markdown（`~/.omt` / `OMT_HOME`）；树在 `shell.overlay` 抽屉；文档详情动态遮蔽 `details` 面板；`@` 触发引用 ticket；`omt_*` 工具 + 内嵌 skill 与其它流程 skill 并行。
> 日期：2026-08-17

---

## 1. 里程碑总览

| 里程碑 | 目标 | 产出 | 依赖 |
|--------|------|------|------|
| **M0 脚手架与加载验证** | 外部插件（host+client）能被 `dsh web` 加载 | 可运行的空壳插件 | — |
| **M1 Host 数据核心** | OMT_HOME 解析、SQLite schema、Markdown 双写、reindex | `src/host/store.ts` `files.ts` + 单测 | M0 |
| **M2 工具层** | 模型可调用 `omt_*` CRUD 工具 | `src/host/tools.ts` | M1 |
| **M3 内嵌 Skill** | `ctx.skills.register()` 注册 omt skill，验证多 skill 并行 | `src/host/skill.ts` | M2 |
| **M4 RPC + `@` 引用** | client↔host 数据通道；输入框 `@` 引用 ticket | `src/host/rpc.ts`、`src/client/trigger/` | M1（host 侧）、M0（client 侧） |
| **M5 界面** | 抽屉树 + details 文档详情 + toolview + 状态条 | `src/client/drawer/` `details/` | M4 |
| **M6 打包分发** | bundle 发布、`dsh plugin add` 安装验证 | npm 包 + README | M2–M5 |

**关键路径**：M0 → M1 → M2 →（M3、M4 并行）→ M5 → M6。
M0 的"外部 client 插件加载"是全项目最大的未知项，必须先做 spike 验证（见 §3）。

---

## 2. M0：脚手架与加载验证（Spike）

### 任务

1. 初始化仓库（当前目录）：pnpm + TypeScript + tsdown（client bundle 参照 `packages/client/tsdown.client.ts` 的预设思路）+ vitest。
2. `package.json` 关键字段：
   - `"dsh": { "bundle": { "patch": "./cordis.patch.yml" } }`（bundle manifest，参照 `docs/user/develop/basic/publish.md`）
   - `dsh.client` manifest（`platform: 'web'`）+ `exports["./client"]` 指向构建产物（参照 `packages/client/ui-jobs/package.json`）
3. `cordis.patch.yml`：两行——host 插件行（指向 `src/host/index.ts` 或构建产物）+ client 插件行。
4. **Spike A（host）**：最小 host 插件注册一个 `greet` 工具，`pnpm dsh web --patch ./cordis.patch.yml` 启动，Web 会话中让模型调用，确认工具可达。
   - 参照：`docs/user/develop/basic/tool.zh.md`（绝对路径挂载本地插件已验证可行）。
5. **Spike B（client）**：最小 client 插件向 `conversation.session.header.actions` 注册一个按钮（照抄 `packages/client/ui-jobs/src/client/index.ts` 模式），确认：
   - 外部 client bundle 能否经 `--patch` overlay 被 `ClientModuleRegistry` 扫描进 `__DSH_BOOT__`；
   - 若 `--patch` 不够，走 profile 路径：建 `$DSH_HOME/profiles/omt-dev`，`dsh plugin add` 本地目录（turtle-ui 是外部 UI 插件的 working example，必要时克隆参照其构建/发布配置）。
   - HMR：client 侧自己跑 tsdown watch 重建 `lib/client.js`，验证 client-hmr 的 stat-poll 触发热重载（`docs/subsystems/client-modules.md`）。

### 验收

- [x] `dsh web` 启动后模型可调用 `greet`；界面 header 出现测试按钮。——2026-08-17 完成：`[omt] host plugin loaded` 日志确认 host 插件与 `omt_greet` 工具注册（模型调用验证随 M2 覆盖）；`__DSH_BOOT__` 含 `oh-my-ticket` 条目、`/plugins/oh-my-ticket/client.js` 正常提供，header 按钮已注册（浏览器目测：`http://127.0.0.1:3180` 开发实例）。
- [x] 确定日常开发回路（`--patch` 还是 profile + watch），写入 README。——结论：**独立开发 profile `omt` + tsdown watch + 页面刷新**；`--patch` 绝对路径仅对 host 插件有效，client 插件发现必须走 profile 包名解析（见 README"关键机制备忘"）。

### 风险

- Spike B 是全项目唯一"文档未明确承诺"的环节；若外部 client 插件加载受阻，回退方案 = 开发期在 DSH checkout 内建软链/工作区包，发布时再验证纯外部路径。

---

## 3. M1：Host 数据核心

### 任务

1. **Home 解析**（`src/host/config.ts`）：`OMT_HOME` 环境变量 > 插件 config（cordis Schema，参照 `docs/user/develop/basic/config.zh.md`）> `~/.omt`；首次使用自动建目录与 `tickets/` 子目录。
2. **SQLite 层**（`src/host/store.ts`）：
   - 惰性 `await import('node:sqlite')`（仓内惯例，保持 Node 22 启动安静），打开 `$OMT_HOME/omt.db`，`PRAGMA journal_mode=WAL`、`foreign_keys=ON`；
   - 建表：`nodes` / `edges`（schema 见 FEASIBILITY §3.2）+ `nodes_search` 内容镜像表（LIKE 搜索；~~FTS5~~ 放弃——unicode61 分词器不切分 CJK，整段中文成一个 token，前缀查询失效，M1 实测后改为参数化 LIKE，本地规模性能足够）；
   - 递归 CTE 查询：取整棵树 / 取子树 / 取父链。
3. **文件层**（`src/host/files.ts`）：
   - 节点 Markdown：frontmatter（`id/type/title/status/parent`）+ 正文 + `## 子节点` 链接区；
   - 写入路径 = `$OMT_HOME/tickets/<ID>-<slug>/…`（FEASIBILITY §3.1）；
   - 创建/移动/删除节点时同步维护父文件的子节点清单。
4. **一致性**（`src/host/reindex.ts`）：扫描 `tickets/` 全部 md → 重建 `nodes`/`edges`/FTS；启动时跑一次快速校验（db 文件缺失或版本不符则全量 reindex）。
5. **领域规则**：合法父子关系表（`epic→story`、`story→substory|ticket`、`substory→ticket`、`ticket→subticket`）、ID 生成（`EPIC-0001` 自增，按类型分别计数）、状态机（`open|in_progress|done|archived`）。
6. 单元测试：双写一致性、层级校验、reindex 幂等、FTS 查询。

### 验收

- [x] 不启动 dsh，纯 vitest 覆盖核心逻辑；手工改乱一个 md 文件后 reindex 能自愈。——2026-08-17 完成：14/14 测试通过（双写一致、层级校验、update/move、reindex 丢库重建/手改自愈/幂等、CJK 搜索）；host 插件在开发实例加载成功，`~/.omt`（omt.db + tickets/）自动初始化。

---

## 4. M2：工具层（`omt_*`）

### 任务

`ctx.tools.register(defineTool({...}))`（`inject = ['tools']`；范式参照 `docs/cookbook/adding-a-tool.zh.md` 与 `packages/todo/tool-todo`）：

| 工具 | 参数要点 | 说明 |
|------|----------|------|
| `omt_create` | `type, title, parentId?, body?` | 层级校验 + 双写 |
| `omt_list` | `type?, status?, query?` | 树/过滤/FTS 搜索 |
| `omt_show` | `id` | 返回正文 + 子节点清单 + 父链 |
| `omt_update` | `id, title?/status?/body?/append?` | `append` 用于追加进度记录 |
| `omt_move` | `id, newParentId` | 层级校验 + 文件搬迁 + 父文件清单同步 |
| `omt_reindex` | — | 手动重建索引 |

注意点：

- **Web 会话工具可见性**：~~web 表面下模型可见工具按 agent preset 挂载（`web-app/cordis.patch.yml:293-408` 把 `tool-*` 行 disabled 后由 preset 组合）。开发期验证：先确认直接注册的工具在当前 preset 下可见；不可见则改经 `agent.ctx` 按 agent 注册（`packages/core/tools/src/index.ts:727` 注释的路径）。~~ **M2 已解决（2026-08-17，代码层面）**：host-plane 注册落入工具注册表全局层，所有会话合并可见；standard preset 未使用 `ctx.tools.restrict` 掩码（grep 验证），且 preset 文件注释确认其工具行同样 "register into the host tools registry"。
- `output.render` 返回面向模型的紧凑文本（树用缩进列表，避免大 JSON）。
- 变更后发布失效信号（供 M4 RPC 推送给 client）。

### 验收

- [x] Web 会话中用自然语言让模型"创建一个 Epic 和两个 Story"，磁盘文件与 SQLite 均正确；模型能 list/show/update 并正确拒绝非法挂载。——2026-08-17 完成：模型在 3180 会话成功调用 `omt_create` ×3（EPIC-0001 用户体系 + STORY-0001/0002），文件树嵌套、epic.md 受管子节点清单、SQLite nodes/edges 三项一致；preset 可见性运行时确认。工具层 8 例单测覆盖层级违规拒绝。

---

## 5. M3：内嵌 Skill

### 任务

1. `src/host/skill.ts`：`ctx.skills.register({ name: 'omt', description, content, invocation: { modelInvocable: true, userInvocable: true } })`，正文 = FEASIBILITY §6.3 草稿（职责边界 + 工具用法 + 工作约定）。
2. 提示词打磨：用真实任务回归（拆解需求建 Epic/Story、按 ticket 推进并更新状态）。
3. **多 skill 并行验证**：同时激活 omt + 一个流程类 skill（如 `/omt /ce-plan` 或让模型自动匹配），确认两者同时生效、职责不串（证据机制：`tool-skill/src/index.ts` 逐 skill 注入）。

### 验收

- [x] `<available_skills>` 目录出现 omt；模型遇到 ticket 任务时自动调用；用户 `/omt` 手势可手动激活；与流程 skill 同消息激活时各自生效。——代码层 2026-08-17 完成：`src/host/skill.ts` 经 `ctx.skills.register()` 注册（source: 'runtime'），单测覆盖注册形状/路由描述/六工具覆盖/职责边界声明；插件加载即注册（畸形注册会抛错使 fiber 失败，当前启动日志干净）。**浏览器 UX 验证（`/` 菜单出现 omt、模型自动加载、多 skill 并行）随 M4/M5 在开发实例一并目测。**

---

## 6. M4：RPC 通道 + `@` 引用

### 任务

1. **RPC**（`src/host/rpc.ts`）：`ctx.connection.rpc.handle('omt', handler, { authority })`（`packages/client/connection/src/rpc.ts`），端点：`tree / search / get / setActive`；payload 用 zod 双向校验。变更推送：工具执行后通过 ServerRequest 下行通知（或 client 轮询失效，MVP 可先轮询）。
2. **Client store**（`src/client/store.ts`）：`createOmtStore()` 工厂（zustand，注册时声明，禁止模块级单例——`packages/client/AGENTS.md` 规则 6），缓存树 + 激活 ticket + 抽屉显隐。
3. **`@` 触发源**（`src/client/trigger/`）：`inject = ['inputTriggers', ...]` + `ctx.inputTriggers.registerSource({ trigger: '@', name: 'ticket', order, candidates, onPick, codec })`：
   - `candidates(query)` → RPC `search`（FTS5），候选带类型图标/状态；
   - `onPick` → `{ insert: ReferenceInsert }`（chip 显示 `@TICKET-0042`，`clipboardText` 为 `/omt TICKET-0042` 之类的纯文本投影）；
   - `codec.serialize` → 取该 ticket 的标题+状态+正文（可截断），异步返回模型可读文本（参照 `types.ts` 的 `ReferenceCodec` 契约；序列化失败会阻止发送，需做降级文案）。

### 验收

- [x] 输入 `@` 出现 ticket 分组候选，选中后草稿出现 chip，发送后模型上下文收到 ticket 内容并能据此回答。——2026-08-17 代码层+通道层完成：`src/host/rpc.ts`（`/omt` channel，loopback authority，zod v4 校验，tree/search/get 三端点，curl 端到端实测通过）+ `src/client/trigger/source.ts`（candidates→ReferenceInsert→codec.serialize 生成 `<omt-ticket>` 块，失败阻断发送）；12 例新单测全绿（36/36）。**浏览器 UX 验证（`@` 候选菜单与 chip）待目测。**

---

## 7. M5：界面

### 任务（均经 `ctx.slots.inject(...)` 延迟注册，遵守 `packages/client/AGENTS.md` 全部纪律）

1. **抽屉树**（`src/client/drawer/`）：
   - `shell.overlay`（list）注册左缘抽屉组件：递归树（类型图标/状态色/折叠）、工具栏（新建/刷新/reindex）、点击节点 → 详情；
   - 开关按钮 ×2：`conversation.session.header.actions` + `sidebar.footer.action`；显隐状态存 OMT store。
2. **文档详情**（`src/client/details/`）：
   - 树中选中节点 → 临时 `ctx.slots.register({ name: 'details', priority: <低于 DetailsPanel> }, DocView)` + `ctx.layout.openDetails()`；关闭/取消选中 → dispose（原工具详情自动恢复）；
   - DocView：Markdown 渲染 + 元信息头（状态/父链/子节点）+ 操作（置状态/追加记录，走 RPC）；
   - 为 `omt_show` 注册 keyed `tool.call.toolview` 渲染器（模型展示的 ticket 在 details 面板富渲染）。
3. **状态条**：`conversation.input.dock` 显示当前激活 ticket（选中即激活；可清除）。
4. 样式：`--dsw-*` 语义 token + CSS Modules，无字面色（`docs/web-styling.md`）；文案中文。

### 验收

- [x] 抽屉开合、树刷新（工具变更后）、详情遮蔽/恢复、toolview、状态条全部可用；窄视口不破坏布局。——2026-08-17 代码层完成：controller + 5 组件（Drawer/DocPanel/ActiveDock/ToggleButton/OmtShowRow）；动态遮蔽经 `slots.inject` 返回的 disposer 实现（attach/dispose 模式，controller 单测 8 例覆盖）；44/44 单测通过；`update`/`reindex` RPC 端点 curl 实测通过。**浏览器 UX 验证待目测**（窄视口行为未测）。

---

## 8. M6：打包分发

1. bundle 发布形态核对：`dsh.bundle` manifest、files 白名单（`lib/`、`cordis.patch.yml`）、host 行引用构建产物而非 src。
2. 安装路径验证（干净环境）：`dsh plugin add <pkg>` 进新 profile → `dsh web` → 全流程冒烟。
3. GitHub 直装（可选）：自包含 `prepare` 构建脚本 + 文档说明 `allowBuilds`（`publish.md:153-173`）。
4. README：安装、配置（`OMT_HOME`）、工具清单、skill 说明、与其它流程 skill 的协作约定。

### 验收（2026-08-17，按用户要求仅做本地打包 + 本地安装测试）

- [x] **自包含构建**：host 半改为全内联（`external: []`——defineTool/schemastery 是纯库，
  无跨插件运行时身份），`lib/index.js` 仅剩 node: 内置 import + 惰性 `node:sqlite`，
  安装后无需从 profile 解析任何 `@deepseek-ai/*` 依赖。
- [x] **本地打包**：`pnpm pack` → `oh-my-ticket-0.1.0.tgz`（含 lib/index.js、
  lib/client.js、cordis.patch.yml、package.json、README.md）。
- [x] **干净环境安装冒烟**：全新 `DSH_HOME=~/.dsh-omt-verify` + profile `verify`（
  dsh-base + dsh-web-app + tarball 安装）→ 3190 启动：插件加载日志正常、
  `__DSH_BOOT__` 含 oh-my-ticket、`/plugins/oh-my-ticket/client.js` 200、
  `/omt/search` RPC 应答正确。验证实例已关闭。
- [ ] GitHub 直装 prepare 脚本与 npm 发布（留待需要分发时）。

---

## 9. 横切事项

- **测试策略**：host 核心 vitest 单测（M1 起逐层）；client 组件按仓内惯例"props 直喂 + 行为断言"（`AGENTS.md` Testing 节）；端到端靠手工冒烟清单（每里程碑的验收项即冒烟用例）。
- **版本锁定**：DSH 处于 rc 阶段，client 契约标注"冻结/需仲裁"——开发与 CI 锁定一个已验证的 DSH 版本，升级时重跑 M0 冒烟。
- **合规红线**（写代码时对照 `packages/client/AGENTS.md`）：组件只见四份 props 不见 ctx；store 工厂非单例；跨包禁 import，只走 slot/RPC；插件卸载可逆（所有注册经 `ctx.effect`）。
- **不在本期范围**：`#` 触发字符上游 PR；右侧并列 dock 上游 PR；多工作区/多用户并发写锁（单用户本地场景，WAL 已够）。

## 10. 工作量粗估

| 里程碑 | 估时 | 备注 |
|--------|------|------|
| M0 | 0.5–1 天 | Spike B 若受阻会拉长，预留缓冲 |
| M1 | 1.5–2 天 | 双写一致性是主要复杂度 |
| M2 | 1 天 | 含 preset 可见性验证 |
| M3 | 0.5 天 | 主要是提示词回归 |
| M4 | 1–1.5 天 | RPC 通道 + codec 契约 |
| M5 | 2–3 天 | slot 纪律 + 动态遮蔽是首次踩的新模式 |
| M6 | 0.5 天 | |
| **合计** | **约 7–10 天** | |
