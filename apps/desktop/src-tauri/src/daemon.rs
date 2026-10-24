//! U9 (R12-R14, KD5): desktop daemon lifecycle — the app is a PEER member
//! of the shared multi-surface runtime, not the daemon's private guardian.
//!
//! Contract:
//! - discover-or-spawn: a live descriptor (pid alive AND endpoint
//!   reachable) is reused as-is; otherwise the sidecar daemon is spawned
//!   DETACHED (its own process group) so window close never kills it —
//!   `RunEvent::ExitRequested` semantics: exit means exit the APP.
//! - handshake kind:"desktop" with the default scoped operations (same
//!   shape every surface gets; admin capability comes only from the
//!   out-of-band grants file).
//! - admin grant self-registration is single-entry REPLACEMENT (never
//!   append): one desktop principal entry, refreshed to the current pid
//!   on every launch — pid-recycled stale entries are dropped, bounding
//!   the privilege-inheritance surface (see plan Risks).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use omt_client::{Client, Descriptor, EnrollOptions, Enrollment};

/// Daemon status projection for the settings page (`daemon status`
/// command) and lifecycle decisions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<i64>,
    pub generation: Option<i64>,
    pub endpoint: Option<String>,
    pub spawned_by_us: bool,
}

/// Resolve a sidecar binary by name: sibling of the running executable
/// (bundled .app AND dev target dir — current_exe().parent(), never
/// resource_dir, per the U8 research contract). U3 泛化：`omt` CLI 作为
/// 第二 sidecar（MCP 桥）共用同一解析约定。
pub fn sidecar_path(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "current_exe has no parent dir".to_string())?;
    let candidate = dir.join(name);
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(format!("no sidecar {name} next to {}", exe.display()))
    }
}

/// `omt mcp` sidecar 的绝对路径（MCP config 的 command 指向它）。
pub fn omt_sidecar_path() -> Result<PathBuf, String> {
    sidecar_path("omt")
}

/// The shared per-user runtime dir (same resolution rule as every other
/// surface — see docs/runtime/config.md).
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OMT_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".omt").join("run")
}

/// Live descriptor = pid alive AND endpoint connectable.
pub fn live_descriptor(dir: &Path) -> Option<Descriptor> {
    let descriptor = omt_client::read_descriptor(dir)?;
    if omt_client::pid_live(descriptor.pid) && omt_client::endpoint_live(&descriptor.endpoint) {
        Some(descriptor)
    } else {
        None
    }
}

