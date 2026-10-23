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
    // F-EXEC-14(4)：force=true = 「放弃租约并删除」——只绕过待对账租约守卫；
    // Starting/Running 与 Orphan 守卫在 delete_agent 侧始终生效。
    //
    // F-EXEC-16 ①（§45）：force 分支**先尽力释放**该 agent 仍待对账的租约
    // （admin connector + 与 shim 相同的释放路径，见
    // omt_exec::executor::force_release_agent_leases），把「真放弃」缩小到
    // 释放失败的那部分——daemon 不会回收租约（omt-runtime 从不
    // janitor_sweep），删除又会抹掉唯一的对账依据。尽力语义：释放失败不拦
    // 删除（用户已两步确认），但如实计入响应——释放失败的租约对应 run item
    // 可能一直显示运行中，需在 Runs 面板手动取消该 run（UI 文案同源）。
    // 非 force（缺省 false）保持常规守卫，不做释放。
    let force = force.unwrap_or(false);
    let mut leases_released = 0usize;
    let mut lease_release_failures: Vec<String> = Vec::new();
    if force {
        let runtime_dir = daemon::runtime_dir();
        let connector = omt_exec::daemon_link::admin_connector(
            runtime_dir.clone(),
            daemon::sidecar_path("omt-daemon").ok(),
        );
        match omt_exec::executor::force_release_agent_leases(
            &runtime_dir,
            &connector,
            &home_id,
            &agent_id,
        ) {
            Ok(outcome) => {
                leases_released = outcome.released;
                lease_release_failures = outcome.failures;
            }
            // 记录读不出来等整体失败：同样不拦删除（delete_agent 的守卫随后
            // 会给出准确的拒绝理由，若记录真有问题删除到不了这里）
            Err(err) => lease_release_failures.push(err),
        }
    }
    omt_exec::agents::delete_agent(&daemon::runtime_dir(), &home_id, &agent_id, force)?;
    Ok(serde_json::json!({
        "deleted": agent_id,
        "leasesReleased": leases_released,
        "leaseReleaseFailures": lease_release_failures,
    }))
}

#[tauri::command(async)]
pub fn bindings_get(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::bindings_get_value(&daemon::runtime_dir(), &home_id)
}

#[tauri::command(async)]
pub fn bindings_gc(home_id: String) -> Result<serde_json::Value, String> {
    omt_exec::agents::bindings_gc_value(&daemon::runtime_dir(), &home_id)
}

/// §41 legacy-degraded-permanent：「确认并归档」未能迁移的旧版存储文件
/// （见 omt_exec::agents::archive_legacy_store）。返回 `{ archived: [文件名] }`。
#[tauri::command(async)]
pub fn agents_archive_legacy(home_id: String) -> Result<serde_json::Value, String> {
    let archived = omt_exec::agents::archive_legacy_store(&daemon::runtime_dir(), &home_id)?;
    Ok(serde_json::json!({ "archived": archived }))
}
