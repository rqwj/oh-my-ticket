# Oh-My-Ticket (OMT)

![version](https://img.shields.io/badge/version-0.2.24-blue)
![tests](https://img.shields.io/badge/tests-115%20passing-brightgreen)
![platform](https://img.shields.io/badge/platform-DeepSeek%20Harness-purple)

**DSH 的 ticket 管理插件**：`Epic → Story → [SubStory] → Ticket → [SubTicket]` 五级任务体系，
SQLite 存元数据与层级关系，Markdown 存正文。ticket 随项目走（`.omt/` 目录可直接进 git），
模型通过工具创建/推进，人通过三种可切换的 UI 展现方式浏览与管理。
插件启用后，完整 OMT 操作规范默认写入系统提示，不必先 load `omt` skill。
设置页可追加约定，并从已装 skill 勾选拆票 skill；提到 ticket / 拆任务 / 节点 id 时再 load。

![image-20260819114848240](attachments/README/image-20260819114848240.png)

## ✨ 功能特性

### 三种展现方式，一键切换（STORY-0006）

同一份 ticket 树，三种外壳共享同一套过滤/排序/搜索交互：

- **左侧抽屉**：`shell.overlay` 浮层，顶部 OMT 按钮开合，宽度可拖拽记忆
- **浮窗**：自由拖拽移动、右下角缩放，位置尺寸持久化；浮窗激活时 OMT Tab 自动隐藏并回退 Chat
- **OMT Tab**：注册进会话视图环，与 Chat ｜ Trajectory 并列，可"弹出为浮窗"

### 完整的树交互

- 类型徽章（E/S/SS/T/ST）、状态点（归档空心化）、优先级信号条（P1–P3 渐强着色）
- 搜索、类型/状态/优先级多选过滤、优先级排序、编号显示开关
- 归档独立维度（与生命周期状态正交，归档只读）、折叠状态跨会话记忆
- 窄视口适配（<640px 抽屉全宽、拖拽把手退役）、键盘焦点环规范
- 变更即时推送：自有 SSE 通道（`/omt/events`），模型改完 UI 立刻刷新

### 文档详情面板

- 选中节点即在右侧 details 列打开完整文档（动态遮蔽，关闭自动恢复原工具详情）
- 标题/状态/优先级/归档操作、追加进度记录、相对时间展示
- "执行"按钮：把 ticket 引用进输入框提交执行，执行中状态展示并锁定面板操作

### 模型与 UI 双向联动

- `@` 输入触发器：引用 ticket 进对话（候选排序 + 状态着色）
- 输入框上方引用条、激活 ticket 状态条
- 每轮对话末尾自动展示"相关 ticket"列表（来源：`@` 引用 ∪ 工具调用 ∪ UI 操作）

### Workspace 归属

- 工作区根目录存在 `.omt/` 时 ticket 随项目走（可进 git），否则回退全局 home
  （插件 config > `OMT_HOME` > `~/.omt`）
- 创建 Epic 时弹窗选择归属；id 跨 home 全局唯一
- 树跟随当前会话的工作区自动切换

## 🤖 模型工具

| 工具 | 用途 |
|---|---|
| `omt_create` | 创建节点（Epic/Story/SubStory/Ticket/SubTicket），层级合法性校验 |
| `omt_list` | 列出节点（类型/状态过滤，关键词搜索） |
| `omt_show` | 节点详情（元信息、正文、父子清单），toolview 渲染为 Markdown 文档 |
| `omt_update` | 标题/状态/优先级/归档，替换正文或追加进度记录 |
| `omt_move` | 移动节点（连同子树） |
| `omt_reindex` | 磁盘 markdown 手工修改后重建 SQLite 索引 |

另附带内嵌 `omt` skill：向模型教授 ticket 体系的操作规范与状态流转约定。

## 📦 安装

```sh
# 本仓库构建并打包（首次需先链接 DSH checkout，见「开发」一节）：
pnpm install && pnpm build && npm pack    # → oh-my-ticket-0.2.24.tgz

# 安装进目标 DSH profile（profile 的 dsh.profile.bundles 依次为）：
#   @deepseek-ai/dsh-base, @deepseek-ai/dsh-web-app, oh-my-ticket
pnpm dsh plugin --profile <profile> add /path/to/oh-my-ticket-0.2.24.tgz
```

在工作区根目录 `mkdir .omt` 即可让该项目拥有独立的 ticket 库（随项目进 git）。

## 🛠 开发

### 首次设置：链接本地 DSH checkout

`@deepseek-ai/*` 依赖未发布到 npm，`package.json` 通过 `link:./.dsh-checkout/...`
引用本机的 deepseek-harness 源码。克隆仓库后，先运行 setup 脚本创建符号链接：

```sh
# 方式一：参数传入 DSH checkout 路径
pnpm run setup /path/to/deepseek-harness

# 方式二：环境变量
DSH_CHECKOUT=/path/to/deepseek-harness pnpm run setup

# 然后正常安装依赖
pnpm install
```

脚本会校验目标目录确实是 deepseek-harness checkout（检查 `vendor/cordis`、
`packages/*` 等子目录是否存在），然后在仓库根目录创建 `.dsh-checkout`
符号链接。该链接已 gitignore，只存在于本机，不含任何机器特定路径。

### 日常开发

```sh
pnpm build        # lib/index.js（host 半）+ lib/client.js（浏览器半）
pnpm watch        # 增量重建
pnpm typecheck    # tsc --noEmit
pnpm test         # vitest（115 例）
```

### 仓库结构

```
├── package.json          # dsh.bundle + dsh.client 双 manifest
├── cordis.patch.yml      # 组合层：insert omt 行
├── tsdown.config.ts      # host 半 + client bundle 构建
├── src/
│   ├── index.ts          # host 插件入口（home 解析 + core 装配 + 工具注册）
│   ├── host/             # 数据核心：core/store/markdown/files/pool/rpc/events/tools/skill
│   └── client/
│       ├── index.ts      # 浏览器半入口（slot 注册装配）
│       ├── controller.ts # snapshot stores 与全部异步流
│       └── components/   # TicketPanel（共享树面板）+ Drawer/FloatWindow/TicketTab 三壳
│                         # + DocPanel/ToggleButton/ActiveDock/ReferencedBar/TurnTickets 等
├── tests/                # vitest 单测（115 例）
└── .omt/tickets/         # 本项目的 ticket 库（SQLite 索引已 gitignore，可 omt_reindex 重建）
```

## 📚 设计文档

- 可行性分析：[`FEASIBILITY.md`](FEASIBILITY.md)
- 代码证据调研：[`RESEARCH-dsh-ticket-plugin.md`](RESEARCH-dsh-ticket-plugin.md)
- 执行计划：[`EXECUTION-PLAN.md`](EXECUTION-PLAN.md)
- 开发笔记：[`docs/dsh-plugin-dev-notes.md`](docs/dsh-plugin-dev-notes.md)

## License

[MIT](LICENSE)
