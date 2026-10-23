//! P3-ACC-6：退出 / 关窗的三按钮确认框。tauri-plugin-dialog 2.7.1 → rfd
//! 0.16.0 在无父窗口时走 `CFUserNotificationDisplayAlert`
//! （rfd `backend/macos/utils/user_alert.rs`），三个按钮都不绑定 Esc；有父窗口
//! 时走 NSAlert sheet，`addButtonWithTitle` 逐个加按钮、同样不设
//! keyEquivalent——NSAlert 只对标题恰为英文 "Cancel" 的按钮自动绑定 Esc。
//! macOS 上改为直接构造 NSAlert：按钮顺序 KEEP（首个 = Return）/ STOP /
//! CANCEL，并给「取消」显式设 keyEquivalent = Esc。其他平台保留插件路径。
//!
//! 线程：调用方都在主线程（RunEvent / WindowEvent 回调）。runModal 是嵌套
//! 事件循环——不在 tao 的事件回调里同步跑，而是投递到 GCD 主队列，下一轮
//! run loop 再弹（与插件「回调式」的语义一致：回调里再做隐藏 / 退出）。
//! 无单测缝（AppKit 胶水）——手动验收见 Task B7 Step 9。

/// 三按钮文案（review R2-6：首个按钮是 Return 默认键，放安全的「继续在
/// 后台运行」；「取消」= 不退出）。
pub const DIALOG_KEEP: &str = "继续在后台运行";
pub const DIALOG_STOP: &str = "全部停止后退出";
pub const DIALOG_CANCEL: &str = "取消";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitChoice {
    Keep,
    Stop,
    Cancel,
}

/// NSAlert 返回码 → 三选一。1000 / 1001 / 1002 = NSAlertFirst / Second /
/// ThirdButtonReturn（按添加顺序）；其余按安全方向归 Cancel。
pub fn choice_from_response(response: isize) -> QuitChoice {
    match response {
        1000 => QuitChoice::Keep,
        1001 => QuitChoice::Stop,
        _ => QuitChoice::Cancel,
    }
}

#[cfg(target_os = "macos")]
pub fn confirm_quit_dialog<F>(_app: &tauri::AppHandle, title: String, message: String, on_choice: F)
where
    F: FnOnce(QuitChoice) + Send + 'static,
{
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            // 不可达：主队列的任务必在主线程上执行；按安全方向不退出
            on_choice(QuitChoice::Cancel);
            return;
        };
        on_choice(run_alert(mtm, &title, &message));
    });
}

#[cfg(target_os = "macos")]
fn run_alert(mtm: objc2::MainThreadMarker, title: &str, message: &str) -> QuitChoice {
    use objc2_app_kit::{NSAlert, NSAlertStyle};
    use objc2_foundation::NSString;
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    let _keep = alert.addButtonWithTitle(&NSString::from_str(DIALOG_KEEP));
    let _stop = alert.addButtonWithTitle(&NSString::from_str(DIALOG_STOP));
    let cancel = alert.addButtonWithTitle(&NSString::from_str(DIALOG_CANCEL));
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    choice_from_response(alert.runModal())
}

#[cfg(not(target_os = "macos"))]
pub fn confirm_quit_dialog<F>(app: &tauri::AppHandle, title: String, message: String, on_choice: F)
where
    F: FnOnce(QuitChoice) + Send + 'static,
{
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};
    app.dialog()
        .message(message)
        .title(title)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            DIALOG_KEEP.into(),
            DIALOG_STOP.into(),
            DIALOG_CANCEL.into(),
        ))
        .show_with_result(move |result| {
            on_choice(match result {
                MessageDialogResult::Custom(label) if label == DIALOG_KEEP => QuitChoice::Keep,
                MessageDialogResult::Custom(label) if label == DIALOG_STOP => QuitChoice::Stop,
                MessageDialogResult::Yes => QuitChoice::Keep,
                MessageDialogResult::No => QuitChoice::Stop,
                _ => QuitChoice::Cancel,
            })
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P3-ACC-6：NSAlert 返回码按按钮**添加顺序**映射——1000 = 第一个（KEEP，
    /// Return 默认键）、1001 = 第二个（STOP）、1002 = 第三个（CANCEL，Esc）；
    /// 未知返回码按安全方向归 Cancel（不退出）。
    #[test]
    fn quit_dialog_maps_alert_responses_safely() {
        assert_eq!(choice_from_response(1000), QuitChoice::Keep);
        assert_eq!(choice_from_response(1001), QuitChoice::Stop);
        assert_eq!(choice_from_response(1002), QuitChoice::Cancel);
        assert_eq!(choice_from_response(-1000), QuitChoice::Cancel);
        assert_eq!(choice_from_response(0), QuitChoice::Cancel);
    }
}
