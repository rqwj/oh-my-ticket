//! U2 (R-B/R-E 内核): ExecutionManager —— harness 子进程的执行内核。
//!
//! 职责：spawn（setpgid 进程组、cwd 剥 `.omt` 尾、env 合并 + OMT_RUNTIME_DIR
//! 强制注入）、stdout/stderr 泵（TranscriptEvent + raw.log）、exec_stop
//! （幂等 + 进程组 TERM→KILL 升级）、起动窗/最大时长兜底、终态分类与
//! agents.json 状态回写。
//!
//! 不在本模块：ensure-run 链与 MCP 注入（U3/U4）、租约释放的 daemon 调用
//! （U4 接线，本模块只发终态回调）、启动 reconcile（U10）。
//!
//! 测试可注入：`startup_window` / `max_duration`（假 binary 快速验证起动
//! 失败与兜底 kill）。事件经 `on_event` 回调抛出——tauri Channel 是其一个
//! 实现（U4 接线），单测用 mpsc。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agents::{self, AgentAttachment, AgentRecordStatus};
use crate::daemon;
use crate::harness;
use crate::mcp_injection::McpInjection;

const STARTUP_WINDOW: Duration = Duration::from_secs(30);
const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(120 * 60);
const KILL_GRACE: Duration = Duration::from_secs(3);
const STDERR_TAIL_MAX_CHARS: usize = 2000;

/// 终止原因（§3.2 KillReason）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillReason {
    User,
    MaxDuration,
    AppExit,
}

impl KillReason {
    fn as_str(&self) -> &'static str {
        match self {
            KillReason::User => "user",
            KillReason::MaxDuration => "maxduration",
            KillReason::AppExit => "appexit",
        }
    }
}

/// 运行时终态（与 agents.json 的 AgentRecordStatus 对应）。
#[derive(Debug, Clone, PartialEq)]
pub enum Terminal {
    Exited(i32),
    Failed(String),
    Killed(KillReason),
}

/// spawn 入参（U4 链步骤 ⑧；args 顺序由调用方保证——用户 extra_args 在前、
/// desktop 注入的 flag 在后，后者覆盖前者）。
pub struct SpawnRequest {
    pub exec_id: String,
    pub agent_id: String,
    pub home_id: String,
    /// home（`.omt`）目录；cwd 剥尾取 workspace 根（§3.6.4）
    pub home_path: PathBuf,
    pub harness: String,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// 用户配置的 env（字面量；保留键已在保存时拒绝，这里再防御一次）
    pub env_additions: BTreeMap<String, String>,
    /// MCP 桥接注入（U3）：spawn 时生效 env，终态时释放 workspace 注入
    pub mcp: McpInjection,
}

/// 事件回调（tauri Channel 的 mpsc 替身）。
pub type EventSink = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// 单个 exec 的注册表条目（Arc 共享给读线程/等待线程）。
pub struct ExecSession {
    pub exec_id: String,
    pub agent_id: String,
    pub home_id: String,
    pub harness: String,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub pid: u32,
    pub pgid: i32,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// epoch 毫秒字符串——transcript header 与 execs[].startedAt 同格式
    /// （F-U4a-6）
    pub started_at_epoch_ms: String,
    pub raw_log: PathBuf,
    pub runtime_dir: PathBuf,
    pub mcp: McpInjection,
    first_event_seen: AtomicBool,
    stop_requested: AtomicBool,
    terminal: Mutex<Option<Terminal>>,
}

