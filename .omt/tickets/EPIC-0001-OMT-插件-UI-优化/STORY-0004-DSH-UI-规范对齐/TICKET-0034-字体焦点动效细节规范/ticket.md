---
id: TICKET-0034
type: ticket
title: 字体/焦点/动效细节规范
status: done
archived: true
priority: 0
parent: STORY-0004
created_at: '2026-08-18T05:00:20.272Z'
updated_at: '2026-08-18T06:37:46.399Z'
---

## 描述

字体、焦点、动效三类细节规范对齐。

## 实现要点

- 字号从 px 迁到 `--dsw-font-*` 族 token（正文/标题/说明文字各归其位）
- 全组件可交互元素补 `:focus-visible` 焦点环（键盘导航 TICKET-0010 的前置）
- 动效复查：扫光/骨架脉冲/高亮闪烁都有 prefers-reduced-motion 回退

## 验收标准

- 无散落 px 字号（或全部有明确理由）
- Tab 键可达元素均有可见焦点态

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

## 完成（TICKET-0034）

**字号 token 化**（`--dsw-font-*-font-size` 族）
- 映射：13→xs-13、12/11.5→xxs-12、11/10.5/10→xxxs-11、12.5→xs-13、14→s-14、15→base-16、20→l-20；覆盖 5 个 module.css + OmtShowRow 内联 style + ToggleButton
- 保留 px（有明确理由，已注释）：`.omt-badge` 9/9.5px——宿主 token 下限 xxxs-11(11px)，badge 字形必须低于它才能装进 13/14px 徽标高

**焦点环**（前置 TICKET-0010）
- `omt-shared.css` 新增 `--omt-focus-ring: var(--dsw-alias-brand-text, …)`
- 全部可交互元素补 `:focus-visible`（outline 2px + offset 1px）：Drawer 5 类（headerButton/reindexButton/archiveButton/filterChip/search）、DocPanel 8 类（statusSelect/closeButton/chip/action/actionPrimary/appendInput/appendButton/titleInput）、ActiveDock 2 类、ReferencedBar/TurnTickets item、ToggleButton
- ToggleButton 内联 style 迁入新建 `ToggleButton.module.css`（内联无法表达 :focus-visible；激活态改 `data-open` 属性驱动）
- 非 tab 可达的可点 div（树行、标题编辑）未加——键盘导航本体归 TICKET-0010

**动效复查**
- 全插件仅两处动画：running 扫光 + 骨架脉冲，均已有 prefers-reduced-motion 回退 ✓；无 transition、无其它闪烁类动效

**验证**：`tsc` ✓ / 102 tests ✓ / build ✓（bundle 含 6 处 focus-visible 规则）

**产物**：`oh-my-ticket-0.2.20.tgz`，已装入 profile（重启 + 刷新生效；Tab 键走查可见焦点环）
