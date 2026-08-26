# 迁移指南（migrations）

## 从 pre-daemon 时代（ts-bridge 直接读写 home）

1. `omt doctor <home>` 看 cohorts：`tsBridgeMarkers`（live/stale）、
   orphan recovery 目录、schema 版本。
2. 旧桥标记在场 → 深度探针让位并附 takeover 指引；
   `omt takeover <home>` 执行静默接管（快照 → 独占迁移 → 代际栅栏 →
   持久 legacy fence）。
3. 偏好文件 `<home>/ui-filters.json` 首次连接自动导入 daemon 存储
   （`dsh:ui` 键），原文件改名 `.imported`。

## 从旧 daemon（无 homeDeclare）

- DSH/桌面打开未注册工作区 → 报「daemon 版本过旧不支持 home/declare，
  请升级」；升级后自动走 declare→重握手链路，无需配置。

## bag 键迁移（U4 自动）

- 裸 `'ui'` filters：首次读自动写穿透到 `dsh:ui`；裸键留作孤儿。
- per-session recent：孤儿化；共享键 `'recent'` 即刻跨面生效。

## 凭据自愈（U7）

CLI 动词路径收到带 requiresRehandshake 提示的 home 范围拒绝 →
删除 `cli-credential.json` 并重新 enroll 恰好一次；用户无感。