impl ExecSession {
    pub fn terminal(&self) -> Option<Terminal> {
        self.terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn kill_tree(&self, sig: i32) {
        // 进程组 kill（setpgid(0,0) 后 pgid == 子 pid）；组已消失则忽略。
        unsafe {
            libc::kill(-self.pgid, sig);
        }
    }
}

/// §3.1 secrets 纪律：无 regex 依赖的行级启发式脱敏——宁枉勿纵，只用于
/// Failed 的 stderr 尾诊断。
fn sanitize_line(line: &str) -> String {
    const KEYWORDS: &[&str] = &["key", "token", "secret", "password", "bearer"];
    let lower = line.to_lowercase();
    let mut cut: Option<usize> = None;
    for kw in KEYWORDS {
        if let Some(pos) = lower.find(kw) {
            let after = lower[pos + kw.len()..].trim_start();
            // 关键字后紧跟分隔符（= 或 :）即视为键值形态；bearer 单独算
            let is_kv = after.starts_with('=') || after.starts_with(':');
            let is_bearer = *kw == "bearer";
            if is_kv || is_bearer {
                let value_start = if is_kv {
                    let rest = &lower[pos + kw.len()..];
                    rest.find(|c: char| c != '=' && c != ':' && c != ' ')
                        .map(|off| {
                            rest.char_indices()
                                .nth(off)
                                .map(|(i, _)| pos + kw.len() + i)
                                .unwrap_or(line.len())
                        })
                        .unwrap_or(line.len())
                } else {
                    pos + kw.len()
                };
                cut = Some(cut.map_or(value_start, |c| c.min(value_start)));
            }
        }
    }
    if let Some(pos) = lower.find("sk-") {
        // sk- 前缀密钥形态：保留 sk-，值部分替换
        cut = Some(cut.map_or(pos + 3, |c| c.min(pos + 3)));
    }
    match cut {
        Some(idx) if idx <= line.len() && line.is_char_boundary(idx) => {
            format!("{} ***", &line[..idx].trim_end())
        }
        _ => line.to_string(),
    }
}

fn sanitize_stderr_tail(tail: &str) -> String {
    let mut joined = tail
        .lines()
        .map(sanitize_line)
        .collect::<Vec<_>>()
        .join("\n");
    joined.truncate(STDERR_TAIL_MAX_CHARS);
    // truncate 可能切在多字节字符中间——按字符边界收缩
    while !joined.is_empty() && !joined.is_char_boundary(joined.len()) {
        joined.pop();
    }
    joined
}

/// home（`.omt`）目录 → workspace 根（§3.6.4：spawn cwd 不允许落在 `.omt` 里）。
pub fn workspace_root_of(home_path: &Path) -> PathBuf {
    if home_path.file_name().map(|n| n == ".omt").unwrap_or(false) {
        home_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| home_path.to_path_buf())
    } else {
        home_path.to_path_buf()
    }
}

#[derive(Default)]
pub struct ExecutionManager {
    sessions: Mutex<HashMap<String, Arc<ExecSession>>>,
    startup_window: Option<Duration>,
    max_duration: Option<Duration>,
}

impl ExecutionManager {
    /// tauri 管理的构造入口：执行启动 demote（F-U4a-2）。
    pub fn new() -> Self {
        let mgr = Self::with_limits(STARTUP_WINDOW, DEFAULT_MAX_DURATION);
        match agents::demote_live_agents(&daemon::runtime_dir()) {
            Ok(n) if n > 0 => eprintln!("启动 demote：{n} 个 Live agent 已降级（app 重启）"),
            Err(err) => eprintln!("启动 demote 失败：{err}"),
            _ => {}
        }
        match crate::mcp_injection::sweep_omp_crash_residue(&daemon::runtime_dir()) {
            Ok(n) if n > 0 => eprintln!("启动清扫：{n} 个 workspace 的 omp MCP 注入残留已恢复"),
            Err(err) => eprintln!("omp 注入残留清扫失败：{err}"),
            _ => {}
        }
        mgr
    }

