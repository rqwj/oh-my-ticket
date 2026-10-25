//! exec_* tauri command 包装。执行面内核在 omt_exec（设计 §3.1）；这里
//! 负责把 Tauri Channel 适配成 EventSink，并把阻塞的派发链交给
//! state 模块的阻塞作用域（U2-1 CF5/CF6：SharedSession → 借用式 rpc 的
//! 适配在 state::BlockingScope::rpc，锁不再公开可达、拿锁能力带不出
//! 阻塞作用域——F-TERM-16）。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use omt_exec::executor::{EventSink, ExecStartOutcome};
use omt_exec::layout::{self, ExecFile};

use crate::daemon;
use crate::state::SharedSession;

/// review R2-4：对账目标的租约走 shim 路径释放（声明 home + 临时授权），
/// 不依赖 desktop 会话此刻是否已打开该 workspace home。后台执行；启动对账与
/// 运行期的执行丢失收尾（F-EXEC-17）共用。
pub(crate) fn release_reconcile_targets_in_background(label: &'static str) {
    let runtime_dir = daemon::runtime_dir();
    let daemon_bin = daemon::sidecar_path("omt-daemon").ok();
    std::thread::spawn(move || {
        let connector = omt_exec::daemon_link::admin_connector(runtime_dir.clone(), daemon_bin);
        let released = omt_exec::executor::reconcile_with_connector(&connector, &runtime_dir);
        if released > 0 {
            eprintln!("{label}：经 shim 路径释放了 {released} 个 exec 的租约");
        }
    });
}

/// §39 webview 重载后的双跟随者：本进程内每个 exec 至多一个跟随者。重载后
/// 前端的 attachedRef 随页面重建而清空，会对同一 exec 再调 exec_attach——
/// 新跟随者接管（它的 Channel 属于新页面），旧跟随者经取消标志在下一次轮询
/// （≤200ms）退出，不再向已失效的 Channel 空推、空转。
static FOLLOWERS: std::sync::Mutex<std::collections::BTreeMap<String, Arc<AtomicBool>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn claim_follower(exec_id: &str) -> Arc<AtomicBool> {
    let token = Arc::new(AtomicBool::new(false));
    let previous = FOLLOWERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(exec_id.to_string(), token.clone());
    if let Some(previous) = previous {
        previous.store(true, Ordering::SeqCst);
    }
    token
}

/// 跟随者结束时摘掉登记——只摘自己那一条（已被接管的旧跟随者不动新登记）。
fn release_follower(exec_id: &str, token: &Arc<AtomicBool>) {
    let mut followers = FOLLOWERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if followers
        .get(exec_id)
        .is_some_and(|current| Arc::ptr_eq(current, token))
    {
        followers.remove(exec_id);
    }
}

/// 尾随 `<agentDir>/<execId>.jsonl` 推送前端（KD-E3：取代进程内 Channel 泵）。
/// F-EXEC-17：shim 已死却没写出 exit 行时，就地对账（落 Failed / Orphan）、
/// 补推合成 exit 让前端收尾，并在后台经 shim 路径释放租约。
fn follow(
    home_id: &str,
    agent_id: &str,
    exec_id: &str,
    after_seq: Option<u64>,
    sink: EventSink,
) -> Result<(), String> {
    let runtime_dir = daemon::runtime_dir();
    let dir = layout::agent_dir(&runtime_dir, home_id, agent_id)?;
    let path = layout::exec_file(&dir, exec_id, ExecFile::Jsonl);
    let (h, a, e) = (
        home_id.to_string(),
        agent_id.to_string(),
        exec_id.to_string(),
    );
    let (lost_rt, lost_h, lost_a, lost_e) = (runtime_dir.clone(), h.clone(), a.clone(), e.clone());
    let on_lost: omt_exec::follow::LostHook = Box::new(move |next_seq| {
        let exit =
            omt_exec::control::settle_lost_exec(&lost_rt, &lost_h, &lost_a, &lost_e, next_seq)?;
        release_reconcile_targets_in_background("执行进程丢失");
        Some(exit)
    });
    let active = Arc::new(move || omt_exec::control::exec_active(&runtime_dir, &h, &a, &e));
    // §39：同一 exec 只保留一个跟随者（exec_start 与 exec_attach 共用本函数）
    let cancel = claim_follower(exec_id);
    let handle = omt_exec::follow::follow_jsonl_with(
        path,
        after_seq,
        sink,
        active,
        Some(on_lost),
        omt_exec::follow::FollowOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        },
    );
    let exec_key = exec_id.to_string();
    std::thread::spawn(move || {
        let _ = handle.join();
        release_follower(&exec_key, &cancel);
    });
    Ok(())
}

