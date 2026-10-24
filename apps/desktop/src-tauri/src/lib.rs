//! U8 scaffold entry (KD4/KD5): the desktop app is a PEER surface of the
//! shared omt-daemon, not its private guardian. U9 wires the lifecycle:
//! discover-or-spawn via the sidecar (current_exe().parent()), handshake
//! kind:"desktop", single-entry admin-grant refresh, and the Rust-held
//! connection bridged to the frontend through tauri commands.

mod agents;
mod daemon;
mod executor;
mod harness;
mod quit_dialog;
mod rpc_bridge;
mod state;

use quit_dialog::{QuitChoice, DIALOG_KEEP};
use state::SharedSession;
use std::path::Path;
use std::sync::atomic::Ordering;
use tauri::Manager;

/// review F6：用户在确认框里选过之后，再次进入 ExitRequested 时直接放行。
/// §51 F-SP2-4：STOP 路径的置位挪到 `run_quit_stop_worker` 的 `exit()` 前
/// 一刻——stop 进行期间该位为 false，再按 ⌘Q 由 DIALOG_OPEN 挡住，不会
/// 中途放行退出（剩余 agent 的分离 shim 会漏掉停止信号）。
static QUIT_CONFIRMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// F-EXEC-6 残留：确认框重入守卫。重复 ⌘Q / 关窗会叠出多个模态框，且旧框
/// 的选择会作用于弹框时刻的过时状态快照——已有确认框在场时，后来的退出/
/// 关窗请求只 prevent 不再叠框。
/// P3-a（§43）：清零只发生在**回到正常运行**的分支（关窗框的 KEEP 隐藏后、
/// 两框的 Cancel）；会退出的分支（STOP，以及退出路径上选择 KEEP/确认退
/// 出）**保持置位**直到进程退出——否则 stop_all_execs 的窗口里再按 ⌘Q 会
/// 叠出第二个（退出后无法应答的）确认框。
/// 无单测缝（tauri 胶水）——手动验收：连按 ⌘Q 只弹一个框；框 A 在场时再
/// ⌘Q 不出现第二个框；关掉 A 后再 ⌘Q 能再弹；选「全部停止后退出」后
/// 立即再按 ⌘Q 不叠框；Esc = 取消（P3-ACC-6）。
static DIALOG_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// F-EXEC-6 / review F6：退出或关窗前的「全部停止」——逐 home 停止，单个
/// 失败不吞掉其余（F-EXEC-3 最小诚实版：错误如实记录，至少留下可诊断的
/// 日志；退出前再弹一次提示属设计选择，见跟进备注）。
fn stop_all_execs(runtime_dir: &Path) {
    for home_id in omt_exec::layout::home_ids(runtime_dir) {
        match omt_exec::control::stop_all(runtime_dir, &home_id) {
            Ok(outcome) if outcome.errors.is_empty() => {}
            Ok(outcome) => {
                eprintln!(
                    "退出停止（home {home_id}）部分失败：{}",
                    outcome.errors.join("；")
                )
            }
            Err(err) => eprintln!("退出停止（home {home_id}）失败：{err}"),
        }
    }
}

/// §49 quit-stop-off-main：Stop 分支的「停止全部 → 退出」序列放到工作线程。
/// 确认框的 `on_choice` 在 GCD 主队列里跑（quit_dialog.rs），而
/// stop_all_execs（逐 agent flock + 对账 + 停止信号）可能耗时数秒——在主
/// 队列里同步跑会阻塞主 run loop（锁竞争下转圈）。退出动作以闭包注入
/// （生产 = `AppHandle::exit(0)`；测试注入记线程 id 的探针，证明序列不在
/// 调用线程上执行）。真正的序列体在 [`run_quit_stop_worker`]（stop 也注入，
/// 测确认位时序与 panic 兜底）。
fn stop_and_exit_off_main(runtime_dir: &Path, exit: impl FnOnce() + Send + 'static) {
    let runtime_dir = runtime_dir.to_path_buf();
    std::thread::spawn(move || run_quit_stop_worker(&runtime_dir, stop_all_execs, exit));
}