    fn with_limits(startup_window: Duration, max_duration: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            startup_window: Some(startup_window),
            max_duration: Some(max_duration),
        }
    }

    fn startup_window(&self) -> Duration {
        self.startup_window.unwrap_or(STARTUP_WINDOW)
    }

    fn max_duration(&self) -> Duration {
        self.max_duration.unwrap_or(DEFAULT_MAX_DURATION)
    }

    /// spawn 一个 harness 会话并接入事件泵。`runtime_dir` 用于 OMT_RUNTIME_DIR
    /// 强制注入与 raw.log 落盘。
    ///
    /// F-U3-9：`req.mcp` 已在调用方 prepare 完毕；本函数的任何一条错误返回
    /// 路径都意味着不会有 `ExecSession`、也就不会有 `finish_terminal` 去
    /// release 它——在这里统一兜底释放，残留仍由启动 sweep 兜底。
    pub fn spawn(
        &self,
        runtime_dir: &Path,
        req: SpawnRequest,
        on_event: EventSink,
    ) -> Result<Arc<ExecSession>, String> {
        let mcp_on_failure = req.mcp.clone();
        let result = self.spawn_inner(runtime_dir, req, on_event);
        if result.is_err() {
            if let Err(err) = crate::mcp_injection::release(runtime_dir, &mcp_on_failure) {
                eprintln!("MCP 注入释放失败（spawn 失败路径）：{err}（残留由启动 sweep 兜底）");
            }
        }
        result
    }

    fn spawn_inner(
        &self,
        runtime_dir: &Path,
        req: SpawnRequest,
        on_event: EventSink,
    ) -> Result<Arc<ExecSession>, String> {
        if req
            .env_additions
            .keys()
            .any(|k| matches!(k.as_str(), "OMT_RUNTIME_DIR" | "OMT_HOME" | "PATH"))
        {
            return Err(
                "env_additions 含保留键（PATH/OMT_RUNTIME_DIR/OMT_HOME 由 desktop 注入）".into(),
            );
        }
        let cwd = workspace_root_of(&req.home_path);
        if !cwd.is_dir() {
            return Err(format!("workspace 根不存在：{}", cwd.display()));
        }
        let raw_log = runtime_dir
            .join("transcripts")
            .join(&req.home_id)
            .join(format!("{}.raw.log", req.exec_id));
        if let Some(dir) = raw_log.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create transcripts dir: {e}"))?;
        }

        let mut cmd = std::process::Command::new(&req.program);
        cmd.args(&req.args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .env("PATH", harness::augmented_path())
            .env("OMT_RUNTIME_DIR", runtime_dir);
        for (k, v) in &req.env_additions {
            cmd.env(k, v);
        }
        if let Some((key, value)) = req.mcp.env_entry() {
            cmd.env(key, value);
        }
        // 子进程自成进程组（§3.6.3：setpgid 而非 setsid——退出钩子要能 killpg）
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {} 失败: {e}", req.program.display()))?;
        let pid = child.id();
        let started_at_epoch_ms = agents::now_epoch_ms();
        let started = Instant::now();

        let session = Arc::new(ExecSession {
            exec_id: req.exec_id.clone(),
            agent_id: req.agent_id.clone(),
            home_id: req.home_id.clone(),
            harness: req.harness.clone(),
            node_id: req.node_id.clone(),
            run_id: req.run_id.clone(),
            pid,
            pgid: pid as i32,
            program: req.program.clone(),
            args: req.args.clone(),
            cwd: cwd.clone(),
            started_at_epoch_ms,
            raw_log,
            runtime_dir: runtime_dir.to_path_buf(),
            mcp: req.mcp.clone(),
            first_event_seen: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            terminal: Mutex::new(None),
        });
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(req.exec_id.clone(), session.clone());

        on_event(serde_json::json!({
            "execId": req.exec_id,
            "kind": "session_start",
            "harness": req.harness,
            "program": session.program.display().to_string(),
            "args": session.args,
            "cwd": cwd.display().to_string(),
            "pid": pid,
        }));

        // stdout/stderr 读线程：逐行 → 事件回调 + raw.log 追加。
        let seq = Arc::new(AtomicU64::new(0));
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_reader(
                out,
                "stdout",
                session.clone(),
                seq.clone(),
                on_event.clone(),
            ));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_reader(
                err,
                "stderr",
                session.clone(),
                seq.clone(),
                on_event.clone(),
            ));
        }

        // 等待线程：终态分类 + kill 升级 + agents.json 回写 + 终态事件。
        let waiter_session = session.clone();
        let waiter_events = on_event.clone();
        let startup_window = self.startup_window();
        let max_duration = self.max_duration();
        std::thread::spawn(move || {
            let mut term_sig: Option<i32> = None;
            let mut kill_reason: Option<KillReason> = None;
            let mut kill_at: Option<Instant> = None;
            loop {
                if kill_reason.is_none() {
                    if waiter_session.stop_requested.load(Ordering::Relaxed) {
                        kill_reason = Some(KillReason::User);
                    } else if started.elapsed() >= max_duration {
                        kill_reason = Some(KillReason::MaxDuration);
                    }
                }
                if kill_reason.is_some() {
                    if term_sig.is_none() {
                        term_sig = Some(libc::SIGTERM);
                        waiter_session.kill_tree(libc::SIGTERM);
                        kill_at = Some(Instant::now());
                    } else if kill_at.is_some_and(|t| t.elapsed() >= KILL_GRACE) {
                        waiter_session.kill_tree(libc::SIGKILL);
                    }
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let terminal = classify(
                            &waiter_session,
                            status.code(),
                            term_sig.is_some(),
                            kill_reason,
                            started,
                            startup_window,
                        );
                        finish_terminal(&waiter_session, &terminal, &waiter_events, started);
                        break;
                    }
                    Ok(None)
                        if Instant::now()
                            >= started + max_duration + KILL_GRACE + Duration::from_secs(5) =>
                    {
                        // 兜底：升级 KILL 后仍未收割（极端）——强制按 Killed 收口
                        waiter_session.kill_tree(libc::SIGKILL);
                        let terminal =
                            Terminal::Killed(kill_reason.unwrap_or(KillReason::MaxDuration));
                        finish_terminal(&waiter_session, &terminal, &waiter_events, started);
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(err) => {
                        eprintln!("try_wait 失败（exec {}）: {err}", waiter_session.exec_id);
                        break;
                    }
                }
            }
            for reader in readers {
                let _ = reader.join();
            }
        });

        Ok(session)
    }

    /// 终止一个 exec（幂等：已终态/不存在的 execId 直接返回成功）。
    pub fn exec_stop(&self, exec_id: &str) -> Result<(), String> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = sessions.get(exec_id) {
            if session.terminal().is_none() {
                session.stop_requested.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// 退出钩子：终止全部会话（KD-D7）。app 退出路径调用。
    pub fn kill_all(&self, reason: KillReason) {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for session in sessions.values() {
            if session.terminal().is_none() {
                session.stop_requested.store(true, Ordering::Relaxed);
                // AppExit 终止不走 stop_requested 的 User 分类——直接标记并 kill
                session.kill_tree(libc::SIGTERM);
                let mut terminal = session
                    .terminal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if terminal.is_none() {
                    *terminal = Some(Terminal::Killed(reason));
                }
            }
        }
        drop(sessions);
        // 给读线程/等待线程一点收割时间；SIGKILL 兜底由各等待线程完成。
        std::thread::sleep(Duration::from_millis(200));
    }

    pub fn get(&self, exec_id: &str) -> Option<Arc<ExecSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(exec_id)
            .cloned()
    }
}

