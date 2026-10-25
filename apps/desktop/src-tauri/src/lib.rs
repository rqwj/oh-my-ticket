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

/// §53 P3-11：停止 worker 进行中标志（`quit_stop_branch` 内**spawn 前**
/// 同步置位、worker `exit()` 前清零）。两个退出 handler（CloseRequested /
/// ExitRequested）**先查它**、直接 prevent——不在主线程跑
/// `active_exec_count`（它经 STORE_LOCK 逐 home 读 agents.json，worker
/// 持锁等 flock 最长 10s，主线程会卡；且读失败按 0 计会放行退出）。
/// worker 结束时自己 `exit()`，prevent 只挡中途退出。
/// §55 P3：置位从 worker 起跑首行挪到分支内（spawn 前）——`on_choice`
/// 返回到 worker 起跑之间（微秒级）的 CloseRequested / ExitRequested
/// 仍会在主线程跑 active_exec_count，窗口就此关闭。
static QUIT_STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
/// 队列里同步跑会阻塞主 run loop（锁竞争下转圈）。stop 与退出动作均以
/// 闭包注入（生产 = `stop_all_execs` / `AppHandle::exit(0)`；测试注入探
/// 针，证明序列不在调用线程上执行、测确认位时序与 panic 兜底）。真正的
/// 序列体在 [`run_quit_stop_worker`]。
fn stop_and_exit_off_main(
    runtime_dir: &Path,
    stop: impl FnOnce(&Path) + Send + 'static,
    exit: impl FnOnce() + Send + 'static,
) {
    let runtime_dir = runtime_dir.to_path_buf();
    std::thread::spawn(move || run_quit_stop_worker(&runtime_dir, stop, exit));
}

/// §53 P3-12：两个 STOP 分支（CloseRequested / ExitRequested 的
/// `QuitChoice::Stop`）的共用体：「标记停止中 → 停止全部 → 退出（off
/// main）」。§51 F-SP2-4 的置位契约由 [`run_quit_stop_worker`] 承担——本
/// 函数**不得**触碰 `QUIT_CONFIRMED`（原 bug 就在分支内提前置位；抽出
/// 共用体后由 `quit_stop_branch_keeps_confirm_false_during_stop` 与源文本
/// 断言 `run_quit_stop_arms_never_store_quit_confirmed_before_helper` 锁定）。
/// §55 P3：`QUIT_STOPPING` 在 spawn **之前**同步置位（此后 mark_stopping、
/// spawn 都在它的保护下）——关闭 `on_choice` 返回到 worker 起跑之间的
/// 微秒窗口。
fn quit_stop_branch(
    runtime_dir: &Path,
    stop: impl FnOnce(&Path) + Send + 'static,
    mark_stopping: impl FnOnce(),
    exit: impl FnOnce() + Send + 'static,
) {
    QUIT_STOPPING.store(true, Ordering::SeqCst);
    mark_stopping();
    stop_and_exit_off_main(runtime_dir, stop, exit);
}

