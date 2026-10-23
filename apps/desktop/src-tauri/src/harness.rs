//! 设置页 Harnesses 数据面的 tauri command 包装。内核在 omt_harness::harness
//! （执行面设计 §3.1）；这里只负责 runtime dir 解析与登录终端拉起。
//! A4 起 `harness_list` / `harness_list_models` 直接转发共用投影
//! （`omt_harness::harness::{list_value, models_value}`，与 `omt harness`
//! 本地动词同源）。

use omt_harness::harness::{
    descriptor_of, harness_entry, launch_preview_value, load_configs, resolve_launch,
    upsert_config, validate_config, HarnessUserConfig,
};

use crate::daemon;

/// 执行弹窗「详情」的只读预览数据面（`harness_launch_preview`）：入参
/// providerId，返回 {program, args, envKeys, unattendedFlag}。
#[tauri::command(async)]
pub fn harness_launch_preview(harness: String) -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let plan = resolve_launch(&runtime_dir, &harness)?;
    Ok(launch_preview_value(&plan))
}

// ---------------------------------------------------------------------------
// 终端登录（F-ACC-13）：订阅/OAuth 认证无法在无头派发进程内完成——
// 设置页「终端登录」按钮在本机 Terminal 里启动 harness 的交互命令。
// binary 解析在 omt_harness::harness::resolve_listing_program（A4 下沉，
// 不要求 enabled）。
// ---------------------------------------------------------------------------

/// POSIX 单引号 shell 转义：`'` → `'\''`（路径含空格/引号时保持单参数）。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// AppleScript 字符串字面量转义：`\` 与 `"`（osascript -e 的 do script 参数）。
fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 组装 `osascript -e` 的源码：activate 前置（Terminal 抢到前台），
/// do script 在新 Terminal 窗口运行登录命令。纯函数供测试。
fn terminal_login_script(shell_command: &str) -> String {
    format!(
        "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
        escape_applescript_string(shell_command)
    )
}

// ---------------------------------------------------------------------------
// 模型列表（执行弹窗下拉）
// ---------------------------------------------------------------------------

/// 执行弹窗「模型」下拉数据面（`harness_list_models`）：共用投影（A4），
/// 运行 `<binary> <model_list_args>`（≤8s，失败/超时 → 可读 Err，弹窗降级
/// 隐藏下拉；不要求 enabled——列模型是启用前的探查动作）。
#[tauri::command(async)]
pub fn harness_list_models(harness: String) -> Result<serde_json::Value, String> {
    omt_harness::harness::models_value(&daemon::runtime_dir(), &harness)
}

/// 设置页「终端登录」（`harness_open_login`）：在 Terminal 里运行
/// `<binary> <login_args>`（omp 裸跑 TUI；opencode 走 `auth login`）。
/// 仅 macOS（osascript / Terminal.app）；命令在用户 Terminal 中交互，
/// 凭据落在 harness 自己的全局存储（~/.omp 等），OMT 不经手。
#[tauri::command(async)]
pub fn harness_open_login(harness: String) -> Result<serde_json::Value, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = &harness;
        return Err("终端登录目前仅支持 macOS（Terminal.app + osascript）".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let runtime_dir = daemon::runtime_dir();
        let program = omt_harness::harness::resolve_listing_program(&runtime_dir, &harness)?;
        let descriptor = descriptor_of(&harness).expect("checked in resolve_listing_program");
        let mut shell_command = shell_quote(&program.display().to_string());
        for arg in descriptor.login_args {
            shell_command.push(' ');
            shell_command.push_str(&shell_quote(arg));
        }
        let script = terminal_login_script(&shell_command);
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn()
            .map_err(|e| format!("打开 Terminal 失败：{e}"))?;
        Ok(serde_json::json!({ "opened": true, "program": program.display().to_string() }))
    }
}

// ---------------------------------------------------------------------------
// tauri commands（设置页数据面；三家并行探测、单家梯子最长 14s，必须 async）
// ---------------------------------------------------------------------------

/// 设置页 Harnesses tab 的数据面：类型表 × 探测 × 用户配置投影（A4 起与
/// `omt harness list` 共用 `list_value`——三家并行探测，最坏 = 最慢单家而非
/// 叠加；仍 async：单家梯子最长 14s）。
#[tauri::command(async)]
pub fn harness_list() -> Result<serde_json::Value, String> {
    omt_harness::harness::list_value(&daemon::runtime_dir())
}

/// 保存一个 provider 的用户配置：校验 id + 整份配置（Manual 空路径、env
/// 保留键/非法键拒绝），锁内 read-merge-write 原子落盘，返回最新投影。
#[tauri::command(async)]
pub fn harness_config_set(
    harness: String,
    config: HarnessUserConfig,
) -> Result<serde_json::Value, String> {
    let d =
        descriptor_of(&harness).ok_or_else(|| format!("unsupported harness type: {harness}"))?;
    validate_config(&config)?;
    let runtime_dir = daemon::runtime_dir();
    upsert_config(&runtime_dir, &harness, config)?;
    let configs = load_configs(&runtime_dir);
    Ok(harness_entry(d, configs.get(&harness)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F-ACC-13：终端登录脚本的转义——敌意路径（空格/单引号/双引号/
    /// 反斜杠）不得破坏 shell 引号或 AppleScript 字面量。
    #[test]
    fn terminal_login_script_escapes_hostile_paths() {
        let normal = shell_quote("/opt/homebrew/bin/omp");
        assert_eq!(normal, "'/opt/homebrew/bin/omp'");
        let hostile = shell_quote("/x/my omp/y'z\"w");
        // 单引号 → '\''，整串仍被引号包裹
        assert!(hostile.starts_with("'/x/my omp"), "{hostile}");
        assert!(hostile.contains(r"'\''"), "单引号必须转义: {hostile}");

        let script = terminal_login_script(&hostile);
        assert!(script.contains("activate"), "Terminal 抢前置前: {script}");
        // 期望行手写字面量（shell 单引号转义 + AppleScript 反斜杠/双引号
        // 转义各就各位）——精确钉住两级转义的产物
        let line = script.lines().nth(2).unwrap_or_default();
        assert_eq!(
            line, r##"do script "'/x/my omp/y'\\''z\"w'""##,
            "敌意路径的两级转义: {script}"
        );

        // registry：omp 裸跑（TUI /login），opencode 走 auth login
        assert_eq!(descriptor_of("omp").unwrap().login_args, &[] as &[&str]);
        // F-PI-2：pi 的模型列表是 flag 不是子命令
        assert_eq!(
            descriptor_of("pi").unwrap().model_list_args,
            &["--list-models"] as &[&str]
        );
        assert_eq!(
            descriptor_of("omp").unwrap().model_list_args,
            &["models"] as &[&str]
        );
        assert_eq!(
            descriptor_of("opencode").unwrap().login_args,
            &["auth", "login"] as &[&str]
        );
    }
}