fn spawn_reader(
    pipe: impl std::io::Read + Send + 'static,
    stream: &'static str,
    session: Arc<ExecSession>,
    seq: Arc<AtomicU64>,
    on_event: EventSink,
) -> std::thread::JoinHandle<()> {
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&session.raw_log)
        .ok();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        let reader = BufReader::new(pipe);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            session.first_event_seen.store(true, Ordering::Relaxed);
            let n = seq.fetch_add(1, Ordering::Relaxed);
            on_event(serde_json::json!({
                "execId": session.exec_id,
                "seq": n,
                "ts": agents::now_epoch_ms(),
                "kind": "raw",
                "stream": stream,
                "text": line,
            }));
            if let Some(log) = log.as_mut() {
                let _ = writeln!(log, "[{stream}] {line}");
            }
        }
    })
}

/// 终态分类（§3.2）：kill → Killed(reason)；起动窗内非零退出 → Failed(stderr
/// 尾，已脱敏)；其余 → Exited(code)。
#[allow(clippy::too_many_arguments)]
fn classify(
    session: &ExecSession,
    code: Option<i32>,
    killed: bool,
    kill_reason: Option<KillReason>,
    started: Instant,
    startup_window: Duration,
) -> Terminal {
    if killed {
        return Terminal::Killed(kill_reason.unwrap_or(KillReason::User));
    }
    let code = code.unwrap_or(-1);
    if started.elapsed() < startup_window && code != 0 {
        let tail = std::fs::read_to_string(&session.raw_log)
            .map(|text| {
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(10);
                lines[start..].join("\n")
            })
            .unwrap_or_default();
        let detail = sanitize_stderr_tail(&tail);
        return Terminal::Failed(if detail.is_empty() {
            format!("harness 在起动窗内退出（code {code}），无输出")
        } else {
            format!("harness 在起动窗内退出（code {code}）：{detail}")
        });
    }
    Terminal::Exited(code)
}

