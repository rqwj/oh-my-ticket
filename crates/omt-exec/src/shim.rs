//! `omt exec-shim <homeId> <agentId> <execId>`（设计 §3.2 / KD-E1）：每 exec
//! 一个分离进程。读 spec → MCP 注入 → 写 prompt 文件 → spawn harness（事件
//! 写入 jsonl）→ 等终态 → 收尾（含租约释放）→ 退出。发起方此后可以退出。
//!
//! 停止：SIGTERM / SIGINT 置标志，主循环转成 `exec_stop`（TERM → 宽限 → KILL
//! 升级沿用执行器原逻辑）。信号处理函数在 exec 时被内核重置，harness 不受影响。
//! **约束（review F1）**：shim 绝不能运行在 `omt` CLI 的信号屏蔽之下——CLI 的
//! `signal::install` 会在所有线程屏蔽 SIGTERM/SIGINT 并由 sigwait 线程消费，
//! 本模块的处理函数将永远收不到信号。CLI 必须在安装信号处理**之前**分派
//! `exec-shim`（见 Task 8 Step 6）。
//!
//! home 保活（review F5）：desktop 关闭后没有别的面访问 workspace home，静默
//! idleQuietMs 后会被 daemon 关闭，harness 经 `omt mcp` 的认领 / 汇报随之失败
//! （mcp 按设计不能 declare）。harness 存活期间，shim 定期经普通（非 admin）
//! 连接发一个只读 RPC，让 home 保持打开；连接时若 home 已关闭会自动 declare。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agents::{self, AgentAttachment, AgentRecordStatus};
use crate::daemon_link::Connector;
use crate::executor::{ExecutionManager, RpcFn, SpawnRequest};
use crate::layout::{self, ExecFile};
use crate::spec::LaunchSpec;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_sig: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_stop_handler() {
    // SAFETY: 处理函数只做一次原子写，满足 async-signal-safe
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_stop_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_stop_signal as *const () as libc::sighandler_t,
        );
    }
}

/// 保活间隔（review R2-2）：从 daemon 读的同一个配置推导——`<runtime>/daemon.json`
/// 的 idleQuietMs（缺省 30 分钟），取其 1/3 且不超过 5 分钟；idleQuietMs 为 0
/// （daemon 不做 idle 退出）时不保活。环境变量 `OMT_SHIM_KEEPALIVE_MS` 优先（测试用）。
fn keepalive_every(runtime_dir: &Path) -> Option<Duration> {
    keepalive_every_from(
        runtime_dir,
        std::env::var("OMT_SHIM_KEEPALIVE_MS").ok().as_deref(),
    )
}

