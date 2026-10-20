//! U8 scaffold entry (KD4/KD5): the desktop app is a PEER surface of the
//! shared omt-daemon, not its private guardian. U9 wires the lifecycle:
//! discover-or-spawn via the sidecar (current_exe().parent()), handshake
//! kind:"desktop", single-entry admin-grant refresh, and the Rust-held
//! connection bridged to the frontend through tauri commands.

mod agents;
mod daemon;
mod executor;
mod harness;
mod rpc_bridge;
mod state;

use state::SharedSession;
use tauri::Manager;

/// review F6：用户在确认框里选过之后，再次进入 ExitRequested 时直接放行。
static QUIT_CONFIRMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 设计 §3.4：桌面退出不再终止执行（KD-D7 已被 KD-E1 修订）；有运行中的
/// exec 时退出前确认，并提供 Agents 面板「停止全部」。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 执行面启动清扫（迁移 / 启动对账 / 临时授权与 MCP 残留）——放在任何
    // command 可被调用之前；对账只改记录，不发信号。
    omt_exec::executor::startup_housekeeping(&daemon::runtime_dir());
    // review R2-4：shim 丢失 / 释放失败的租约走 shim 路径释放（声明 home + 临时
    // 授权），不依赖 desktop 会话此刻是否已打开该 workspace home。后台执行，
    // 不阻塞窗口出现；旧记录仍由 rpc_bridge 的 U10 对账处理。
    {
        let runtime_dir = daemon::runtime_dir();
        let daemon_bin = daemon::sidecar_path("omt-daemon").ok();
        std::thread::spawn(move || {
            let connector = omt_exec::daemon_link::admin_connector(runtime_dir.clone(), daemon_bin);
            let released = omt_exec::executor::reconcile_with_connector(&connector, &runtime_dir);
            if released > 0 {
                eprintln!("启动对账：经 shim 路径释放了 {released} 个 exec 的租约");
            }
        });
    }
    // single-instance 必须最先注册（插件约定）：双开 = 两个发起方对
    // agents/bindings 的跨进程并发写（计划 §3.6.9），第二实例直接退出并聚焦首实例。
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
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SharedSession::new())
        // 自定义应用菜单（review R2-5）：默认 macOS 菜单的 Quit 走 `terminate:`，
        // 直接结束进程，不产生 ExitRequested，下面的退出确认永远弹不出来。
        // 自定义 Quit 菜单项改调 app.exit(0)——经 RequestExit 产生 ExitRequested。
        .menu(|handle| {
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
            let quit = MenuItem::with_id(
                handle,
                "quit",
                "退出 oh-my-ticket",
                true,
                Some("CmdOrCtrl+Q"),
            )?;
            let app_menu = Submenu::with_items(
                handle,
                "oh-my-ticket",
                true,
                &[
                    &PredefinedMenuItem::about(handle, None, None)?,
                    &PredefinedMenuItem::separator(handle)?,
                    &PredefinedMenuItem::hide(handle, None)?,
                    &PredefinedMenuItem::hide_others(handle, None)?,
                    &PredefinedMenuItem::separator(handle)?,
                    &quit,
                ],
            )?;
            // 编辑菜单必须保留：没有它，webview 里的 ⌘C / ⌘V / ⌘A 在 macOS 上失效
            let edit_menu = Submenu::with_items(
                handle,
                "编辑",
                true,
                &[
                    &PredefinedMenuItem::undo(handle, None)?,
                    &PredefinedMenuItem::redo(handle, None)?,
                    &PredefinedMenuItem::separator(handle)?,
                    &PredefinedMenuItem::cut(handle, None)?,
                    &PredefinedMenuItem::copy(handle, None)?,
                    &PredefinedMenuItem::paste(handle, None)?,
                    &PredefinedMenuItem::select_all(handle, None)?,
                ],
            )?;
            let window_menu = Submenu::with_items(
                handle,
                "窗口",
                true,
                &[
                    &PredefinedMenuItem::minimize(handle, None)?,
                    &PredefinedMenuItem::close_window(handle, None)?,
                ],
            )?;
            Menu::with_items(handle, &[&app_menu, &edit_menu, &window_menu])
        })
        .on_menu_event(|app, event| {
            if event.id() == "quit" {
                app.exit(0);
            }
        })
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
            harness::harness_list,
            harness::harness_config_set,
            harness::harness_launch_preview,
            harness::harness_open_login,
            harness::harness_list_models,
            agents::agents_list,
            agents::agents_delete,
            agents::bindings_get,
            agents::bindings_gc,
            executor::exec_start,
            executor::exec_stop,
            executor::exec_transcript_tail,
            executor::exec_kill_orphan,
            executor::exec_stop_all,
            executor::exec_attach,
        ])
        .build(tauri::generate_context!())
        .expect("error while building oh-my-ticket desktop")
        .run(|app, event| {
            // 设计 §3.4：退出不再终止执行（KD-D7 已被 KD-E1 修订）。还有运行中的
            // exec 时先问用户：全部停止后退出，或让它们在后台继续（review F6）
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                use std::sync::atomic::Ordering;
                if QUIT_CONFIRMED.load(Ordering::SeqCst) {
                    return;
                }
                let runtime_dir = daemon::runtime_dir();
                let running = omt_exec::control::active_exec_count(&runtime_dir);
                if running == 0 {
                    return;
                }
                api.prevent_exit();
                let handle = app.clone();
                use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};
                const KEEP: &str = "继续在后台运行";
                const STOP: &str = "全部停止后退出";
                const CANCEL: &str = "取消";
                app.dialog()
                    .message(format!(
                        "还有 {running} 个 agent 在运行。选择「{KEEP}」后，它们会一直执行到完成或超时；\
                         随时可以重新打开本应用查看或停止。"
                    ))
                    .title("退出 oh-my-ticket")
                    // review R2-6：第一个按钮是 Return 默认键，放安全的「继续在后台运行」；
                    // 「取消」= 不退出（误按 ⌘Q 时可以反悔）
                    .buttons(MessageDialogButtons::YesNoCancelCustom(
                        KEEP.into(),
                        STOP.into(),
                        CANCEL.into(),
                    ))
                    .show_with_result(move |result| {
                        let choice = match result {
                            MessageDialogResult::Custom(label) => label,
                            MessageDialogResult::Yes => KEEP.to_string(),
                            MessageDialogResult::No => STOP.to_string(),
                            _ => CANCEL.to_string(),
                        };
                        match choice.as_str() {
                            KEEP => {}
                            STOP => {
                                for home_id in omt_exec::layout::home_ids(&runtime_dir) {
                                    let _ = omt_exec::control::stop_all(&runtime_dir, &home_id);
                                }
                            }
                            _ => return, // 取消 / Esc / 关闭对话框：不退出
                        }
                        QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                        handle.exit(0);
                    });
            }
        });
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
