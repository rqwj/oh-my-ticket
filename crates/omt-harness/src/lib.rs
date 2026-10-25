//! omt-harness：执行面设计 §3.1 的纯库半——REGISTRY / 探测 / 启动参数 /
//! 流 adapters / 评测分析器 / 本地 JSON 状态文件的原子读写。不管进程生命
//! 周期（那是 omt-exec）。

pub mod adapters;
pub mod atomic_json;
pub mod bench;
pub mod harness;
pub mod sanitize;

/// F-BENCH-5：探针 / capture / 分析器的最小 pub 面——registry 驱动。
pub use harness::{descriptor_of, probe_version, which_binary, HarnessDescriptor, REGISTRY};
