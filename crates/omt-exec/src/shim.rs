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

/// F-EXEC-10：租约释放序列（连接 + run/get + 逐项 report/retry）的有界等待
/// 上限——wedged daemon 不得挂死收尾（finish_terminal 同步等它）。生产默认
/// 30s（与 daemon_link 的 socket 超时同量级，这是同一防线的收尾级兜底）；
/// 环境变量 `OMT_SHIM_LEASE_TIMEOUT_MS` 覆盖（测试用）。
const DEFAULT_LEASE_TIMEOUT: Duration = Duration::from_secs(30);

/// F-EXEC-10：终态后等 keepalive 线程退出的上限。keeper 的连接调用受
/// daemon_link 的 socket 超时约束、休眠段每 100ms 查 done——2s 覆盖正常
/// 退出；超时 detach（shim 进程退出时线程随之结束），不再无界 join。
const KEEPER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

fn lease_timeout() -> Duration {
    std::env::var("OMT_SHIM_LEASE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_LEASE_TIMEOUT)
}

/// F-EXEC-10：把可能无限阻塞的闭包放到工作线程里跑，主调有界等待。超时
/// （或工作线程未交付结果即结束）按 Err 返回——调用方走既有的失败路径
///（租约释放 → lease_release_pending），收尾绝不因 wedged 对端挂死。
/// 工作线程超时后随句柄丢弃而 detach。每次租约释放（每 exec 恰一次）才
/// 起一个线程，不在热路径。
fn run_bounded<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
    timeout: Duration,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("有界等待超时（{timeout:?}）"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("有界等待中断（工作线程未交付结果）".to_string())
        }
    }
}

/// F-EXEC-10：有界 join——超时返回 false，调用方丢弃句柄即 detach；线程靠
/// done 标志自行退出（下一次 100ms 检查点，或当前连接调用的 socket 超时后）。
fn join_bounded(handle: std::thread::JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.join().is_ok()
}

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
            // F-EXEC-10：先置 done 再有界等 keeper 退出——wedged daemon 下
            // keeper 的连接调用可能长时间不返回，无界 join 会把 shim 挂死在
            // 退出路径；超时 detach（连接受 socket 超时约束，线程自行结束）
            done.store(true, Ordering::SeqCst);
            if !join_bounded(keeper, KEEPER_JOIN_TIMEOUT) {
                eprintln!(
                    "exec-shim {exec_id}: keepalive 线程 {} 内未退出，detach（其连接受 socket 超时约束，将自行结束）",
                    KEEPER_JOIN_TIMEOUT.as_secs(),
                );
            }
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
/// 收尾的其余步骤。F-EXEC-10：整个释放序列经 `run_bounded` 有界等待——
/// finish_terminal 同步等本回调，wedged daemon（accept 但永不回复）不得
/// 借此挂死 shim（exec_active 永真、followers 不结束、stop_exec 空转）。
fn lease_releaser(
    runtime_dir: PathBuf,
    agent_id: String,
    home_path: PathBuf,
    connector: Connector,
) -> crate::executor::LeaseRelease {
    Arc::new(move |home_id: &str, run_id: &str, exec_id: &str| {
        let home_id = home_id.to_string();
        let run_id = run_id.to_string();
        let exec_id = exec_id.to_string();
        // 工作闭包须 'static（跑在独立线程里）：捕获值克隆进去
        let release = {
            let connector = connector.clone();
            let home_path = home_path.clone();
            let home_id = home_id.clone();
            let run_id = run_id.clone();
            let exec_id = exec_id.clone();
            move || {
                connector(&home_path, &home_id).and_then(|rpc: RpcFn| {
                    crate::executor::release_items_for_exec(&*rpc, &home_id, &run_id, &exec_id)
                })
            }
        };
        let outcome = run_bounded(release, lease_timeout());
        if let Err(err) = outcome {
            eprintln!("exec-shim {exec_id}: 租约释放失败（{err}），留待对账");
            let _ = agents::update_exec(&runtime_dir, &home_id, &agent_id, &exec_id, |exec| {
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

    /// F-EXEC-10：run_bounded——阻塞超过上限的闭包在界内按 Err 返回；
    /// 正常闭包透传 Ok/Err 载荷。
    #[test]
    fn run_bounded_returns_err_within_bound_on_slow_closure() {
        let started = Instant::now();
        let err = run_bounded(
            || {
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            },
            Duration::from_millis(100),
        )
        .expect_err("慢闭包必须在界内超时报错");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时返回应远早于闭包完成：{:?}",
            started.elapsed()
        );
        assert!(err.contains("超时"), "{err}");
        assert_eq!(run_bounded(|| Ok(42), Duration::from_secs(5)).unwrap(), 42);
        assert_eq!(
            run_bounded::<()>(|| Err("daemon down".to_string()), Duration::from_secs(5))
                .unwrap_err(),
            "daemon down",
            "闭包自身的 Err 载荷必须透传（不得误报超时）"
        );
    }

    /// F-EXEC-10：join_bounded——慢线程超时返回 false（调用方 detach）；
    /// 已完成线程正常 join 返回 true。
    #[test]
    fn join_bounded_detaches_slow_thread() {
        let slow = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(60)));
        let started = Instant::now();
        assert!(!join_bounded(slow, Duration::from_millis(100)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "有界 join 不得等满慢线程：{:?}",
            started.elapsed()
        );
        let quick = std::thread::spawn(|| ());
        assert!(join_bounded(quick, Duration::from_secs(5)));
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

    /// F-EXEC-10：wedged daemon（连接建立但 rpc 永不返回）不得挂死收尾——
    /// 租约释放序列必须有界等待（生产默认 30s；`OMT_SHIM_LEASE_TIMEOUT_MS`
    /// 调小），超时按失败处理：execute() 完成 + lease_release_pending 置位，
    /// 留给 U10 对账。
    #[test]
    fn wedged_lease_rpc_completes_with_pending_marker() {
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        let wedged: Connector = Arc::new(|_: &Path, _: &str| {
            let rpc: RpcFn = Arc::new(|_: &str, _: serde_json::Value| {
                std::thread::sleep(Duration::from_secs(300));
                Ok(serde_json::json!({ "items": [] }))
            });
            Ok(rpc)
        });
        std::env::set_var("OMT_SHIM_LEASE_TIMEOUT_MS", "200");
        let started = Instant::now();
        let code = execute(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            wedged,
            idle_keepalive(),
        );
        std::env::remove_var("OMT_SHIM_LEASE_TIMEOUT_MS");
        assert_eq!(code, 0, "wedged rpc 下收尾仍须完成");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "execute 应在租约超时后随即返回，实际 {:?}",
            started.elapsed()
        );
        let agent = crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
            .unwrap()
            .unwrap();
        assert!(
            agent.execs[0].lease_release_pending,
            "超时的释放必须标记待对账：{:?}",
            agent.execs[0]
        );
    }
}
