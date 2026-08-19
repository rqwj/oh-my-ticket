---
id: TICKET-0033
type: ticket
title: 窄视口适配
status: done
archived: true
priority: 0
parent: STORY-0004
created_at: '2026-08-18T05:00:20.255Z'
updated_at: '2026-08-18T06:37:47.310Z'
---

## 描述

窄视口/小屏下抽屉与详情面板布局不破坏。

## 实现要点

- 抽屉宽度上限 `min(当前宽度, 100vw)`；窄屏（如 <640px）下抽屉全宽覆盖或自动收起（参照 AppFrame narrow 断点语义）
- 详情面板头部在 300px 最小宽（DETAILS_MIN）下再验收一遍（v0.2.13 紧凑化只按 360px 验证）
- 拖拽手柄在触屏/窄屏的可用性

## 验收标准

- 320px 宽视口下抽屉/面板可用且不破坏宿主布局

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

## 完成（TICKET-0033）

**宿主语义查证**（ui-layout columns.ts）：`SIDEBAR_AUTO_COLLAPSE=1024`、`CENTER_MIN=640`、`DETAILS_MIN=300`；视口 <~996px 时宿主自动关闭 details 列，故详情面板真实最小宽即 300px。

**实现**

- 抽屉宽度：`effectiveDrawerWidth(width, viewport)` 纯函数（Drawer.tsx 导出）——宽屏 `min(存储宽度, 100vw)`；<640px（断点取宿主 CENTER_MIN）全宽覆盖；`isNarrowViewport` 控制拖拽手柄在窄屏退休（无可调宽度，也减少触屏争用）；视口经 resize 监听跟踪
- 拖拽手柄触屏可用性：`.dragHandle` 加 `touch-action: none`（触摸拖拽走 pointer capture 而非滚动手势）
- 详情面板 300px 复核：头部（id + 状态/优先级下拉 + 关闭）按最长 option 估算中英均 ≤234px < 272px 内容宽，不换行不破；`.actions` 四个按钮在英文下 ~300px 会溢出，加 `flex-wrap: wrap` 修复
- 新增 `tests/drawer-width.spec.ts`：宽屏偏好 / min(width,100vw) 封顶 / 窄屏全宽 / 断点边界 / 手柄退休判定，5 用例

**验证**

- `tsc` ✓ / 102 tests（+5）✓ / build ✓
- 320px 视口：抽屉全宽覆盖、可关闭、过滤区自换行；面板由宿主在 <996px 时自动关闭，宿主布局不被破坏

**产物**：`oh-my-ticket-0.2.19.tgz`，已装入 profile（重启 + 刷新生效；窄屏验证可用浏览器设备模拟 320px）