/// §51 F-SP2-4：停止+退出序列的可测内芯。
/// - `QUIT_CONFIRMED` 只在 `exit()` 前一刻置位——stop 进行中再按 ⌘Q 不得
///   中途放行（此前在 stop 开始前置位，第二次退出请求直接穿透，剩余 agent
///   的分离 shim 收不到停止信号继续运行）。stop 期间的重入由 `DIALOG_OPEN`
///   挡住（全程置位：prevent 且不叠框）。
/// - §53 P3-11 → §55 P3：`QUIT_STOPPING` 的置位由 `quit_stop_branch` 在
///   spawn 前同步完成——worker 只负责 `exit()` 前清零（静态位卫生）。
/// - panic 兜底：`stop` panic 时也必须置位并退出——否则 `DIALOG_OPEN` 永远
///   卡在 true，窗口再也关不掉。
fn run_quit_stop_worker(runtime_dir: &Path, stop: impl FnOnce(&Path), exit: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stop(runtime_dir)))
    {
        eprintln!("退出停止 panic（{panic:?}）——跳过剩余停止，强制退出");
    }
    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
    QUIT_STOPPING.store(false, Ordering::SeqCst);
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
            // §53 P3-11：停止 worker 进行中直接 prevent_close——先于
            // active_exec_count 检查（worker 持 STORE_LOCK 等 flock 最长
            // 10s，主线程读计数会卡；读失败按 0 计会放行退出）。
            if QUIT_STOPPING.load(Ordering::SeqCst) {
                api.prevent_close();
                return;
            }
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
                        // §53 P3-12：分支主体抽为共用的 quit_stop_branch（两个
                        // STOP 分支同体）；QUIT_CONFIRMED 的置位时序由
                        // run_quit_stop_worker 承担（§51 F-SP2-4）。
                        let stopping_handle = handle.clone();
                        let exit_handle = handle.clone();
                        quit_stop_branch(
                            &runtime_dir,
                            stop_all_execs,
                            move || mark_stopping(&stopping_handle),
                            move || exit_handle.exit(0),
                        );
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
                    // §53 P3-11：停止 worker 进行中直接 prevent_exit——先于
                    // code 判断（code:None 的自然退出请求同样不得在 stop
                    // 中途放行），也不在主线程跑 active_exec_count（worker
                    // 持 STORE_LOCK 等 flock 最长 10s，主线程会卡；读失败
                    // 按 0 计会放行退出）。worker 结束自己 exit()。
                    if QUIT_STOPPING.load(Ordering::SeqCst) {
                        api.prevent_exit();
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
                                // 保持置位直到进程退出。
                                // §57 P3：确认+退出由各分支自带（Keep 直接置位
                                // 退出；Stop 经 quit_stop_branch 的 worker 在 exit
                                // 前置位）——match 后没有共享尾部，任何分支都
                                // 不可能「落入」置位+退出（§51 F-SP2-4 的回归
                                // 形态在结构上不可复现；源文本断言
                                // run_quit_stop_arms_never_touch_quit_confirmed_in_any_form
                                // 锁定）。
                                QuitChoice::Keep => {
                                    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                                    handle.exit(0);
                                }
                                // §53 P3-12：分支主体抽为共用的 quit_stop_branch
                                //（两个 STOP 分支同体）；确认的置位时序由
                                // run_quit_stop_worker 承担（§51 F-SP2-4）
                                QuitChoice::Stop => {
                                    let stopping_handle = handle.clone();
                                    let exit_handle = handle.clone();
                                    quit_stop_branch(
                                        &runtime_dir,
                                        stop_all_execs,
                                        move || mark_stopping(&stopping_handle),
                                        move || exit_handle.exit(0),
                                    );
                                }
                                // 取消 / Esc：不退出——只有回到正常运行的分支
                                // 清 DIALOG_OPEN（P3-a）
                                QuitChoice::Cancel => {
                                    DIALOG_OPEN.store(false, Ordering::SeqCst);
                                }
                            }
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
        parse_allow_multi_instance, quit_stop_branch, run_quit_stop_worker, stop_all_execs,
        stop_and_exit_off_main, QUIT_CONFIRMED, QUIT_STOPPING,
    };
    use std::sync::atomic::Ordering;

    /// QUIT_CONFIRMED / QUIT_STOPPING 是进程级静态——worker 时序测试共用
    /// 它们，串行化防止并行测试互相污染读数。
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
    /// §55 P3：本测的 worker 会写 QUIT_CONFIRMED/QUIT_STOPPING（进程级
    /// 静态），与轮询这些静态量的时序测试并行跑会互相污染——拿
    /// QUIT_TEST_LOCK 串行化（防并行偶发红）。
    #[test]
    fn stop_and_exit_runs_off_the_calling_thread() {
        let _guard = QUIT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let caller_thread = std::thread::current().id();
        stop_and_exit_off_main(dir.path(), stop_all_execs, move || {
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

    /// §53 P3-11 → §55 P3 修订：QUIT_STOPPING 的**置位**挪到
    /// quit_stop_branch（spawn 前同步）——worker 内芯不再置位（直接调用
    /// worker 时保持调用前的值，此处显式预置 false 锁定「不置位」），但
    /// exit 前仍负责清零（静态位卫生：进程即将退出）。
    #[test]
    fn quit_stop_worker_no_longer_sets_stopping_and_clears_before_exit() {
        let _guard = QUIT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        QUIT_CONFIRMED.store(false, Ordering::SeqCst);
        QUIT_STOPPING.store(false, Ordering::SeqCst);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let probe_stop = seen.clone();
        let probe_exit = seen.clone();
        run_quit_stop_worker(
            std::path::Path::new("/nonexistent-rt"),
            move |_dir| {
                probe_stop
                    .lock()
                    .unwrap()
                    .push(QUIT_STOPPING.load(Ordering::SeqCst));
            },
            move || {
                probe_exit
                    .lock()
                    .unwrap()
                    .push(QUIT_STOPPING.load(Ordering::SeqCst));
            },
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "stop 与 exit 探针都应执行: {seen:?}");
        assert!(
            !seen[0],
            "worker 不得置位 QUIT_STOPPING（§55：置位在 quit_stop_branch，spawn 前同步）"
        );
        assert!(!seen[1], "exit 前必须清零 QUIT_STOPPING（进程即将退出）");
    }

    /// §53 P3-12 → §55 P3 修订：STOP 分支共用体 quit_stop_branch 的时序
    /// 契约——分支自身**不得**触碰 QUIT_CONFIRMED（§51 F-SP2-4 原始 bug 就
    /// 在分支内提前置位；把 `QUIT_CONFIRMED.store(true)` 加回本函数本测
    /// 即红）：**spawn 前**同步置 QUIT_STOPPING（mark 探针在分支内同步
    /// 执行，此时必须已见 true——§55 之前置位在 worker 起跑首行，mark 时
    /// 仍为 false）；stop 时 confirm=false、stopping=true；exit 时
    /// confirm=true、stopping=false。
    #[test]
    fn quit_stop_branch_keeps_confirm_false_during_stop() {
        let _guard = QUIT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        QUIT_CONFIRMED.store(false, Ordering::SeqCst);
        QUIT_STOPPING.store(false, Ordering::SeqCst);
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let marked = events.clone();
        let stop_events = events.clone();
        let exit_events = events.clone();
        quit_stop_branch(
            std::path::Path::new("/nonexistent-rt"),
            move |_dir| {
                stop_events.lock().unwrap().push((
                    "stop",
                    QUIT_CONFIRMED.load(Ordering::SeqCst),
                    QUIT_STOPPING.load(Ordering::SeqCst),
                ));
            },
            || {
                // mark 探针在分支内同步执行（spawn 之前）——读真实静态量，
                // 不写死：§55 要求 mark 时 QUIT_STOPPING 已置位
                marked.lock().unwrap().push((
                    "mark",
                    QUIT_CONFIRMED.load(Ordering::SeqCst),
                    QUIT_STOPPING.load(Ordering::SeqCst),
                ));
            },
            move || {
                exit_events.lock().unwrap().push((
                    "exit",
                    QUIT_CONFIRMED.load(Ordering::SeqCst),
                    QUIT_STOPPING.load(Ordering::SeqCst),
                ));
            },
        );
        // worker 是异步的——等 exit 探针落地（有界）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while events.lock().unwrap().len() < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "分支应驱动 mark → stop → exit 全序列"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let events = events.lock().unwrap();
        assert_eq!(events[0].0, "mark", "标记「停止中」最先（同步）执行");
        assert!(
            !events[0].1,
            "mark 时 QUIT_CONFIRMED 必须为 false（分支不得触碰）"
        );
        assert!(
            events[0].2,
            "§55：mark 时 QUIT_STOPPING 必须已置位——置位发生在 spawn 前（同步），微秒窗口关闭"
        );
        assert_eq!(events[1].0, "stop");
        assert_eq!(events[2].0, "exit");
        assert!(
            !events[1].1,
            "stop 进行中 QUIT_CONFIRMED 必须为 false（分支不得提前置位）"
        );
        assert!(events[1].2, "stop 进行中 QUIT_STOPPING 必须为 true");
        assert!(events[2].1, "exit 前必须置位 QUIT_CONFIRMED");
        assert!(!events[2].2, "exit 前 QUIT_STOPPING 已清零");
    }

    /// §51 F-SP2-4 panic 兜底：stop_all_execs panic 时也必须置位并退出——
    /// 否则进程永不退出且 DIALOG_OPEN 卡 true，窗口再也关不掉。
    /// §53 P3-11：panic 路径同样要清 QUIT_STOPPING（否则 exit 后静态位
    /// 残留 true，后续关窗被永远 prevent）。
    #[test]
    fn quit_stop_worker_exits_even_when_stop_panics() {
        let _guard = QUIT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        QUIT_CONFIRMED.store(false, Ordering::SeqCst);
        QUIT_STOPPING.store(false, Ordering::SeqCst);
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
        assert!(
            !QUIT_STOPPING.load(Ordering::SeqCst),
            "panic 路径同样要清 QUIT_STOPPING（防静态位残留）"
        );
    }

    /// §55 P3 → §57 P3 → §59 P3 修订：源文本断言守 run() 的两个 STOP 分支
    /// （与 rpc_bridge 的 daemon_homes_tests 同法）——helper 级测试
    /// （quit_stop_branch / run_quit_stop_worker）锁不住调用点。
    /// §57 发现旧断言只扫 `QuitChoice::Stop =>` 到 `quit_stop_branch(`
    /// 之间；§58 重构（共享尾部删除、Keep 分支自带确认+退出）后扫描改为
    /// arm 整段。§59 变异确认仍漏两种回归：
    /// - 最后一个 QuitChoice arm 之后的代码从不被扫描：在 ExitRequested 的
    ///   match 之后加回共享尾部（store + exit(0)）→ Stop/Cancel 落入尾部
    ///   （§51 F-SP2-4 复现）而测试仍绿；
    /// - `keep_owns_exit >= 1` 区分不了两处 Keep：给 CloseRequested 的 Keep
    ///   （本应只隐藏窗口、后台继续）加 store + exit(0) 仍绿。
    ///
    /// §59 修订：两个 on_choice 闭包**到闭包结束**整体入扫（含 match 之后
    /// 的任何尾部）——`QUIT_CONFIRMED.<method>`（任何形态）与 `.exit(0`
    /// 只允许出现在各自的 arm 里：
    /// - CloseRequested（关窗）：Keep 只隐藏窗口——两者皆无；Stop 经
    ///   quit_stop_branch；除 Stop 分支外整个闭包无 `.exit(0`、无
    ///   `QUIT_CONFIRMED.`（任何形态）；
    /// - ExitRequested（⌘Q）：Keep 必须自带 `QUIT_CONFIRMED.store` +
    ///   `.exit(0`；除 Keep（store）与 Stop（helper 的 exit 闭包）外，
    ///   闭包其余部分（含 Cancel 与 match 之后的尾部）两者皆无。
    ///
    ///（needle 运行时拼接，避免本测的字面量被自己扫到。）
    #[test]
    fn run_quit_stop_arms_never_touch_quit_confirmed_in_any_form() {
        let full = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
            .expect("read lib.rs source");
        // 只扫 run() 的生产代码：截断到本测试模块之前（测试模块的注释会
        // 引用 arm 字面量，混进扫描会造出假 arm）
        let test_module = ["#[cfg(", "test)]"].join("");
        let src = &full[..full.find(&test_module).expect("lib.rs 应有测试模块")];
        let next_choice = ["QuitChoice", "::"].join("");
        let helper = ["quit_stop_branch", "("].join("");
        // 代码形态 needle：`QUIT_CONFIRMED.<method>`（去空白后）——命中
        // `QUIT_CONFIRMED.store` / `QUIT_CONFIRMED\n.store` / `.swap(true` /
        // 任何其它方法调用形态；分支注释里的说明文字（后接中文/空格）不算
        let confirm_ref = ["QUIT_", "CONFIRMED."].join("");
        let confirm_store = ["QUIT_", "CONFIRMED.store"].join("");
        // §61 P3：exit needle 放宽为 `exit(`（去空白后）——`.exit(0` 认不出
        // `std::process::exit(0)` / `handle.exit(1)`（§61 变异确认存活）。
        let exit_call = ["exit", "("].join("");
        let collapse =
            |text: &str| -> String { text.chars().filter(|c| !c.is_whitespace()).collect() };
        // ── §61 P3：Stop 分支改在闭包区内检查（剥注释），arm 无下一个
        // QuitChoice:: 时延伸到闭包区结束——Stop 为末分支时 match 之后的
        // 尾部并入 Stop 区域，由 confirm 断言兜住，不再从剩余区豁免 ──
        // 闭包区结束锚：CloseRequested（on_window_event 内）之后是 `.menu(`
        // builder 调用；ExitRequested（run 事件 arm 内）之后是 Reopen 事件臂
        let choice_head = ["move |choice", "|"].join("");
        let keep_arm_c = ["QuitChoice::Keep", "=>"].concat();
        let stop_arm_c = ["QuitChoice::Stop", "=>"].concat();
        // 区间会延伸到闭包结束锚（下一个 builder 调用 / 下一个事件臂），
        // 中间的说明注释（如菜单 Quit 的 `app.exit(0)` 说明）不是代码——
        // 扫描前剥掉行注释（本文件生产区不用块注释，字符串里无 `//`）
        let strip_comments = |text: &str| -> String {
            text.lines()
                .map(|line| match line.find("//") {
                    Some(at) => &line[..at],
                    None => line,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut closure_regions: Vec<String> = Vec::new();
        {
            let mut rest = src;
            while let Some(idx) = rest.find(&choice_head) {
                let tail = &rest[idx..];
                let end = tail
                    .find(".menu(")
                    .or_else(|| tail.find("RunEvent::Reopen"))
                    .expect("§59：on_choice 闭包区应有结束锚（.menu( / RunEvent::Reopen）");
                closure_regions.push(collapse(&strip_comments(&rest[idx..idx + end])));
                rest = &rest[idx + end..];
            }
        }
        assert!(
            closure_regions.len() >= 2,
            "应锁定 CloseRequested / ExitRequested 两个 on_choice 闭包，实际 {} 个",
            closure_regions.len()
        );
        // arm 子区（去空白后的 needle；闭包区内到下一个 QuitChoice::，没有
        // 下一个时到闭包区结束——见上方 §61 P3 说明）
        let arm_in = |region: &str, arm: &str| -> String {
            let start = region
                .find(arm)
                .unwrap_or_else(|| panic!("on_choice 闭包区应有 {arm}"));
            let end = region[start + arm.len()..]
                .find(&next_choice)
                .map(|off| start + arm.len() + off)
                .unwrap_or(region.len());
            region[start..end].to_string()
        };
        // 去掉 Keep / Stop 两个 arm 后的闭包剩余（闭包头 / Cancel arm /
        // match 之后的任何尾部——§59 变异确认的共享尾部落点）
        let remainder_of = |region: &str| -> String {
            let keep = arm_in(region, &keep_arm_c);
            let stop = arm_in(region, &stop_arm_c);
            region.replacen(&keep, "", 1).replacen(&stop, "", 1)
        };
        // Stop 分支：必须经 helper 收尾；不得直接触碰 QUIT_CONFIRMED（任何
        // 形态）。exit( 不能在 Stop 上断言——helper 的 exit 闭包实参
        // （`move || …exit(0)`）是合法内容；exit 形态收紧由 Keep/剩余区承担。
        let mut stop_arms = 0usize;
        for region in &closure_regions {
            if region.find(&stop_arm_c).is_some() {
                let arm = arm_in(region, &stop_arm_c);
                assert!(
                    arm.contains(&helper),
                    "每个 Stop 分支都必须经 quit_stop_branch 收尾"
                );
                assert!(
                    !arm.contains(&confirm_ref),
                    "run() 的 QuitChoice::Stop 分支（含 Stop 为末分支时并入的 match \
                     尾部）不得触碰 QUIT_CONFIRMED（任何方法形态：store / swap / 跨行；\
                     时序契约由 run_quit_stop_worker 承担）"
                );
                stop_arms += 1;
            }
        }
        assert!(
            stop_arms >= 2,
            "应锁定 CloseRequested / ExitRequested 两个 STOP 分支，实际找到 {stop_arms} 个"
        );
        // CloseRequested（关窗）：Keep 只隐藏窗口——无 store 无 exit
        let close_requested = &closure_regions[0];
        let close_keep = arm_in(close_requested, &keep_arm_c);
        assert!(
            !close_keep.contains(&exit_call),
            "CloseRequested 的 Keep 分支只隐藏窗口、后台继续——不得 exit（§59 变异：\
             给它加 store+exit 曾让旧断言仍绿）"
        );
        assert!(
            !close_keep.contains(&confirm_ref),
            "CloseRequested 的 Keep 分支不得触碰 QUIT_CONFIRMED（任何形态）"
        );
        let close_remainder = remainder_of(close_requested);
        assert!(
            !close_remainder.contains(&exit_call) && !close_remainder.contains(&confirm_ref),
            "CloseRequested 的 on_choice 闭包除 Stop 分支外不得出现 .exit(0 / \
             QUIT_CONFIRMED（任何形态，含 match 之后的尾部）"
        );
        // ExitRequested（⌘Q）：Keep 自带确认+退出；其余（含尾部）两者皆无
        let exit_requested = &closure_regions[1];
        let exit_keep = arm_in(exit_requested, &keep_arm_c);
        assert!(
            exit_keep.contains(&confirm_store) && exit_keep.contains(&exit_call),
            "ExitRequested 的 Keep 分支必须自带 QUIT_CONFIRMED.store + exit(0)（§57 重构后\
             无共享尾部可落入——把确认+退出挪回 match 之后是 §57/§59 点名的回归位点）"
        );
        let exit_remainder = remainder_of(exit_requested);
        assert!(
            !exit_remainder.contains(&confirm_ref) && !exit_remainder.contains(&exit_call),
            "ExitRequested 的 on_choice 闭包除 Keep/Stop 分支外不得出现 .exit(0 / \
             QUIT_CONFIRMED（任何形态）——match 之后加回共享尾部（§59 变异确认的\
             漏网位点，Stop/Cancel 落入即 F-SP2-4 复现）必须被抓"
        );
    }
}
