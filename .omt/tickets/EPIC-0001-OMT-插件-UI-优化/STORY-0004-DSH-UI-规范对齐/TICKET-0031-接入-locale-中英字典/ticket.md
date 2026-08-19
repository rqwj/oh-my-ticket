---
id: TICKET-0031
type: ticket
title: 接入 locale 中英字典
status: done
archived: true
priority: 0
parent: STORY-0004
created_at: '2026-08-18T04:59:55.114Z'
updated_at: '2026-08-18T06:37:49.274Z'
---

## 描述

接入 DSH locale 系统：全部内联中文文案迁移到 zh/en 字典。

## 实现要点

- 建立 `locales.ts`（NS='omt'），逐组件收敛文案键：`ctx.locale.register(NS, { zh, en })`
- 每个 slot 注册加 `locale: NS`；组件经 `t()`（PropsLocale）取用
- `declare module '@deepseek-ai/dsh-client-ui-slots' { interface LocaleNamespaceMap { 'omt': ... } }`
- 覆盖组件：Drawer（含 ReindexButton/过滤 chips/开关）/DocPanel/ActiveDock/ReferencedBar/TurnTickets/ToggleButton
- 动态文案（带 id/标题的）用插值

## 验收标准

- 英文界面下插件全部文案为英文
- 无内联中文残留（grep 可验证）

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

## 完成（TICKET-0031）

**实现**

- 新建 `src/client/locales.ts`：NS='omt'，zh/en 全量字典（约 80 键，`satisfies` + `Record<OmtKey,string>` 双向校验键平衡）、`OmtKey`/`Translate` 类型、`declare module '@deepseek-ai/dsh-client-ui-slots'` 合并（`externals.d.ts` 补空 `LocaleNamespaceMap` 使树外 typecheck 通过）
- `index.ts`：`inject` 增加 `'locale'`，apply 时 `ctx.locale.register(NS, { zh, en })`；全部 8 个 slot 注册加 `locale: NS`；`package.json` dsh.client.inject 增加 `@deepseek-ai/dsh-client-locale`
- 组件全部经 `t()` 取用：Drawer（含 ReindexButton/DragHandle/过滤 chips/归档与编号开关/排序）、DocPanel（状态下拉/优先级下拉/时间/执行中条/父子 chips/操作按钮/追加区，动态文案全部插值）、ActiveDock、ReferencedBar、TurnTickets、ToggleButton、OmtShowRow、PriorityIcon
- 共享 helper 改为 t 驱动：`priority.ts`（labelKey + `priorityLabel`/`priorityOptionLabel(t,…)`）、`relative-time.ts`（`formatRelative(t,…)`，绝对日期经 `time.localeTag` 键跟随界面语言）
- 超范围补一刀：`trigger/source.ts` 序列化块（父节点/子节点/错误消息）经 `ctx.locale.bind(NS)` 本地化——它会进入用户发出的消息，属于可见文案
- `controller.ts` 两处中文注释翻成英文，grep 全净

**验证**

- `tsc --noEmit` ✓；`vitest` 16 文件 97 用例全过（relative-time/priority/trigger 三个 spec 改用 zh 字典驱动的测试 t）✓；`pnpm build` ✓
- grep `[\p{Han}]`：`src/client` 中文仅存于 `locales.ts` 的 zh 字典本体，无内联残留 ✓
- 英文界面实机验证需安装 0.2.17 后切换语言（字典链路：`register` → 注册即 bump revision → 已挂载 outlet 自动刷新）

**产物**：`oh-my-ticket-0.2.17.tgz`
