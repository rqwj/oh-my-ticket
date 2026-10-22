//! jsonl 尾随（设计 §3.4 / KD-E3，P-6 纯轮询）：从文件读出新增的完整行，
//! 按原样推给 sink（事件形状与旧 Channel 推送一致），读到 exit 即结束；
//! exec 已不活跃且文件连续两次不增长时也结束（shim 崩溃的兜底）——此时调用
//! `on_lost`（F-EXEC-17），其返回的合成 exit 事件照常推给 sink，前端据此收尾。

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::executor::EventSink;

const POLL: Duration = Duration::from_millis(200);
/// 文件尚不存在时最多等多久（shim 还没写出 header）。
const APPEAR_TIMEOUT: Duration = Duration::from_secs(30);

/// 「不活跃却没读到 exit」时的收尾钩子：入参为合成事件应使用的 seq（已见
/// 最大 seq + 1），返回要补推给 sink 的事件（通常是合成 exit），None 不推。
pub type LostHook = Box<dyn FnOnce(u64) -> Option<serde_json::Value> + Send>;

pub fn follow_jsonl(
    path: PathBuf,
    after_seq: Option<u64>,
    sink: EventSink,
    active: Arc<dyn Fn() -> bool + Send + Sync>,
    on_lost: Option<LostHook>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut offset: u64 = 0;
        let mut next_seq: u64 = after_seq.map_or(0, |after| after + 1);
        let mut pending: Vec<u8> = Vec::new();
        let mut idle_inactive = 0u32;
        let appear_deadline = std::time::Instant::now() + APPEAR_TIMEOUT;
        loop {
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
                Err(_) if std::time::Instant::now() < appear_deadline => {}
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
}