/// §51 F-SP2-4：停止+退出序列的可测内芯。
/// - `QUIT_CONFIRMED` 只在 `exit()` 前一刻置位——stop 进行期间再按 ⌘Q 不得
///   中途放行（此前在 stop 开始前置位，第二次退出请求直接穿透，剩余 agent
///   的分离 shim 收不到停止信号继续运行）。stop 期间的重入由 `DIALOG_OPEN`
///   挡住（全程置位：prevent 且不叠框）。
/// - panic 兜底：`stop` panic 时也必须置位并退出——否则 `DIALOG_OPEN` 永远
///   卡在 true，窗口再也关不掉。
fn run_quit_stop_worker(runtime_dir: &Path, stop: impl FnOnce(&Path), exit: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stop(runtime_dir)))
    {
        eprintln!("退出停止 panic（{panic:?}）——跳过剩余停止，强制退出");
    }
    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
    exit();
}

/// §51 F-SP2-4「停止中…」提示：STOP 的确认框关闭后、工作线程完成前，窗口
/// 层给一个可见信号（标题后缀）。进程随后退出，无需恢复标题；panic 兜底
/// 路径同样以退出收场。tauri 胶水无单测缝——手动验收：选「全部停止后退出」
/// 后标题短暂显示「停止中…」，期间再按 ⌘Q 不弹框、不退出。
fn mark_stopping(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_title("oh-my-ticket — 停止中…");
    }
}