/// 终态收口：写 agents.json（仅状态变化时落盘）+ 终态事件。
fn finish_terminal(
    session: &ExecSession,
    terminal: &Terminal,
    on_event: &EventSink,
    started: Instant,
) {
    {
        let mut guard = session
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_some() {
            return; // 已收口（如 kill_all 与等待线程竞争）
        }
        *guard = Some(terminal.clone());
    }
    let runtime_dir = &session.runtime_dir;
    let (status, detail) = match terminal {
        Terminal::Exited(_) => (AgentRecordStatus::Exited, None),
        Terminal::Failed(detail) => (AgentRecordStatus::Failed, Some(detail.clone())),
        Terminal::Killed(_) => (AgentRecordStatus::Killed, None),
    };
    // omp workspace 注入的恢复（env 注入随进程消失；persistent 为 no-op）
    if let Err(err) = crate::mcp_injection::release(runtime_dir, &session.mcp) {
        eprintln!(
            "MCP 注入释放失败（exec {}）：{err}（残留由启动 sweep 兜底）",
            session.exec_id
        );
    }
    let _ = agents::update_agent_status(
        runtime_dir,
        &session.home_id,
        &session.agent_id,
        status,
        AgentAttachment::None,
    );
    if let Some(detail) = detail {
        let exec_id = session.exec_id.clone();
        let _ = agents::update_exec(
            runtime_dir,
            &session.home_id,
            &session.agent_id,
            &exec_id,
            |exec| {
                exec.failure_detail = Some(detail);
            },
        );
    }
    let duration_ms = started.elapsed().as_millis() as u64;
    on_event(serde_json::json!({
        "execId": session.exec_id,
        "kind": "exit",
        "status": match status {
            AgentRecordStatus::Exited => "exited",
            AgentRecordStatus::Failed => "failed",
            AgentRecordStatus::Killed => "killed",
            _ => "exited",
        },
        "code": match terminal {
            Terminal::Exited(code) => Some(*code),
            _ => None,
        },
        "killReason": match terminal {
            Terminal::Killed(reason) => Some(reason.as_str()),
            _ => None,
        },
        "durationMs": duration_ms,
    }));
}

