//! launch spec（P-4）：launcher 写、shim 读。只放重算启动参数所需的输入，
//! 不放 env 值（secrets 纪律）；shim 用 resolve_launch + compose_launch_args
//! 重新计算 argv 与 env。文件位置：`<agentDir>/<execId>.spec.json`（0600）。

use std::path::{Path, PathBuf};

pub const SPEC_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    pub schema_version: u32,
    pub home_id: String,
    /// `.omt` 目录（cwd = 其父目录，见 executor::workspace_root_of）
    pub home_path: PathBuf,
    pub agent_id: String,
    pub exec_id: String,
    pub harness: String,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub unattended: bool,
    pub model: Option<String>,
    pub resume_session: Option<String>,
    /// 渲染后的派发 prompt（模板 + run_id，非敏感）
    pub prompt: String,
    pub max_duration_secs: u64,
    /// MCP 桥二进制（omt）
    pub omt_path: PathBuf,
}

impl LaunchSpec {
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        omt_harness::atomic_json::write_atomic(path, &text)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read spec {}: {e}", path.display()))?;
        let spec: Self = serde_json::from_str(&text).map_err(|e| format!("parse spec: {e}"))?;
        if spec.schema_version != SPEC_SCHEMA_VERSION {
            return Err(format!(
                "spec schemaVersion {} 不受支持（期望 {SPEC_SCHEMA_VERSION}）",
                spec.schema_version
            ));
        }
        Ok(spec)
    }
}

#[cfg(test)]
pub(crate) fn sample(home_path: &Path, omt_path: &Path) -> LaunchSpec {
    LaunchSpec {
        schema_version: SPEC_SCHEMA_VERSION,
        home_id: "h_test".into(),
        home_path: home_path.to_path_buf(),
        agent_id: "agent-exec-s".into(),
        exec_id: "exec-s".into(),
        harness: "pi".into(),
        node_id: None,
        run_id: None,
        unattended: true,
        model: None,
        resume_session: None,
        prompt: "do it".into(),
        max_duration_secs: 60,
        omt_path: omt_path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_no_env_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("exec-s.spec.json");
        let spec = sample(&tmp.path().join(".omt"), Path::new("/bin/omt"));
        spec.write(&path).unwrap();
        assert_eq!(LaunchSpec::read(&path).unwrap(), spec);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("\"env"), "spec 不得含 env：{raw}");
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.json");
        let mut spec = sample(tmp.path(), Path::new("/bin/omt"));
        spec.schema_version = 99;
        spec.write(&path).unwrap();
        assert!(LaunchSpec::read(&path)
            .unwrap_err()
            .contains("schemaVersion"));
    }
}
