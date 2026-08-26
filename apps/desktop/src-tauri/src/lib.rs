//! U8 scaffold entry (KD4/KD5): the desktop app is a PEER surface of the
//! shared omt-daemon, not its private guardian. The sidecar plugin is
//! registered here; daemon lifecycle wiring (spawn via
//! current_exe().parent(), handshake kind:"desktop") is unit U9.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .run(tauri::generate_context!())
        .expect("error while running oh-my-ticket desktop")
}