/// 幂等终止（§3.4）：对已终态/不存在的 execId 直接成功。
#[tauri::command(async)]
pub fn exec_stop(
    state: tauri::State<'_, crate::state::ExecutionState>,
    exec_id: String,
) -> Result<(), String> {
    state.0.exec_stop(&exec_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::mpsc;

    fn wait_for<T>(mut check: impl FnMut() -> Option<T>, timeout: Duration) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(v) = check() {
                return Some(v);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn spawn_streams_events_and_stop_kills_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_path = dir.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let manager = ExecutionManager::with_limits(STARTUP_WINDOW, DEFAULT_MAX_DURATION);
        // 孙进程：脚本内再起一个 sleep——killpg 必须连它一起清
        let script = write_script(
            dir.path(),
            "talker",
            "echo hello\nsh -c 'sleep 30' >/dev/null 2>&1 &\nwait\n",
        );
        let (tx, rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let session = manager
            .spawn(
                &runtime,
                SpawnRequest {
                    exec_id: "exec-kill".into(),
                    agent_id: "agent-kill".into(),
                    home_id: "h_test".into(),
                    home_path: home_path.clone(),
                    harness: "fake".into(),
                    node_id: None,
                    run_id: None,
                    program: script,
                    args: vec![],
                    env_additions: BTreeMap::new(),
                    mcp: McpInjection::None,
                },
                sink,
            )
            .unwrap();

        // 流事件可见（stdout "hello"）
        let saw_hello = wait_for(
            || {
                while let Ok(event) = rx.try_recv() {
                    if event["text"].as_str() == Some("hello") {
                        return Some(());
                    }
                }
                None
            },
            Duration::from_secs(5),
        );
        assert!(saw_hello.is_some(), "应收到 stdout raw 事件");

        // 终止 → Killed(User)，进程组（含孙进程）无残留
        manager.exec_stop("exec-kill").unwrap();
        let killed = wait_for(
            || match session.terminal() {
                Some(t @ Terminal::Killed(_)) => Some(t),
                _ => None,
            },
            Duration::from_secs(10),
        );
        assert!(matches!(killed, Some(Terminal::Killed(_))), "{killed:?}");
        // 进程组已无存活成员（killpg 生效到孙进程）
        let alive = unsafe { libc::kill(-session.pgid, 0) };
        assert_ne!(alive, 0, "进程组应已清空");
        // 幂等：再次 stop 与未知 id 均成功
        manager.exec_stop("exec-kill").unwrap();
        manager.exec_stop("exec-unknown").unwrap();
    }

    #[test]
    fn startup_window_failure_yields_failed_with_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_path = dir.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let manager = ExecutionManager::with_limits(Duration::from_secs(10), DEFAULT_MAX_DURATION);
        let script = write_script(dir.path(), "boom", "exit 1");
        let (tx, _rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let session = manager
            .spawn(
                &runtime,
                SpawnRequest {
                    exec_id: "exec-boom".into(),
                    agent_id: "agent-boom".into(),
                    home_id: "h_test".into(),
                    home_path: home_path.clone(),
                    harness: "fake".into(),
                    node_id: None,
                    run_id: None,
                    program: script,
                    args: vec![],
                    env_additions: BTreeMap::new(),
                    mcp: McpInjection::None,
                },
                sink,
            )
            .unwrap();
        let terminal = wait_for(|| session.terminal(), Duration::from_secs(5));
        assert!(
            matches!(terminal, Some(Terminal::Failed(_))),
            "{terminal:?}"
        );
    }

    #[test]
    fn failed_stderr_tail_is_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_path = dir.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let manager = ExecutionManager::with_limits(Duration::from_secs(10), DEFAULT_MAX_DURATION);
        let script = write_script(
            dir.path(),
            "leaky",
            "echo 'API_KEY=sk-abcdef123' >&2\necho 'Authorization: Bearer abc.def' >&2\nexit 3\n",
        );
        let (tx, _rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let session = manager
            .spawn(
                &runtime,
                SpawnRequest {
                    exec_id: "exec-leak".into(),
                    agent_id: "agent-leak".into(),
                    home_id: "h_test".into(),
                    home_path: home_path.clone(),
                    harness: "fake".into(),
                    node_id: None,
                    run_id: None,
                    program: script,
                    args: vec![],
                    env_additions: BTreeMap::new(),
                    mcp: McpInjection::None,
                },
                sink,
            )
            .unwrap();
        let terminal = wait_for(|| session.terminal(), Duration::from_secs(5));
        let Some(Terminal::Failed(detail)) = terminal else {
            panic!("应为 Failed：{terminal:?}");
        };
        assert!(!detail.contains("sk-abcdef123"), "{detail}");
        assert!(!detail.contains("abc.def"), "{detail}");
        assert!(detail.contains("API_KEY="), "{detail}");
        assert!(detail.contains("Bearer"), "{detail}");
    }

    #[test]
    fn env_merge_forces_desktop_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_path = dir.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let manager = ExecutionManager::with_limits(STARTUP_WINDOW, DEFAULT_MAX_DURATION);

        // 保留键（OMT_RUNTIME_DIR）在 spawn 入口被拒绝——desktop 注入的值
        // 才是唯一来源（§3.1）
        let script = write_script(
            dir.path(),
            "envpeek",
            "echo \"RT=$OMT_RUNTIME_DIR\"\necho \"FLAG=$MY_FLAG\"\nkill -TERM $$\n",
        );
        let (tx, rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let mut env = BTreeMap::new();
        env.insert("OMT_RUNTIME_DIR".to_string(), "/evil".to_string());
        env.insert("MY_FLAG".to_string(), "1".to_string());
        let rejected = manager.spawn(
            &runtime,
            SpawnRequest {
                exec_id: "exec-env-reserved".into(),
                agent_id: "agent-env".into(),
                home_id: "h_test".into(),
                home_path: home_path.clone(),
                harness: "fake".into(),
                node_id: None,
                run_id: None,
                program: script.clone(),
                args: vec![],
                env_additions: env,
                mcp: McpInjection::None,
            },
            sink.clone(),
        );
        assert!(
            rejected.is_err()
                && rejected
                    .err()
                    .map(|e| e.contains("保留键"))
                    .unwrap_or(false),
            "保留键必须被拒绝"
        );

        // 合法 env（MY_FLAG）注入生效；子进程看到的 OMT_RUNTIME_DIR 是 desktop 的
        let mut env = BTreeMap::new();
        env.insert("MY_FLAG".to_string(), "1".to_string());
        let _session = manager
            .spawn(
                &runtime,
                SpawnRequest {
                    exec_id: "exec-env".into(),
                    agent_id: "agent-env".into(),
                    home_id: "h_test".into(),
                    home_path: home_path.clone(),
                    harness: "fake".into(),
                    node_id: None,
                    run_id: None,
                    program: script,
                    args: vec![],
                    env_additions: env,
                    mcp: McpInjection::None,
                },
                sink,
            )
            .unwrap();
        let seen = wait_for(
            || {
                while let Ok(event) = rx.try_recv() {
                    if let Some(text) = event["text"].as_str() {
                        if let Some(rt) = text.strip_prefix("RT=") {
                            return Some(rt.to_string());
                        }
                    }
                }
                None
            },
            Duration::from_secs(5),
        );
        assert_eq!(seen.as_deref(), Some(runtime.to_str().unwrap()));
        let _ = manager.exec_stop("exec-env");
    }

    #[test]
    fn max_duration_watchdog_kills_long_running_exec() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_path = dir.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let manager =
            ExecutionManager::with_limits(Duration::from_secs(30), Duration::from_millis(400));
        let script = write_script(dir.path(), "sleeper", "sleep 30\n");
        let (tx, _rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let session = manager
            .spawn(
                &runtime,
                SpawnRequest {
                    exec_id: "exec-slow".into(),
                    agent_id: "agent-slow".into(),
                    home_id: "h_test".into(),
                    home_path: home_path.clone(),
                    harness: "fake".into(),
                    node_id: None,
                    run_id: None,
                    program: script,
                    args: vec![],
                    env_additions: BTreeMap::new(),
                    mcp: McpInjection::None,
                },
                sink,
            )
            .unwrap();
        let terminal = wait_for(|| session.terminal(), Duration::from_secs(10));
        assert!(
            matches!(&terminal, Some(Terminal::Killed(KillReason::MaxDuration))),
            "{terminal:?}"
        );
    }

    #[test]
    fn kill_all_marks_app_exit() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let home_path = dir.path().join("ws").join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let manager = ExecutionManager::with_limits(STARTUP_WINDOW, DEFAULT_MAX_DURATION);
        let script = write_script(dir.path(), "long", "sleep 30\n");
        let (tx, _rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let session = manager
            .spawn(
                &runtime,
                SpawnRequest {
                    exec_id: "exec-exit".into(),
                    agent_id: "agent-exit".into(),
                    home_id: "h_test".into(),
                    home_path: home_path.clone(),
                    harness: "fake".into(),
                    node_id: None,
                    run_id: None,
                    program: script,
                    args: vec![],
                    env_additions: BTreeMap::new(),
                    mcp: McpInjection::None,
                },
                sink,
            )
            .unwrap();
        manager.kill_all(KillReason::AppExit);
        let terminal = wait_for(|| session.terminal(), Duration::from_secs(10));
        assert!(matches!(
            terminal,
            Some(Terminal::Killed(KillReason::AppExit))
        ));
    }

    #[test]
    fn workspace_root_strips_omt_suffix() {
        assert_eq!(
            workspace_root_of(Path::new("/ws/.omt")),
            PathBuf::from("/ws")
        );
        assert_eq!(workspace_root_of(Path::new("/ws")), PathBuf::from("/ws"));
    }

    #[test]
    fn spawn_failure_releases_mcp_injection() {
        // F-U3-9: any spawn() error path must release an already-prepared
        // MCP bridge — nobody else will ever call finish_terminal for a
        // session that never got created. Go through the real prepare
        // path (pub(crate) `prepare_with`, injectable omt_path) rather
        // than hand-building a McpInjection — release now drives launcher
        // deletion off the tracked `launchers` set in state.json
        // (F-U3-12), which a hand-built value bypasses entirely.
        use crate::mcp_injection;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let workspace = dir.path().join("ws");
        let home_path = workspace.join(".omt");
        std::fs::create_dir_all(&home_path).unwrap();
        let fake_omt = dir.path().join("omt");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&fake_omt, "#!/bin/sh\necho fake omt\n").unwrap();
            std::fs::set_permissions(&fake_omt, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mcp = mcp_injection::prepare_with(
            &fake_omt,
            &runtime,
            &workspace,
            "omp",
            "exec-mcp-fail",
            false,
        )
        .unwrap();
        let launcher_path = runtime
            .join("harness")
            .join("omp-launchers")
            .join("exec-mcp-fail.sh");
        assert!(launcher_path.is_file(), "prepare 应已写出 launcher");
        let manager = ExecutionManager::with_limits(STARTUP_WINDOW, DEFAULT_MAX_DURATION);
        let (tx, _rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let result = manager.spawn(
            &runtime,
            SpawnRequest {
                exec_id: "exec-mcp-fail".into(),
                agent_id: "agent-mcp-fail".into(),
                home_id: "h_test".into(),
                home_path,
                harness: "fake".into(),
                node_id: None,
                run_id: None,
                program: dir.path().join("definitely-does-not-exist"),
                args: vec![],
                env_additions: BTreeMap::new(),
                mcp,
            },
            sink,
        );
        assert!(result.is_err(), "spawn 应因程序不存在而失败");
        assert!(
            !launcher_path.exists(),
            "spawn 失败必须释放已准备的 MCP 注入（F-U3-9）"
        );
    }

    #[test]
    fn sanitize_line_redacts_secret_shapes() {
        assert_eq!(sanitize_line("API_KEY=sk-abcdef123"), "API_KEY= ***");
        assert!(sanitize_line("Authorization: Bearer abc.def").starts_with("Authorization:"));
        assert!(sanitize_line("Authorization: Bearer abc.def").ends_with("***"));
        assert!(sanitize_line("password: hunter2").ends_with("***"));
        assert!(!sanitize_line("plain log line").contains("***"));
    }
}
