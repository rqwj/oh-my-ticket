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
    tauri::Builder::default()
        // single-instance 必须最先注册（插件约定）：双开 = 两个 ExecutionManager
        // 对 agents/bindings 的跨进程并发写（计划 §3.6.9），第二实例直接退出并聚焦首实例。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }))
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