fn keepalive_every_from(runtime_dir: &Path, override_ms: Option<&str>) -> Option<Duration> {
    if let Some(ms) = override_ms.and_then(|v| v.parse::<u64>().ok()) {
        return Some(Duration::from_millis(ms));
    }
    let idle_ms = std::fs::read_to_string(runtime_dir.join("daemon.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|config| config["idleQuietMs"].as_i64())
        .unwrap_or(1_800_000);
    if idle_ms <= 0 {
        return None;
    }
    Some(Duration::from_millis(
        (idle_ms as u64 / 3).min(5 * 60 * 1000),
    ))
}

/// 进程入口：安装停止信号处理后执行。返回进程退出码。
pub fn run(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Connector,
    keepalive: Connector,
) -> i32 {
    install_stop_handler();
    execute(runtime_dir, home_id, agent_id, exec_id, lease, keepalive)
}

/// shim 主体（不装信号处理，单测直接调用）；0 = 走完收尾，1 = 启动阶段失败。
pub fn execute(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Connector,
    keepalive: Connector,
) -> i32 {
    match run_inner(runtime_dir, home_id, agent_id, exec_id, lease, keepalive) {
        Ok(()) => 0,
        Err(detail) => {
            eprintln!("exec-shim {exec_id}: {detail}");
            let _ = agents::update_exec(runtime_dir, home_id, agent_id, exec_id, |exec| {
                if exec.failure_detail.is_none() {
                    exec.failure_detail = Some(detail.clone());
                }
            });
            let _ = agents::update_agent_status(
                runtime_dir,
                home_id,
                agent_id,
                AgentRecordStatus::Failed,
                AgentAttachment::None,
            );
            1
        }
    }
}

fn run_inner(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Connector,
    keepalive: Connector,
) -> Result<(), String> {
    layout::validate_segment("exec id", exec_id)?;
    let agent_dir = layout::agent_dir(runtime_dir, home_id, agent_id)?;
    let spec = LaunchSpec::read(&layout::exec_file(&agent_dir, exec_id, ExecFile::Spec))?;
    if spec.home_id != home_id || spec.agent_id != agent_id || spec.exec_id != exec_id {
        return Err("spec 与命令行参数不一致".into());
    }
    let plan = omt_harness::harness::resolve_launch(runtime_dir, &spec.harness)?;
    let prompt_file =
        omt_harness::harness::prompt_file_path(&agent_dir, exec_id, plan.prompt_channel);
    let args = omt_harness::harness::compose_launch_args(
        &plan,
        spec.unattended,
        &spec.prompt,
        prompt_file.as_deref(),
        spec.model.as_deref(),
        spec.resume_session.as_deref(),
    );
    let env_additions = plan.sanitized_env();
    let cwd = crate::executor::workspace_root_of(&spec.home_path);
    let mcp = crate::mcp_injection::prepare(
        &spec.omt_path,
        runtime_dir,
        &cwd,
        &spec.harness,
        exec_id,
        false,
    )?;
    if let Some(path) = &prompt_file {
        if let Err(e) = std::fs::write(path, &spec.prompt) {
            let _ = crate::mcp_injection::release(runtime_dir, &mcp);
            return Err(format!("write prompt file: {e}"));
        }
    }

    let manager = ExecutionManager::with_max_duration(Duration::from_secs(spec.max_duration_secs));
    manager.set_lease_release(lease_releaser(
        runtime_dir.to_path_buf(),
        agent_id.to_string(),
        spec.home_path.clone(),
        lease,
    ));
    let request = SpawnRequest {
        exec_id: exec_id.to_string(),
        agent_id: agent_id.to_string(),
        home_id: home_id.to_string(),
        home_path: spec.home_path.clone(),
        harness: spec.harness.clone(),
        node_id: spec.node_id.clone(),
        run_id: spec.run_id.clone(),
        program: plan.program.clone(),
        args,
        env_additions,
        mcp,
        transcript_dir: agent_dir.clone(),
    };
    // 事件已由执行器写入 jsonl（KD-E3）；进程内 sink 无人消费
    let session = manager.spawn(runtime_dir, request, Arc::new(|_| {}))?;
    let done = Arc::new(AtomicBool::new(false));
    let keeper = spawn_keepalive(
        keepalive_every(runtime_dir),
        spec.home_path.clone(),
        home_id.to_string(),
        keepalive,
        done.clone(),
    );
    loop {
        if session.terminal().is_some() {
            done.store(true, Ordering::SeqCst);
            let _ = keeper.join();
            return Ok(());
        }
        if STOP_REQUESTED.swap(false, Ordering::SeqCst) {
            manager.exec_stop(exec_id)?;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// review F5：harness 存活期间保持 workspace home 打开。连接按需建立，失败就
/// 丢弃，下一轮重连（重连时 DaemonLink::connect 会在 home 已被关闭时自动
/// declare）。`run/list` 是只读 RPC，任何到达 home actor 的请求都会重置它的
/// 静默计时。homeId 用 spec 里的值——connect 已校验它就是 daemon 为该路径
/// 分配的 id（review R2-3 / R2-8），否则请求会在到达 home actor 前被拒，
/// 起不到保活作用。`every` 为 None（daemon 不做 idle 退出）时不保活。
fn spawn_keepalive(
    every: Option<Duration>,
    home_path: PathBuf,
    home_id: String,
    connector: Connector,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Some(every) = every else {
            return;
        };
        let mut rpc: Option<RpcFn> = None;
        loop {
            let deadline = Instant::now() + every;
            while Instant::now() < deadline {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if rpc.is_none() {
                rpc = connector(&home_path, &home_id).ok();
            }
            let alive = rpc
                .as_ref()
                .map(|call| call("run/list", serde_json::json!({ "homeId": home_id })).is_ok())
                .unwrap_or(false);
            if !alive {
                rpc = None;
            }
        }
    })
}

/// 终态租约释放：按需连接 daemon（长时间执行期间 daemon 可能已 idle 退出；
/// 连接时保证 home 在凭据范围内，review F4）。失败只记待对账标记，不影响
/// 收尾的其余步骤。
fn lease_releaser(
    runtime_dir: PathBuf,
    agent_id: String,
    home_path: PathBuf,
    connector: Connector,
) -> crate::executor::LeaseRelease {
    Arc::new(move |home_id: &str, run_id: &str, exec_id: &str| {
        let outcome = connector(&home_path, home_id).and_then(|rpc: RpcFn| {
            crate::executor::release_items_for_exec(&*rpc, home_id, run_id, exec_id)
        });
        if let Err(err) = outcome {
            eprintln!("exec-shim {exec_id}: 租约释放失败（{err}），留待对账");
            let _ = agents::update_exec(&runtime_dir, home_id, &agent_id, exec_id, |exec| {
                exec.lease_release_pending = true;
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// 准备：harness 配置（Manual 指向假脚本）、home、已注册的 agent 与 spec。
    fn rig(
        harness_body: &str,
        run_id: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf, crate::spec::LaunchSpec) {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run");
        let home_path = tmp.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let fake = write_script(tmp.path(), "fake-pi", harness_body);
        let fake_omt = write_script(tmp.path(), "omt", "echo omt");
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
        let agent = crate::agents::register_agent(
            &runtime,
            "h_test",
            "pi",
            crate::agents::NewExec {
                exec_id: "exec-s".into(),
                node_id: None,
                run_id: run_id.map(str::to_string),
                binary_path: None,
                binary_args: vec![],
                model: None,
            },
        )
        .unwrap();
        let mut spec = crate::spec::sample(&home_path, &fake_omt);
        spec.agent_id = agent.id.clone();
        spec.run_id = run_id.map(str::to_string);
        let dir = crate::layout::agent_dir(&runtime, "h_test", &agent.id).unwrap();
        spec.write(&crate::layout::exec_file(
            &dir,
            "exec-s",
            crate::layout::ExecFile::Spec,
        ))
        .unwrap();
        (tmp, runtime, spec)
    }

    fn recording_connector(calls: Arc<Mutex<Vec<String>>>) -> Connector {
        Arc::new(move |_home: &Path, _home_id: &str| {
            let calls = calls.clone();
            let rpc: RpcFn = Arc::new(move |method: &str, _params: serde_json::Value| {
                calls.lock().unwrap().push(method.to_string());
                Ok(serde_json::json!({ "items": [] }))
            });
            Ok(rpc)
        })
    }

    /// 保活在单测里不连 daemon（保活间隔远大于测试时长，不会触发）。
    fn idle_keepalive() -> Connector {
        Arc::new(|_: &Path, _: &str| Err("keepalive not used in unit tests".to_string()))
    }

    /// review R2-2：保活间隔从 daemon.json 的 idleQuietMs 推导（≤ 5 分钟、取 1/3），
    /// 0 表示 daemon 不做 idle 退出、无需保活；环境变量优先。
    #[test]
    fn keepalive_interval_follows_daemon_config() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path();
        assert_eq!(
            keepalive_every_from(rt, None),
            Some(Duration::from_secs(300)),
            "缺省 30 分钟 → 5 分钟"
        );
        std::fs::write(rt.join("daemon.json"), r#"{"idleQuietMs": 60000}"#).unwrap();
        assert_eq!(
            keepalive_every_from(rt, None),
            Some(Duration::from_millis(20_000))
        );
        std::fs::write(rt.join("daemon.json"), r#"{"idleQuietMs": 0}"#).unwrap();
        assert_eq!(keepalive_every_from(rt, None), None);
        assert_eq!(
            keepalive_every_from(rt, Some("250")),
            Some(Duration::from_millis(250))
        );
    }

    #[test]
    fn shim_runs_harness_to_completion_and_archives() {
        let (_t, runtime, spec) = rig("echo '{\"type\":\"agent_end\"}'\nexit 0", Some("RUN-1"));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let code = execute(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            recording_connector(calls.clone()),
            idle_keepalive(),
        );
        assert_eq!(code, 0);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(agent.status, crate::agents::AgentRecordStatus::Exited);
        assert_eq!(agent.execs[0].exit_code, Some(0));
        assert!(agent.execs[0].pid.is_some());
        let dir = crate::layout::agent_dir(&runtime, "h_test", &spec.agent_id).unwrap();
        let jsonl = std::fs::read_to_string(crate::layout::exec_file(
            &dir,
            "exec-s",
            crate::layout::ExecFile::Jsonl,
        ))
        .unwrap();
        assert!(jsonl.lines().last().unwrap().contains("\"exit\""));
        assert_eq!(
            calls.lock().unwrap().first().map(String::as_str),
            Some("run/get"),
            "收尾应查询租约"
        );
    }

    /// U11（原 executor 侧 dispatch_captures_harness_session_id_end_to_end
    /// 的移植——spawn 已随执行面移入 shim）：假 pi 流携带 session id →
    /// 终态归档 exec.harness_session_id 并镜像 agent.last_session_id。
    #[test]
    fn shim_captures_harness_session_id_and_mirrors_agent() {
        let (_t, runtime, spec) = rig(
            "echo '{\"type\":\"session\",\"version\":3,\"id\":\"ses_fake123\"}'\nexit 0",
            Some("RUN-1"),
        );
        let code = execute(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            recording_connector(Default::default()),
            idle_keepalive(),
        );
        assert_eq!(code, 0);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            agent.execs[0].harness_session_id.as_deref(),
            Some("ses_fake123"),
            "终态应归档捕获的 session id"
        );
        assert_eq!(
            agent.last_session_id.as_deref(),
            Some("ses_fake123"),
            "update_exec 应镜像 agent.last_session_id"
        );
    }

    #[test]
    fn shim_marks_failed_when_spec_missing() {
        let (_t, runtime, spec) = rig("exit 0", None);
        let dir = crate::layout::agent_dir(&runtime, "h_test", &spec.agent_id).unwrap();
        std::fs::remove_file(crate::layout::exec_file(
            &dir,
            "exec-s",
            crate::layout::ExecFile::Spec,
        ))
        .unwrap();
        let code = execute(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            recording_connector(Default::default()),
            idle_keepalive(),
        );
        assert_eq!(code, 1);
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(agent.status, crate::agents::AgentRecordStatus::Failed);
        assert!(agent.execs[0]
            .failure_detail
            .as_deref()
            .unwrap_or("")
            .contains("spec"));
    }

    #[test]
    fn unreachable_daemon_leaves_lease_release_pending() {
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        let failing: Connector = Arc::new(|_: &Path, _: &str| Err("daemon down".to_string()));
        assert_eq!(
            execute(
                &runtime,
                "h_test",
                &spec.agent_id,
                "exec-s",
                failing,
                idle_keepalive()
            ),
            0
        );
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert!(agent.execs[0].lease_release_pending);
    }
}
