# DSH 第三方 Ticket 管理插件可行性调研报告

调研对象：`/Users/robertq/Tools/dsh/deepseek-harness/`（TypeScript pnpm monorepo）。
下文所有路径均相对该 checkout。

---

## 1. 客户端插件如何注册 UI：slot 系统

### 1.1 核心机制

Slot 系统是纯 TS 注册表 + 声明合并（declaration merging）的契约系统：

- **`packages/client/ui-slots/src/index.ts`**：`export interface SlotMap {}`（第 24 行）是空契约表，各插件包通过 `declare module '@deepseek-ai/dsh-client-ui-slots'` 合并自己的 slot 键。运行时核心是 `SlotCore` 类（第 678 行起）。
- **唯一组合 API**：`ctx.slots.register({ name, children?, store?, inject?, locale?, ...kind 字段 }, Component)`（`SlotCore.register`，第 741/768 行两个重载）。
  - `children` 表 = **声明 + 独占渲染授权**："Declaring is claiming"，声明一个子 slot 的 entry 是唯一被允许渲染它的 entry（第 140-145 行注释）。
  - slot 有两个轴：`kind: 'single' | 'list' | 'keyed' | 'chain'`（第 88 行）和 `scope: 'root' | 'session-maybe' | 'session'`（第 91 行）。
  - **遮蔽（shadowing）**：single/keyed/list 的同单元 entry 按 `priority` 升序共存，最低者渲染；同优先级冲突抛错（第 794-824 行）。
  - 组件 props = 四份交集：`PropsRuntime`（owner props + 框架标准件如 `sessionId`/`useSession`）& `PropsRenderSlots`（`renderSlot`/`renderSlotChain`）& `PropsStore`（声明的 store 的 `useStore`/`actions`）& inject 业务面（`ComposedProps`，第 442 行）。
- **延迟/条件注册**：`ctx.slots.inject(name, () => ctx.slots.register(...))` 等待目标 slot 声明出现再注册（见 `packages/client/AGENTS.md` "New plugin package checklist" 第 4 条；范例 `packages/client/ui-jobs/src/client/index.ts:30-39`）。
- 规则文件：`packages/client/AGENTS.md`（"Slot and props discipline" 7 条）、`.agents/notes/implemented/architecture/2026-07-22-slot-type-chain-implementation.md`（slot 类型标准）。

### 1.2 全部已声明 slot（证据：各包 `declare module` 合并块）

| slot 名 | kind/scope | 声明位置 | 位置/用途 |
|---|---|---|---|
| `root` | single/root | `packages/client/runtime/src/client/slots.ts:41` | 渲染树根洞（框架内置） |
| `sidebar` | single/root | `packages/client/ui-layout/src/client/index.ts:49` | **整个左列**；被 ui-sidebar 的 SidebarRoot 占据 |
| `conversation` | single/session-maybe | 同上 :62 | **整个中列**；被 ui-conversation 的 ConversationRoot 占据 |
| `details` | single/session | 同上 :72 | **右侧详情列**（当前唯一右侧面板位）；被 ui-conversation 的 DetailsPanel 占据 |
| `shell.overlay` | list/root | 同上 :83 | 全 frame 浮动层（badge/toast/浮窗），**可加式**：新 `id` 并列添加 |
| `sidebar.workspaces` | single/root | `packages/client/ui-sidebar/src/client/contract/slots.ts:24` | 侧栏工作区/会话浏览区 |
| `sidebar.settings` | single/root | 同上 :30 | 侧栏底部设置位 |
| `sidebar.footer.action` | list/root | 同上 :35 | 侧栏底部设置旁的**可加式**操作按钮 |
| `conversation.session` | single/session | `packages/client/ui-conversation/src/client/contract/slots.ts:44` | 单个 session 的整个主体 |
| `conversation.session.header` | single/session | 同上 :52 | session 顶条（标题/标签页/操作行） |
| `conversation.session.header.actions` | list/session | 同上 :63 | 顶条操作按钮（**可加式**，ui-jobs 范例） |
| `conversation.session.header.utilities` | list/session | 同上 :68 | 顶条右端工具（可加式） |
| `conversation.view` | list/session | 同上 :76 | 视图标签环（chat/trajectory…） |
| `conversation.chat.node` | keyed/session | 同上 :78 | 聊天业务节点渲染器（按 node kind 派发） |
| `conversation.chat.commandview` | keyed/session | 同上 :94 | 斜杠命令行渲染 |
| `conversation.chat.turnTail` | chain/session | 同上 :101 | Turn 尾部扩展链 |
| `conversation.chat.assistant-actions` | list/session | 同上 :109 |  assistant 消息操作条（可加式；ui-message-feedback 用） |
| `conversation.details.tool` | single/session | 同上 :124 | 详情面板内 tool 输出主体 |
| `conversation.composer` | chain/session | 同上 :132 | 输入框接管链 |
| `conversation.composer.bar` | single/session-maybe | 同上 :201 | 默认输入框主体 |
| `conversation.hero.workspace` / `.agentPreset` | single/root | 同上 :139/145 | 新会话 hero 区 |
| `conversation.input.dock` / `conversation.composer.dock` | list/session | 同上 :161/170 | 输入框上/下的整行停靠区（可加式；GoalBar、stats 行在此） |
| `conversation.input.left` / `.right` | list/session | 同上 :179/187 | 输入框工具行内小控件（可加式） |
| `conversation.input.plan` / `.model` | single/session | 同上 :211/221 | 输入行命名控制位 |
| `conversation.input.overlay` | list/session | `packages/client/ui-input-trigger/src/client/slots.ts:24` | 输入卡片内浮层（菜单等） |
| `settings.trigger` / `.header` / `.action` / `.close` / `.section` / `.plugins.tab` / `.onboarding` / `.general.item` | 各 root | `packages/client/ui-settings/src/client/contract/slots.ts:23-88` | 设置面板各区域（`.section`、`.general.item` 等为可加式 list） |
| `settings.plugin.item` | list/root | `packages/client/ui-settings-plugins/src/client/slot-contract.ts:16` | 插件配置卡片 |
| `tool.call.toolview` | keyed/session | `packages/client/ui-tool/src/client/contract/slots.ts:23` | 按工具名的 tool 卡片渲染器 |
| `conversation.hero.workspace.directoryFlow` / `sidebar.workspaces.directoryFlow` | single/root | `packages/client/ui-workspace/src/client/contract/slots.ts:56-58` | 目录选择流程 |

