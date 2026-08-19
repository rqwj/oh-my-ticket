---
title: 用户管理模块（Demo） - Plan
type: feat
date: 2026-08-17
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
execution: code
product_contract_source: ce-plan-bootstrap
---

# 用户管理模块（Demo） - Plan

## Goal Capsule

- **目标**：在 demo 工作区实现一个轻量「用户管理模块」——管理员可列表/搜索用户、创建、编辑资料与角色、禁用/启用、重置密码；管理操作仅限管理员角色。
- **权威层级**：本文件是决策权威；实现时以 U-IDs 与 R-IDs 为准，不引入规划外行为。
- **停止条件**：R1–R6 全部满足；U1–U3 各自验收通过；模块仅面向管理员。
- **执行画像**：Lightweight 深度；`execution: code`；单会话 demo，非生产交付。
- **尾部所有权**：ce-work 或手动实现均可；demo 不要求部署与运营。

## Product Contract

### Summary

规划一个 demo 级用户管理模块：用户 CRUD + 状态管理（禁用/启用）+ 密码重置 + 管理员授权守卫，落在 demo 工作区（当前为空目录）的 TypeScript/React 结构上，复用 oh-my-ticket 的 SQLite + vitest 惯例。

### Problem Frame

oh-my-ticket（OMT）是 DSH ticket 管理插件，其数据核心与工具层已完成（M0–M3）。本需求是用户体系的配套 demo：既有登录/注册（STORY-0001/0002）只覆盖自助侧，缺少管理端对用户的治理能力。该模块同时作为 OMT 自身"多 skill 并行"（omt + ce-plan）能力的验收用例。

### Requirements

- R1. 用户列表支持关键字搜索与分页。
- R2. 支持创建用户：用户名、邮箱、初始密码、角色。
- R3. 支持编辑用户资料与角色。
- R4. 支持禁用/启用用户；禁用后该用户不可登录。
- R5. 支持管理员重置用户密码。
- R6. 管理操作仅管理员角色可执行。

### Scope Boundaries

- **Deferred to Follow-Up Work**：自助资料页（编辑本人资料/改密）；角色权限矩阵的后台管理界面；审计日志。
- **Outside this product's identity**：登录/注册流程（归属 STORY-0001/0002）；通知系统（EPIC-0002）；多租户与组织管理。

## Planning Contract

### Key Technical Decisions

- KTD1. 存储沿用 oh-my-ticket 的 SQLite 惯例（`node:sqlite` + WAL），用户表在原实体上新增 `status`（active/disabled）与 `role` 字段，而非新建独立表。
- KTD2. 授权采用最小守卫：管理端接口入口校验当前用户 `role === 'admin'`，不引入完整 RBAC 框架。
- KTD3. 重置密码采用管理员生成临时密码（一次性、强制下次登录修改）的简化方式，不做邮件/链接流程。

### Assumptions

- A1. 目标代码库为 demo 工作区（当前为空目录）；实现时在其下新建 `src/` 结构，路径以本文件 U 单元为准。
- A2. 用户实体与登录/注册（STORY-0001/0002）共用同一 `users` 表；本模块只扩展字段与新增接口，不改动登录路径既有行为。
- A3. 不引入新依赖；沿用项目现有 vitest / react / tsdown 工具链。
- A4. "禁用后不可登录"的判定在登录服务侧实现，管理端只维护状态字段。

### Sequencing

U1 → U2 → U3，严格依赖顺序；U1 完成前不开始 U2/U3。

## Implementation Units

### U1. 用户数据模型与存储层

- **Goal**：定义用户实体（含 `status`/`role`），实现分页+搜索、创建、编辑、禁用/启用、重置密码的存储原语。
- **Requirements**：R1–R5
- **Dependencies**：无（登录/注册的用户表已存在）
- **Files**：
  - `src/users/model.ts`（实体与字段定义）
  - `src/users/store.ts`（存储原语）
  - `tests/users/store.spec.ts`
- **Approach**：
  1. 在既有用户实体上扩展 `status: 'active' | 'disabled'` 与 `role: 'user' | 'admin'`。
  2. 实现 `listUsers({ query, page, pageSize })`：用户名/邮箱模糊匹配 + 分页，返回总数。
  3. 实现 `createUser` / `updateUser` / `setUserStatus` / `resetPassword`，用户名与邮箱唯一性约束。
  4. 密码存储沿用现有哈希方式；重置密码生成临时密码并标记 `mustChangePassword`。
