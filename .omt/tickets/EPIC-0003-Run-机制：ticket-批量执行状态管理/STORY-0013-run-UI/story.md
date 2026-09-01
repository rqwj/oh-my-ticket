---
id: STORY-0013
type: story
title: run UI
status: done
priority: 2
parent: EPIC-0003
created_at: '2026-08-19T09:09:31.810Z'
updated_at: '2026-08-20T00:14:00.241Z'
---

# Run UI

独立 run 视图 + 加入 run 入口 + 新状态呈现。**树节点上不加批次进度条**。

## 范围

- 树节点与详情面板「加入 run」按钮：收集当前节点+全部下级；目标 run 唯一时
  直接加入，多个活跃 run 时弹窗由用户选择；不做多选勾选
- run 视图：run 列表 + 详情（item 状态、执行者谱系「父会话 ↳ subagent」、
  进度、pause/resume/cancel/retry 操作）
- blocked/stopped/skipped 黄色呈现（徽章 + 状态点）
- awaiting_confirmation 确认入口
- SSE run 事件接入前端 store

详见 Epic 正文决策 4/6/15/16。

<!-- omt:children -->
## 子节点

- [TICKET-0067 树与详情的「加入 run」按钮](TICKET-0067-树与详情的「加入-run」按钮/ticket.md) — done
- [TICKET-0068 run 列表与详情视图](TICKET-0068-run-列表与详情视图/ticket.md) — done
- [TICKET-0069 blocked/stopped/skipped 黄色呈现](TICKET-0069-blockedstoppedskipped-黄色呈现/ticket.md) — done
- [TICKET-0070 awaiting_confirmation 确认入口](TICKET-0070-awaiting_confirmation-确认入口/ticket.md) — done
- [TICKET-0071 SSE run 事件前端接入](TICKET-0071-SSE-run-事件前端接入/ticket.md) — done
<!-- /omt:children -->


## 进度（P2 UI worker，2026-08-19）

全部 5 个子 ticket 落地（0067/0068/0069/0070/0071），详见各 ticket 进度记录。前端新增：store run 类型 + run-view.ts 纯函数 + controller run flows、RunsView/RunPicker/NoticeBar 组件、TicketPanel 区块导航与过滤扩展、DocPanel run 链接与确认标识、locales 中英、omt-shared.css 警示族 token（全部走宿主 --dsw-* token）。测试 303 passed（新增 run-ui 21 + run-components 17）+ tsc 绿 + tsdown 构建绿。浏览器验证不可用，以 jsdom（react-dom）组件测试替代并覆盖主要交互路径。


## 进度记录

- 2026-08-19 P2 完成（commits 4020ed7 / 1a42f0a / 04ce3c6）：三层 hook 完整化
  （0063-0066）+ run RPC/SSE（0071 host）+ 全部 UI（0067-0070 客户端）落地。
  全量 303 测试通过，typecheck 与 tsdown 构建干净。
- 取舍：浏览器验证不可用，以 jsdom + react-dom 组件测试替代；窄视口/暗色为
  代码级检查（全宿主 token，未真实目验）。--omt-danger-bg 用 color-mix 调和
  （宿主 error 族无 tertiary token）。
- 已知边界：pending run 的 item 不接受 report（先 start）；interrupted run 需
  resume 后再加入/打回。