### 1.3 面板类插件范例：ui-jobs

`packages/client/ui-jobs/src/client/index.ts`（全文 40 行）：

```ts
export const inject = ['sessions', 'slots', 'locale']
export function apply(ctx: ClientContext): void {
  ctx.effect(() => ctx.locale.register(NS, { zh, en }), 'ui-job: dictionaries')
  ctx.slots.inject('conversation.session.header.actions', () =>
    ctx.slots.register({
      name: 'conversation.session.header.actions',
      id: 'job-list', order: 20, locale: NS,
    }, JobListAction))
}
```

要点：注册进 list slot 只需 `id` + `order`；数据经 `jobsBySession` 镜像到达，**零 RPC、零自有状态**（弹层可见性除外）。ui-message-feedback（`packages/client/ui-message-feedback/src/client/index.ts`）是带 RPC 的范例（见 §4）。

### 1.4 对 ticket 插件的含义

- **没有现成的"第二个右侧面板"或"可并列添加的右侧 dock"座位**。`details` 是 single-kind：注册进去是**替换**现有 DetailsPanel（并连带收掉它声明的 `conversation.details.tool` 座位）。不过 single-kind 支持 priority 遮蔽——可以用更高优先级（数字更低者胜；实际是同 cell 不同 priority 共存、最低者渲染）接管。
- 可加式入口（不替换任何东西）：`conversation.session.header.actions`（开一个按钮控制自己的 UI）、`sidebar.footer.action`、`shell.overlay`（浮动面板）、`conversation.view`（加一个整页标签视图）、`settings.section`（配置区）。
- 新增一个真正的"右侧可收起面板 list 座位"需要改 `packages/client/ui-layout`（AppFrame 的 children 声明 + 布局 store），即改 dsh 源码。

---

## 2. 第三方插件如何被加载

### 2.1 官方支持外部（out-of-tree）插件——profile/bundle 机制

证据：**`docs/user/develop/basic/publish.md`**（整篇就是"打包并安装插件"教程）：

- **bundle** = 带 `dsh.bundle` manifest 的 npm 包：`"dsh": { "bundle": { "patch": "./cordis.patch.yml" } }`（publish.md:42）。patch 里用包名引用插件：
  ```yaml
  - insert:
      - id: hello
        name: dsh-hello-plugin
  ```
