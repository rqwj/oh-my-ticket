# Oh-My-Ticket（OMT）— DSH Ticket 管理插件可行性分析

> 调研对象：`/Users/robertq/Tools/dsh/deepseek-harness/`（v0.1.0-rc.5）
> 详细代码证据见同目录 [`RESEARCH-dsh-ticket-plugin.md`](RESEARCH-dsh-ticket-plugin.md)
> 结论日期：2026-08-17

---

## 0. 总体结论

**整体可行，且大部分能力可以零侵入（不改 DSH 源码）实现。**

| # | 需求 | 可行性 | 说明 |
|---|------|--------|------|
| 1 | Ticket 管理插件 | ✅ 完全可行 | DSH 官方支持独立仓库的外部插件（bundle + profile + `dsh plugin add`），有现成样例 [turtle-ui](https://github.com/deepseek-harness/turtle-ui) |
| 2 | SQLite 元数据 + 磁盘 Markdown，home 目录 `~/.omt` / `OMT_HOME` | ✅ 完全可行 | Host 插件即 Node 进程代码；仓内标准是内置 `node:sqlite`（Node ≥22，无原生依赖）；配置可走 cordis 插件 config + 环境变量 |
| 3 | Epic → Story → [SubStory] → Ticket → [SubTicket] 五级体系 | ✅ 完全可行 | 纯数据建模问题，本文 §3 给出文件布局与 SQLite schema 设计 |
| 4 | 右侧树形面板 + 按钮收起 | ⚠️ 可行但有妥协 | 收起/展开机制全部现成，但右侧 `details` 座位是**单占替换语义**，"并列第二个右侧面板"需改 DSH 源码；已定稿"树在 `shell.overlay` 抽屉 + 文档详情动态遮蔽 details 面板"的零侵入方案（§4.3） |
| 5 | 输入框引用 ticket（定为 `@`） | ✅ 可行（用 `@`） | 触发字符 `'/'` / `'@'` 是**硬编码冻结契约**；已决策在 `@` 下注册 ticket 引用源（ReferenceInsert + codec），体验与 `#` 等价（§5）。`#` 仅留作可选上游 PR |
| 6 | 与开发流程 skill 共存 / 多 skill 同时激活 | ✅ 完全可行 | 代码层面确认同一会话可同时激活多个 skill；插件可通过 `ctx.skills.register()` 内嵌自己的 skill（§6） |

**推荐形态**：一个仓库产出三个部件——Host 插件（数据 + 工具 + RPC）、Client 插件（树形面板 + `@` 引用源）、内嵌 Skill（ticket 操作提示词），由同一个 `dsh.bundle` 组合分发。

---

## 1. 插件载体与加载方式

DSH 插件 = 导出 `apply(ctx)`（+ 可选 `inject`）的模块，基于 cordis 框架。外部独立仓库插件是**官方支持的一等公民**：

- **bundle**：npm 包声明 `"dsh": { "bundle": { "patch": "./cordis.patch.yml" } }`，patch 内按包名引用插件行。
- **profile**：`$DSH_HOME/profiles/<name>` 组合各 bundle；用户通过 `dsh plugin add <你的包>` 安装（支持 npm 与 `github:` 源）。
- **Web 客户端 half**：包声明 `dsh.client` manifest 并导出 `./client` 构建产物，host 的 `ClientModuleRegistry` 扫描后注入 `window.__DSH_BOOT__`，经 `/plugins/<id>/client.js` 提供给浏览器。
- 开发期也可用 `pnpm dsh web --patch ./your-overlay.yml` 以绝对路径直接挂载本地插件（`docs/user/develop/basic/index.zh.md`）。

**无需修改 DSH 源码**即可加载。两个现实约束：

1. 客户端 SlotMap 类型是 TS 声明合并——外部包编译期依赖已发布的 `@deepseek-ai/dsh-client-*` 类型包即可（turtle-ui 即如此）。
2. Typert `@Remote` 严格 RPC 描述符依赖 monorepo 构建期代码生成；外部插件建议走通用 RPC 通道（`ctx.connection.rpc.handle/call`）或 `ctx.webServer.register()` 注册自有 HTTP 端点 + fetch（§4 详述）。

## 2. 存储层：SQLite + 磁盘 Markdown（需求 2）

### 2.1 可行性

- Host 插件就是 Node 代码，文件系统全量访问；仓内 sqlite 标准是 **`node:sqlite` 的 `DatabaseSync`**（`packages/storage/storage-sqlite`、`packages/session/session-persistence-sqlite` 均如此），零原生依赖，Node ≥22 内置。
- 仓内还有更高层的 `ctx.storageDomain.open(spec)`（zod 校验 + 变更事件 + json/sqlite 可路由后端，`packages/feedback/message-feedback` 是消费范例）。**但 OMT 有自定义关系表和目录布局诉求，建议直接持有自己的 `DatabaseSync` 句柄**，不套 storageDomain。

### 2.2 Home 目录解析

```
OMT_HOME 环境变量  >  插件 config（cordis.yml 可配）  >  ~/.omt
```

cordis 插件 config 支持 `!!js process.env.OMT_HOME` 表达式，也可在 apply 内自行解析——三种方式都简单，推荐 apply 内解析（用户零配置即可用，改环境变量即生效）。

### 2.3 职责划分（双写的经典取舍）

| 存储 | 内容 | 理由 |
|------|------|------|
| **SQLite**（`$OMT_HOME/omt.db`） | 节点元数据（id、类型、标题、状态、优先级、时间戳）、**父子关系边**、内容搜索镜像 | 树形查询、状态过滤、界面结构化呈现都走 SQL，避免扫盘 |
| **Markdown 文件**（`$OMT_HOME/**/*.md`） | 节点详细内容（描述、验收标准、讨论记录），**文件内以链接清单呈现子节点** | 人可读、可手改、可被 `@` 文件引用、可进 git |

⚠️ 一致性风险：SQLite 与文件是双写。建议 **SQLite 为权威索引、Markdown 为权威内容**：启动时做一次校验/重建（扫描文件 frontmatter 重建关系表），并提供 `omt reindex` 工具。文件内子节点清单由插件维护（工具写入时同步更新父文件），人工改文件靠 reindex 兜底。

## 3. 体系结构建模（需求 3）

层级：`Epic → Story → [SubStory] → Ticket → [SubTicket]`（SubStory/SubTicket 为可选层，深度允许有限递归还是仅一层，建议**仅一层**，模型与 UI 都简单）。

### 3.1 目录与文件布局（建议）

```
$OMT_HOME/
├── omt.db                        # SQLite 元数据 + 关系
└── tickets/
    └── EPIC-0001-用户体系/
        ├── epic.md               # 节点正文 + 子节点链接清单
        └── STORY-0001-登录/
            ├── story.md
            ├── SUBSTORY-0001-第三方登录/substory.md
            └── TICKET-0001-登录接口/
                ├── ticket.md
                └── SUBTICKET-0001-参数校验/subticket.md
```

节点 Markdown 建议带 frontmatter（`id/type/status/parent/children`），正文内维护 `## 子节点` 链接区——既满足"子节点在文件中呈现"，又让 reindex 可从文件重建 SQLite。

### 3.2 SQLite schema（建议）

```sql
CREATE TABLE nodes (
  id         TEXT PRIMARY KEY,      -- 'EPIC-0001' / 'TICKET-0042'
  type       TEXT NOT NULL,         -- epic|story|substory|ticket|subticket
  title      TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'open',   -- open|in_progress|done|archived
  priority   INTEGER DEFAULT 0,
  path       TEXT NOT NULL,         -- 相对 $OMT_HOME 的 md 路径
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE edges (
  parent_id  TEXT NOT NULL REFERENCES nodes(id),
  child_id   TEXT NOT NULL REFERENCES nodes(id),
  ord        INTEGER NOT NULL DEFAULT 0,     -- 同级排序
  PRIMARY KEY (parent_id, child_id)
);
-- 内容搜索：nodes_search(id, title, body) 镜像表 + 参数化 LIKE
-- （M1 实测后放弃 FTS5：unicode61 分词器不切分 CJK，中文整段成一个 token）
```

`edges` 单独成表（而非 nodes 上挂 parent_id 列）便于排序与未来的多挂/看板扩展；树查询用递归 CTE 一次取整棵子树喂给界面。

### 3.3 类型规则

在 host 服务里固化合法父子关系：`epic→story`，`story→substory|ticket`，`substory→ticket`，`ticket→subticket`；工具层做校验，非法结构直接拒绝。

## 4. 界面：右侧树形面板（需求 4）

### 4.1 DSH 的 UI 组合机制

客户端插件唯一组合 API 是 `ctx.slots.register({ name, children?, store?, inject? }, Component)`，slot 由各自属主声明（"声明即授权"）。关键事实：

- 右侧详情列只有一个 slot：`details`（`kind: 'single'`，`packages/client/ui-layout`），当前被会话的工具详情面板占据。**single 语义是替换，不是并列添加**（同 cell 按 priority 遮蔽，最低 priority 者渲染，但同时只渲染一个）。
- 收起/展开机制**完全现成**：ui-layout 的 root store（宽度即状态，0=关闭）+ 跨插件服务 `ctx.layout.toggleSidebar()/openDetails()/closeDetails()`，含拖动宽度、持久化偏好、窄视口自动收起。

### 4.2 三条实现路线（按推荐度排序）

**路线 A（推荐，零侵入）：header 按钮 + 接管 `details` 面板**

- 在 `conversation.session.header.actions`（list 型，可加式，`ui-jobs` 是 40 行的范例）注册一个 "Tickets" 按钮。
- 点击后用更高优先级（更小 priority 值）向 `details` 注册 OMT 树面板并 `ctx.layout.openDetails()`；再点或面板内关闭按钮则注销/收起。
- 代价：OMT 面板打开时，工具详情面板被遮蔽（用户看 OMT 树还是工具详情是二选一）。对"ticket 管理"这个独占场景通常可接受。

**路线 B（零侵入，并列共存）：浮动面板或标签视图**

- `shell.overlay`（list 型可加式）：做可拖动的浮动 ticket 面板，显隐自管理；或
- `conversation.view`：注册一个 "Tickets" 整页标签（与 Chat/Trajectory 并列），内部自己做左右分栏树+详情。
- 代价：不是"固定在右侧的栏"，与需求字面有出入。

**路线 C（体验最贴需求，需改 DSH 源码 / 上游 PR）**

- 在 `ui-layout` 的 AppFrame 增加一个 list 型右侧 dock 座位。改动很小（children 声明 + 布局 store 一列），但属于上游变更：短期 fork/patch 维护，长期建议提 PR。

### 4.3 定稿方案：树与详情分置（2026-08-17 决策）

经进一步核实 details 面板契约（`ui-conversation/src/client/contract/slots.ts:115-123`）：其内容座位 `conversation.details.tool` 是"选中 tool call 的输出主体"，由共享 details store 驱动，**没有通用内容入口**。因此最终采用"树在别处、详情复用 details 面板"的分置设计：

- **树（主入口）**：`shell.overlay` 浮动抽屉（list 可加式，零冲突），锚定侧缘、与聊天同屏可见；由 `conversation.session.header.actions` 与 `sidebar.footer.action` 的按钮控制开合。
- **树（管理视图，可选）**：`conversation.view` 增加 "Tickets" 整页标签（与 Chat/Trajectory 并列），内部左树右预览，适合批量管理。
- **文档详情**：**动态遮蔽**——树中选中节点时，OMT 临时向 `details` 注册文档视图（priority 遮蔽 DetailsPanel）并 `ctx.layout.openDetails()`；关闭时 dispose 注册，原工具详情面板自动恢复。复用原生面板几何（开/关/拖宽/持久化），与工具详情自然切换。
- **顺带获得**：为 `omt_show` 工具注册 keyed 的 `tool.call.toolview` 渲染器，模型展示 ticket 时其工具卡片在 details 面板中即为富渲染文档。
- **补充**：`conversation.input.dock` 放置"当前激活 ticket"状态条。
- **不可行项**：左侧 sidebar 无可加座位（整列 single 被 ui-sidebar 占据，子座位均为 single），仅 `sidebar.footer.action` 可放触发按钮。

该组合完全零侵入；路线 C（上游并列 dock PR）仅作为长期优化保留。

### 4.3 数据通路

树数据走 host → client：

- **推荐**：host 插件用 `ctx.connection.rpc.handle('...')` 注册通用 RPC（或 `ctx.webServer.register()` 暴露 `/omt/*` REST），client 插件 fetch + 声明 zustand store（`createOmtStore()` 工厂，`register` 时声明）缓存树；变更由工具执行后主动推送（ServerRequest 下行）或前端轮询失效。
- 仓内端到端模板：`packages/feedback/message-feedback`（host，`@Remote` + storageDomain）↔ `packages/client/ui-message-feedback`（client，store + slot 注册）。

## 5. 输入框 `#` 引用（需求 5）

### 5.1 现状（代码证据）

`packages/client/ui-input-trigger` 是触发流水线：检测 + 候选菜单 + pick 路由。触发字符是**冻结契约**：

- `src/types.ts`：`export type TriggerChar = '/' | '@'`，文件头注明"changes require main-thread arbitration"；
- `src/core/detect.ts`：`if (ch !== '/' && ch !== '@') continue`——`#` 根本不会进入检测。

**外部插件无法新增 `#` 触发字符。**

### 5.2 可行方案

**方案 1（已定稿，零侵入）：`@` 触发下注册 ticket 引用源**

扩展点是 `ctx.inputTriggers.registerSource(src)`（运行时服务，支持迟到注册）。OMT client 插件注册一个 `@` source：

- `candidates(query)`：经 RPC 查 SQLite（LIKE 内容搜索）返回匹配的 ticket 候选（名称/描述/图标按类型区分）；
- pick 返回 **`ReferenceInsert`**：草稿中插入 U+FFFC 占位符 chip（显示如 `@TICKET-0042`），提交时由 source 的 `ReferenceCodec.serializeReference` 异步序列化为模型可读文本（如 ticket 标题 + 状态 + 正文摘要，或正文全文）。
- 这是 input-trigger 契约里现成的引用注入机制（`PickOutcome` 三分支之一），用户体感与 `#` 引用完全一致，仅触发键不同。注：当前仓内 `@` 下的已有 source 是 ui-subagent（引用运行中的子 agent），ticket 源与其按 `order` 分组并列共存即可。

**方案 2（坚持要 `#`）**：修改 `ui-input-trigger`（`TriggerChar` 联合类型加 `'#'` + `detect.ts` 放行 + 菜单分组）。代码量极小，但属上游变更：fork/patch 维护或提 PR。注意 `#` 在 Markdown 标题等输入习惯上可能有歧义，检测守卫（如仅行首/空白后触发）要一并设计。

**结论**：采用方案 1（用户已决策 `@` 即可）；方案 2 的 `#` 触发字符仅作为长期可选的上游贡献议题。

## 6. 与开发流程 skill 共存（需求 6）

### 6.1 多 skill 同时激活——代码层面确认支持

- skill 机制三层：`dsh-skill`（`ctx.skills` 注册表，分层合并 project/user/bundled 等提供方）、`dsh-skill-filesystem`（扫 `.dsh/skills`、`.agents/skills` 等 5 个根）、`dsh-tool-skill`（模型目录 + `skill` 工具）。
- **模型侧**：会话中注入 `<available_skills>` 目录（仅 name+description），模型按任务自行调 `skill({name})` 工具加载正文——可先后/同时加载多个，互不排斥。
- **用户侧**：`/skill-name` 手势激活。铁证：`packages/skill/tool-skill/src/index.ts` 的 `SKILL_GESTURE` 正则带 `g` 标志扫描整条用户消息收集**去重名称数组**，随后 `for (const name of names)` 为**每个** skill 各注入一条 `<skill_content>`——一条消息里 `/omt /ce-plan` 两个 skill 会同时生效。

### 6.2 插件自带 skill——一等模式

OMT 插件可在 `apply` 里 `ctx.skills.register({...})` 注册内嵌运行时 skill（随插件启停，可逆），无需用户手工放置 SKILL.md。也可同时发布独立 SKILL.md 包供非插件用户用。

### 6.3 与流程类 skill 的协作分工（提示词设计）

原则：**OMT 只管"ticket 的记录、结构与状态"，不规定"怎么开发"**。流程类 skill（计划、实现、调试、提交）管"怎么做"，OMT 管"做的事挂在哪个 ticket 上、做到哪一步了"。ticket 系统"调用"其它 skill 不需要特殊机制——skill 目录都在模型面前，提示词里点名引导即可（模型读到一个 skill 的正文里建议配合另一个 skill 时，会自行调用 `skill` 工具加载它）。

建议的 OMT skill 提示词草稿（SKILL.md 正文要点）：

```markdown
---
name: omt
description: OMT ticket 体系的操作规范：Epic→Story→[SubStory]→Ticket→[SubTicket]
  五层结构的创建、查询、状态流转与引用。当用户提到 ticket、epic、story、
  任务拆解、进度记录，或消息中出现 #TICKET-/EPIC- 等 OMT 引用时使用。
  本 skill 只负责 ticket 管理，不规定开发流程；开发方法论由其它 skill 负责。
---

# OMT Ticket 管理

## 职责边界（重要）
- 你负责且仅负责：ticket 节点的 CRUD、层级合法性、状态流转、进度记录。
- 你**不负责**规定开发流程（如何规划、实现、调试、提交）。当当前任务
  涉及开发方法论时，检查 <available_skills> 中是否有对应流程类 skill
  （如计划/实现/调试/commit 类），有则调用 skill 工具加载并遵循它；
  你与其并行生效：它管"怎么做"，你管"进度记在哪"。

## 工具
使用 omt_* 工具（omt_create / omt_update / omt_move / omt_list /
omt_show / omt_reindex）操作 ticket，不要直接手改 $OMT_HOME 下的文件。
层级规则：epic→story，story→substory|ticket，substory→ticket，
ticket→subticket；SubStory/SubTicket 各最多一层。

## 工作约定
- 开始处理某 ticket 前先 omt_show 读取正文与验收标准，并将其状态置为
  in_progress；完成时把关键结论追加到 ticket 正文并置为 done。
- 任务拆解时先建 Epic/Story 骨架再逐层细化，禁止跨层挂载。
- 用户在输入中用 @ 引用的 ticket 视为当前上下文，相关操作默认作用于它。
```

配套地，流程类 skill 一侧若想反向联动，只需在其正文加一句"如环境提供 omt skill / omt_* 工具，开始前将任务登记为 ticket 并在完成后更新状态"——这是纯提示词约定，无机制依赖。

### 6.4 skill 之外的提示词注入途径（可选增强）

OMT 还可以利用 DSH 的其它注入通道强化"ticket 感知"（均为 host 插件可逆注册）：

- `ctx.systemPrompt.section({ name, order, text })`：贡献固定指引段（如"本环境装备了 OMT ticket 体系"），order 建议取工具指导区段 100–199；
- `ctx.systemPrompt.context(provider)`：动态 runtime context，每步组装时求值——适合注入"当前激活 ticket / 进行中的 ticket 统计"；
- `agent/pre-step` waterfall 或 `agent.inject()`：追加持久 user 消息——适合在 ticket 状态变更时（如某 ticket 被置为 done）主动通知模型。

注意节制：skill 正文已覆盖操作规范，固定段保持一两行即可，避免每步都消耗上下文。

## 7. 建议的插件架构与实施路径

### 7.1 仓库结构（单仓三部件）

```
oh-my-ticket/
├── package.json                 # dsh.bundle manifest + cordis.patch.yml
├── cordis.patch.yml             # 组合 host 行 + client 行（dsh.client）
├── src/host/                    # Host 插件
│   ├── index.ts                 # apply：config 解析 OMT_HOME、开库、注册服务/工具/RPC
│   ├── store.ts                 # node:sqlite schema + 树查询 + LIKE 内容搜索
│   ├── files.ts                 # Markdown 读写、frontmatter、子节点清单维护、reindex
│   ├── tools.ts                 # omt_create/update/move/list/show/reindex（defineTool）
│   ├── rpc.ts                   # ctx.connection.rpc.handle（或 webServer REST）
│   └── skill.ts                 # ctx.skills.register 内嵌 omt skill
└── src/client/                  # Client 插件（dsh.client, ./client bundle）
    ├── index.ts                 # apply：slots.inject + registerSource + store 声明
    ├── drawer/                  # 树面板：shell.overlay 抽屉 + header/sidebar 开关按钮（§4.3）
    ├── details/                 # 文档详情：details 动态遮蔽视图 + omt_show 的 toolview
    └── trigger/                 # @ ticket 引用源（candidates + ReferenceCodec）
```

### 7.2 里程碑

1. **M1 Host 核心**：目录解析、schema、文件双写、reindex、六个 CRUD 工具 → 纯命令行/会话内即可用。
2. **M2 Skill + 提示词**：内嵌 skill 注册，验证与其它流程 skill 并行激活。
3. **M3 界面**：header 按钮 + details 树面板（路线 A）+ RPC 数据通路。
4. **M4 引用**：`@` ticket 触发源 + ReferenceInsert codec。
5. **M5（可选，上游）**：`#` 触发字符 PR；右侧并列 dock 座位 PR。

### 7.3 风险清单

| 风险 | 等级 | 缓解 |
|------|------|------|
| `details` 替换语义导致与工具详情面板互斥 | 中 | 路线 A/B 并行提供；长期推路线 C 上游 PR |
| `#` 触发需改上游 | 低 | `@` 源体验等价；`#` 作为独立上游贡献 |
| SQLite 与 Markdown 双写不一致 | 中 | SQLite 作索引、文件作内容权威；启动校验 + `omt_reindex` |
| 外部插件 RPC 无 Typert 严格描述符 | 低 | 通用 `connection.rpc` channel + zod 自校验 |
| DSH 版本演进（rc 阶段 API 未冻结） | 中 | 锁定已验证版本开发；client 契约文件头标注了仲裁要求，关注变更日志 |
| Web 会话工具可见性按 agent preset 挂载 | 低 | 参照 `tool-todo` 的注册方式，必要时经 `agent.ctx` 按 agent 注册 |

---

## 附：关键证据索引

- Slot 系统与全部已声明 slot 清单：`packages/client/ui-slots/src/index.ts`；`packages/client/ui-layout/src/client/index.ts:49-83`
- 外部插件加载：`docs/user/develop/basic/publish.md`；turtle-ui 样例；`docs/subsystems/client-modules.md`
- 面板收起：`packages/client/ui-layout/src/client/stores.ts` + `service.ts`（`ctx.layout`）
- 触发字符冻结：`packages/client/ui-input-trigger/src/types.ts`（`TriggerChar = '/' | '@'`）、`src/core/detect.ts:52`；注册扩展点 `ctx.inputTriggers.registerSource`（`src/client/service.ts`）
- 多 skill 同时激活：`packages/skill/tool-skill/src/index.ts`（SKILL_GESTURE 数组 + 逐个注入）
- 插件注册运行时 skill：`ctx.skills.register()`（`packages/skill/skill/src/index.ts`，`docs/subsystems/skills.zh.md`）
- sqlite 标准：`packages/storage/storage-sqlite`（`node:sqlite` / `DatabaseSync`）
- 端到端模板：`packages/feedback/message-feedback` ↔ `packages/client/ui-message-feedback`；轻量面板范例 `packages/client/ui-jobs`
