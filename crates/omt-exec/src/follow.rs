//! jsonl 尾随（设计 §3.4 / KD-E3，P-6 纯轮询）：从文件读出新增的完整行，
//! 按原样推给 sink（事件形状与旧 Channel 推送一致），读到 exit 即结束；
//! exec 已不活跃且文件连续两次不增长时也结束（shim 崩溃的兜底）——此时调用
//! `on_lost`（F-EXEC-17），其返回的合成 exit 事件照常推给 sink，前端据此收尾。

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::executor::EventSink;

const POLL: Duration = Duration::from_millis(200);
/// 文件尚不存在时最多等多久（shim 还没写出 header）。
const APPEAR_TIMEOUT: Duration = Duration::from_secs(30);

/// 「不活跃却没读到 exit」时的收尾钩子：入参为合成事件应使用的 seq（已见
/// 最大 seq + 1），返回要补推给 sink 的事件（通常是合成 exit），None 不推。
pub type LostHook = Box<dyn FnOnce(u64) -> Option<serde_json::Value> + Send>;

/// 跟随选项（§39）。
/// - `appear_timeout`：文件尚不存在时，exec **已不活跃**之后最多再等多久；
///   exec 仍活跃（shim 在、或仍在拉起宽限期内）就一直等——此前 launcher 的
///   `Ready::Timeout`（30s）之后，跟随者的 30s 先到期就静默退出，shim 之后
///   写出的事件再也接不回来。
/// - `cancel`：置位后跟随者在下一次轮询（≤ POLL）退出，不调 `on_lost`——
///   桌面端每个 exec 只保留一个跟随者，新的接管时取消旧的。
pub struct FollowOptions {
    pub appear_timeout: Duration,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for FollowOptions {
    fn default() -> Self {
        Self {
            appear_timeout: APPEAR_TIMEOUT,
            cancel: None,
        }
    }
}

pub fn follow_jsonl(
    path: PathBuf,
    after_seq: Option<u64>,
    sink: EventSink,
    active: Arc<dyn Fn() -> bool + Send + Sync>,
    on_lost: Option<LostHook>,
) -> std::thread::JoinHandle<()> {
    follow_jsonl_with(
        path,
        after_seq,
        sink,
        active,
        on_lost,
        FollowOptions::default(),
    )
}

pub fn follow_jsonl_with(
    path: PathBuf,
    after_seq: Option<u64>,
    sink: EventSink,
    active: Arc<dyn Fn() -> bool + Send + Sync>,
    on_lost: Option<LostHook>,
    opts: FollowOptions,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut offset: u64 = 0;
        let mut next_seq: u64 = after_seq.map_or(0, |after| after + 1);
        let mut pending: Vec<u8> = Vec::new();
        let mut idle_inactive = 0u32;
        let appear_deadline = std::time::Instant::now() + opts.appear_timeout;
        loop {
            // §39：被新的跟随者接管——直接退出（不是执行丢失，不调 on_lost）
            if opts
                .cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::SeqCst))
            {
                return;
            }
            let mut grew = false;
            match std::fs::File::open(&path) {
                Ok(mut file) => {
                    if file.seek(SeekFrom::Start(offset)).is_ok() {
                        let mut buf = Vec::new();
                        if let Ok(n) = file.read_to_end(&mut buf) {
                            if n > 0 {
                                offset += n as u64;
                                pending.extend_from_slice(&buf);
                                grew = true;
                            }
                        }
                    }
                }
                // §39：文件还没出现——exec 仍活跃就一直等；appear_timeout 只
                // 约束已不活跃的情形
                Err(_) if active() || std::time::Instant::now() < appear_deadline => {}
                Err(_) => return,
            }
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let Ok(event) = serde_json::from_slice::<serde_json::Value>(&line) else {
                    continue;
                };
                if event["kind"] == "session_header" {
                    continue;
                }
                if let Some(seq) = event["seq"].as_u64() {
                    next_seq = next_seq.max(seq + 1);
                }
                if let (Some(after), Some(seq)) = (after_seq, event["seq"].as_u64()) {
                    if seq <= after {
                        continue;
                    }
                }
                let is_exit = event["kind"] == "exit";
                sink(event);
                if is_exit {
                    return;
                }
            }
            if grew {
                idle_inactive = 0;
            } else if !active() {
                idle_inactive += 1;
                if idle_inactive >= 2 {
                    if let Some(event) = on_lost.and_then(|hook| hook(next_seq)) {
                        sink(event);
                    }
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    fn collector() -> (EventSink, Arc<Mutex<Vec<serde_json::Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        (Arc::new(move |v| s.lock().unwrap().push(v)), seen)
    }

    #[test]
    fn streams_lines_written_after_start_and_stops_at_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(
            &path,
            "{\"kind\":\"session_header\"}\n{\"kind\":\"session_start\",\"seq\":0}\n",
        )
        .unwrap();
        let (sink, seen) = collector();
        let handle = follow_jsonl(path.clone(), None, sink, Arc::new(|| true), None);
        std::thread::sleep(Duration::from_millis(300));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        // 半行先写、再补全：不得把半行当事件
        f.write_all(b"{\"kind\":\"raw\",\"seq\":1,").unwrap();
        f.flush().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        f.write_all(b"\"text\":\"hi\"}\n{\"kind\":\"exit\",\"seq\":2}\n")
            .unwrap();
        handle.join().unwrap();
        let kinds: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, vec!["session_start", "raw", "exit"]);
    }

    #[test]
    fn skips_seq_up_to_after_seq() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(
            &path,
            "{\"kind\":\"raw\",\"seq\":0}\n{\"kind\":\"raw\",\"seq\":1}\n{\"kind\":\"exit\",\"seq\":2}\n",
        )
        .unwrap();
        let (sink, seen) = collector();
        follow_jsonl(path, Some(0), sink, Arc::new(|| true), None)
            .join()
            .unwrap();
        let seqs: Vec<u64> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn gives_up_when_exec_inactive_and_file_stops_growing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"raw\",\"seq\":0}\n").unwrap();
        let (sink, seen) = collector();
        let started = std::time::Instant::now();
        follow_jsonl(path, None, sink, Arc::new(|| false), None)
            .join()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    /// F-EXEC-17：不活跃放弃时调用 on_lost（seq 续接已见最大值），其返回的
    /// 合成 exit 推给 sink；读到真实 exit 结束时不调用。
    #[test]
    fn abandon_invokes_on_lost_and_forwards_synthetic_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"raw\",\"seq\":4}\n").unwrap();
        let (sink, seen) = collector();
        let hook: LostHook = Box::new(|seq| {
            Some(serde_json::json!({ "kind": "exit", "seq": seq, "status": "failed" }))
        });
        follow_jsonl(path, None, sink, Arc::new(|| false), Some(hook))
            .join()
            .unwrap();
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["kind"], "exit");
        assert_eq!(events[1]["seq"], 5);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("done.jsonl");
        std::fs::write(&path, "{\"kind\":\"exit\",\"seq\":0}\n").unwrap();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&called);
        let hook: LostHook = Box::new(move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            None
        });
        let (sink, _) = collector();
        follow_jsonl(path, None, sink, Arc::new(|| false), Some(hook))
            .join()
            .unwrap();
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "真实 exit 结束不得触发 on_lost"
        );
    }

    /// §39：Ready::Timeout 后文件迟迟不出现——exec 仍活跃就一直等，不因
    /// appear_timeout 放弃；文件出现后照常推事件。
    #[test]
    fn waits_past_appear_timeout_while_exec_is_active() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("late.jsonl");
        let (sink, seen) = collector();
        let handle = follow_jsonl_with(
            path.clone(),
            None,
            sink,
            Arc::new(|| true),
            None,
            FollowOptions {
                appear_timeout: Duration::from_millis(100),
                cancel: None,
            },
        );
        std::thread::sleep(Duration::from_millis(600));
        std::fs::write(
            &path,
            "{\"kind\":\"session_start\",\"seq\":0}\n{\"kind\":\"exit\",\"seq\":1}\n",
        )
        .unwrap();
        handle.join().unwrap();
        let kinds: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, vec!["session_start", "exit"]);
    }

    /// 反例（行为不变）：已不活跃且文件始终不出现——超过 appear_timeout 即结束。
    #[test]
    fn gives_up_on_missing_file_once_inactive_and_past_appear_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let (sink, seen) = collector();
        let started = std::time::Instant::now();
        follow_jsonl_with(
            tmp.path().join("never.jsonl"),
            None,
            sink,
            Arc::new(|| false),
            None,
            FollowOptions {
                appear_timeout: Duration::from_millis(100),
                cancel: None,
            },
        )
        .join()
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(seen.lock().unwrap().is_empty());
    }

    /// §39：取消标志置位后跟随者在下一次轮询退出，且不调 on_lost（它不是
    /// 「执行丢失」，只是被新的跟随者接管）。
    #[test]
    fn cancel_stops_the_follower_without_on_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("e.jsonl");
        std::fs::write(&path, "{\"kind\":\"raw\",\"seq\":0}\n").unwrap();
        let (sink, seen) = collector();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&called);
        let hook: LostHook = Box::new(move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            None
        });
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = follow_jsonl_with(
            path,
            None,
            sink,
            Arc::new(|| true),
            Some(hook),
            FollowOptions {
                appear_timeout: APPEAR_TIMEOUT,
                cancel: Some(cancel.clone()),
            },
        );
        std::thread::sleep(Duration::from_millis(300));
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !handle.is_finished() {
            assert!(std::time::Instant::now() < deadline, "取消后 2s 内必须退出");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.join().unwrap();
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "取消不是执行丢失"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
