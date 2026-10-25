//! §49 P3 清理的 CLI 本地动词用例：Ctrl-C 可打断（退出 130 语义）、
//! `--home <相对路径>` 的 exec list 解析、Q-13 先判后声明。
mod common;
use common::TestCtx;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn omt_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_omt"))
}

/// §49「本地动词期间 Ctrl-C 无效」：`harness models` 对挂起的 harness
/// 二进制（模型列表探测 8s 超时）收到 SIGINT 后必须以默认信号语义立刻
/// 终止（shell 报 130）。修复前 real_run_with 装了 sigwait 处理但本地动词
/// 不登记 fd、不检查 CANCELED——SIGINT 被吞，CLI 跑满 8s 后以 exit 3 收场。
#[test]
fn ctrl_c_interrupts_harness_models_promptly() {
    let ctx = TestCtx::spawn_named("cli-sigint");
    // 假 pi：先落 marker（就绪信号），然后挂住不输出——模型列表探测只能等超时
    let marker = ctx.dir.path().join("pi-started");
    let fake = ctx.dir.path().join("fake-pi");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &fake,
            format!("#!/bin/sh\ntouch {}\nsleep 30\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    omt_harness::harness::upsert_config(
        &ctx.runtime_dir,
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
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .args(["harness", "models", "pi"])
        .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
        .process_group(0) // 收尾时整组清掉假 pi 的 sleep
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn omt harness models");
    // 就绪：假 pi 已被拉起（CLI 已进入模型列表探测）
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.is_file() {
        assert!(
            Instant::now() < deadline,
            "假 pi 10s 内未被拉起——CLI 未到达模型列表探测"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    // 4s 内必须退出（探测超时 8s + 余量：被吞掉的旧实现要跑满 8s）。
    // 每条路径都先收割 child（try_wait 得 Some 即已收割；超时分支先
    // SIGKILL 整组再 wait），不留僵尸。
    let deadline = Instant::now() + Duration::from_secs(4);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
            let _ = child.wait();
            panic!("SIGINT 后 4s 仍未退出——sigwait 吞掉了信号（§49 Ctrl-C 无效）");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    use std::os::unix::process::ExitStatusExt;
    assert!(
        status.signal().is_some(),
        "Ctrl-C 应按默认语义以信号终止（shell 报 130）：{status:?}"
    );
    // 清场：假 pi 的 sleep 与 CLI 同组，组内可能仍有残留
    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
}

/// §49「`omt --home .omt exec list` 报 invalid home id」：resolve_home_id 以
/// 「含不含 `/`」区分路径与 homeId，相对路径被当成 homeId。修复后按
/// resolve_home 的方式：先按 homeId 匹配已服务 home，再按规范路径匹配。
#[test]
fn exec_list_accepts_relative_home_path_like_exec_start() {
    let ctx = TestCtx::spawn_named("cli-relhome");
    let ws_root = ctx.dir.path().join("ws");
    let ws_home = ws_root.join(".omt");
    std::fs::create_dir_all(&ws_home).unwrap();
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ws_home.to_str().unwrap()]);
    common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("daemon");
    let out = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .arg("--home")
        .arg(".omt") // 相对路径（cwd = workspace 根），无 `/` 前缀
        .args(["exec", "list", "--json"])
        .current_dir(&ws_root)
        .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
        .output()
        .expect("run omt exec list");
    assert!(
        out.status.success(),
        "相对路径 --home 应像 exec start 一样解析到已服务 home：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["execs"], serde_json::json!([]));
}

/// §51 F-SP2-5(a)：磁盘上存在的 homeId，`exec list` 只读 `agents/<homeId>/`
/// ——无 daemon 时不得为解析 home 拉起 daemon（旧实现所有 homeId 都要求
/// daemon 的 served 列表，只读动词也会 spawn）。断言：成功列出且 runtime
/// 目录里没有 descriptor（没有任何 daemon 被拉起）。
#[test]
fn exec_list_by_disk_home_id_never_spawns_a_daemon() {
    let ctx = TestCtx::spawn_named("cli-disk-home");
    // 磁盘上的 home（曾服务过的形态：agents/h_x/ 存在即可——home_ids 只看目录）
    std::fs::create_dir_all(ctx.runtime_dir.join("agents").join("h_x")).unwrap();
    let out = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .arg("--home")
        .arg("h_x")
        .args(["exec", "list", "--json"])
        .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
        .output()
        .expect("run omt exec list");
    assert!(
        out.status.success(),
        "磁盘上的 homeId 应直接解析（不要求被服务）：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["execs"], serde_json::json!([]));
    assert!(
        common::Descriptor::read(&ctx.runtime_dir).is_none(),
        "只读动词不得为解析 home 拉起 daemon（§51 F-SP2-5）"
    );
}

/// §51 F-SP2-5(b)：idle 关闭的 home（磁盘上有 agents/<homeId>/，daemon 只服务
/// 另一个 home）——`exec list` 应照常列出，而不是 NOT_FOUND（旧实现要求
/// homeId 出现在 daemon 当前服务的 home 中）。
#[test]
fn exec_list_serves_an_idle_closed_home_from_disk() {
    let (ctx, homes) = TestCtx::spawn_with_homes("cli-idle-closed-home", 2);
    // home-01 曾被服务（agents 目录留存），现在不被服务（daemon 只开 home-00）
    std::fs::create_dir_all(ctx.runtime_dir.join("agents").join("home-01")).unwrap();
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", homes[0].to_str().unwrap()]);
    common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("daemon");
    let out = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .arg("--home")
        .arg("home-01")
        .args(["exec", "list", "--json"])
        .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
        .output()
        .expect("run omt exec list");
    assert!(
        out.status.success(),
        "idle 关闭的 home 应从磁盘解析（exec list 只读）：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["execs"], serde_json::json!([]));
}

/// §51 F-SP2-5(d)：输入不存在的 homeId（want=None）时不得匹配「自身路径
/// 也无法规范化」的已服务 home（目录已删）——旧实现 None == None 会把拼写
/// 错误静默解析到那个 home（exit 0 空列表）；必须是 NOT_FOUND（exit 3）。
#[test]
fn exec_list_with_unknown_home_id_is_not_found_even_when_a_served_home_dir_is_gone() {
    let ctx = TestCtx::spawn_named("cli-gone-home");
    let ws = ctx.dir.path().join("ws-home");
    std::fs::create_dir_all(&ws).unwrap();
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ws.to_str().unwrap()]);
    common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("daemon");
    // 服务中的 home 目录被删：daemon 的 served 列表仍含它（内存态），但其
    // 路径已无法 canonicalize——正是与 want=None 误匹配的形状
    std::fs::remove_dir_all(&ws).expect("remove served home dir");
    let out = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .arg("--home")
        .arg("h_typo")
        .args(["exec", "list", "--json"])
        .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
        .output()
        .expect("run omt exec list");
    assert_eq!(
        out.status.code(),
        Some(3),
        "拼写错误的 homeId 必须 NOT_FOUND：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let problem: serde_json::Value = serde_json::from_slice(out.stderr.trim_ascii_end()).unwrap();
    assert_eq!(problem["code"], "NOT_FOUND", "{problem}");
}

/// §49 Q-13「CLI 声明之后才判」：`omt --home <全局 home> exec start …` 对
/// 未被服务的已初始化全局 home，必须在 home/declare **之前**按路径拒绝
/// （POLICY_DENIED / workspaces），且 daemon 的服务清单里不留下该 home。
#[test]
fn cli_exec_start_judges_global_home_before_declaring() {
    let ctx = TestCtx::spawn_named("cli-q13-predeclare");
    // 「全局 home」：起一次 daemon 初始化（留下 omt.db）再停掉——此后不被服务
    let global = ctx.dir.path().join("g-home");
    std::fs::create_dir_all(&global).unwrap();
    {
        let mut first = common::DaemonProcess::spawn(&ctx, &["--home", global.to_str().unwrap()]);
        common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("daemon");
        first.kill();
        assert!(common::wait_descriptor_gone(
            &ctx.runtime_dir,
            Duration::from_secs(20)
        ));
    }
    // 工作 daemon 只服务 workspace home
    let ws = ctx.dir.path().join("ws-home");
    std::fs::create_dir_all(&ws).unwrap();
    let _daemon = common::DaemonProcess::spawn(&ctx, &["--home", ws.to_str().unwrap()]);
    common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).expect("daemon");

    let out = Command::new(omt_path())
        .arg("--runtime-dir")
        .arg(ctx.runtime_dir_str())
        .arg("--home")
        .arg(global.to_str().unwrap())
        .args(["exec", "start", "--harness", "pi", "--node", "TICKET-1"])
        .env("OMT_RUNTIME_DIR", ctx.runtime_dir_str())
        // CLI 侧把该路径认作全局 home（预判规则与 daemon 的 OMT_HOME 一致）
        .env("OMT_HOME", global.to_str().unwrap())
        .output()
        .expect("run omt exec start");
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let problem: serde_json::Value = serde_json::from_slice(out.stderr.trim_ascii_end()).unwrap();
    assert_eq!(problem["code"], "POLICY_DENIED", "{problem}");
    assert_eq!(problem["details"]["field"], "workspaces", "{problem}");

    // daemon 未声明该 home：handshake 的 homes[] 只有 workspace home
    let descriptor =
        common::wait_for_descriptor(&ctx.runtime_dir, Duration::from_secs(20)).unwrap();
    let mut client = common::TestClient::connect(&descriptor.endpoint).expect("connect");
    let handshake = client
        .call(
            "handshake/request",
            serde_json::json!({
                "protocolVersion": "1.0",
                "client": { "kind": "cli", "name": "cli-q13-check" },
            }),
        )
        .expect("helper handshake");
    let served = handshake["homes"].as_array().cloned().unwrap_or_default();
    assert!(
        !served
            .iter()
            .any(|h| h["path"].as_str().is_some_and(|p| p.contains("g-home"))),
        "被拒请求不得先声明 home（§49 先判后声明）：{served:?}"
    );
    assert_eq!(served.len(), 1, "只有 workspace home 被服务：{served:?}");
    let _ = Path::new(&ctx.runtime_dir);
}
