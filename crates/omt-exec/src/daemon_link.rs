//! shim 侧的 daemon 连接（设计 §3.2「daemon 连接」）：discover-or-spawn →
//! enroll（kind 由调用方给，shim 用 "cli"）。不复用存储的凭据，principal
//! 恒为 `<kind>:<本进程 pid>`，P-1 的临时授权据此登记。
//!
//! **home 必须在凭据范围内**（review F4，P-12）：desktop 关闭后没有别的面再
//! 访问 workspace home，它静默 idleQuietMs 后会被关闭，daemon 随之退出；凭据
//! 能访问哪些 home 在注册那一刻就固定了（auth.rs）。所以连上的 daemon 没有
//! 该 home 时，先 `home/declare` 再重新注册。
//!
//! **拉起 daemon 时不带 `--home`**（review R2-1）：与 desktop / CLI / DSH 拉起
//! 的 daemon 一致，打开全局 home（`$OMT_HOME` 或 `~/.omt`；home 的 kind 由路径
//! 是否等于全局 home 决定，homes.rs）。只带 workspace `--home` 的 daemon 没有
//! 全局 home，DSH 插件与 CLI 会把 workspace home 误当成全局 home。workspace
//! home 由上面的 declare 加入。测试通过给子进程设置 `OMT_HOME` 指向临时目录，
//! 避免碰到开发者真实的 `~/.omt`。
//!
//! **home 路径必须与 homeId 绑定**（review R2-8）：spec 里的 home 路径与
//! homeId 不一致时，shim 会把任意目录初始化成新 home。给了期望 homeId 时，
//! 只声明已初始化的 home（含 `omt.db`），并校验最终 homeId。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::executor::RpcFn;
use crate::grants::TempGrant;

/// 参数 = (需要在凭据范围内的 home（`.omt` 目录）, 它应有的 homeId)。
pub type Connector = Arc<dyn Fn(&Path, &str) -> Result<RpcFn, String> + Send + Sync>;

/// F-EXEC-10：daemon socket 的读写超时——wedged daemon（accept UDS 连接但
/// 永不回复）不得挂死 shim 的租约释放/保活。生产默认 30s；环境变量
/// `OMT_DAEMON_RPC_TIMEOUT_MS` 覆盖（测试用，先例 OMT_SHIM_KEEPALIVE_MS）。
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

