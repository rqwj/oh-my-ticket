//! `omt` CLI binary entry (thin): the verb surface lives in the library.
//!
//! The hidden `exec-shim` verb (design §3.2 / KD-E1) is dispatched HERE, not
//! in the library: the `omt_exec` wiring must stay out of `omt_runtime`'s lib
//! so `omt-daemon` never links it (KD-E9, verified by the Task 8 nm check).
//! The dispatch also has to precede `cli::real_run_code` — the library's
//! `signal::install` masks SIGTERM/SIGINT on every thread and consumes them
//! from a sigwait thread, so the shim's own handler would never fire (F1).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = exec_shim_entry(&args) {
        std::process::exit(code);
    }
    std::process::exit(omt_runtime::cli::real_run_code());
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
            let lease = omt_exec::daemon_link::admin_connector(
                target.runtime_dir.clone(),
                daemon_bin.clone(),
            );
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