/// F-EXEC-6：主窗口恢复显示。关窗确认选「继续在后台运行」只隐藏不销毁窗口
/// （CloseRequested 已 prevent），macOS dock 图标点击（RunEvent::Reopen /
/// applicationShouldHandleReopen）或第二实例启动时由此恢复。
fn restore_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 设计 §3.4：桌面退出不再终止执行（KD-D7 已被 KD-E1 修订）；有运行中的
/// exec 时退出前确认，并提供 Agents 面板「停止全部」。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // single-instance 必须最先注册（插件约定）：双开 = 两个发起方对
    // agents/bindings 的跨进程并发写（计划 §3.6.9），第二实例直接退出并聚焦首实例。
    // 注意这是 best-effort 守卫（如 socket 被同机其他用户占用时插件会静默放行），
    // 跨进程写安全的正确性守卫是 U4a 的文件锁（计划 §3.6.6）——不要省掉文件锁。
    let single_instance = tauri_plugin_single_instance::init(|app, _args, _cwd| {
        restore_main_window(app);
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
        // §39 housekeeping-before-single-instance：执行面启动清扫（迁移 /
        // 启动对账 / 临时授权与 MCP 残留 / 删除残留）与 shim 路径的租约释放
        // 放在 app setup 里——single-instance 插件在 plugin setup 阶段就让
        // 第二实例 exit(0)，plugin setup 先于 app setup，于是只有存活的那个
        // 实例跑它们。仍早于任何 command 被调用；对账只改记录，不发信号。
        .setup(|_app| {
            omt_exec::executor::startup_housekeeping(&daemon::runtime_dir());
            executor::release_reconcile_targets_in_background("启动对账");
            Ok(())
        })
        // F-EXEC-6：主窗口 CloseRequested 把关——有运行中的 exec 时不许窗口
        // 直接销毁（销毁后才发出 ExitRequested{code:None}，届时 Cancel 只能
        // prevent_exit，留下无窗口且无法恢复的应用）。这里先 prevent_close
        // 再弹同款三按钮：KEEP = 隐藏窗口、应用后台继续（dock 图标仍在，
        // Reopen 恢复）；STOP = 全部停止后退出；CANCEL = 窗口保持原样。
        // 没有运行中的 exec 时放行关闭（随后的 ExitRequested{code:None}
        // 直通退出）。
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let runtime_dir = daemon::runtime_dir();
            let running = omt_exec::control::active_exec_count(&runtime_dir);
            if running == 0 {
                return;
            }
            api.prevent_close();
            // F-EXEC-6 残留：已有确认框在场（ExitRequested 或另一关窗已弹出）
            // 时不再叠框——旧框选择会作用于过时状态；prevent 已做，直接返回。
            if DIALOG_OPEN.swap(true, Ordering::SeqCst) {
                return;
            }
            let handle = window.app_handle().clone();
            let dialog_app = handle.clone();
            quit_dialog::confirm_quit_dialog(
                &dialog_app,
                "关闭 oh-my-ticket".to_string(),
                format!(
                    "还有 {running} 个 agent 在运行。选择「{DIALOG_KEEP}」后窗口会隐藏，\
                     它们继续执行到完成或超时；点击 dock 图标可重新打开本应用查看或停止。"
                ),
                move |choice| match choice {
                    QuitChoice::Keep => {
                        // 后台继续：隐藏保活的窗口，应用不退出——回到正常运行，
                        // 清 DIALOG_OPEN 放行后续弹框
                        if let Some(win) = handle.get_webview_window("main") {
                            let _ = win.hide();
                        }
                        DIALOG_OPEN.store(false, Ordering::SeqCst);
                    }
                    QuitChoice::Stop => {
                        // 退出路径（P3-a）：DIALOG_OPEN 保持置位直到进程退出。
                        // §49 quit-stop-off-main：停止+退出序列放到工作线程——
                        // on_choice 在 GCD 主队列里跑，stop_all_execs（逐 agent
                        // flock + 对账）不得阻塞主 run loop。§51 F-SP2-4：
                        // QUIT_CONFIRMED 不在这里置位——改由 worker 在 exit 前
                        // 一刻置位，stop 期间再按 ⌘Q 由 DIALOG_OPEN 挡住（prevent
                        // 且不叠框），不得中途放行退出。
                        mark_stopping(&handle);
                        let exit_handle = handle.clone();
                        stop_and_exit_off_main(&runtime_dir, move || exit_handle.exit(0));
                    }
                    // 取消 / Esc：窗口保持原样（close 已被 prevent）——回到正常
                    // 运行，清 DIALOG_OPEN
                    QuitChoice::Cancel => DIALOG_OPEN.store(false, Ordering::SeqCst),
                },
            );
        })
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
            agents::agents_archive_legacy,
            agents::bindings_get,
            agents::bindings_gc,
            executor::exec_start,
            executor::exec_stop,
            executor::exec_transcript_tail,
            executor::exec_kill_orphan,
            executor::exec_stop_all,
            executor::exec_attach,
            executor::exec_policy_get,
            executor::exec_policy_set,
        ])
        .build(tauri::generate_context!())
        .expect("error while building oh-my-ticket desktop")
        .run(|app, event| {
            // 设计 §3.4：退出不再终止执行（KD-D7 已被 KD-E1 修订）。还有运行中的
            // exec 时先问用户：全部停止后退出，或让它们在后台继续（review F6）
            match event {
                tauri::RunEvent::ExitRequested { code, api, .. } => {
                    if QUIT_CONFIRMED.load(Ordering::SeqCst) {
                        return;
                    }
                    // F-EXEC-6：code None = 最后一个窗口关闭后的自然退出请求。
                    // 窗口关闭已在 CloseRequested 处把关（有运行中 exec 时
                    // prevent 并弹确认），能走到这里说明没有需要确认的执行
                    // ——直通退出。修复前 handler 无视 code，对这种窗口销毁
                    // 后才到达的事件也弹确认，Cancel 后就剩下无窗口、无法
                    // 恢复的应用进程。显式退出（code Some：菜单 Quit /
                    // app.exit / STOP 路径）仍走三按钮确认。
                    if code.is_none() {
                        return;
                    }
                    let runtime_dir = daemon::runtime_dir();
                    let running = omt_exec::control::active_exec_count(&runtime_dir);
                    if running == 0 {
                        return;
                    }
                    api.prevent_exit();
                    // F-EXEC-6 残留：已有确认框在场（关窗或更早的 ⌘Q 已弹出）时不
                    // 再叠框——旧框选择会作用于过时状态；prevent 已做，直接返回。
                    if DIALOG_OPEN.swap(true, Ordering::SeqCst) {
                        return;
                    }
                    let handle = app.clone();
                    quit_dialog::confirm_quit_dialog(
                        app,
                        "退出 oh-my-ticket".to_string(),
                        format!(
                            "还有 {running} 个 agent 在运行。选择「{DIALOG_KEEP}」后，它们会一直执行到完成或超时；\
                             随时可以重新打开本应用查看或停止。"
                        ),
                        move |choice| {
                            match choice {
                                // KEEP / STOP 都是退出路径（P3-a）：DIALOG_OPEN
                                // 保持置位直到进程退出
                                QuitChoice::Keep => {}
                                // §49 quit-stop-off-main：停止+退出序列放工作
                                // 线程（on_choice 在 GCD 主队列里跑，逐 agent
                                // flock + 对账不得阻塞主 run loop）。§51
                                // F-SP2-4：QUIT_CONFIRMED 由 worker 在 exit 前
                                // 一刻置位——stop 期间再按 ⌘Q 不得中途放行
                                QuitChoice::Stop => {
                                    mark_stopping(&handle);
                                    let exit_handle = handle.clone();
                                    stop_and_exit_off_main(&runtime_dir, move || {
                                        exit_handle.exit(0)
                                    });
                                    return;
                                }
                                // 取消 / Esc：不退出——只有回到正常运行的分支
                                // 清 DIALOG_OPEN（P3-a）
                                QuitChoice::Cancel => {
                                    DIALOG_OPEN.store(false, Ordering::SeqCst);
                                    return;
                                }
                            }
                            QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                            handle.exit(0);
                        },
                    );
                }
                // F-EXEC-6：KEEP 隐藏后的恢复——用户点击 dock 图标
                // （applicationShouldHandleReopen）时重新显示主窗口。
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen {
                    has_visible_windows: false,
                    ..
                } => restore_main_window(app),
                _ => {}
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
    use super::{
        parse_allow_multi_instance, run_quit_stop_worker, stop_and_exit_off_main, QUIT_CONFIRMED,
    };
    use std::sync::atomic::Ordering;

    /// QUIT_CONFIRMED 是进程级静态——两个 worker 时序测试共用它，串行化
    /// 防止并行测试互相污染读数。
    static QUIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// §49 quit-stop-off-main：Stop 分支的停止+退出序列必须不在调用线程
    ///（生产 = GCD 主队列）上执行——探针闭包回报自己的线程 id，与调用线程
    /// 不同即证。旧实现（on_choice 内联 stop_all_execs）在本测的等价形态下
    /// 线程 id 相同。
    #[test]
    fn stop_and_exit_runs_off_the_calling_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let caller_thread = std::thread::current().id();
        stop_and_exit_off_main(dir.path(), move || {
            tx.send(std::thread::current().id()).unwrap();
        });
        let worker_thread = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("停止+退出序列应完成并回调退出动作");
        assert_ne!(
            caller_thread, worker_thread,
            "序列必须在独立线程上执行，不得阻塞主 run loop"
        );
    }

    /// §51 F-SP2-4：QUIT_CONFIRMED 只在 exit 前一刻置位——stop 进行中必须
    /// 仍为 false（此前在 stop 开始前置位，stop 期间再按 ⌘Q 会直接放行，
    /// 进程中途退出，剩余 agent 的分离 shim 收不到停止信号）。
    #[test]
    fn quit_stop_worker_confirms_only_right_before_exit() {
        let _guard = QUIT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        QUIT_CONFIRMED.store(false, Ordering::SeqCst);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let probe_stop = seen.clone();
        let probe_exit = seen.clone();
        run_quit_stop_worker(
            std::path::Path::new("/nonexistent-rt"),
            move |_dir| {
                probe_stop
                    .lock()
                    .unwrap()
                    .push(QUIT_CONFIRMED.load(Ordering::SeqCst));
            },
            move || {
                probe_exit
                    .lock()
                    .unwrap()
                    .push(QUIT_CONFIRMED.load(Ordering::SeqCst));
            },
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "stop 与 exit 探针都应执行: {seen:?}");
        assert!(
            !seen[0],
            "stop 进行中 QUIT_CONFIRMED 必须为 false（再按 ⌘Q 不得中途放行）"
        );
        assert!(seen[1], "exit 前必须置位（随后的 ExitRequested 直接放行）");
    }

    /// §51 F-SP2-4 panic 兜底：stop_all_execs panic 时也必须置位并退出——
    /// 否则进程永不退出且 DIALOG_OPEN 卡 true，窗口再也关不掉。
    #[test]
    fn quit_stop_worker_exits_even_when_stop_panics() {
        let _guard = QUIT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        QUIT_CONFIRMED.store(false, Ordering::SeqCst);
        let exited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = exited.clone();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_quit_stop_worker(
                std::path::Path::new("/nonexistent-rt"),
                |_| panic!("stop_all_execs 崩溃（§51 兜底测试）"),
                move || {
                    probe.store(true, Ordering::SeqCst);
                },
            );
        }))
        .expect("worker 自身的 catch_unwind 必须兜住 stop 的 panic");
        assert!(
            exited.load(Ordering::SeqCst),
            "stop panic 后仍必须调用 exit（DIALOG_OPEN 已卡 true，不退出就再也关不掉窗口）"
        );
        assert!(
            QUIT_CONFIRMED.load(Ordering::SeqCst),
            "panic 路径同样要在退出前置位"
        );
    }
}
