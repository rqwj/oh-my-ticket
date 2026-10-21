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
use crate::grants::TempGrant;
use crate::layout::{self, ExecFile};
use crate::spec::LaunchSpec;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// P3-b：租约释放的 daemon 连接 seam——连接 + 临时 admin 授权**成对**返回。
/// 与 Connector（admin_connector）的差别：TempGrant 显式交给调用方持有，
/// 释放超时（run_bounded 放弃其工作线程）后仍能在本侧立即撤销授权，
/// 不留 pid 复用可乘之机的临时 admin 窗口。
pub type Leaser = Arc<dyn Fn(&Path, &str) -> Result<(RpcFn, TempGrant), String> + Send + Sync>;

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
    lease: Leaser,
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
    lease: Leaser,
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

/// F-EXEC-2：身份自写是 shim 的第一个动作——参数校验之后、任何 spec 相关
/// 工作之前的最早安全点（home/agent 段校验由 update_exec 内部完成）。把
/// 自身 pid + 内核起始标识写进 exec 记录：launcher 在 spawn 后也写同一笔
/// （F-EXEC-2 起改为关键步骤），此处自写治愈场景 A——launcher 侧写失败或
/// desktop 在 spawn 与写之间退出时，没有这笔记录的话宽限期内 stop 是
/// no-op、宽限期后被误判 SHIM_LOST/Orphan 并在执行中释放租约；场景 B
/// （launcher 读身份失败留下 start_id None）也由此补齐。本笔失败不阻断
/// （记录不存在等错误由后续步骤的失败臂另行落档）。
///
/// P3-a：计算出的 start_id 为 None（读取失败）时**不得**覆盖 launcher 已写
/// 的 Some——覆盖会让存活校验从此永假（stop 变 no-op、宽限后误判 Orphan）。
/// 只在 Some 时写 shim_start_id；shim_pid 仅在当前为 None 时写（生产里与
/// launcher 写的是同一 pid，无行为差）。
fn write_own_identity(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    pid: u32,
    start_id: Option<String>,
) -> Result<bool, String> {
    agents::update_exec(runtime_dir, home_id, agent_id, exec_id, |e| {
        if e.shim_pid.is_none() {
            e.shim_pid = Some(pid);
        }
        if start_id.is_some() {
            e.shim_start_id.clone_from(&start_id);
        }
    })
}