fn rpc_timeout() -> Duration {
    std::env::var("OMT_DAEMON_RPC_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RPC_TIMEOUT)
}

pub struct DaemonLink {
    client: omt_client::Client,
    principal: String,
    handshake: serde_json::Value,
}

fn live_descriptor(runtime_dir: &Path) -> Option<omt_client::Descriptor> {
    let d = omt_client::read_descriptor(runtime_dir)?;
    (omt_client::pid_live(d.pid) && omt_client::endpoint_live(&d.endpoint)).then_some(d)
}

/// §39 daemon-link-stale-descriptor：轮询**存活**的 descriptor（pid 存活 +
/// endpoint 可连，与 `live_descriptor` 同一判据）。`omt_client::
/// wait_for_descriptor` 只看文件存在，会接受崩溃 daemon 残留的旧
/// descriptor，拉起后的第一次 connect 因此必然失败。
fn wait_for_live_descriptor(
    runtime_dir: &Path,
    timeout: Duration,
) -> Option<omt_client::Descriptor> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(descriptor) = live_descriptor(runtime_dir) {
            return Some(descriptor);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 以新会话拉起 daemon 并返回其 pid。§39 shim-daemon-zombie：起一个
/// `wait()` 回收线程（同 `launch::spawn_shim`）——daemon 退出后不留僵尸
/// （僵尸让 pid_live 恒真，`omt daemon-stop` 每次 stop-timeout）。shim 先
/// 退出时线程随进程结束，daemon 由 launchd 收养。
fn spawn_daemon_process(bin: &Path, runtime_dir: &Path) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(bin);
    command
        .arg("--runtime-dir")
        .arg(runtime_dir)
        // 不带 --home：打开全局 home，与其他面拉起的 daemon 一致（review R2-1）
        .env("OMT_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid 在 fork 与 exec 之间脱离会话，daemon 不随 shim 退出
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn omt-daemon: {e}"))?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn spawn_daemon(bin: &Path, runtime_dir: &Path) -> Result<omt_client::Descriptor, String> {
    spawn_daemon_process(bin, runtime_dir)?;
    wait_for_live_descriptor(runtime_dir, Duration::from_secs(10))
        .ok_or_else(|| "omt-daemon 10s 内未产出可用的 descriptor".to_string())
}

impl DaemonLink {
    fn enroll(runtime_dir: &Path, kind: &str, timeout: Duration) -> Result<Self, String> {
        let descriptor = live_descriptor(runtime_dir).ok_or("daemon descriptor 不可用")?;
        // F-EXEC-10：自行 connect 并先在 socket 上装读写超时，再经 omt_client
        // 公开的 from_stream + enroll 组装连接——`connect_and_enroll` 不带
        // 超时，直接用会被 wedged daemon（accept 但永不回复）挂死。超时装在
        // socket 上（SO_RCVTIMEO/SO_SNDTIMEO），Client::request 里 try_clone 出
        // 的读端共享同一 socket，handshake 与后续全部调用都被界住；omt-client
        // 其他使用方（CLI/mcp）不走本路径，行为不变。
        let stream = std::os::unix::net::UnixStream::connect(&descriptor.endpoint)
            .map_err(|err| format!("endpoint connect: {err}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|err| format!("set read timeout: {err}"))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|err| format!("set write timeout: {err}"))?;
        let mut client = omt_client::Client::from_stream(stream);
        let options = omt_client::EnrollOptions {
            kind: kind.into(),
            name: Some(format!("omt-exec-{kind}")),
            actor_namespace: None,
            operations: None,
            credential_path: None,
        };
        let handshake = client
            .enroll(&options)
            .map_err(|p| format!("enroll: {} ({})", p.message, p.code))?;
        let principal = handshake["credential"]["principalId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if principal.is_empty() {
            return Err("handshake 缺少 credential.principalId".into());
        }
        Ok(Self {
            client,
            principal,
            handshake,
        })
    }

    /// discover-or-spawn，并保证 `home_path`（`.omt` 目录）在本连接的凭据范围内。
    /// `expected_home_id` 为 Some 时（shim 的所有调用）：只声明已初始化的 home，
    /// 且最终 homeId 必须与之相等。
    pub fn connect(
        runtime_dir: &Path,
        kind: &str,
        daemon_bin: Option<&Path>,
        home_path: &Path,
        expected_home_id: Option<&str>,
    ) -> Result<Self, String> {
        Self::connect_with(
            runtime_dir,
            kind,
            daemon_bin,
            home_path,
            expected_home_id,
            rpc_timeout(),
        )
    }

    /// `connect` 的内芯（§41 test-env-var-parallel：socket 超时从参数注入，
    /// 单测不再 set_var `OMT_DAEMON_RPC_TIMEOUT_MS`）。
    pub(crate) fn connect_with(
        runtime_dir: &Path,
        kind: &str,
        daemon_bin: Option<&Path>,
        home_path: &Path,
        expected_home_id: Option<&str>,
        rpc_timeout: Duration,
    ) -> Result<Self, String> {
        if live_descriptor(runtime_dir).is_none() {
            let bin = daemon_bin.ok_or("daemon 不在线，且没有可拉起的 omt-daemon")?;
            spawn_daemon(bin, runtime_dir)?;
        }
        let mut link = Self::enroll(runtime_dir, kind, rpc_timeout)?;
        if link.home_id_for(home_path).is_none() {
            if expected_home_id.is_some() && !home_path.join("omt.db").is_file() {
                return Err(format!(
                    "{} 不是已初始化的 omt home，拒绝声明（shim 不新建 home）",
                    home_path.display()
                ));
            }
            // daemon 在线但没打开这个 home（idle 关闭 / 由别的面拉起）：声明后
            // 重新注册——declare 返回 requiresRehandshake，旧凭据看不到新 home
            link.call(
                "home/declare",
                serde_json::json!({ "path": home_path.display().to_string() }),
            )?;
            link = Self::enroll(runtime_dir, kind, rpc_timeout)?;
        }
        match (link.home_id_for(home_path), expected_home_id) {
            (None, _) => Err(format!(
                "home {} 声明后仍不在凭据范围内",
                home_path.display()
            )),
            (Some(actual), Some(expected)) if actual != expected => Err(format!(
                "home {} 的 id 是 {actual}，与期望的 {expected} 不一致",
                home_path.display()
            )),
            _ => Ok(link),
        }
    }

    /// handshake 的 `homes[]`（已按凭据范围过滤）里路径与 `home_path` 相同的 homeId。
    pub fn home_id_for(&self, home_path: &Path) -> Option<String> {
        self.handshake_entry_for(home_path)
            .and_then(|home| home["homeId"].as_str().map(str::to_string))
    }

    /// 同一条目的 `kind`。§49 Q-13 fail closed：缺 kind 返回 None（调用方按
    /// "unknown" 处理 → workspace 范围判定拒绝），绝不默认 "workspace"。
    pub fn home_kind_for(&self, home_path: &Path) -> Option<String> {
        self.handshake_entry_for(home_path)
            .and_then(|home| home["kind"].as_str().map(str::to_string))
    }

    fn handshake_entry_for(&self, home_path: &Path) -> Option<&serde_json::Value> {
        let want = std::fs::canonicalize(home_path).unwrap_or_else(|_| home_path.to_path_buf());
        self.handshake["homes"].as_array()?.iter().find(|home| {
            let Some(path) = home["path"].as_str() else {
                return false;
            };
            std::fs::canonicalize(path).ok() == Some(want.clone())
        })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.client
            .call(method, params)
            .map_err(|p| format!("{} ({})", p.message, p.code))
    }
}

/// P-1：连接 + 临时 admin 授权。返回的 RpcFn 持有连接与授权守卫；RpcFn
/// 被 drop（shim 释放完租约）时授权随之撤销。
pub fn admin_connector(runtime_dir: PathBuf, daemon_bin: Option<PathBuf>) -> Connector {
    Arc::new(move |home_path: &Path, home_id: &str| {
        let link = DaemonLink::connect(
            &runtime_dir,
            "cli",
            daemon_bin.as_deref(),
            home_path,
            Some(home_id),
        )?;
        let grant = TempGrant::add(&runtime_dir, link.principal())?;
        let shared = Arc::new(Mutex::new((link, grant)));
        let rpc: RpcFn = Arc::new(move |method: &str, params: serde_json::Value| {
            let mut guard = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.0.call(method, params)
        });
        Ok(rpc)
    })
}

/// F-EXEC-15：连接（含必要的 home/declare 与 homeId 校验，与
/// admin_connector 同一规则）并校验 handshake principal 与
/// `expected_principal` 一致，**不建立授权**——返回的 RpcFn 不持有任何
/// TempGrant。调用方（shim 的 lease_releaser）先把确定性 principal
/// `cli:<pid>` 的临时授权建好（本地文件 IO，不阻塞）再进入有界等待，
/// connect + enroll + declare 的全部 daemon 往返因此都受
/// OMT_SHIM_LEASE_TIMEOUT_MS 约束——半卡死 daemon（accept 但卡在 declare）
/// 不再把 30s 的释放上限撑到 ~130s。授权按 expected principal 发放，
/// handshake 不一致即 Err（错配的连接不可用）。kind 固定 "cli"，handshake
/// principal 恒为 `cli:<本进程 pid>`（daemon_link 模块不变式）。
pub fn admin_rpc_prepared(
    runtime_dir: &Path,
    daemon_bin: Option<&Path>,
    home_path: &Path,
    home_id: &str,
    expected_principal: &str,
) -> Result<RpcFn, String> {
    let link = DaemonLink::connect(runtime_dir, "cli", daemon_bin, home_path, Some(home_id))?;
    if link.principal() != expected_principal {
        return Err(format!(
            "handshake principal {} 与预期的 {expected_principal} 不一致",
            link.principal()
        ));
    }
    let shared = Arc::new(Mutex::new(link));
    let rpc: RpcFn = Arc::new(move |method: &str, params: serde_json::Value| {
        shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .call(method, params)
    });
    Ok(rpc)
}

/// 只连接、不授权（Task 8 的 home 保活用）。
pub fn plain_connector(runtime_dir: PathBuf, daemon_bin: Option<PathBuf>) -> Connector {
    Arc::new(move |home_path: &Path, home_id: &str| {
        let link = DaemonLink::connect(
            &runtime_dir,
            "cli",
            daemon_bin.as_deref(),
            home_path,
            Some(home_id),
        )?;
        let shared = Arc::new(Mutex::new(link));
        let rpc: RpcFn = Arc::new(move |method: &str, params: serde_json::Value| {
            shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .call(method, params)
        });
        Ok(rpc)
    })
}

/// Q-3：daemon 当前服务的 home（homeId, path）。daemon 不在线且给了二进制时拉起
/// （与 connect 同语义，不带 --home）。CLI/MCP 据此把 homeId 映射为路径——spec
/// 的 home_path 绝不取自调用方输入（P-12）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedHome {
    pub home_id: String,
    pub path: PathBuf,
    /// handshake 的 `kind`："workspace" | "global"（Q-13 据此拒绝全局 home）
    pub kind: String,
}

pub fn served_homes(
    runtime_dir: &Path,
    daemon_bin: Option<&Path>,
) -> Result<Vec<ServedHome>, String> {
    if live_descriptor(runtime_dir).is_none() {
        let bin = daemon_bin.ok_or("daemon 不在线，且没有可拉起的 omt-daemon")?;
        spawn_daemon(bin, runtime_dir)?;
    }
    let link = DaemonLink::enroll(runtime_dir, "cli", rpc_timeout())?;
    Ok(link.handshake["homes"]
        .as_array()
        .map(|homes| {
            homes
                .iter()
                .filter_map(|h| {
                    Some(ServedHome {
                        home_id: h["homeId"].as_str()?.to_string(),
                        path: PathBuf::from(h["path"].as_str()?),
                        // §49 Q-13 fail closed：缺 kind → "unknown"（workspace
                        // 范围判定按拒绝处理），不默认 "workspace"
                        kind: h["kind"].as_str().unwrap_or("unknown").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// F-EXEC-10：wedged daemon（accept UDS 连接但永不回复）不得挂死
    /// DaemonLink——socket 必须带读写超时（生产默认 30s；单测经
    /// `connect_with` 直接注入 300ms，不 set_var），enroll 在界内报错返回。
    #[test]
    fn wedged_daemon_connection_times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let socket = tmp.path().join("wedged.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            // accept 并持有连接、永不回复（live_descriptor 的探活连接也吃掉）
            let mut held = Vec::new();
            for _ in 0..8 {
                if let Ok((stream, _)) = listener.accept() {
                    held.push(stream);
                }
            }
            std::thread::sleep(Duration::from_secs(30));
        });
        std::fs::write(
            omt_client::descriptor_path(&runtime),
            serde_json::json!({
                "schemaVersion": 1,
                "endpoint": socket.display().to_string(),
                "generation": 1,
                "pid": std::process::id(),
                "bootToken": "test",
            })
            .to_string(),
        )
        .unwrap();
        let home_path = tmp.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();

        let started = Instant::now();
        let err = match DaemonLink::connect_with(
            &runtime,
            "cli",
            None,
            &home_path,
            Some("h_wedged"),
            Duration::from_millis(300),
        ) {
            Ok(_) => panic!("wedged daemon 的 enroll 必须在界内报错"),
            Err(err) => err,
        };

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "enroll 应在 300ms 超时界内返回，实际 {:?}（err={err}）",
            started.elapsed()
        );
    }

    fn write_descriptor(runtime: &Path, endpoint: &Path, pid: u32) {
        std::fs::create_dir_all(runtime).unwrap();
        std::fs::write(
            omt_client::descriptor_path(runtime),
            serde_json::json!({
                "schemaVersion": 1,
                "endpoint": endpoint.display().to_string(),
                "generation": 1,
                "pid": pid,
                "bootToken": "test",
            })
            .to_string(),
        )
        .unwrap();
    }

    /// §39 daemon-link-stale-descriptor：崩溃 daemon 残留的 descriptor（pid
    /// 已死、socket 不可连）不算就绪——等待必须超时返回 None；pid 存活且
    /// socket 可连才返回。
    #[test]
    fn wait_for_live_descriptor_ignores_stale_descriptors() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let dead_pid = gone.id();
        gone.wait().unwrap();
        write_descriptor(&runtime, &tmp.path().join("missing.sock"), dead_pid);
        assert!(
            wait_for_live_descriptor(&runtime, Duration::from_millis(300)).is_none(),
            "陈旧 descriptor 不得被当成就绪"
        );
        let socket = tmp.path().join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        write_descriptor(&runtime, &socket, std::process::id());
        assert!(wait_for_live_descriptor(&runtime, Duration::from_millis(300)).is_some());
    }

    /// §39 shim-daemon-zombie：shim 拉起的 daemon 退出后必须被回收——僵尸
    /// 进程对 kill(pid, 0) 仍成功，pid_live 恒真。
    #[test]
    fn spawned_daemon_is_reaped_after_exit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("fake-daemon");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pid = spawn_daemon_process(&bin, &tmp.path().join("runtime")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while omt_client::pid_live(i64::from(pid)) {
            assert!(
                Instant::now() < deadline,
                "daemon 退出后 5s 仍是僵尸（未回收）"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
