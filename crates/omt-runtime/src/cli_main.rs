//! `omt` CLI binary entry (thin): the verb surface lives in the library.
//!
//! The hidden `exec-shim` verb (design §3.2 / KD-E1) is dispatched HERE, not
//! in the library: the `omt_exec` wiring must stay out of `omt_runtime`'s lib
//! so `omt-daemon` never links it (KD-E9, verified by the Task 8 nm check).
//! The dispatch also has to precede `cli::real_run_code` — the library's
//! `signal::install` masks SIGTERM/SIGINT on every thread and consumes them
//! from a sigwait thread, so the shim's own handler would never fire (F1).
//!
//! 子项目 2（A5 / Q-1）：本地工具族的执行面实现同样只属于本二进制——
//! `mod local_impl;` 不进 lib.rs，omt-daemon 不编译不链接它（KD-E9）。

mod local_impl;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = exec_shim_entry(&args) {
        std::process::exit(code);
    }
    let runtime_dir =
        omt_runtime::paths::resolve(omt_runtime::cli::runtime_dir_arg(&args).as_deref());
    let local = local_impl::LocalImpl::new(runtime_dir);
    std::process::exit(omt_runtime::cli::real_run_code_with(Some(&local)));
}

/// `omt [--runtime-dir <d>] exec-shim <homeId> <agentId> <execId>`：每 exec
/// 一个分离 shim，launcher 拉起。返回 None 表示不是 exec-shim 调用。
fn exec_shim_entry(args: &[String]) -> Option<i32> {
    use omt_runtime::cli::{ExecShimParse, EXIT_USAGE};
    match omt_runtime::cli::parse_exec_shim(args) {
        ExecShimParse::NotExecShim => None,
        ExecShimParse::Usage => {
            eprintln!("usage: omt [--runtime-dir <dir>] exec-shim <homeId> <agentId> <execId>");
            Some(EXIT_USAGE)
        }
        ExecShimParse::Target(target) => {
            let daemon_bin = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("omt-daemon")))
                .filter(|path| path.is_file());
            // F-EXEC-15：Leaser 只连接并校验 principal（admin_rpc_prepared），
            // 不携带授权——临时授权由 shim 的 lease_releaser 以确定性
            // principal `cli:<本进程 pid>` 预先建立，connect+enroll+declare
            // 的全部 daemon 往返都在租约超时的有界等待内部；授权在有界等待
            // 之外持有，成功/失败/超时都会立即撤销（P3-b）。
            let lease_runtime = target.runtime_dir.clone();
            let lease_bin = daemon_bin.clone();
            let lease: omt_exec::shim::Leaser =
                std::sync::Arc::new(move |home_path: &std::path::Path, home_id: &str| {
                    omt_exec::daemon_link::admin_rpc_prepared(
                        &lease_runtime,
                        lease_bin.as_deref(),
                        home_path,
                        home_id,
                        &format!("cli:{}", std::process::id()),
                    )
                });
            let keepalive =
                omt_exec::daemon_link::plain_connector(target.runtime_dir.clone(), daemon_bin);
            Some(omt_exec::shim::run(
                &target.runtime_dir,
                &target.home_id,
                &target.agent_id,
                &target.exec_id,
                lease,
                keepalive,
            ))
        }
    }
}
