//! omt exec-shim 端到端（设计 §6「shim 生命周期」）：真二进制、假 harness。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn omt() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_omt"))
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Rig {
    _tmp: tempfile::TempDir,
    runtime: PathBuf,
    home_id: String,
    agent_id: String,
}

fn rig(harness_body: &str) -> Rig {
    rig_at(tempfile::tempdir().unwrap(), "h_test", harness_body)
}

/// `tmp` 下固定布局：`run/`（runtime）、`ws/.omt`（home）、`global/`（OMT_HOME）。
fn rig_at(tmp: tempfile::TempDir, home_id: &str, harness_body: &str) -> Rig {
    let runtime = tmp.path().join("run");
    let home_path = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&home_path).unwrap();
    std::fs::create_dir_all(tmp.path().join("global")).unwrap();
    let fake = write_script(tmp.path(), "fake-pi", harness_body);
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
        home_id,
        "pi",
        omt_exec::agents::NewExec {
            exec_id: "exec-e2e".into(),
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
    let spec = omt_exec::spec::LaunchSpec {
        schema_version: omt_exec::spec::SPEC_SCHEMA_VERSION,
        home_id: home_id.into(),
        home_path,
        agent_id: agent.id.clone(),
        exec_id: "exec-e2e".into(),
        harness: "pi".into(),
        node_id: None,
        run_id: None,
        unattended: true,
        model: None,
        resume_session: None,
        prompt: "go".into(),
        max_duration_secs: 60,
        omt_path: omt(),
    };
    let dir = omt_exec::layout::agent_dir(&runtime, home_id, &agent.id).unwrap();
    spec.write(&omt_exec::layout::exec_file(
        &dir,
        "exec-e2e",
        omt_exec::layout::ExecFile::Spec,
    ))
    .unwrap();
    Rig {
        _tmp: tmp,
        runtime,
        home_id: home_id.to_string(),
        agent_id: agent.id,
    }
}

/// shim 子进程的 OMT_HOME 指向临时目录：它若需要拉起 daemon，daemon 打开的
/// 全局 home 是临时的，不碰开发者真实的 ~/.omt。
fn shim(rig: &Rig) -> Command {
    let mut cmd = Command::new(omt());
    cmd.args([
        "--runtime-dir",
        rig.runtime.to_str().unwrap(),
        "exec-shim",
        &rig.home_id,
        &rig.agent_id,
        "exec-e2e",
    ])
    .env("OMT_HOME", rig._tmp.path().join("global"));
    cmd
}

fn agent(rig: &Rig) -> omt_exec::agents::AgentRecord {
    omt_exec::agents::get_agent(&rig.runtime, &rig.home_id, &rig.agent_id)
        .unwrap()
        .unwrap()
}

#[test]
fn shim_binary_runs_exec_to_exit() {
    let rig = rig("echo hello\nexit 0");
    let status = shim(&rig).status().unwrap();
    assert!(status.success(), "{status:?}");
    let a = agent(&rig);
    assert_eq!(a.status, omt_exec::agents::AgentRecordStatus::Exited);
    assert_eq!(a.execs[0].exit_code, Some(0));
}

#[test]
fn sigterm_to_shim_kills_harness_as_user_stop() {
    let rig = rig("trap '' TERM\necho READY\nsleep 30");
    let mut child = shim(&rig).spawn().unwrap();
    // 等 harness 起来（exec.pid 落盘）
    let deadline = Instant::now() + Duration::from_secs(30);
    while agent(&rig).execs[0].pid.is_none() {
        assert!(Instant::now() < deadline, "harness 未在 30s 内拉起");
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let started = Instant::now();
    let status = child.wait().unwrap();
    assert!(status.success());
    // harness 忽略 TERM → 宽限后 KILL 升级（KILL_GRACE = 3s）
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let a = agent(&rig);
    assert_eq!(a.status, omt_exec::agents::AgentRecordStatus::Killed);
    assert_eq!(a.execs[0].kill_reason.as_deref(), Some("user"));
}

/// review F5 / R2-2 / R2-3：desktop 关闭后，home 只靠 shim 保活。daemon 的静默
/// 阈值调到 1.5s（保活间隔由此推导为 500ms，不设环境变量——顺带验证推导）；
/// harness 睡 4s 后用 CLI 访问 home（代替 MCP 的认领 / 汇报）。spec 用 daemon
/// 为该 home 分配的真实 homeId（否则保活请求在到达 home 前就被拒）。有保活
/// 时成功；用环境变量把间隔调大（相当于没有保活）时，home 被关闭，访问失败——
/// 反例证明这个测试确实测到了保活。
#[test]
fn shim_keeps_workspace_home_open_while_harness_runs() {
    fn run_case(keepalive_override_ms: Option<&str>) -> Option<i32> {
        let omt_bin = omt();
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run");
        let home = tmp.path().join("ws").join(".omt");
        let global = tmp.path().join("global");
        for dir in [&runtime, &home, &global] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(runtime.join("daemon.json"), r#"{"idleQuietMs": 1500}"#).unwrap();
        let started = Command::new(&omt_bin)
            .args([
                "--runtime-dir",
                runtime.to_str().unwrap(),
                "--home",
                home.to_str().unwrap(),
                "daemon-start",
            ])
            .env("OMT_HOME", &global)
            .status()
            .unwrap();
        assert!(
            started.success(),
            "daemon-start（模拟 desktop 打开的 home）"
        );
        let home_id =
            omt_exec::daemon_link::DaemonLink::connect(&runtime, "cli", None, &home, None)
                .unwrap()
                .home_id_for(&home)
                .unwrap();
        let body = format!(
            "sleep 4\n\"{}\" --runtime-dir \"$OMT_RUNTIME_DIR\" --home \"$PWD/.omt\" run-list >/dev/null 2>&1\nexit $?",
            omt_bin.display()
        );
        let rig = rig_at(tmp, &home_id, &body);
        let mut cmd = shim(&rig);
        if let Some(ms) = keepalive_override_ms {
            cmd.env("OMT_SHIM_KEEPALIVE_MS", ms);
        }
        assert!(cmd.status().unwrap().success());
        let code = agent(&rig).execs[0].exit_code;
        let _ = Command::new(&omt_bin)
            .args([
                "--runtime-dir",
                rig.runtime.to_str().unwrap(),
                "daemon-stop",
            ])
            .status();
        code
    }
    assert_eq!(
        run_case(None),
        Some(0),
        "有保活（间隔由 idleQuietMs 推导）：home 在 harness 运行期间保持打开"
    );
    assert_ne!(
        run_case(Some("600000")),
        Some(0),
        "无保活：home 静默关闭后访问失败（反例）"
    );
}