fn run_inner(
    runtime_dir: &Path,
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    lease: Leaser,
    keepalive: Connector,
) -> Result<(), String> {
    layout::validate_segment("exec id", exec_id)?;
    let _ = write_own_identity(
        runtime_dir,
        home_id,
        agent_id,
        exec_id,
        std::process::id(),
        crate::executor::process_start_identity(std::process::id()).ok(),
    );
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
    manager.set_lease_release(lease_releaser(spec.home_path.clone(), lease));
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
/// 连接时保证 home 在凭据范围内，review F4）。失败只记待对账标记（由
/// finish_terminal 落 pending，F-EXEC-13），不影响收尾的其余步骤。
/// F-EXEC-10：释放序列经 `run_bounded` 有界等待——finish_terminal 同步等本
/// 回调，wedged daemon（accept 但永不回复）不得借此挂死 shim（exec_active
/// 永真、followers 不结束、stop_exec 空转）。连接自身受 daemon_link 的
/// socket 超时约束，不会挂死，故在有界等待之外建立。
/// P3-b：TempGrant 持有在 run_bounded **之外**——超时路径 run_bounded 会
/// 放弃其工作线程，授权若只被线程内的值持有就泄漏到进程结束（pid 复用
/// 即提权窗口）；成功/失败/超时三条路径都在本侧 drop(grant) 立即撤销。
fn lease_releaser(home_path: PathBuf, leaser: Leaser) -> crate::executor::LeaseRelease {
    Arc::new(move |home_id: &str, run_id: &str, exec_id: &str| {
        let home_id = home_id.to_string();
        let run_id = run_id.to_string();
        let exec_id = exec_id.to_string();
        let outcome = match leaser(&home_path, &home_id) {
            // 连接/授权失败：TempGrant 未建立（add 失败不产生守卫），无需界内等待
            Err(err) => Err(err),
            Ok((rpc, grant)) => {
                // 工作闭包须 'static（跑在独立线程里）：rpc 克隆进去，grant 留下
                let bounded = {
                    let rpc = rpc.clone();
                    let home_id = home_id.clone();
                    let run_id = run_id.clone();
                    let exec_id = exec_id.clone();
                    move || {
                        crate::executor::release_items_for_exec(&*rpc, &home_id, &run_id, &exec_id)
                    }
                };
                let result = run_bounded(bounded, lease_timeout());
                drop(grant); // 成功/失败/超时统一在此撤销临时授权
                result
            }
        };
        if let Err(err) = &outcome {
            eprintln!("exec-shim {exec_id}: 租约释放失败（{err}），留待对账");
        }
        outcome
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

    /// P3-b：测试假 Leaser 在真实临时 runtime 上建真实 TempGrant——Drop 即
    /// 撤销授权，不污染同进程其他测试；同时按 method 记账供断言。
    fn recording_leaser(calls: Arc<Mutex<Vec<String>>>, runtime: &Path) -> Leaser {
        let runtime = runtime.to_path_buf();
        Arc::new(move |_home: &Path, _home_id: &str| {
            let calls = calls.clone();
            let rpc: RpcFn = Arc::new(move |method: &str, _params: serde_json::Value| {
                calls.lock().unwrap().push(method.to_string());
                Ok(serde_json::json!({ "items": [] }))
            });
            let grant = TempGrant::add(&runtime, &format!("cli:{}", std::process::id()))?;
            Ok((rpc, grant))
        })
    }

    /// 保活在单测里不连 daemon（保活间隔远大于测试时长，不会触发）。
    fn idle_keepalive() -> Connector {
        Arc::new(|_: &Path, _: &str| Err("keepalive not used in unit tests".to_string()))
    }

    /// `OMT_SHIM_LEASE_TIMEOUT_MS` 是进程级 env——并行测试互相踩 set/remove
    /// 会把另一方的界放大回 30s 默认（其「界内完成」断言随即失真）。所有
    /// 改它的测试经此锁串行。
    static LEASE_TIMEOUT_ENV_LOCK: Mutex<()> = Mutex::new(());

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
            recording_leaser(calls.clone(), &runtime),
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
            recording_leaser(Default::default(), &runtime),
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

    /// P3-a：自身份写入不得用 None 覆盖 launcher 已落的 Some——身份计算失败
    /// （computed None）时记录保持原值（修复前 None 会把好值抹掉，存活校验
    /// 从此永假）；Some 时正常写入；shim_pid 仅在当前为 None 时写（生产里
    /// 与 launcher 写的是同一值，写不写无差）。
    #[test]
    fn identity_write_preserves_existing_start_id_when_compute_fails() {
        let (_t, runtime, spec) = rig("exit 0", None);
        let read_exec = || {
            crate::agents::get_agent(&runtime, "h_test", &spec.agent_id)
                .unwrap()
                .unwrap()
                .execs[0]
                .clone()
        };
        // launcher 已写好身份；shim 自算失败（None）
        crate::agents::update_exec(&runtime, "h_test", &spec.agent_id, "exec-s", |e| {
            e.shim_pid = Some(4242);
            e.shim_start_id = Some("macos:9:9".into());
        })
        .unwrap();
        write_own_identity(&runtime, "h_test", &spec.agent_id, "exec-s", 4242, None).unwrap();
        let exec = read_exec();
        assert_eq!(
            exec.shim_start_id.as_deref(),
            Some("macos:9:9"),
            "None 不得覆盖 Some: {exec:?}"
        );
        assert_eq!(exec.shim_pid, Some(4242));
        // Some 时正常写入
        write_own_identity(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            4242,
            Some("macos:1:1".into()),
        )
        .unwrap();
        assert_eq!(read_exec().shim_start_id.as_deref(), Some("macos:1:1"));
        // pid 仅在当前为 None 时写（已有 4242 → 传入 999 不覆盖）
        write_own_identity(&runtime, "h_test", &spec.agent_id, "exec-s", 999, None).unwrap();
        assert_eq!(
            read_exec().shim_pid,
            Some(4242),
            "shim_pid 只在空时写: {:?}",
            read_exec()
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
            recording_leaser(Default::default(), &runtime),
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
        // F-EXEC-2(1)：身份自写是 shim 的第一个动作——即使随后的 spec 读取就
        // 失败，exec 记录里也必须有它自己的 pid + 起始身份（单测在进程内跑，
        // std::process::id() 即测试进程自身，起始标识必然可读）。否则 launcher
        // 侧落档失败 / desktop 提前退出时，停止在宽限期内是 no-op、宽限后被
        // 误判 SHIM_LOST/Orphan 并释放租约。
        assert!(
            agent.execs[0].shim_pid.is_some(),
            "shim 应先落自身 pid（F-EXEC-2）: {:?}",
            agent.execs[0]
        );
        assert!(
            agent.execs[0].shim_start_id.is_some(),
            "shim 应先落自身起始身份（F-EXEC-2）: {:?}",
            agent.execs[0]
        );
    }

    #[test]
    fn unreachable_daemon_leaves_lease_release_pending() {
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        let failing: Leaser = Arc::new(|_: &Path, _: &str| Err("daemon down".to_string()));
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
        let env_guard = LEASE_TIMEOUT_ENV_LOCK.lock().unwrap();
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        let rt = runtime.clone();
        let wedged: Leaser = Arc::new(move |_: &Path, _: &str| {
            let rpc: RpcFn = Arc::new(|_: &str, _: serde_json::Value| {
                std::thread::sleep(Duration::from_secs(300));
                Ok(serde_json::json!({ "items": [] }))
            });
            let grant = TempGrant::add(&rt, "cli:111111")?;
            Ok((rpc, grant))
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
        drop(env_guard);
    }

    /// P3-b：租约释放超时（run_bounded 放弃其工作线程）后，临时 admin 授权
    /// 必须已被撤销——admin-grants.json 里不再有该 principal、
    /// exec/temp-grants.json 登记表已清。修复前授权只被工作线程内的守卫
    /// 持有，超时 detach 后泄漏到下次 desktop 启动清扫，pid 复用即提权窗口。
    #[test]
    fn lease_timeout_revokes_temp_grant_immediately() {
        let env_guard = LEASE_TIMEOUT_ENV_LOCK.lock().unwrap();
        let (_t, runtime, spec) = rig("exit 0", Some("RUN-1"));
        let rt = runtime.clone();
        let wedged: Leaser = Arc::new(move |_: &Path, _: &str| {
            let rpc: RpcFn = Arc::new(|_: &str, _: serde_json::Value| {
                std::thread::sleep(Duration::from_secs(300));
                Ok(serde_json::json!({ "items": [] }))
            });
            let grant = TempGrant::add(&rt, "cli:222222")?;
            Ok((rpc, grant))
        });
        std::env::set_var("OMT_SHIM_LEASE_TIMEOUT_MS", "200");
        let code = execute(
            &runtime,
            "h_test",
            &spec.agent_id,
            "exec-s",
            wedged,
            idle_keepalive(),
        );
        std::env::remove_var("OMT_SHIM_LEASE_TIMEOUT_MS");
        assert_eq!(code, 0);
        let principals = crate::grants::principals(&runtime);
        assert!(
            !principals.contains(&"cli:222222".to_string()),
            "超时后临时授权必须立即撤销: {principals:?}"
        );
        let registry = std::fs::read_to_string(runtime.join("exec").join("temp-grants.json"))
            .unwrap_or_default();
        assert!(
            !registry.contains("cli:222222"),
            "临时登记表必须已清: {registry}"
        );
        drop(env_guard);
    }
}
