//! Agents / bindings 面板的 tauri command 包装；存储内核在 omt_exec::agents。

use crate::daemon;

#[tauri::command(async)]
pub fn agents_list(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::agents_list_value(&daemon::runtime_dir(), &home_id)
}

#[tauri::command(async)]
pub fn agents_delete(home_id: String, agent_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::delete_agent(&daemon::runtime_dir(), &home_id, &agent_id)?;
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