- **Patterns to follow**：`src/host/store.ts` 的 SQLite 打开方式（WAL、参数化语句）与 `tests/core.spec.ts` 的测试风格。
- **Test scenarios**：
  - 分页：第 2 页只返回第 2 页条目且总数正确。
  - 搜索：关键字命中用户名或邮箱；空关键字返回全量。
  - 边界：空结果页返回空数组与正确总数；`pageSize` 超上限被截断。
  - 错误：重复用户名/邮箱抛唯一性错误；`setUserStatus` 对不存在 id 抛错。
  - 集成：重置密码后 `mustChangePassword` 为真，且旧密码不可登录（可先在 U1 用存储断言，登录侧联动归 U2 集成场景）。
- **Verification**：`pnpm test` 通过全部 U1 用例；`pnpm typecheck` 无错。

### U2. 用户管理 API（含授权守卫）

- **Goal**：暴露管理端接口：列表/搜索、创建、编辑、禁用/启用、重置密码；接口入口统一校验管理员身份。
- **Requirements**：R1–R6
- **Dependencies**：U1
- **Files**：
  - `src/users/api.ts`（接口路由与请求/响应契约）
  - `tests/users/api.spec.ts`
- **Approach**：
  1. 路由：`GET /api/users`（分页+搜索）、`POST /api/users`、`PATCH /api/users/:id`、`POST /api/users/:id/status`、`POST /api/users/:id/reset-password`。
  2. 守卫：每个路由先校验当前会话用户 `role === 'admin'`，否则返回拒绝。
  3. 请求体用 zod 校验，非法输入返回 400 类错误。
- **Patterns to follow**：oh-my-ticket `src/host/tools.ts` 中工具入参校验与错误返回的写法。
- **Test scenarios**：
  - 权限：非 admin 调用任一接口被拒；admin 放行。
  - 列表：分页/搜索参数正确透传 U1；非法 `page`/`pageSize` 返回 400。
  - 创建：合法请求创建成功返回实体；缺邮箱/角色返回 400；重复用户名返回 409 类冲突。
  - 状态：禁用后再次启用成功；对不存在 id 返回 404。
  - 集成：禁用用户后，登录接口拒绝该用户（联动 STORY-0001 登录服务）。
- **Verification**：`pnpm test` 通过全部 U2 用例；`pnpm typecheck` 无错。

### U3. 用户管理界面

- **Goal**：管理端页面：用户表格（搜索/分页）、创建/编辑弹窗、禁用/启用与重置密码操作。
- **Requirements**：R1–R6
- **Dependencies**：U2
- **Files**：
  - `src/users/UserList.tsx`（表格 + 搜索 + 分页）
  - `src/users/UserForm.tsx`（创建/编辑弹窗）
  - `src/users/UserActions.tsx`（禁用/启用、重置密码）
  - `tests/users/ui.spec.tsx`
- **Approach**：
  1. 表格消费 `GET /api/users`，搜索框防抖触发查询，分页控件驱动翻页。
  2. 弹窗提交创建/编辑；操作按钮调用对应接口并刷新列表。
  3. 非 admin 用户进入页面时展示无权限提示（后端已拒绝，前端兜底）。
- **Patterns to follow**：oh-my-ticket client 侧的 React 组件写法（props 直喂 + 行为断言，见 README 测试策略）。
- **Test scenarios**：
  - 列表：首屏加载展示用户；搜索词变化后表格按结果刷新。
  - 分页：翻页后展示对应页数据。
  - 创建/编辑：提交表单后列表出现/更新新数据；必填缺失时表单校验拦截。
  - 操作：禁用后行内状态与操作按钮变化；重置密码成功提示；接口失败展示错误提示。
  - 权限：非 admin 视角显示无权限态。
- **Verification**：`pnpm test` 通过全部 U3 用例；`pnpm typecheck` 无错。

## Verification Contract

- 单元测试：`pnpm test`（vitest），覆盖 U1–U3 全部测试场景。
- 类型检查：`pnpm typecheck`（tsc --noEmit）无错误。
- 本 demo 无部署环节；接口与 UI 的联动以 U2/U3 集成场景为准。

## Definition of Done

- **全局**：R1–R6 全部满足；三单元验收与验证契约通过；无规划外行为引入。
- **每单元**：该单元测试场景全绿、类型检查通过、无遗留 TODO。
- **清理**：实现过程中废弃的尝试性代码必须删除，不留死代码。
