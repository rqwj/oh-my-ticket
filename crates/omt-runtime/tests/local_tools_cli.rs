//! 子项目 2 A5/A9：CLI 本地动词 e2e（不经 daemon 的只读动词 + policy 拒绝路径，
//! 以及真 daemon + 假 harness 的 CLI 派发全链路）。
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

// ── A9：CLI 派发全链路（真 daemon + 假 harness）─────────────────────────

/// 复制 exec_shim.rs `rig_at` 的必要部分：可执行脚本 + Manual harness 配置。
fn install_fake_pi(ctx: &TestCtx, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = ctx.dir.path().join("fake-pi");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    omt_harness::harness::upsert_config(
        &ctx.runtime_dir,
        "pi",
        omt_harness::harness::HarnessUserConfig {
            enabled: true,
            binary: omt_harness::harness::BinarySource::Manual {
                path: path.display().to_string(),
            },
            extra_args: vec![],
            env: Default::default(),
        },
    )
    .unwrap();
}

/// epic → story → ticket 三连建（`omt create`），返回 ticket 的 nodeId。
fn create_ticket_chain(ctx: &TestCtx) -> String {
    let mut parent = String::new();
    for (kind, title) in [
        ("epic", "e2e epic"),
        ("story", "e2e story"),
        ("ticket", "e2e ticket"),
    ] {
        let mut args = vec!["create", "--type", kind, "--title", title];
        if !parent.is_empty() {
            args.extend(["--parent", parent.as_str()]);
        }
        let out = run_cli(ctx, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        parent = value["node"]["nodeId"]
            .as_str()
            .unwrap_or_else(|| panic!("create 未返回 nodeId：{value}"))
            .to_string();
    }
    parent
}

#[test]
fn cli_exec_start_launches_tails_and_records_origin() {
    let ctx = TestCtx::spawn_named("local-cli-e2e");
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ctx.home_str()]);
    common::wait_for_descriptor(&ctx.runtime_dir, std::time::Duration::from_secs(20))
        .expect("daemon");
    install_fake_pi(
        &ctx,
        r#"echo '{"type":"session","id":"s-1"}'; sleep 1; exit 0"#,
    );
    // 建一张 ticket（epic → story → ticket）
    let ticket = create_ticket_chain(&ctx);
    let out = run_cli(
        &ctx,
        &[
            "--home",
            ctx.home_str(),
            "exec",
            "start",
            "--harness",
            "pi",
            "--node",
            &ticket,
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let exec_id = started["execId"].as_str().unwrap().to_string();

    // 等到 tail 报告 done（最多 30s）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut since: Option<u64> = None;
    let mut kinds = Vec::new();
    loop {
        let mut args = vec!["exec", "tail", exec_id.as_str(), "--json"];
        let since_text;
        if let Some(s) = since {
            since_text = s.to_string();
            args.extend(["--since", since_text.as_str()]);
        }
        let page: serde_json::Value = serde_json::from_slice(&run_cli(&ctx, &args).stdout).unwrap();
        for ev in page["events"].as_array().unwrap() {
            kinds.push(ev["kind"].as_str().unwrap().to_string());
        }
        since = page["nextSeq"].as_u64().or(since);
        if page["done"] == true {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tail 30s 内未结束：{kinds:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    assert_eq!(kinds.last().map(String::as_str), Some("exit"), "{kinds:?}");

    let status: serde_json::Value =
        serde_json::from_slice(&run_cli(&ctx, &["exec", "status", &exec_id, "--json"]).stdout)
            .unwrap();
    assert_eq!(status["exec"]["launchedBy"], "cli");
    // Q-2：node 状态没有被 node/execute 改成 in_progress（假 harness 不 claim，节点保持 open）
    let shown = run_cli(&ctx, &["--home", ctx.home_str(), "show", &ticket]);
    let node: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(node["node"]["status"], "open");
}
