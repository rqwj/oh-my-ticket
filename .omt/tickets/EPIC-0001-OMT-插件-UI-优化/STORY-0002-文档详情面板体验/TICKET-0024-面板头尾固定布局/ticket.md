---
id: TICKET-0024
type: ticket
title: 面板头尾固定布局
status: done
archived: true
priority: 0
parent: STORY-0002
created_at: '2026-08-18T02:55:12.502Z'
updated_at: '2026-08-18T04:52:43.354Z'
---

## 描述

文档详情面板目前整体滚动：顶部信息栏（id/状态/操作行）和底部追加进度区随正文一起滚走。

## 实现要点

- 三段式布局：头部信息区固定（flex none）、正文区独立滚动（flex 1 + overflow auto）、底部追加区固定（flex none）
- 面板容器改为 overflow hidden

## 验收标准

- 滚动正文时，顶部信息栏与底部追加区保持固定
- 短内容时布局不出现异常空隙

## 进度记录

<!-- omt:children -->
## 子节点

（暂无子节点）
<!-- /omt:children -->

- 2026-08-18 实现完成（v0.2.1）：面板改三段式布局——头部信息区（id/状态/关系/操作行）固定、正文独立滚动区（flex:1 + overflow auto + min-height:0）、底部追加进度区固定。79/79 单测通过。

- 2026-08-18 头部紧凑化（v0.2.13）：details 列默认宽 360px（宿主常量 DETAILS_DEFAULT）下头部内容溢出换行——已归档徽标移至标题行；头部强制单行（flex-nowrap）；id 省略号收缩（悬停显全）；两个下拉与关闭按钮 flex:none + 更小字号。93/93 单测通过。