- **profile** = `$DSH_HOME/profiles/<name>` 目录，`package.json` 里 `dsh.profile.bundles` 按序列出组合包；由 `dsh plugin --profile <name> add ./hello-plugin`（内部转发 pnpm）自动维护（publish.md:75-101）。
- 层叠顺序：profile 列出的各 bundle patch → profile 自己的 `cordis.patch.yml` → `$DSH_HOME/cordis.patch.yml` → `--patch` overlay（publish.md:112-119；`docs/architecture.md:27`）。
- 从 GitHub 直装也支持（`dsh plugin add github:you/hello-plugin`），但需作者提供自包含的 `prepare` 构建脚本 + 用户在 `pnpm-workspace.yaml` 的 `allowBuilds` 放行（publish.md:153-173）。**现成的外部 UI 插件例子：[turtle-ui](https://github.com/deepseek-harness/turtle-ui)**（publish.md:163 明确称其为 working example）。

### 2.2 Web 客户端插件的发现与服务机制

证据：**`docs/subsystems/client-modules.md`** 与 **`packages/client/modules`**：

- 包在 `package.json` 声明 `dsh.client`（`platform: 'web'`、可选 `inject` 边、可选 `immediately`），并在 `exports["./client"]` 导出构建好的 bundle（client-modules.md:49）。范例 `packages/client/ui-jobs/package.json`：`dsh.client` + `"./client": { "default": "./lib/client.js" }`。
- host 侧的 `ClientModuleRegistry`（`ctx.clientModules`）扫描 Loader 条目中声明了 `dsh.client` 的包，组合 `window.__DSH_BOOT__` 入口图，在 `/plugins/<id>/client.js` 提供 bundle，并经 index tap 注入启动 manifest（client-modules.md:5）。
- **包解析锚定在 `ctx.baseUrl`（cordis.yml 所在目录）**——即 profile 目录，其 package.json 把每个被组合的插件声明为依赖（client-modules.md:49）。`packages/client/AGENTS.md` checklist 第 2 条还提到 "profile boots resolve bare row names through the healed `$DSH_HOME/profiles/node_modules` fallback"——外部包名由此解析。

### 2.3 是否需要改 dsh 源码？

**运行时加载不需要。** 外部插件走 §2.1 的 bundle+profile 路径即可被 `dsh web` 加载；`packages/bundle/web-app/cordis.patch.yml`、`tsconfig.client.json` 的编辑只属于 **monorepo 内开发**路径（`docs/cookbook/adding-a-package.md` §2 的表格：`tsconfig.client.json` 加 references 条目）。

但有两个外部开发的现实约束：

1. **类型层面**：客户端 SlotMap 是声明合并，外部包只需在编译时依赖已发布的 `@deepseek-ai/dsh-client-ui-slots` 等包的 `.d.ts` 即可 merge，无需 dsh 源码（turtle-ui 即如此）。
2. **Typert Remote 的严格描述符是 monorepo 构建期生成的**（见 §4 注意点），外部 host 插件暴露 RPC 时走通用 RPC channel 更现实。
3. `pnpm run dev:web` 的 HMR watch 只覆盖 monorepo 内 client 插件（`docs/api-gateway.md:148`）；外部插件自己跑 tsdown watch 重建 `lib/client.js` 后，client-hmr 的 stat-poll 仍会对图内每一行 bundle 探测变更并广播（client-modules.md:63），所以浏览器热重载可用，只是构建责任在插件侧。

---

## 3. 面板收起/展开

### 3.1 现成范例

**布局几何集中在 ui-layout 的 root store**（`packages/client/ui-layout/src/client/stores.ts`）：

```ts
type LayoutState = { sidebar: number; details: number; narrow: boolean; narrowExpanded: boolean }
// 宽度即偏好，0 = 关闭
actions: {
  toggleSidebar: (d) => { ... d.sidebar = d.sidebar === 0 ? SIDEBAR_DEFAULT : 0 },
  openDetails: (d) => { if (d.details === 0) d.details = DETAILS_DEFAULT },
  closeDetails: (d) => { d.details = 0 },
}
```

- **跨插件面板动作面 `ctx.layout`**（`packages/client/ui-layout/src/client/service.ts:23-30`）：
  ```ts
  export interface ILayout {
    toggleSidebar(): void
    openDetails(): void
    closeDetails(): void
  }
  ```
- **侧栏收起**：`ui-sidebar` 的 SidebarRoot 消费 owner props `{ collapsed, width }`（`ui-layout/src/client/index.ts:94-99`），收起时渲染 56px 紧凑控制轨（`ui-sidebar/src/client/contract/slots.ts:44-48`：`wide` + `expandSidebar`）。窄视口自动收起由 `narrow`/`narrowExpanded` 处理。
- **右侧面板（details）收起**：`packages/client/ui-layout/src/client/AppFrame.tsx:170,191,198` —— `cols.details > 0` 才渲染详情列和拖动手柄；关闭按钮在 ui-conversation 的 DetailsPanel，经 inject 面 `closeDetails: () => void`（`ui-conversation/src/client/contract/slots.ts:719-722`）调 `ctx.layout.closeDetails()`。

### 3.2 插件能否注册"按钮控制收起的右侧面板"？

**机制完备，但座位语义是"替换"不是"添加"**：

- ✅ 收起/展开的状态机、持久化宽度、拖动、按钮触发（`ctx.layout.openDetails/closeDetails`）全部现成，任何插件注入 `layout` 服务即可驱动。
- ⚠️ `details` slot 是 `kind: 'single'`：第三方注册会**替换**现有工具详情面板（及其声明的 `conversation.details.tool` 子座位）。利用 single-kind 的 priority 遮蔽可以做到"我的面板优先渲染"，但同时只能有一个存活 occupant 被渲染——这不是"并列的第二个右侧面板"。
- 替代方案（均不改 dsh 源码）：用 `shell.overlay`（list，可加式）做浮动 ticket 面板，自己管理显隐；或注册 `conversation.view` 加一个 "Tickets" 标签页；或在 `conversation.session.header.actions` 放按钮 + 自有浮层。
- 若要"和详情面板并列的可收起右侧 dock"，需要 ui-layout 增加一个 list-kind 右侧座位——这是 dsh 源码改动（fork/patch 层面很小，但属于上游变更）。

---

## 4. 客户端插件与 host 通信（RPC）

### 4.1 分层与协议

`.agents/notes/implemented/architecture/2026-07-19-gui-layering-and-rpc-protocol.md`：

- 分层：`packages/host/*`（host 能力 + 协议）/ `packages/client/*`（纯库、静态到达入口、fetch 到达插件）/ `apps/*`。
- 四象限消息模型：ClientRequest（`POST /api/<method>`）/ ServerResponse / ServerRequest（WebSocket 下行）/ ClientResponse（`POST /api/respond`）；rpcId 发起方铸造、响应方回显。zod 双向校验。

### 4.2 主路径：Typert Remote（`@Remote` 装饰器 + 代码生成）

`docs/api-gateway.md`：

- host 服务继承 `TypertRemoteService`，方法标 `@Remote('name')` / `@RemoteScope(...)`；构建期 `dsh-typert-generator` 从 host `ts.Program` 生成严格描述符和 client 贡献。
- client 侧经 `ctx.remote.<namespace>.<method>(...)` 调用：`"The Client Remote calls connection.rpc.call('/api', '<namespace>/<method>', { args }, signal)"`（api-gateway.md:121）。
- **端到端范例（ticket 插件的最佳模板）**：
  - host：`packages/feedback/message-feedback/src/index.ts:150-271` —— `class MessageFeedbackService extends TypertRemoteService`，`static inject = ['storageDomain', 'sessionPersistence', 'sessions']`，`@Remote('list')` / `@Remote('put')` / `@Remote('delete')`。
  - client：`packages/client/ui-message-feedback/src/client/index.ts:32,46` —— `inject = ['slots', 'remote', 'remote.messageFeedback', 'locale']`，`new MessageFeedbackController(ctx.remote.messageFeedback, sessionId)`。

⚠️ **外部插件注意**：严格 client 描述符由 monorepo host 构建生成（api-gateway.md:97,137 —— "the Client Remote refuses to mount SRC descriptors that lack strict codecs"）。独立仓库的 host 插件要用 `@Remote`，需要自己跑 typert 生成（可行性未在文档中承诺）；更稳妥的外部路径是下面的通用 RPC channel。

### 4.3 备选：通用 Connection RPC channel（无需代码生成）

`packages/client/connection/src/rpc.ts` + `rpc-host.ts` + `client/rpc.ts`：

- host 注册：`ctx.connection.rpc.handle(channel, handler, { authority })`（`rpc.ts:25-50`；`ConnectionRpcAuthority = 'trusted-host' | 'loopback'` 信任栅栏）。
- 浏览器调用：`ctx.connection.rpc.call(channel, endpoint, payload, signal?)`（`client/rpc.ts:19-49`）——实际发 `POST <channel>/<endpoint>`，body 为 `ClientRequest`，响应经 zod 校验 + rpcId 匹配。
- Typert 与 API Proxy 共享这条 `/api` 通道（api-gateway.md:121-123）。

### 4.4 无 RPC 的数据通道

session 事件流/投影镜像经 SSE/WebSocket 下行到达对象层，client 插件用框架标准件（`useSession`/`useProjection`）或镜像读取——ui-jobs 全程零 RPC（`ui-jobs/src/client/index.ts:1-6` 注释）。ticket 列表若建模为 session projection，可走同一路径。

---

## 5. Host 端插件能力

`packages/host/*` 插件是普通 cordis 插件（`docs/cordis-primer.md`：plugin = `apply(ctx)` + `inject`；服务经 `ctx.<key>`；注册必须可逆）。能力逐项确认：

| 能力 | 可行性 | 证据 |
|---|---|---|
| **注册 HTTP 端点** | ✅ `ctx.webServer.register({ kind: 'exact' \| 'prefix', path, handler })`，另有 `registerUpgrade`（WebSocket）和 `tapIndex`（HTML 变换）；重复 (kind,path) 抛错 | `docs/subsystems/web-server.md`（Routes/The service 节）；现例：`packages/client/modules/src/index.ts`（`/plugins/<id>/client.js`）、`packages/client/connection/src/index.ts`（`/api`）、`packages/host/frontend-static`（fallback） |
| **注册 LLM 工具** | ✅ `ctx.tools.register(defineTool({...}))` 返回 disposer | `packages/core/tools/src/index.ts:1037`；范例 `packages/todo/tool-todo/src/index.ts:149`（`todo_write`）。目录：`docs/tool-catalog.md` ⚠️ Web 表面下模型可见工具按 **agent preset 每会话挂载**——`packages/bundle/web-app/cordis.patch.yml:293-408` 把 `tool-*` 行全部 `disabled: true` 并改由 `agent-presets` 组合；host-plane 注册的工具能否进 web session 取决于该工具的注册方式（也可经 `agent.ctx` 按 agent 注册，见 tools/index.ts:727 注释） |
| **访问文件系统** | ✅ host 插件即 Node 进程代码，全量 fs 访问 | 例：`packages/bundle/web-app/cordis.patch.yml:57` `storage-json` 写 `dshHomePath('storages')` |
| **使用 sqlite** | ✅ 仓内标准是 **`node:sqlite`（`DatabaseSync`）**，非 better-sqlite3 | `packages/session/session-persistence-sqlite/src/index.ts:13`、`packages/storage/storage-sqlite/src/schema.ts:9`、`packages/session-query/session-query-sqlite/src/schema.ts:52`（惰性 `await import('node:sqlite')`）。全仓 grep 无 better-sqlite3 |
| **结构化 KV 存储（推荐给 ticket 数据）** | ✅ `ctx.storageDomain.open(spec)`：zod 校验、变更事件、后端可路由（json/sqlite） | `packages/storage/storage-domain/src/index.ts`（`defineDomain`/`domainTable`）；消费范例 `packages/feedback/message-feedback/src/index.ts:174` |
| **对浏览器暴露服务** | ✅ Typert `@Remote`（§4.2）或通用 RPC channel（§4.3） | 见上 |

---

## 总结：ticket 插件可行性结论

1. **UI**：slot 系统成熟且文档完备；ticket 插件可以零侵入地加 header 按钮、浮层（`shell.overlay`）、视图标签页（`conversation.view`）、设置区。"可收起的专属右侧面板"机制（store + `ctx.layout`）现成，但 `details` 座位是 single-kind 替换语义；并列第二右面板需上游改 ui-layout。
2. **加载**：**外部独立仓库插件被官方支持**（bundle + profile + `dsh plugin add`），无需修改 dsh 源码；有 turtle-ui 现成样例。monorepo 内路径（cordis.patch.yml + tsconfig.client.json）仅适用于在 dsh checkout 内开发。
3. **收起/展开**：范例充分（sidebar 收起、details 开/关、拖动宽度）。
4. **RPC**：Typert Remote 是一等路径但 client 描述符依赖 monorepo 构建；外部插件更现实的是 `ctx.connection.rpc.handle/call` 通用 channel，或 webserver 自有路由 + fetch。
5. **Host 端**：HTTP 端点、LLM 工具、fs、`node:sqlite`、`storageDomain` KV 全部可行；message-feedback（host）+ ui-message-feedback（client）组合是 ticket 插件最贴近的端到端模板。
