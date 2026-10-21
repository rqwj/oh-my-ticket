//! Agents / bindings 面板的 tauri command 包装；存储内核在 omt_exec::agents。

use crate::daemon;

#[tauri::command(async)]
pub fn agents_list(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::agents_list_value(&daemon::runtime_dir(), &home_id)
}

#[tauri::command(async)]
pub fn agents_delete(
    home_id: String,
    agent_id: String,
    force: Option<bool>,
) -> Result<serde_json::Value, String> {
    // F-EXEC-14(4)：force=true = 「放弃租约并删除」（待对账租约守卫绕过，
    // 租约留给 daemon 侧）；缺省 false 保持常规守卫。Starting/Running 与
    // Orphan 守卫在 Rust 侧始终生效。
    omt_exec::agents::delete_agent(
        &daemon::runtime_dir(),
        &home_id,
        &agent_id,
        force.unwrap_or(false),
    )?;
    Ok(serde_json::json!({ "deleted": agent_id }))
}

#[tauri::command(async)]
pub fn bindings_get(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::bindings_get_value(&daemon::runtime_dir(), &home_id)
}

#[tauri::command(async)]
pub fn bindings_gc(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::bindings_gc_value(&daemon::runtime_dir(), &home_id)
}
