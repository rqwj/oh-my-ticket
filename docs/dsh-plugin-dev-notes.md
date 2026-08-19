# DSH 插件开发注意事项（OMT 实战沉淀）

> 来源：oh-my-ticket 插件 0→1 全过程（M0–M6 + 后续迭代至 v0.2.16）。
> 每条都是实测结论或踩过的坑，附证据位置。基础调研见根目录 `RESEARCH-dsh-ticket-plugin.md`。

---

## 一、加载与打包

### 1.1 外部插件的加载路径

- 官方路径：**bundle（`dsh.bundle` manifest + cordis.patch.yml）+ profile（`dsh plugin add`）**。插件行按**包名**引用。
- **client 插件必须能从 profile 目录按包名解析**——`ClientModuleRegistry` 用 `createRequire(ctx.baseUrl)` 找 package.json。`--patch` 绝对路径行只对 host 插件有效，对 client 插件的发现无效。
- 开发期依赖用 `link:` 指向 DSH 源码 checkout（npm 上 `@deepseek-ai/*` 是 0.0.1-rc.1，远落后于源码 0.1.0-rc.5，**不要用 npm 版本**）。

### 1.2 打包自包含

- host 半构建产物要**全内联**（tsdown `external: []`）：defineTool（dsh-tools）、schemastery 是纯库，无跨插件运行时身份，内联后安装即运行，不依赖 profile 侧的包解析。
- client 半：**平台模块必须 external**（react、cordis、ui-slots、ui-primitives、`dsh-client-runtime/client` 等，见 `packages/client/web/src/platform.ts` + RUNTIME_STORE_EXEMPTION），其余一律 inline——require 到表外模块必崩。
- client bundle 形态：CJS 闭包工厂，`window.__ModuleLoader__.load({id, factory})` 包裹（banner/footer）。

### 1.3 安装与升级

- **同名 tarball 重新 `dsh plugin add` 不会刷新内容**（file: 依赖按 spec 字符串去重）。每次发版必须 bump 版本号产出新文件名。
- **插件集合变更需要重启**：组合在启动时完成；包元数据按名缓存永不过期；只有 profile/home 的 `cordis.patch.yml` 编辑会被 watch 热重放。HMR 只管已注册条目的 bundle 内容变化。

### 1.4 开发回路

- client 改动：`tsdown --watch` 重建 `lib/client.js` + **刷新页面**（index tap 每次渲染注入最新图，rev 防缓存）。
- host 改动：必须重启实例。
- 多实例开发**必须隔离 DSH_HOME**（`DSH_HOME=~/.dsh-omt pnpm dsh ...`）——两个实例共享 sessions home 会同写 session 日志，seq 撞号导致 "corrupt session log"（本仓库 `scripts/repair-session-log.mjs` 是修复工具）。

---

## 二、Client 插件（slot 体系）

### 2.1 组合纪律

- 唯一组合 API：`ctx.slots.register({ name, children?, store?, inject?, locale?, priority?/id?/order?/key?/select? }, Component)`；延迟/条件挂载用 `ctx.slots.inject(name, () => register(...))`（返回 disposer）。
- kind 语义：**single**（遮蔽，priority 数字小者胜）、**list**（可加式并列）、**keyed**（按键派发）、**chain**（首个 select 非空者胜，见 2.4）。
- 组件只见 props 不见 ctx；跨包不 import，只走 slot/RPC。

### 2.2 状态管理

- 状态放 apply 闭包的 `createSnapshotStore`（platform external `@deepseek-ai/dsh-client-runtime/client`），经注册的 `inject: () => ({ hooks: { x: store }, ...callbacks })` 暴露；组件得到 `useX` 选择器 hook。
- 同一 store handle 只能注册在**一个 scope**（root/session 冲突会抛错）；session 作用域组件要数据就走 hooks 通道，不要试图共享 root store。
- `persist: { name }` 选项即可持久化到刷新后（抽屉宽度/折叠状态就是这么做的）。

### 2.3 details 面板（右侧）

- `details` 是 single 座位，被 ui-conversation 的 DetailsPanel 占据；**没有并列第二面板的座位**（上游议题）。
- 临时接管模式（实测可用）：选中时 `slots.register({ name: 'details', priority: -10 }, …)` + `ctx.layout.openDetails()`；关闭时 dispose 注册，原面板自动恢复。
- 宿主列宽常量：`DETAILS_DEFAULT=360 / MIN=300 / MAX=520`——面板内容按 360px 设计。
- 点击工具行（`data-chat-call-id`）的单向让位：捕获阶段 document 监听 + dispose 遮蔽（不关列）。

### 2.4 chain 型 slot 的坑（真实 bug，TICKET-0021）

- **matched share 在选举瞬间冻结**。select 里读全局"当前会话"之类的侧信道 → 会话切换首帧读到旧值且不可更正。
- 正确做法：select 无条件认领（`select: () => ({})`），组件用**框架下发的 sessionId prop**（session 作用域标准件）做归属。