fn channel_sink(channel: tauri::ipc::Channel<serde_json::Value>) -> EventSink {
    Arc::new(move |value| {
        let _ = channel.send(value);
    })
}

/// U6 (R-B)：exec_start。派发链 ①–⑥ 在本进程，执行在分离的 exec-shim 里；
/// 前端 Channel 由 jsonl 尾随喂入（事件形状不变）。
/// U2-1（CF5）：整个 `launch::start`（多次 RPC + 进程启动，阻塞）连同
/// 会话锁访问经 in_blocking_scope 跑在阻塞线程池，不占 tokio worker；
/// CF6/F-TERM-16：rpc 经阻塞令牌（state::BlockingScope::rpc）取得且
/// **借用 scope**——launch::start 本就接受借用的 DaemonRpc<'_>，拿锁
/// 能力带不出阻塞作用域。
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub async fn exec_start(
    shared: tauri::State<'_, SharedSession>,
    home_id: String,
    home_path: String,
    node_id: String,
    harness: String,
    unattended: bool,
    model: Option<String>,
    resume_session: Option<String>,
    agent_id: Option<String>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<ExecStartOutcome, String> {
    let shared = shared.inner().clone();
    shared
        .in_blocking_scope(move |scope| {
            let rpc = scope.rpc();
            let omt_bin = daemon::omt_sidecar_path()?;
            let runtime_dir = daemon::runtime_dir();
            let outcome = omt_exec::launch::start(
                &rpc,
                &omt_exec::launch::LaunchRequest {
                    runtime_dir: &runtime_dir,
                    omt_bin: &omt_bin,
                    home_id: &home_id,
                    home_path: Path::new(&home_path),
                    target: omt_exec::executor::LaunchTarget::Node(&node_id),
                    origin: omt_exec::policy::LaunchOrigin::Desktop,
                    harness: &harness,
                    unattended,
                    model: model.as_deref(),
                    resume_session: resume_session.as_deref(),
                    target_agent_id: agent_id.as_deref(),
                    parent_agent_id: None,
                    max_duration: omt_exec::executor::DEFAULT_MAX_DURATION,
                },
            )?;
            follow(
                &home_id,
                &outcome.agent_id,
                &outcome.exec_id,
                None,
                channel_sink(channel),
            )?;
            Ok(outcome)
        })
        .await
}

/// 幂等终止：SIGTERM 发给该 exec 的 shim（身份校验后）。
#[tauri::command(async)]
pub fn exec_stop(exec_id: String) -> Result<(), String> {
    omt_exec::control::stop_exec(&daemon::runtime_dir(), &exec_id)
}

/// P-10：停止当前 home 的全部运行中 exec（替代旧的「桌面退出即终止」）。
/// F-EXEC-3：单个失败不中止其余——失败明细**进返回值**（F-EXEC-12：此前
/// 只进 stderr，UI 把部分失败当成功提示「当前没有运行中的 agent」）。
///
/// 返回 JSON（camelCase 键）形状：
/// `{ "signaled": <number>, "failed": [<string>, ...] }`
/// - `signaled`（number）：实际发出停止信号的 exec 数（ESRCH 竞态计入；
///   shim 已死走对账收尾的不计）。
/// - `failed`（string[]）：逐 agent/exec 收集的失败明细，空数组 = 全部成功。
#[tauri::command(async)]
pub fn exec_stop_all(home_id: String) -> Result<serde_json::Value, String> {
    let outcome = omt_exec::control::stop_all(&daemon::runtime_dir(), &home_id)?;
    Ok(stop_all_value(&home_id, &outcome))
}

/// F-EXEC-12：exec_stop_all 的内芯（tauri 胶水之外的可测面）——把
/// StopAllOutcome 映射成前端契约 JSON；失败明细同时保留 stderr 记录。
fn stop_all_value(home_id: &str, outcome: &omt_exec::control::StopAllOutcome) -> serde_json::Value {
    if !outcome.errors.is_empty() {
        eprintln!(
            "exec_stop_all（home {home_id}）：{} 个 agent 停止失败：{}",
            outcome.errors.len(),
            outcome.errors.join("；")
        );
    }
    serde_json::json!({
        "signaled": outcome.signaled_count,
        "failed": outcome.errors,
    })
}

/// P-10：重新接上仍在运行的 exec 的实时流（桌面重启后 shim 仍在跑）。
/// `after_seq` = 前端已有的最后一个 seq（无则 None，从头推）。
#[tauri::command(async)]
pub fn exec_attach(
    home_id: String,
    exec_id: String,
    after_seq: Option<u64>,
    channel: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    let (agent, _) =
        omt_exec::agents::find_exec_in_home(&daemon::runtime_dir(), &home_id, &exec_id)?
            .ok_or_else(|| format!("exec {exec_id} 不存在"))?;
    follow(
        &home_id,
        &agent.id,
        &exec_id,
        after_seq,
        channel_sink(channel),
    )
}

/// 历史 transcript 尾部回载（F-PI-3 / F-U12-4）。
#[tauri::command(async)]
pub fn exec_transcript_tail(
    home_id: String,
    exec_id: String,
    max_lines: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    Ok(omt_exec::executor::transcript_tail_from(
        &daemon::runtime_dir(),
        &home_id,
        &exec_id,
        max_lines,
    ))
}

/// Orphan 强杀。Renderer 只能提交持久化记录身份，不能提交 pid。
#[tauri::command(async)]
pub fn exec_kill_orphan(home_id: String, agent_id: String, exec_id: String) -> Result<(), String> {
    omt_exec::executor::kill_orphan(&daemon::runtime_dir(), &home_id, &agent_id, &exec_id)
}

/// A8：设置页「派发策略」。无效文件不报 Err——返回 {error, path, defaults} 让界面
/// 提示「CLI/MCP 派发已全部拒绝」并预填文档默认值，绝不伪装成文件内容。
#[tauri::command(async)]
pub fn exec_policy_get() -> Result<serde_json::Value, String> {
    let runtime_dir = daemon::runtime_dir();
    let file = omt_exec::layout::policy_file(&runtime_dir);
    let path = file.display().to_string();
    let defaults = omt_exec::policy::ExecPolicy::default();
    match omt_exec::policy::load(&runtime_dir) {
        Ok(policy) => Ok(serde_json::json!({
            "policy": policy,
            "defaults": defaults,
            "source": if file.is_file() { "file" } else { "default" },
            "path": path,
        })),
        Err(problem) => Ok(serde_json::json!({
            "error": format!("{}: {}", problem.code, problem.message),
            "defaults": defaults,
            "path": path,
        })),
    }
}

/// A8：保存派发策略（校验 + 原子落盘在 omt_exec::policy::save），回读最新视图。
#[tauri::command(async)]
pub fn exec_policy_set(policy: omt_exec::policy::ExecPolicy) -> Result<serde_json::Value, String> {
    omt_exec::policy::save(&daemon::runtime_dir(), &policy)?;
    exec_policy_get()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F-EXEC-12：exec_stop_all 的返回值必须以**数据**携带失败明细（旧形状
    /// `Promise<number>` 只回 signaled 计数、失败仅进 stderr，UI 把部分失败
    /// 当成功提示「当前没有运行中的 agent」）。契约形状（camelCase 键）：
    /// `{ "signaled": <number>, "failed": [<string>, ...] }`。
    #[test]
    fn stop_all_value_carries_failed_list() {
        let outcome = omt_exec::control::StopAllOutcome {
            signaled_count: 2,
            errors: vec!["agent a1: 记录损坏".into(), "exec e2: kill 失败".into()],
        };
        let value = stop_all_value("h_x", &outcome);
        assert_eq!(value["signaled"], 2, "{value}");
        let failed = value["failed"].as_array().expect("failed 必须是数组");
        assert_eq!(failed.len(), 2, "{value}");
        assert!(failed[0].as_str().unwrap().contains("a1"));

        let clean = stop_all_value("h_x", &Default::default());
        assert_eq!(clean["signaled"], 0, "{clean}");
        assert_eq!(clean["failed"], serde_json::json!([]), "{clean}");
    }

    /// §39：同一 exec 的第二个跟随者（webview 重载后的 exec_attach）接管，
    /// 第一个被取消；过期的 release 不得摘掉当前的登记。
    #[test]
    fn a_second_follower_for_the_same_exec_cancels_the_first() {
        use std::sync::atomic::Ordering;
        let first = claim_follower("exec-dup-test");
        let second = claim_follower("exec-dup-test");
        assert!(first.load(Ordering::SeqCst), "旧跟随者必须被取消");
        assert!(!second.load(Ordering::SeqCst));
        release_follower("exec-dup-test", &first); // 旧的结束：不得影响新的
        assert!(FOLLOWERS
            .lock()
            .unwrap()
            .get("exec-dup-test")
            .is_some_and(|current| Arc::ptr_eq(current, &second)));
        release_follower("exec-dup-test", &second);
        assert!(!FOLLOWERS.lock().unwrap().contains_key("exec-dup-test"));
    }
}
