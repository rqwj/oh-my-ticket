---
id: TICKET-0069
type: ticket
title: blocked/stopped/skipped 黄色呈现
status: done
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.383Z'
updated_at: '2026-08-20T00:14:00.284Z'
---

## 任务

blocked / skipped 两状态的黄色呈现：

- 状态点、徽章、过滤 chip 增加两状态，黄色系（对齐宿主语义 token，
  参考 STORY-0004 的 token 纪律；警示色系如 --dsw-alias-state-warning-*）
- 过滤器增加两状态选项
- 与 done（绿）/ in_progress（蓝）/ archived（空心）视觉层级协调
- （stopped 已移出枚举扩展，见 TICKET-0060）

## 验收

- 无字面色，全部走宿主 token
- 两状态在树/详情/过滤器一致呈现
- 窄视口与暗色主题检查

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->


## 进度（P2 UI worker，2026-08-19）

- 状态点 .omt-status-blocked/.omt-status-skipped 走宿主警示族 token：--dsw-alias-state-warn-primary / --dsw-alias-state-warn-secondary（经 --omt-* 桥，fallback 为 token 化前字面色）。无新增字面色；已 grep 宿主 checkout 验证 token 名存在（warn-* 族，非 warning-*）。
- 过滤器新增 blocked/skipped 两个 chip（沿用 dot+label 模式）；DocPanel 状态下拉新增 ⚪🔵🟢 之外的 🟡 受阻 / ⏭ 已跳过选项。
- run item blocked/skipped 同样映射警示色（.omt-itemstate-*），树/详情/过滤器一致。
- 窄视口：chips/行 flex-wrap；暗色主题：全部走宿主 token 自动适配（design-platform.css 双主题定义）。
- 组件测试断言 chips 存在与详情状态文本呈现。