### 2.5 输入触发与 chip

- 触发字符 `'/' | '@'` 是冻结契约（`ui-input-trigger/src/types.ts` + detect.ts 硬编码），外部插件加不了 `#`；用 `ctx.inputTriggers.registerSource()` 在现有字符下注册新 source。
- **`ReferenceInsert.source` 必须等于 source 的 `name`**——提交时按 `roster.find(s => s.name === occurrence.source)` 查 codec，不匹配报 "no serializer for reference source"。
- chip 是 textarea 背景层的固定 4em 单元格（一个 U+FFFC 占位符）：label 必然裁切、不可点击。完整标题/点击交互用 `conversation.input.dock` 的引用条承载——dock 的 owner（InputZone）暴露 `input.occurrences` 引用占位表，删 chip/提交自动同步。
- dock 对齐公式（TodoPanel 同款）：`margin: 0 auto; width: calc(100% - 2×side-clearance - 4×dock-inset); max-width: calc(card-max-width - 4×dock-inset)`。

### 2.6 样式

- CSS Modules 需要内联插件（lightningcss transform + `<style data-plugin>` 注入），复刻自 `packages/client/tsdown.client.ts`。
- 颜色走 `--dsw-*` 语义 token；字号用 `--dsw-font-*` 族；暗色主题下 `--dsw-alias-brand-primary` 是近白色（按钮填色别用它，发送按钮用 `--dsw-alias-button-info-fill` + 白字）。
- 原生 `<select>` 的 `<option>` 里 emoji 正常显色，普通字符无法按选项着色。
- 面板拖拽用 AppFrame 模式：`setPointerCapture` + rAF 节流 + 起点基准宽度。

---

## 三、Host 插件

### 3.1 工具

- `ctx.tools.register(defineTool({...}))`；host-plane 注册落全局层，**所有会话可见**（standard preset 无 restrict 掩码）。
- 参数 DSL：可选默认、`required: true`、`enum`；output 规范值要是程序化 JSON（对象字段需 `required: true` 否则类型全可选）。
- 拿 cwd/会话：`exec.agent?.session.header.cwd` / `.id`。
- 执行中询问用户：`ctx.userQuestions.ask({ questions, agent: exec.agent, signal: exec.signal })`（无 UI 环境会抛，需回退路径）。

### 3.2 数据与存储

- sqlite 用内置 `node:sqlite`（**惰性 `await import`**，Node 22 有 ExperimentalWarning）；WAL 模式。
- **FTS5 的 unicode61 分词器不切分 CJK**（整段中文成一个 token，前缀查询失效）——中文搜索用参数化 LIKE。
- schema 迁移：`PRAGMA table_info` 探测 + `ALTER TABLE` + 版本号记 meta 表。
- 双写一致性（SQLite 索引 + Markdown 内容）：文件为内容权威、库为查询索引，frontmatter 驱动 `reindex` 自愈；文件内的受管区块用 HTML 注释标记（`<!-- omt:children -->`）。
- 多 home 场景 id 要全局唯一：pool 分配时同步各 home 计数器。

### 3.3 RPC 与推送

- 外部插件用通用通道：`ctx.connection.rpc.handle('/omt', handler, { authority: 'loopback' })`（Typert `@Remote` 的严格 client 描述符依赖 monorepo 构建）。envelope：`POST /omt/<endpoint>`，body `{type:'client-request', rpcId, method, payload}`。
- zod 用 **v4**（DSH 版本；v3 的 ZodIssue 类型不兼容）。
- host→browser 推送：转发事件走 `API_REMOTE_FORWARDED_EVENTS` **固定白名单（不可扩展）**；插件自有推送用 `ctx.webServer.register()` 起 **SSE 端点**（Node req/res 形态），client 用 EventSource（同源、自动重连）。
- skill 内嵌：`ctx.skills.register({ name, description, content, source: 'runtime' })`；多 skill 可同时激活（用户手势逐个注入、模型按需加载）。

---

## 四、测试

- host 核心逻辑纯 vitest（node 环境，temp dir）。
- client controller 测试：`vitest.config.ts` alias 掉 platform 模块（`dsh-client-runtime/client` → 本地 mock）；涉及 DOM 的用例文件首行 `// @vitest-environment jsdom`（jsdom 要显式装）。
- e2e 需要模型时：实例要带 provider token 的环境变量（settings.yaml 只存 `apiKeyEnv` 名字）。
- RPC 端点可以直接 curl 冒烟（envelope 简单），不必等浏览器。

## 五、协作机制（OMT 自身约定，可供参考）

- ticket 操作全走 `omt_*` 工具；接手先 `omt_show` + 置 in_progress；完成 append 结论 + 置 done。
- 归档是独立维度（archived 布尔），归档即只读（core 层强制），恢复后才能改。
- Epic 创建的归属（workspace/global）由用户弹窗决定，模型不替用户决定。
