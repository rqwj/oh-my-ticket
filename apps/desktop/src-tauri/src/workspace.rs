//! U2（R4/R6/R9/R32；KTD17/KTD19）：工作区命令——「在 Finder 中显示」与
//! 按工作区统计运行中会话数。
//!
//! - `workspace_reveal(home_id)`（KTD17）：服务端由 homeId 解析出路径
//!   （daemon 握手的已开 home 优先，known-homes 的 lastHomeId 兜底），再执行
//!   `open -R <工作区根目录>`——不接受前端传来的路径，也不引入 opener 插件。
//!   未知 homeId 报错且不调用 open。
//! - `workspace_session_counts()`：用 `omt_exec::layout::home_ids` 遍历全部
//!   home（含只在 runtime 里有 agents 目录、未在 daemon 声明过的），按交互式
//!   /其他分开计数（交互式字段在 U4 落地前恒为 0），并按 KTD19 返回
//!   `terminalCapable`——复用 `omt_exec::local::workspace_scope_denial` 的
//!   判定（全局 home、workspace 根等于用户主目录都算没有项目目录）；前端
//!   以后端字段为准，不自己猜。
//!
//! CF5/CF6：两条命令都是 `#[tauri::command(async)]`，阻塞部分（会话握手/
//! RPC、目录遍历、`open` 子进程）经 [`SharedSession::in_blocking_scope`] 的
//! `spawn_blocking` 跑在阻塞线程池；会话锁只经阻塞令牌取得（模块外无同步
//! 锁入口）。

use std::path::{Path, PathBuf};

use tauri::State;

use crate::daemon;
use crate::state::SharedSession;

/// 握手 home 投影（homes 数组条目的最小形状）。
struct HandshakeHome {
    home_id: String,
    path: String,
}

/// known-homes 条目的最小形状（home/list-known 的 homes 数组条目）。
struct KnownHomeEntry {
    path: String,
    last_home_id: String,
    /// workspace / global（F-TERM-20：未在本次握手里的 home 由它判定
    /// terminalCapable）
    kind: String,
}

