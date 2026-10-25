//! P-1 / P-12 端到端：shim 侧连接以 cli 身份 enroll；临时授权开关 admin；
//! 在线 daemon 缺少 workspace home 时 declare + 重新注册；home 路径必须与
//! homeId 绑定。daemon 零改动的前提下验证。
//!
//! daemon 不带 `--home` 拉起时会打开全局 home（`$OMT_HOME`）。本文件的测试
//! 都会拉起真 daemon，所以**串行执行**，并把 `OMT_HOME` 指向各自的临时目录：
//! 绝不碰开发者真实的 `~/.omt`，也避免两个 daemon 争同一个全局 home 的锁。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn isolated(tmp: &Path) -> MutexGuard<'static, ()> {
    let guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let global = tmp.join("global-home");
    std::fs::create_dir_all(&global).unwrap();
    // 子进程（daemon）继承本进程环境；串行锁保证没有别的测试同时改它
    std::env::set_var("OMT_HOME", &global);
    guard
}

fn daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_omt-daemon"))
}

fn stop_daemon(runtime: &Path) {
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_omt"))
        .args(["--runtime-dir", runtime.to_str().unwrap(), "daemon-stop"])
        .status();
}

fn connect(
    runtime: &Path,
    home: &Path,
    expected: Option<&str>,
) -> Result<omt_exec::daemon_link::DaemonLink, String> {
    omt_exec::daemon_link::DaemonLink::connect(runtime, "cli", Some(&daemon_bin()), home, expected)
}

/// review F4 / R2-1：shim 拉起的 daemon 带全局 home；在线 daemon 缺 workspace
/// home 时，connect 先 declare 再重新注册。
#[test]
fn connect_opens_global_and_declares_workspace_homes() {
    let tmp = tempfile::tempdir().unwrap();
    let _serial = isolated(tmp.path());
    let runtime = tmp.path().join("run");
    let home_a = tmp.path().join("a").join(".omt");
    let home_b = tmp.path().join("b").join(".omt");
    for dir in [&runtime, &home_a, &home_b] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let first = connect(&runtime, &home_a, None).expect("spawn（全局 home）+ declare a");
    assert!(first.home_id_for(&home_a).is_some());
    assert!(
        first.home_id_for(&tmp.path().join("global-home")).is_some(),
        "shim 拉起的 daemon 必须带全局 home（review R2-1）"
    );
    let second = connect(&runtime, &home_b, None).expect("declare b on the live daemon");
    assert!(
        second.home_id_for(&home_b).is_some(),
        "declare 后重新注册，b 应在凭据范围内"
    );
    stop_daemon(&runtime);
}

/// review R2-8：给了期望 homeId 时，路径与 id 不一致或目录不是已初始化的 home → 拒绝。
#[test]
fn connect_rejects_unbound_home_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let _serial = isolated(tmp.path());
    let runtime = tmp.path().join("run");
    let real = tmp.path().join("real").join(".omt");
    let blank = tmp.path().join("blank").join(".omt");
    for dir in [&runtime, &real, &blank] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let id = connect(&runtime, &real, None)
        .unwrap()
        .home_id_for(&real)
        .unwrap();
    assert!(
        connect(&runtime, &real, Some(&id)).is_ok(),
        "id 一致 → 通过"
    );
    let err = connect(&runtime, &real, Some("h_wrong"))
        .err()
        .expect("id 不一致必须拒绝");
    assert!(err.contains("不一致"), "{err}");
    let err = connect(&runtime, &blank, Some("h_any"))
        .err()
        .expect("未初始化目录必须拒绝");
    assert!(err.contains("不是已初始化"), "{err}");
    assert!(
        !blank.join("omt.db").exists(),
        "拒绝时不得在该目录里建 home"
    );
    stop_daemon(&runtime);
}

#[test]
fn temp_grant_toggles_admin_authority() {
    let tmp = tempfile::tempdir().unwrap();
    let _serial = isolated(tmp.path());
    let runtime = tmp.path().join("run");
    let home = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mut link = connect(&runtime, &home, None).expect("discover-or-spawn + enroll");
    assert!(link.principal().starts_with("cli:"), "{}", link.principal());

    // 未授权：admin-only 的 home/reindex（human_administrative）应被拒
    let denied = link.call("home/reindex", serde_json::json!({ "homeId": "h_none" }));
    assert!(denied.is_err(), "{denied:?}");

    {
        let _g = omt_exec::grants::TempGrant::add(&runtime, link.principal()).unwrap();
        let err = link
            .call("home/reindex", serde_json::json!({ "homeId": "h_none" }))
            .unwrap_err();
        // 授权后不再是授权错误（homeId 不存在会报 NOT_FOUND 之类的业务错误）
        assert!(!err.contains("admin-required"), "{err}");
    }
    let again = link
        .call("home/reindex", serde_json::json!({ "homeId": "h_none" }))
        .unwrap_err();
    assert!(again.contains("admin"), "撤销后应回到被拒：{again}");

    stop_daemon(&runtime);
}
