//! U8 scaffold entry (KD4/KD5): the desktop app is a PEER surface of the
//! shared omt-daemon, not its private guardian. U9 wires the lifecycle:
//! discover-or-spawn via the sidecar (current_exe().parent()), handshake
//! kind:"desktop", single-entry admin-grant refresh, and the Rust-held
//! connection bridged to the frontend through tauri commands.

mod daemon;
mod rpc_bridge;
mod state;

use state::SharedSession;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // single-instance 必须最先注册（插件约定）：双开 = 两个 ExecutionManager
    // 对 agents/bindings 的跨进程并发写（计划 §3.6.9），第二实例直接退出并聚焦首实例。
    // 注意这是 best-effort 守卫（如 socket 被同机其他用户占用时插件会静默放行），
    // 跨进程写安全的正确性守卫是 U4a 的文件锁（计划 §3.6.6）——不要省掉文件锁。
    let single_instance = tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.set_focus();
        }
    });
    // F-U15-2：single-instance socket 仅由 identifier 派生，dev 实例会与打包版互判为
    // 第二实例。debug 构建可用 OMT_ALLOW_MULTI_INSTANCE=1 跳过守卫（多 worktree 并行
    // 开发同理）；release 构建不受该 env 影响。
    let allow_multi =
        parse_allow_multi_instance(std::env::var("OMT_ALLOW_MULTI_INSTANCE").ok().as_deref());
    let enable_single_instance = !(cfg!(debug_assertions) && allow_multi);
    let builder = tauri::Builder::default();
    let builder = if enable_single_instance {
        builder.plugin(single_instance)
    } else {
        builder
    };
    builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SharedSession::new())
        .invoke_handler(tauri::generate_handler![
            rpc_bridge::omt_call,
            rpc_bridge::daemon_status,
            rpc_bridge::daemon_ensure,
            rpc_bridge::daemon_homes,
            rpc_bridge::daemon_reconnect,
            rpc_bridge::events_subscribe,
            rpc_bridge::harness_detect,
            rpc_bridge::harness_validate_checkout,
            rpc_bridge::dsh_plugin_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running oh-my-ticket desktop")
}

/// F-U15-4：按仓库 env 约定（docs/runtime/config.md「空即未设置」）解析布尔开关——
/// 未设置/空串/纯空白/`0`/`false`（忽略大小写）均为关，其余非空值为开。
fn parse_allow_multi_instance(v: Option<&str>) -> bool {
    v.map(str::trim)
        .filter(|v| {
            !v.is_empty() && !v.eq_ignore_ascii_case("0") && !v.eq_ignore_ascii_case("false")
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::parse_allow_multi_instance;

    #[test]
    fn allow_multi_instance_env_treats_empty_as_unset() {
        assert!(!parse_allow_multi_instance(None));
        assert!(!parse_allow_multi_instance(Some("")));
        assert!(!parse_allow_multi_instance(Some("   ")));
        assert!(!parse_allow_multi_instance(Some("0")));
        assert!(!parse_allow_multi_instance(Some("false")));
        assert!(!parse_allow_multi_instance(Some("False")));
        assert!(parse_allow_multi_instance(Some("1")));
        assert!(parse_allow_multi_instance(Some("true")));
        assert!(parse_allow_multi_instance(Some(" yes ")));
    }
}
