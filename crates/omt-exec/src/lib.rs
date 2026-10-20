//! omt-exec：执行面（设计 §3.1–§3.3）。agent 存储、MCP 注入、执行器内核；
//! 后续 Task 加入 launcher / shim / 控制与对账。daemon 代码路径不引用本 crate。

pub mod agents;
pub mod executor;
pub mod mcp_injection;
