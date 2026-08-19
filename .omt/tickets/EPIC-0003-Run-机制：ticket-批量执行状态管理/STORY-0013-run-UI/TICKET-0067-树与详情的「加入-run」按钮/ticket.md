---
id: TICKET-0067
type: ticket
title: 树与详情的「加入 run」按钮
status: open
priority: 2
parent: STORY-0013
created_at: '2026-08-19T09:12:00.351Z'
updated_at: '2026-08-19T13:27:14.619Z'
---

## 任务

「加入 run」入口（不做多选勾选，按钮式明确操作）：

- 树节点操作区与详情面板各加一个「加入 run」按钮
- 点击收集当前节点 + 全部下级 ticket（展开为 id 列表）；
  **跳过已 done/archived 的下级**并在结果提示中报告跳过数量；
  **in_progress 节点加入时 item 直接置 running 并从 RunningRegistry 快照
  当前 executor_session_id**（不产生新跃迁，必须显式置位）
- **「活跃 run」= 非终态 run（pending/running/paused）**；终态 run 不出现在
  选择弹窗且不可加入
- 当前 workspace 只有一个活跃 run → 直接加入；多个 → 弹窗让用户选择目标 run
  （行展示 title（缺省回退 id）+ 进度 + 创建时间）
- 没有活跃 run → 引导创建：**一键默认配置直建**（config 全走默认值），
  run config 在 run 详情页只读展示；高级配置经 omt_run_create（模型侧）
- **加入既有 run 时执行与 omt_run_create 相同的同 home 校验**，不通过则报错
  并引导另建 run
- 已在该 run 中的节点去重跳过并提示

## 验收

- 下级收集含 SubStory 嵌套；done/archived 跳过并报告数量
- in_progress 加入即 running + executor 快照
- 多 run 选择弹窗交互（仅非终态 run 可选，title 展示）
- 默认配置直建路径
- 跨 home 加入被拒绝并提示
- 浏览器测试覆盖

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->