/// 提取 homeId/path 对（handshake_homes() 返回的已是 homes **数组**：
/// enrollment.handshake["homes"]，不是包一层的对象）。
fn handshake_homes_of(homes: &serde_json::Value) -> Vec<HandshakeHome> {
    homes
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|home| {
                    Some(HandshakeHome {
                        home_id: home["homeId"].as_str()?.to_string(),
                        path: home["path"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// known-homes 条目的最小形状（home/list-known 的返回是 `{"homes": [...]}`
/// 包装对象；也兼容裸数组，便于测试直构）。
fn known_homes_of(homes: &serde_json::Value) -> Vec<KnownHomeEntry> {
    homes["homes"]
        .as_array()
        .or_else(|| homes.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|home| {
                    Some(KnownHomeEntry {
                        path: home["path"].as_str()?.to_string(),
                        last_home_id: home["lastHomeId"].as_str().unwrap_or("").to_string(),
                        kind: home["kind"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// homeId → home 目录路径：daemon 握手的已开 home（当前 id，权威）优先，
/// known-homes 的 lastHomeId（历史值，未开的工作区）兜底。
fn home_path_of(
    handshake: &[HandshakeHome],
    known: &[KnownHomeEntry],
    home_id: &str,
) -> Option<String> {
    handshake
        .iter()
        .find(|h| h.home_id == home_id)
        .map(|h| h.path.clone())
        .or_else(|| {
            known
                .iter()
                .find(|k| k.last_home_id == home_id && !k.last_home_id.is_empty())
                .map(|k| k.path.clone())
        })
}

/// KTD17 的可测内芯：解析 homeId → 工作区根目录，`open -R` 经注入的
/// `spawn` 执行——未知 homeId 报错且不调用 open（测试用记录型 spawn 断言）。
/// 工作区根 = `omt_exec::executor::workspace_root_of`（尾段 `.omt` 剥掉）。
fn reveal_workspace_with(
    handshake: &serde_json::Value,
    known: &serde_json::Value,
    home_id: &str,
    spawn: &mut dyn FnMut(std::process::Command) -> Result<(), String>,
) -> Result<(), String> {
    let Some(path) = home_path_of(
        &handshake_homes_of(handshake),
        &known_homes_of(known),
        home_id,
    ) else {
        return Err(format!(
            "未知的工作区 {home_id}：daemon 已开 home 与 known-homes 里都没有它"
        ));
    };
    let root = omt_exec::executor::workspace_root_of(Path::new(&path));
    let mut command = std::process::Command::new("open");
    command.arg("-R").arg(&root);
    spawn(command).map_err(|err| format!("在 Finder 中显示失败：{err}"))
}

/// R4/R6「在 Finder 中显示」（KTD17）：服务端解析路径 + `open -R`。仅
/// macOS（Finder）；其他平台如实报错。阻塞部分（握手 RPC + 子进程）在
/// 阻塞线程池（CF5）。
#[tauri::command(async)]
pub async fn workspace_reveal(
    shared: State<'_, SharedSession>,
    home_id: String,
) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (&shared, &home_id);
        return Err("「在 Finder 中显示」目前仅支持 macOS".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let shared = shared.inner().clone();
        shared
            .in_blocking_scope(move |scope| {
                let handshake = scope.with_session(|session| session.handshake_homes())?;
                let known = scope.rpc()("home/list-known", serde_json::Value::Null)
                    .unwrap_or_else(|_| serde_json::json!({ "homes": [] }));
                reveal_workspace_with(&handshake, &known, &home_id, &mut |mut command| {
                    command.spawn().map(|_| ()).map_err(|err| err.to_string())
                })
            })
            .await
    }
}

/// 一个 home 的活跃 exec 判定（活跃口径 = `control::exec_active`：shim 存活
/// 或仍在拉起宽限期内，进程身份在服务端解析——绝不信任 renderer 的 pid）。
fn active_count_of_home(runtime_dir: &Path, home_id: &str) -> usize {
    let Ok((agents, _)) = omt_exec::agents::list_agents(runtime_dir, home_id) else {
        return 0; // 读不出的 home 计 0（图标栏计数宁可少报）
    };
    agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                omt_exec::agents::AgentRecordStatus::Starting
                    | omt_exec::agents::AgentRecordStatus::Running
            )
        })
        .filter(|agent| {
            agent.last_exec_id.as_deref().is_some_and(|exec_id| {
                omt_exec::control::exec_active(runtime_dir, home_id, &agent.id, exec_id)
            })
        })
        .count()
}

/// R2/R9/R32：`workspace_session_counts` 的可测内芯。
///
/// F-TERM-20：home 集合 = runtime 里有 `agents/` 目录的 home ∪ 本次会话握手
/// 的已开 home ∪ known-homes 记录过的 home（`lastHomeId`）。`agents/<homeId>`
/// 目录要到首次派发才出现，握手 homes 又只是建会话那一刻的快照——只遍历
/// 前者会让从未派发过、或不在快照里的工作区（daemon 重启、空闲关闭、其他
/// 客户端打开）没有条目，前端按「未知」处理：隐藏「在此工作区打开终端」、
/// 移除总走两段确认。kind/path 先取握手（当前 id，权威）、再取 known-homes；
/// 没有 agents 目录的计 0。KTD19 仍复用 `workspace_scope_denial`；三处都给不出
/// kind 的（只剩 agents 目录、daemon 与 known-homes 都不认识）才按拒绝
/// ——fail-closed，与 local.rs 口径一致。
fn session_counts_value(
    handshake: &serde_json::Value,
    known: &serde_json::Value,
    runtime_dir: &Path,
    user_home: Option<&Path>,
) -> serde_json::Value {
    let handshake_homes = handshake.as_array().cloned().unwrap_or_default();
    let known_homes = known_homes_of(known);
    let user_home = user_home.map(PathBuf::from);

    // 稳定顺序：agents 目录 → 握手 → known-homes，各自去重
    let mut home_ids: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        if !id.is_empty() && !home_ids.iter().any(|seen| seen == id) {
            home_ids.push(id.to_string());
        }
    };
    for id in omt_exec::layout::home_ids(runtime_dir) {
        push(&id);
    }
    for home in &handshake_homes {
        if let Some(id) = home["homeId"].as_str() {
            push(id);
        }
    }
    for entry in &known_homes {
        push(&entry.last_home_id);
    }

    let mut homes = Vec::new();
    for home_id in home_ids {
        let kind_and_path = handshake_homes
            .iter()
            .find(|h| h["homeId"].as_str() == Some(home_id.as_str()))
            .and_then(|h| {
                Some((
                    h["kind"].as_str()?.to_string(),
                    h["path"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .or_else(|| {
                known_homes
                    .iter()
                    .find(|k| k.last_home_id == home_id && !k.kind.is_empty())
                    .map(|k| (k.kind.clone(), k.path.clone()))
            });
        let terminal_capable = kind_and_path.is_some_and(|(kind, path)| {
            omt_exec::local::workspace_scope_denial(&kind, Path::new(&path), user_home.as_deref())
                .is_none()
        });
        let others = active_count_of_home(runtime_dir, &home_id);
        homes.push(serde_json::json!({
            "homeId": home_id,
            // 交互式 exec 在 U4 落地前不存在——字段恒为 0，UI/Rust 契约先立好
            "terminals": 0,
            "others": others,
            "terminalCapable": terminal_capable,
        }));
    }
    serde_json::json!({ "homes": homes })
}

/// R2/R9/R32 的数据面：全部 home 的运行中会话计数 + terminalCapable。
/// 阻塞部分（握手 + 目录遍历 + agents.json 读取）在阻塞线程池（CF5）。
#[tauri::command(async)]
pub async fn workspace_session_counts(
    shared: State<'_, SharedSession>,
) -> Result<serde_json::Value, String> {
    let shared = shared.inner().clone();
    shared
        .in_blocking_scope(move |scope| {
            let handshake = scope.with_session(|session| session.handshake_homes())?;
            // F-TERM-20：known-homes 补齐未在握手快照里的 home；拉取失败按空
            // 处理（计数照常，这些 home 的能力判定 fail-closed）
            let known = scope.rpc()("home/list-known", serde_json::Value::Null)
                .unwrap_or_else(|_| serde_json::json!({ "homes": [] }));
            Ok(session_counts_value(
                &handshake,
                &known,
                &daemon::runtime_dir(),
                std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            ))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录型 spawn：不真正执行，记下 (program, args)。
    struct Recorder {
        calls: Vec<(String, Vec<String>)>,
    }

    impl Recorder {
        fn spawn(&mut self, command: std::process::Command) -> Result<(), String> {
            self.calls.push((
                command.get_program().to_string_lossy().to_string(),
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().to_string())
                    .collect(),
            ));
            Ok(())
        }
    }

    fn handshake(homes: &[(&str, &str, &str)]) -> serde_json::Value {
        serde_json::json!(homes
            .iter()
            .map(|(id, kind, path)| serde_json::json!({
                "homeId": id, "kind": kind, "path": path,
            }))
            .collect::<Vec<_>>())
    }

    fn known(homes: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({ "homes": homes.iter().map(|(last, path)| serde_json::json!({
            "path": path, "lastHomeId": last, "kind": "workspace", "name": ".omt",
            "open": false, "missing": false,
        })).collect::<Vec<_>>() })
    }

    /// KTD17：握手已开 home 命中 → `open -R <工作区根>`（剥掉尾段 `.omt`）。
    #[test]
    fn reveal_resolves_handshake_home_and_opens_workspace_root() {
        let mut recorder = Recorder { calls: Vec::new() };
        reveal_workspace_with(
            &handshake(&[("h_a", "workspace", "/tmp/ws1/.omt")]),
            &known(&[]),
            "h_a",
            &mut |cmd| recorder.spawn(cmd),
        )
        .expect("已开 home 应可显示");
        assert_eq!(
            recorder.calls,
            vec![(
                "open".to_string(),
                vec!["-R".to_string(), "/tmp/ws1".to_string()]
            )],
        );
    }

    /// KTD17：握手没有、known-homes 的 lastHomeId 命中（未在 daemon 开的
    /// 工作区）→ 兜底解析成功。
    #[test]
    fn reveal_falls_back_to_known_homes_last_home_id() {
        let mut recorder = Recorder { calls: Vec::new() };
        reveal_workspace_with(
            &handshake(&[]),
            &known(&[("h_b", "/tmp/ws2/.omt")]),
            "h_b",
            &mut |cmd| recorder.spawn(cmd),
        )
        .expect("known-homes 兜底应可显示");
        assert_eq!(recorder.calls.len(), 1);
        assert_eq!(recorder.calls[0].1, vec!["-R", "/tmp/ws2"]);
    }

    /// KTD17：未知 homeId 报错且**不调用 open**。
    #[test]
    fn reveal_rejects_unknown_home_without_calling_open() {
        let mut recorder = Recorder { calls: Vec::new() };
        let err = reveal_workspace_with(
            &handshake(&[("h_a", "workspace", "/tmp/ws1/.omt")]),
            &known(&[("h_b", "/tmp/ws2/.omt")]),
            "h_x",
            &mut |cmd| recorder.spawn(cmd),
        )
        .expect_err("未知 homeId 必须报错");
        assert!(err.contains("h_x"), "错误点名 homeId：{err}");
        assert!(
            recorder.calls.is_empty(),
            "未知 homeId 不得调用 open（KTD17）"
        );
    }

    /// 写一个最小 agent.json（运行中 + 最近启动无 shim pid → 拉起宽限期内）。
    fn seed_running_agent(runtime_dir: &Path, home_id: &str, agent_id: &str) {
        let dir = runtime_dir.join("agents").join(home_id).join(agent_id);
        std::fs::create_dir_all(&dir).expect("create agent dir");
        let record = serde_json::json!({
            "id": agent_id,
            "displayName": "Agent 1",
            "harness": "omp",
            "status": "running",
            "attachment": "none",
            "lastExecId": "exec-1",
            "lastSessionId": null,
            "execs": [{
                "execId": "exec-1",
                "nodeId": null,
                "runId": null,
                "startedAt": omt_exec::agents::now_epoch_ms(),
                "exitCode": null,
                "killReason": null,
                "failureDetail": null,
                "harnessSessionId": null,
                "transcriptFile": null,
                "binaryPath": null,
                "binaryArgs": [],
                "pid": null,
                "processStartId": null,
            }],
        });
        std::fs::write(dir.join("agent.json"), record.to_string()).expect("write agent.json");
    }

    /// R2/R9：对只含 agents 目录、没在 daemon 声明过的 home 也能计数
    /// （terminalCapable 按 fail-closed = false）。
    #[test]
    fn session_counts_count_undeclared_home_and_split_terminal_capability() {
        let runtime = tempfile::tempdir().expect("tempdir");
        seed_running_agent(runtime.path(), "h_x", "agent-1");
        // 另一个 home 只有目录、没有任何 agent
        std::fs::create_dir_all(runtime.path().join("agents/h_g")).expect("create home dir");

        let value = session_counts_value(
            &handshake(&[
                ("h_x", "workspace", "/tmp/ws1/.omt"),
                ("h_g", "global", "/Users/tester/.omt"),
            ]),
            &known(&[]),
            runtime.path(),
            Some(Path::new("/Users/tester")),
        );
        let homes = value["homes"].as_array().expect("homes 数组");
        assert_eq!(homes.len(), 2, "{value}");

        let h_x = homes.iter().find(|h| h["homeId"] == "h_x").expect("h_x");
        assert_eq!(h_x["others"], 1, "宽限期内的运行中 exec 计 1：{h_x}");
        assert_eq!(h_x["terminals"], 0, "交互式在 U4 前恒为 0");
        assert_eq!(h_x["terminalCapable"], true, "普通 workspace 根可开终端");

        let h_g = homes.iter().find(|h| h["homeId"] == "h_g").expect("h_g");
        assert_eq!(h_g["others"], 0);
        assert_eq!(
            h_g["terminalCapable"], false,
            "KTD19：全局 home 没有项目目录"
        );

        // 只含 agents 目录、未在 daemon 声明过的 home：计数照常、能力 fail-closed
        let undeclared = session_counts_value(&handshake(&[]), &known(&[]), runtime.path(), None);
        let list = undeclared["homes"].as_array().expect("homes 数组");
        let h_x = list.iter().find(|h| h["homeId"] == "h_x").expect("h_x");
        assert_eq!(h_x["others"], 1, "未声明的 home 也能计数：{undeclared}");
        assert_eq!(h_x["terminalCapable"], false, "无法判定 kind → fail-closed");
    }

    /// F-TERM-20：从未派发过（没有 agents 目录）的工作区也要有条目——只在
    /// 握手里的、只在 known-homes 里的都计 0 且按 kind 判定能力；known-homes
    /// 的全局 home 不可开终端；三处都给不出 kind 的（只剩 agents 目录）才
    /// fail-closed。修复前只遍历 agents 目录，前两类工作区没有条目，前端
    /// 隐藏「在此工作区打开终端」、移除总走两段确认。
    #[test]
    fn session_counts_cover_homes_without_agents_dir() {
        let runtime = tempfile::tempdir().expect("tempdir");
        // 只有 agents 目录、daemon 与 known-homes 都不认识
        std::fs::create_dir_all(runtime.path().join("agents/h_orphan")).expect("create home dir");

        let known_homes = serde_json::json!({ "homes": [
            { "path": "/tmp/ws2/.omt", "lastHomeId": "h_known", "kind": "workspace",
              "name": ".omt", "open": false, "missing": false },
            { "path": "/Users/tester/.omt", "lastHomeId": "h_global", "kind": "global",
              "name": ".omt", "open": false, "missing": false },
        ]});
        let value = session_counts_value(
            &handshake(&[("h_open", "workspace", "/tmp/ws1/.omt")]),
            &known_homes,
            runtime.path(),
            Some(Path::new("/Users/tester")),
        );
        let homes = value["homes"].as_array().expect("homes 数组");
        let get = |id: &str| {
            homes
                .iter()
                .find(|h| h["homeId"] == id)
                .unwrap_or_else(|| panic!("{id} 必须有条目：{value}"))
                .clone()
        };

        let open = get("h_open");
        assert_eq!(open["others"], 0, "没有 agents 目录计 0");
        assert_eq!(
            open["terminalCapable"], true,
            "握手里的普通 workspace 可开终端"
        );

        let known_only = get("h_known");
        assert_eq!(known_only["others"], 0);
        assert_eq!(
            known_only["terminalCapable"], true,
            "不在握手快照里、但 known-homes 记得的 workspace 可开终端"
        );

        assert_eq!(
            get("h_global")["terminalCapable"],
            false,
            "KTD19：全局 home"
        );
        assert_eq!(
            get("h_orphan")["terminalCapable"],
            false,
            "三处都给不出 kind → fail-closed"
        );
        assert_eq!(homes.len(), 4, "{value}");
    }
}