/// Spawn the sidecar daemon detached (own process group) and wait for its
/// descriptor. Detachment is THE R14 guarantee: the daemon outlives the
/// app; an idle watchdog exits it when truly unused.
#[cfg(unix)]
pub fn spawn_sidecar(dir: &Path) -> Result<Descriptor, String> {
    use std::os::unix::process::CommandExt;
    let binary = sidecar_path("omt-daemon")?;
    let mut command = Command::new(binary);
    command
        .arg("--runtime-dir")
        .arg(dir)
        .env("OMT_RUNTIME_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid in the pre-exec gap detaches the daemon from our
    // process group/session — window close can never signal it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|err| format!("spawn sidecar: {err}"))?;
    omt_client::wait_for_descriptor(dir, Duration::from_secs(10))
        .ok_or_else(|| "sidecar produced no descriptor within 10s".to_string())
}

/// §49 desktop-dispatch-rpc-bound：desktop 会话 socket 的读写超时。桌面端
/// 派发链（rpc_of → session.call）在跨进程 exec/launch.lock 内逐条调
/// daemon——卡死的 daemon（accept 但永不回复）此前让桌面端无限期持锁，
/// CLI/MCP 每次 60s 后 LAUNCH_FAILED。与 daemon_link 的 DEFAULT_RPC_TIMEOUT
/// （30s）同值：卡死时最坏「一次调用超时 + state.rs 的单次重连」各 30s，
/// 仍在锁超时的量级内有界返回，锁最终释放。daemon 各操作都是本地快速
/// 往返（事件长轮询在 rpc_bridge 自己的连接上，不受影响）。
const SESSION_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// §49 desktop-dispatch-rpc-bound：enroll 内芯（超时注入的测试缝）——与
/// daemon_link::DaemonLink::enroll（F-EXEC-10）同法：自行 connect 并先在
/// socket 上装读写超时（SO_RCVTIMEO/SO_SNDTIMEO），再经 omt_client 公开的
/// from_stream + enroll 组装——`connect_and_enroll` 不带超时，直接用会被
/// wedged daemon（accept 但永不回复）挂死，派发锁被无限期传导占用。
fn enroll_with_timeout(
    descriptor: &Descriptor,
    options: &EnrollOptions,
    timeout: Duration,
) -> Result<Enrollment, String> {
    let stream = std::os::unix::net::UnixStream::connect(&descriptor.endpoint)
        .map_err(|err| format!("endpoint connect: {err}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| format!("set read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| format!("set write timeout: {err}"))?;
    let mut client = Client::from_stream(stream);
    let handshake = client
        .enroll(options)
        .map_err(|problem| format!("enroll: {} ({})", problem.message, problem.code))?;
    Ok(Enrollment { client, handshake })
}

/// discover-or-spawn → handshake kind:"desktop". Idempotent: a live
/// descriptor is reused verbatim (second-instance semantics, R13).
pub fn discover_or_spawn_and_enroll(dir: &Path) -> Result<(Enrollment, bool), String> {
    let (descriptor, spawned) = match live_descriptor(dir) {
        Some(descriptor) => (descriptor, false),
        None => {
            let descriptor = spawn_sidecar(dir)?;
            (descriptor, true)
        }
    };
    let options = EnrollOptions {
        kind: "desktop".into(),
        name: Some("oh-my-ticket-desktop".into()),
        actor_namespace: None,
        operations: None,
        credential_path: None,
    };
    let enrollment = enroll_with_timeout(&descriptor, &options, SESSION_RPC_TIMEOUT)?;
    Ok((enrollment, spawned))
}

/// §51 timeout-no-replay：30s 读超时（socket 的 WouldBlock / TimedOut 一族）
/// **不是**「daemon 已断」——是慢但存活的 daemon。omt_client 把读错误包成
/// `connection closed: <io err>`，超时形态的文案是
/// "Resource temporarily unavailable"（macOS 的 WouldBlock）或 "timed out"
/// 一族，此前恰好命中 is_daemon_gone 的 "connection closed" 子串，触发
/// 「重连 + 重放一次」：不带 commandId 的写（node/create、run/create、
/// node/update …）会被重复提交。
/// §53 P3：规则限定在**会话 socket 自身**的错误形态（`connection closed:`
/// / `request write:` 前缀，含写超时）——此前作用于任何 `code == "IO"`
/// 的问题，daemon 回传的 IO 错误（message 含「timed out」）会被误判为
/// 本地超时而无谓断连。
pub fn is_read_timeout(problem_message: &str, problem_code: &str) -> bool {
    problem_code == "IO"
        && (problem_message.starts_with("connection closed:")
            || problem_message.starts_with("request write:"))
        && (problem_message.contains("Resource temporarily unavailable")
            || problem_message.to_ascii_lowercase().contains("timed out"))
}

/// §57 F-SP2-11 / §59 P3-1：超时文案开头的机器可读前缀——前端 bridge.ts
/// 的 `DAEMON_TIMEOUT_MARKER` 据此把超时类失败分流到独立的 timeoutNotice
/// 状态。真正的跨语言锁定在前端：vitest（daemon-timeout-message.ts 与
/// exec-plane-p59.spec）直接读本文件抽取本常量与
/// [`DAEMON_TIMEOUT_MESSAGE`] 的字面量，断言 message 以 TS 侧 marker 开头、
/// 两边 marker 同值——改任何一边，测试即红（此前两边各写一份硬编码、
/// 各自对自己的拷贝断言，Rust 侧连前缀带测试一起改前端会静默退回）。
// crate 内只有测试引用本常量（消费方在前端）；staticlib/cdylib 目标会把
// 无生产引用的 pub const 当死码——显式标注。
#[allow(dead_code)]
pub const DAEMON_TIMEOUT_MARKER: &str = "[daemon-timeout]";

/// §51 timeout-no-replay：超时路径的统一错误文案（与 daemon 已断的重连+
/// 重放相区分）。连接在 state.rs 侧被丢弃，下次调用惰性重建。
/// §53 P3：如实说明**结果未知、可能已生效**——读超时时 daemon 通常仍会
/// 完成写入，「已放弃且未重试」会诱导用户立即重试（正是要避免的
/// 重复提交）；引导先刷新确认结果再重试。
/// §57 F-SP2-11：开头带机器可读前缀 [`DAEMON_TIMEOUT_MARKER`]——前端在
/// bridge 出口（omtCall / execStart，§59 F-SP2-12）剥掉前缀并按类型分流；
/// 友好文案仍以本常量为唯一来源（前端不复制文案）。
pub const DAEMON_TIMEOUT_MESSAGE: &str =
    "[daemon-timeout] daemon 无响应（30s）：请求结果未知、可能已生效——\
     为避免重复提交未自动重试；请刷新确认结果后再决定是否重试（连接已丢弃，下次调用将重建）";

/// Whether an IO-shaped failure means "the daemon is gone — rediscover"
/// (legal idle exit, crash, generation rotation): connection closed /
/// refused / broken pipe all qualify; protocol problems do not.
/// §51：读超时形态（[`is_read_timeout`]）不算——那是慢但存活的 daemon，
/// 重连重放会造成非幂等写重复提交。
pub fn is_daemon_gone(problem_message: &str, problem_code: &str) -> bool {
    if is_read_timeout(problem_message, problem_code) {
        return false;
    }
    problem_code == "IO"
        && (problem_message.contains("connection closed")
            || problem_message.contains("endpoint connect")
            || problem_message.contains("request write"))
}

/// Single-entry admin-grant registration for this desktop principal
/// (REPLACEMENT semantics — see module docs). Reads and rewrites
/// `<runtime-dir>/admin-grants.json` atomically (tmp + rename); all other
/// principal entries are preserved verbatim, only desktop-kind entries
/// are collapsed to the one current principal.
/// 写入经 omt_exec::grants 加锁。
pub fn register_admin_grant(dir: &Path, principal_id: &str) -> Result<(), String> {
    // P-1：与 exec-shim 的临时授权共用同一把 flock，避免并发覆盖
    omt_exec::grants::register_replacing_prefix(dir, "desktop:", principal_id)
}

/// Current status projection (settings page).
pub fn status(dir: &Path) -> DaemonStatus {
    match omt_client::read_descriptor(dir) {
        None => DaemonStatus {
            running: false,
            pid: None,
            generation: None,
            endpoint: None,
            spawned_by_us: false,
        },
        Some(descriptor) => {
            let running = omt_client::pid_live(descriptor.pid)
                && omt_client::endpoint_live(&descriptor.endpoint);
            DaemonStatus {
                running,
                pid: Some(descriptor.pid),
                generation: Some(descriptor.generation),
                endpoint: Some(descriptor.endpoint),
                spawned_by_us: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_grant_registration_replaces_desktop_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("admin-grants.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schemaVersion": 1,
                "principalIds": ["desktop:111", "desktop:222", "dsh:333", "cli:444"]
            })
            .to_string(),
        )
        .expect("seed grants");

        register_admin_grant(dir.path(), "desktop:999").expect("register");

        let raw = std::fs::read_to_string(&path).expect("read grants");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse grants");
        let ids: Vec<&str> = value["principalIds"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|id| id.as_str())
            .collect();
        let desktop_entries: Vec<&&str> =
            ids.iter().filter(|id| id.starts_with("desktop:")).collect();
        assert_eq!(
            desktop_entries.len(),
            1,
            "exactly one desktop entry: {ids:?}"
        );
        assert_eq!(
            *desktop_entries[0], "desktop:999",
            "entry is the current principal"
        );
        assert!(
            ids.contains(&"dsh:333") && ids.contains(&"cli:444"),
            "other principals preserved"
        );

        // Second launch replaces again — never accumulates.
        register_admin_grant(dir.path(), "desktop:1000").expect("re-register");
        let raw = std::fs::read_to_string(&path).expect("read grants 2");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse grants 2");
        let ids: Vec<&str> = value["principalIds"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|id| id.as_str())
            .collect();
        let desktop_entries: Vec<&&str> =
            ids.iter().filter(|id| id.starts_with("desktop:")).collect();
        assert_eq!(
            desktop_entries.len(),
            1,
            "still one entry after second launch: {ids:?}"
        );
        assert_eq!(*desktop_entries[0], "desktop:1000");
    }

    #[test]
    fn daemon_gone_classification_covers_shutdown_shapes_only() {
        assert!(is_daemon_gone("connection closed: eof", "IO"));
        assert!(is_daemon_gone("endpoint connect: refused", "IO"));
        assert!(is_daemon_gone("request write: broken pipe", "IO"));
        assert!(!is_daemon_gone("home not in credential scope", "FORBIDDEN"));
        assert!(!is_daemon_gone("unrelated io error", "IO"));
    }

    /// §51 timeout-no-replay：30s 读超时必须与「daemon 已断」分开分类——
    /// 超时是慢但存活的 daemon，重连 + 重放会把不带 commandId 的写
    /// （node/create 等）重复提交。§51 实测的超时文案形态
    /// （`connection closed: Resource temporarily unavailable`）此前恰好
    /// 命中 is_daemon_gone。
    #[test]
    fn read_timeouts_are_not_daemon_gone_and_never_replayed() {
        // macOS SO_RCVTIMEO 到期的实测形态
        assert!(is_read_timeout(
            "connection closed: Resource temporarily unavailable",
            "IO"
        ));
        // TimedOut 一族（Linux / 其它读路径）
        assert!(is_read_timeout(
            "connection closed: Operation timed out",
            "IO"
        ));
        assert!(is_read_timeout("connection closed: read timed out", "IO"));
        // 超时形态不得再命中 daemon-gone（否则 state.rs 会重连 + 重放）
        assert!(!is_daemon_gone(
            "connection closed: Resource temporarily unavailable",
            "IO"
        ));
        assert!(!is_daemon_gone("connection closed: read timed out", "IO"));
        // 非超时的 IO 形态仍按 daemon-gone 分类（合法退出 / 断连 → 重放）
        assert!(is_daemon_gone("connection closed: eof", "IO"));
        assert!(is_daemon_gone("connection closed before response", "IO"));
        // 非 IO 不属于任何一类
        assert!(!is_read_timeout("connection closed: eof", "CANCELED"));
        assert!(!is_read_timeout(
            "Operation timed out somewhere",
            "FORBIDDEN"
        ));
    }

    /// §53 P3：超时判定限定在**会话 socket 自身**的错误形态（`connection
    /// closed:` / `request write:` 前缀——omt_client 的读/写错误文案）。
    /// 此前规则作用于任何 `code == "IO"` 的问题：daemon 回传的 IO 错误只要
    /// message 里含「timed out」就被误判为本地读超时 → 无谓丢弃连接 +
    /// 「daemon 无响应」提示。
    #[test]
    fn timeout_classification_is_scoped_to_session_socket_shapes() {
        // 会话 socket 自身的读/写超时形态（§51 实测的文案）
        assert!(is_read_timeout(
            "connection closed: Resource temporarily unavailable",
            "IO"
        ));
        assert!(is_read_timeout("connection closed: read timed out", "IO"));
        assert!(is_read_timeout(
            "request write: Resource temporarily unavailable",
            "IO"
        ));
        assert!(is_read_timeout("request write: Operation timed out", "IO"));
        // daemon 回传的 IO 错误（非 socket 形态）：含 timed out 也不得误判
        assert!(
            !is_read_timeout("node snapshot io timed out", "IO"),
            "daemon 回传的 IO 错误不得误判为本地读超时"
        );
        assert!(!is_read_timeout("lock wait timed out on agents.json", "IO"));
        assert!(!is_read_timeout("something timed out mid-write", "IO"));
        // 前缀不符的连接类文案：交给 daemon-gone / 普通错误路径分类
        assert!(!is_read_timeout(
            "endpoint connect: Operation timed out",
            "IO"
        ));
    }

    /// §53 P3：超时提示如实说明**结果未知**——「已放弃且未重试」会诱导
    /// 用户立即手动重试（读超时时 daemon 通常仍会完成写入，重试正是要
    /// 避免的重复提交）。
    #[test]
    fn daemon_timeout_message_is_honest_about_unknown_outcome() {
        assert!(
            DAEMON_TIMEOUT_MESSAGE.contains("结果未知"),
            "提示必须如实说明结果未知：{DAEMON_TIMEOUT_MESSAGE}"
        );
        assert!(
            DAEMON_TIMEOUT_MESSAGE.contains("可能已生效"),
            "提示必须说明可能已生效：{DAEMON_TIMEOUT_MESSAGE}"
        );
        assert!(
            DAEMON_TIMEOUT_MESSAGE.contains("刷新"),
            "提示必须引导先刷新确认再重试：{DAEMON_TIMEOUT_MESSAGE}"
        );
        assert!(
            !DAEMON_TIMEOUT_MESSAGE.contains("已放弃"),
            "「已放弃」诱导用户立即重试（重复提交）：{DAEMON_TIMEOUT_MESSAGE}"
        );
    }

    /// §57 F-SP2-11 → §59 P3-1：超时文案必须带机器可读前缀（本 crate 内
    /// message↔marker 一致性）——跨语言方向由前端 vitest 锁定：它直接读
    /// 本文件抽取两个常量的字面量，断言 message 以 TS 侧 marker 开头、
    /// marker 两边同值（此前本测试硬编码 `"[daemon-timeout] "`，与前端
    /// 各自对自己的拷贝断言——Rust 侧连前缀带测试一起改前端会静默退回）。
    #[test]
    fn daemon_timeout_message_carries_machine_marker() {
        assert!(
            DAEMON_TIMEOUT_MESSAGE.starts_with(DAEMON_TIMEOUT_MARKER),
            "超时文案必须以 {DAEMON_TIMEOUT_MARKER} 开头（前端 bridge.ts 依赖此前缀分流）：\
             {DAEMON_TIMEOUT_MESSAGE}"
        );
    }

    /// §49「跨进程派发锁把桌面端卡死的 daemon 传导给 CLI/MCP」：desktop 会话
    /// 的 daemon RPC 此前无读超时——卡死的 daemon（accept 但永不回复）让桌面
    /// 端派发在 exec/launch.lock 内无限期挂住。socket 必须带读写超时
    /// （与 daemon_link 的 F-EXEC-10 同法）：enroll 在界内报错返回。
    #[test]
    fn wedged_daemon_session_times_out() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let socket = tmp.path().join("wedged.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        std::thread::spawn(move || {
            // accept 并持有连接、永不回复（live 探活连接也吃掉）
            let mut held = Vec::new();
            for _ in 0..4 {
                if let Ok((stream, _)) = listener.accept() {
                    held.push(stream);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        let descriptor = Descriptor {
            schema_version: 1,
            endpoint: socket.display().to_string(),
            generation: 1,
            pid: i64::from(std::process::id()),
            boot_token: "test".into(),
        };
        let options = EnrollOptions {
            kind: "desktop".into(),
            name: Some("wedged-test".into()),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let err =
            match enroll_with_timeout(&descriptor, &options, std::time::Duration::from_millis(300))
            {
                Ok(_) => panic!("wedged daemon 的 enroll 必须在界内报错"),
                Err(err) => err,
            };
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "enroll 应在 300ms 超时界内返回，实际 {:?}（err={err}）",
            started.elapsed()
        );
    }

    #[test]
    fn runtime_dir_prefers_env_then_home_default() {
        let dir = runtime_dir();
        assert!(dir.ends_with(".omt/run") || std::env::var("OMT_RUNTIME_DIR").is_ok());
    }
}
