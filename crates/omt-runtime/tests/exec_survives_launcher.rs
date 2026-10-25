//! 设计 §6「发起方退出后执行继续」：launcher 所在进程被 KILL 后，shim 仍
//! 把 exec 跑完并收尾。用一个子进程充当发起方（调用 omt exec-shim 后立即被杀）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn exec_completes_after_launcher_is_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    let home_path = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&home_path).unwrap();
    let fake = write_script(tmp.path(), "fake-pi", "sleep 2\necho done\nexit 0");
    omt_harness::harness::upsert_config(
        &runtime,
        "pi",
        omt_harness::harness::HarnessUserConfig {
            enabled: true,
            binary: omt_harness::harness::BinarySource::Manual {
                path: fake.display().to_string(),
            },
            extra_args: vec![],
            env: Default::default(),
        },
    )
    .unwrap();
    let agent = omt_exec::agents::register_agent(
        &runtime,
        "h_test",
        "pi",
        omt_exec::agents::NewExec {
            exec_id: "exec-surv".into(),
            node_id: None,
            run_id: None,
            binary_path: None,
            binary_args: vec![],
            model: None,
            launched_by: None,
            parent_agent_id: None,
        },
    )
    .unwrap();
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    let dir = omt_exec::layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
    omt_exec::spec::LaunchSpec {
        schema_version: omt_exec::spec::SPEC_SCHEMA_VERSION,
        home_id: "h_test".into(),
        home_path,
        agent_id: agent.id.clone(),
        exec_id: "exec-surv".into(),
        harness: "pi".into(),
        node_id: None,
        run_id: None,
        unattended: true,
        model: None,
        resume_session: None,
        prompt: "go".into(),
        max_duration_secs: 60,
        omt_path: omt.clone(),
    }
    .write(&omt_exec::layout::exec_file(
        &dir,
        "exec-surv",
        omt_exec::layout::ExecFile::Spec,
    ))
    .unwrap();

    // 发起方：一个 sh，后台拉起 shim 后挂住，随即被 KILL（只杀发起方单个 pid，
    // 与桌面端 ⌘Q 的语义一致）。macOS 无 setsid 命令；生产路径的 setsid 由
    // launch::spawn_shim 在 pre_exec 里做，Task 9 已覆盖
    let launcher_script = format!(
        "{omt} --runtime-dir {rt} exec-shim h_test {agent} exec-surv </dev/null >/dev/null 2>&1 &\nsleep 30",
        omt = omt.display(),
        rt = runtime.display(),
        agent = agent.id,
    );
    let mut launcher = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&launcher_script)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    launcher.kill().unwrap();
    let _ = launcher.wait();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let a = omt_exec::agents::get_agent(&runtime, "h_test", &agent.id)
            .unwrap()
            .unwrap();
        // F-EXEC-11：终态 status 与 exit_code 现在同一笔事务落盘，读侧按构造
        // 不会再看到「Exited + 空 exit_code」；轮询条件仍取**两者齐备**——
        // 对任何历史中间态都稳健，断言只在校验完整终态时触发。
        let terminal = matches!(
            a.status,
            omt_exec::agents::AgentRecordStatus::Exited
                | omt_exec::agents::AgentRecordStatus::Failed
                | omt_exec::agents::AgentRecordStatus::Killed
        );
        let exit_code = a.execs.first().and_then(|e| e.exit_code);
        if terminal && exit_code.is_some() {
            assert_eq!(a.status, omt_exec::agents::AgentRecordStatus::Exited);
            assert_eq!(exit_code, Some(0));
            break;
        }
        assert!(Instant::now() < deadline, "发起方被杀后 exec 未完成：{a:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
