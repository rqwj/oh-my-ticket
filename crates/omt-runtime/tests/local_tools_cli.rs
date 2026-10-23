//! 子项目 2 A5：CLI 本地动词 e2e（不经 daemon 的只读动词 + policy 拒绝路径）。
mod common;
use common::TestCtx;

fn omt_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_omt"))
}

fn run_cli(ctx: &TestCtx, args: &[&str]) -> std::process::Output {
    std::process::Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(&ctx.runtime_dir)
        .args(args)
        .env("OMT_RUNTIME_DIR", &ctx.runtime_dir)
        .output()
        .expect("run omt")
}

#[test]
fn harness_list_and_policy_work_without_a_daemon() {
    let ctx = TestCtx::spawn_named("local-cli-ro");
    let out = run_cli(&ctx, &["harness", "list", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["id"] == "pi"));

    let out = run_cli(&ctx, &["exec", "policy", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["policy"]["allowFromMcp"], false);
    assert_eq!(value["source"], "default");
}

#[test]
fn exec_start_denied_by_policy_exits_3_with_structured_problem() {
    let ctx = TestCtx::spawn_named("local-cli-deny");
    std::fs::create_dir_all(ctx.runtime_dir.join("exec")).unwrap();
    std::fs::write(
        ctx.runtime_dir.join("exec/policy.json"),
        r#"{"allowFromCli": false}"#,
    )
    .unwrap();
    let out = run_cli(
        &ctx,
        &["exec", "start", "--harness", "pi", "--node", "TICKET-1"],
    );
    assert_eq!(out.status.code(), Some(3));
    let problem: serde_json::Value = serde_json::from_slice(out.stderr.trim_ascii_end()).unwrap();
    assert_eq!(problem["code"], "POLICY_DENIED");
    assert_eq!(problem["details"]["field"], "allowFromCli");
}

#[test]
fn exec_list_on_empty_runtime_is_empty_and_status_unknown_is_not_found() {
    let ctx = TestCtx::spawn_named("local-cli-list");
    let out = run_cli(&ctx, &["exec", "list", "--all-homes", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["execs"], serde_json::json!([]));
    let out = run_cli(&ctx, &["exec", "status", "exec-nope"]);
    assert_eq!(out.status.code(), Some(3));
}

/// Q-12：带 OMT_EXEC_ID 的进程（被派发的 harness）不能派发、不能停止。
#[test]
fn nested_dispatch_marker_denies_start_and_stop() {
    let ctx = TestCtx::spawn_named("local-cli-nested");
    for args in [
        vec!["exec", "start", "--harness", "pi", "--node", "TICKET-1"],
        vec!["exec", "stop", "exec-anything"],
    ] {
        let out = std::process::Command::new(omt_path())
            .arg("--runtime-dir")
            .arg(&ctx.runtime_dir)
            .args(&args)
            .env("OMT_RUNTIME_DIR", &ctx.runtime_dir)
            .env("OMT_EXEC_ID", "exec-parent")
            .output()
            .expect("run omt");
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        let problem: serde_json::Value =
            serde_json::from_slice(out.stderr.trim_ascii_end()).unwrap();
        assert_eq!(problem["code"], "POLICY_DENIED", "{args:?}");
        assert_eq!(problem["details"]["field"], "nestedDispatch", "{args:?}");
    }
    // 只读动词不受影响
    let out = std::process::Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(&ctx.runtime_dir)
        .args(["exec", "policy"])
        .env("OMT_EXEC_ID", "exec-parent")
        .output()
        .unwrap();
    assert!(out.status.success());
}
