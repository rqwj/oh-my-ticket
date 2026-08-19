---
id: TICKET-0032
type: ticket
title: 颜色 token 化与样式去重
status: done
archived: true
priority: 0
parent: STORY-0004
created_at: '2026-08-18T04:59:55.131Z'
updated_at: '2026-08-18T06:37:48.218Z'
---

## 描述

颜色全部 token 化，消除字面色与跨文件重复样式。

## 实现要点

- 新建共享样式表定义 `--omt-*` 语义变量：类型 5 色、状态 3 色 + 归档描边、优先级 3 色、警示/错误色——单文件注入（参照 CSS 模块插件的 style 注入）
- 可映射宿主语义 token 的直接用（done → --dsw-alias-state-success-primary、错误 → state-error-*、文字/背景/边框 → label/bg/border 族）
- 四处重复的 badge/dot/status 样式合并为共享类
- priority.ts 内联色值改读变量（或保留常量表但来源同一文件）
- 验收 grep：`src/client` 下无十六进制字面色（共享变量定义文件除外）

## 验收标准

- 颜色全部经 var() 引用
- 重复样式定义合并为一份

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

## 完成（TICKET-0032）

**实现**

- 新建 `src/client/omt-shared.css`：`:root` 上全部 `--omt-*` 语义变量 + 共享全局类（单文件注入）
  - 类型 5 色（插件自有色板）；状态 3 色 + 归档描边；优先级 3 色；danger/danger-hover（reindex 两段确认）、warn（错误图标）、on-accent、toggle 激活底、拖拽提示色
  - 宿主可映射的直接映射：done → `--dsw-alias-state-success-primary`，in_progress/p1 → `--dsw-static-deepseek-500`，p2/warn → `--dsw-alias-state-warn-primary`，p3/danger → `--dsw-alias-state-error-primary`（hover → error-secondary），on-accent → `--dsw-alias-label-primary-inverted`（均带原色值兜底）
  - 共享类：`.omt-badge`(+`--lg`)、`.omt-dot`(+`--lg`)、`.omt-type-*`、`.omt-status-*`（4 处重复定义合并为一份）
- tsdown CSS 插件扩展：普通 `.css` 与 `.module.css` 同一注入通道（无 class map）；`index.ts` 侧效应引入共享表
- 四个 module.css 去重：Drawer/TurnTickets/ReferencedBar/DocPanel 的 badge/dot/type/status 重复块全部删除，组件改用全局类字符串；Drawer 过滤 chip 的覆盖选择器改 `:global(.omt-type-*)`（bundle 中已验证编译正确）
- `priority.ts` 色值改读 `var(--omt-priority-pN)`；ToggleButton 内联 rgba 改读 `var(--omt-toggle-active-bg)`；DocPanel errorIcon/按钮文字/归档虚线、Drawer reindexArmed/drag cue 全部 token 化
- 附带清理：module.css 中 `var(--dsw-*, 字面兜底)` 的 hex/rgba 兜底全部去掉，直接引用宿主 token（ui-theme 在 shell 中必加载）

**验证**

- `tsc` ✓ / 97 tests ✓ / build ✓（priority.spec 断言更新为 var() 引用）
- 验收 grep：`src/client` 下十六进制色 0 残留（omt-shared.css 除外）✓
- 保留项：Drawer 的 box-shadow rgba(0,0,0,0.3)（投影而非调色板色，不在验收 grep 范围）

**产物**：`oh-my-ticket-0.2.18.tgz`，已装入 profile（重启服务 + 刷新页面后生效）

## 后续（0.2.21）：border token 失效回归修复

**回归根因**：本 ticket 删除 var() 兜底时引用的 `--dsw-alias-border-l` 在宿主 ui-theme 中**从未定义**（此前 token 审计 grep `\-\-dsw-alias-[a-z-]*` 把 `--dsw-alias-border-l4` 前缀误配出 `border-l`，导致误判其存在）。16 处边框声明全部失效 → 按钮/chip 失去可点击外观（0.2.21 前用户可见症状）。

**修复**：按角色改引真实存在的 l1–l4 族——结构分隔线 → `border-l1`；输入框（search/select/textarea）→ `border-l2`；可交互元素（filterChip/reindexButton/chip/action/item/filterDivider）→ `border-l3`（比原 0.14 略强，兼作可点击感优化）。

**流程教训**：引用宿主 token 前必须用 `token名:`（带冒号、防前缀误配）验证其真实定义；已用此法全量复审 src/client 所有 var(--dsw-*/--dsh-*) 引用，仅此一个失效。
