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
use std::time::Duration;

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

fn spawn_daemon(bin: &Path, runtime_dir: &Path) -> Result<omt_client::Descriptor, String> {
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
    command
        .spawn()
        .map_err(|e| format!("spawn omt-daemon: {e}"))?;
    omt_client::wait_for_descriptor(runtime_dir, Duration::from_secs(10))
        .ok_or_else(|| "omt-daemon 10s 内未产出 descriptor".to_string())
}

impl DaemonLink {
    fn enroll(runtime_dir: &Path, kind: &str) -> Result<Self, String> {
        let descriptor = live_descriptor(runtime_dir).ok_or("daemon descriptor 不可用")?;
        // F-EXEC-10：自行 connect 并先在 socket 上装读写超时，再经 omt_client
        // 公开的 from_stream + enroll 组装连接——`connect_and_enroll` 不带
        // 超时，直接用会被 wedged daemon（accept 但永不回复）挂死。超时装在
        // socket 上（SO_RCVTIMEO/SO_SNDTIMEO），Client::request 里 try_clone 出
        // 的读端共享同一 socket，handshake 与后续全部调用都被界住；omt-client
        // 其他使用方（CLI/mcp）不走本路径，行为不变。
        let stream = std::os::unix::net::UnixStream::connect(&descriptor.endpoint)
            .map_err(|err| format!("endpoint connect: {err}"))?;
        let timeout = rpc_timeout();
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
        if live_descriptor(runtime_dir).is_none() {
            let bin = daemon_bin.ok_or("daemon 不在线，且没有可拉起的 omt-daemon")?;
            spawn_daemon(bin, runtime_dir)?;
        }
        let mut link = Self::enroll(runtime_dir, kind)?;
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
            link = Self::enroll(runtime_dir, kind)?;
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
        let want = std::fs::canonicalize(home_path).unwrap_or_else(|_| home_path.to_path_buf());
        self.handshake["homes"].as_array()?.iter().find_map(|home| {
            let path = std::fs::canonicalize(home["path"].as_str()?).ok()?;
            if path == want {
                home["homeId"].as_str().map(str::to_string)
            } else {
                None
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// F-EXEC-10：wedged daemon（accept UDS 连接但永不回复）不得挂死
    /// DaemonLink——socket 必须带读写超时（生产默认 30s；测试经
    /// `OMT_DAEMON_RPC_TIMEOUT_MS` 调小），enroll 在界内报错返回。
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

        std::env::set_var("OMT_DAEMON_RPC_TIMEOUT_MS", "300");
        let started = Instant::now();
        let err = match DaemonLink::connect(&runtime, "cli", None, &home_path, Some("h_wedged")) {
            Ok(_) => panic!("wedged daemon 的 enroll 必须在界内报错"),
            Err(err) => err,
        };
        std::env::remove_var("OMT_DAEMON_RPC_TIMEOUT_MS");

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "enroll 应在 300ms 超时界内返回，实际 {:?}（err={err}）",
            started.elapsed()
        );
    }
}
