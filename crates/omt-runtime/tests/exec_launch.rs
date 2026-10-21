//! launcher 端到端：假 daemon RPC（脚本化响应）+ 真 omt exec-shim + 假 harness。
//! 覆盖：就绪握手成功、发起方不等执行结束、shim 拉不起来时同步报错并补偿。
//! （shim 启动后在 MCP 注入阶段失败、经就绪握手 Failed 分支回报的路径，只由
//! Task 8 的 shim 单测覆盖——launcher 只有一个 omt_bin，无法在这里单独让 MCP
//! 桥缺失而 shim 存在。）

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// launcher 拉起的 shim 继承本进程环境；shim 收尾时可能拉起真 daemon（打开
/// 全局 home）。把 OMT_HOME 指向临时目录，绝不碰开发者真实的 ~/.omt；本文件
/// 只有一个测试会走到 daemon，串行锁保证环境变量不被并发修改。
static SERIAL: Mutex<()> = Mutex::new(());

fn isolated(tmp: &Path) -> std::sync::MutexGuard<'static, ()> {
    let guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let global = tmp.join("global-home");
    std::fs::create_dir_all(&global).unwrap();
    std::env::set_var("OMT_HOME", &global);
    guard
}

struct FakeDaemon {
    calls: Mutex<Vec<String>>,
}

impl FakeDaemon {
    fn rpc(&self, method: &str, _params: serde_json::Value) -> Result<serde_json::Value, String> {
        self.calls.lock().unwrap().push(method.to_string());
        Ok(match method {
            "node/get" => {
                serde_json::json!({"node": {"type": "ticket", "status": "todo"}, "children": [], "runs": []})
            }
            "run/create" => serde_json::json!({"run": {"runId": "RUN-1"}}),
            "run/get" => serde_json::json!({"run": {"status": "pending"}, "items": []}),
            _ => serde_json::json!({}),
        })
    }
}

fn setup(harness_body: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    let home_path = tmp.path().join("ws").join(".omt");
    std::fs::create_dir_all(&home_path).unwrap();
    let fake = write_script(tmp.path(), "fake-opencode", harness_body);
    omt_harness::harness::upsert_config(
        &runtime,
        "opencode",
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
    (tmp, runtime, home_path)
}

fn request<'a>(
    runtime: &'a Path,
    home_path: &'a Path,
    omt_bin: &'a Path,
) -> omt_exec::launch::LaunchRequest<'a> {
    omt_exec::launch::LaunchRequest {
        runtime_dir: runtime,
        omt_bin,
        home_id: "h_test",
        home_path,
        node_id: "TICKET-0001",
        harness: "opencode",
        unattended: true,
        model: None,
        resume_session: None,
        target_agent_id: None,
        max_duration: Duration::from_secs(60),
    }
}

#[test]
fn launch_returns_once_harness_is_spawned_and_exec_finishes_later() {
    // harness 跑 5s：launcher 在 pid 落盘后就返回，此刻 exec 必然尚未结束。
    // 不断言耗时——macOS 首次 exec 新写入脚本可能停顿数秒（F-BENCH-22）。
    let (t, runtime, home_path) = setup("sleep 5\necho done\nexit 0");
    let _serial = isolated(t.path());
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    let fake = FakeDaemon {
        calls: Mutex::new(vec![]),
    };
    let outcome = omt_exec::launch::start(
        &|m: &str, p: serde_json::Value| fake.rpc(m, p),
        &request(&runtime, &home_path, &omt),
    )
    .unwrap();
    assert_eq!(outcome.run_id, "RUN-1");
    let at_return = omt_exec::agents::get_agent(&runtime, "h_test", &outcome.agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        at_return.status,
        omt_exec::agents::AgentRecordStatus::Starting,
        "launcher 不应等执行结束"
    );
    assert!(at_return.execs[0].pid.is_some(), "就绪 = harness 已 spawn");
    // F-EXEC-2：launcher 的 shim 身份落档是关键步骤——pid 与内核起始标识
    // 都必须在（缺起始身份会被 wait_ready / 存活检查当成未知或已死）。
    assert!(
        at_return.execs[0].shim_pid.is_some(),
        "shim pid 应已落档: {:?}",
        at_return.execs[0]
    );
    assert!(
        at_return.execs[0].shim_start_id.is_some(),
        "shim 起始身份应已落档: {:?}",
        at_return.execs[0]
    );
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let agent = omt_exec::agents::get_agent(&runtime, "h_test", &outcome.agent_id)
            .unwrap()
            .unwrap();
        if agent.status == omt_exec::agents::AgentRecordStatus::Exited {
            break;
        }
        assert!(Instant::now() < deadline, "exec 未在 40s 内结束：{agent:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
    // 收尾时 shim 会为释放 RUN-1 的租约，用 --home <临时 home> 拉起临时 runtime
    // 里的真 daemon（假 run 不存在 → lease_release_pending）。状态写成终态在
    // 租约释放之前，所以要等 shim 进程退出后再停 daemon，否则 daemon 会在
    // stop 之后才被拉起而泄漏（review F2）
    let shim_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let agent = omt_exec::agents::get_agent(&runtime, "h_test", &outcome.agent_id)
            .unwrap()
            .unwrap();
        if !omt_exec::control::shim_alive(&agent.execs[0]) {
            break;
        }
        assert!(Instant::now() < shim_deadline, "shim 未在 30s 内退出");
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::process::Command::new(&omt)
        .args(["--runtime-dir", runtime.to_str().unwrap(), "daemon-stop"])
        .status();
}

#[test]
fn unspawnable_shim_fails_synchronously_and_cancels_new_run() {
    let (t, runtime, home_path) = setup("exit 0");
    let _serial = isolated(t.path());
    // omt_bin 指向不存在的路径 → spawn_shim 本身失败（shim 根本没起来），
    // launcher 走补偿：agent 转 Failed + cancel 本次新建的 run
    let omt = PathBuf::from(env!("CARGO_BIN_EXE_omt"));
    let fake = FakeDaemon {
        calls: Mutex::new(vec![]),
    };
    let mut req = request(&runtime, &home_path, &omt);
    let bogus = PathBuf::from("/nonexistent/omt");
    req.omt_bin = &bogus;
    let err =
        omt_exec::launch::start(&|m: &str, p: serde_json::Value| fake.rpc(m, p), &req).unwrap_err();
    assert!(err.contains("exec-shim") || err.contains("omt"), "{err}");
    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        calls.last().map(String::as_str),
        Some("run/control"),
        "应 cancel 新建 run：{calls:?}"
    );
}
